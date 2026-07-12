use anyhow::{bail, Context, Result};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use crate::host::{self, HostContext};
use crate::image;

/// One bind-mount the container needs. Almost every mount lands at the same
/// absolute path on both sides (`src == dst`) — that's what makes cargo's
/// incremental fingerprints carry across the host ↔ container boundary.
///
/// The exception is `--profile NAME`: each agent's entire state tree is sourced
/// from under `~/.arbox/profiles/NAME/` while `dst` stays the canonical path
/// the agent expects (e.g. src `~/.arbox/profiles/personal/.claude` →
/// dst `~/.claude`). The agent reads/writes the standard location, unaware its
/// whole config — auth *and* history together — actually lives in the profile
/// dir, so the two can never drift out of sync the way redirecting auth alone
/// would risk.
pub struct MountSpec {
    /// Host source path. Existence is checked against this.
    pub src: PathBuf,
    /// Container destination path. Equals `src` unless this is a redirect.
    pub dst: PathBuf,
    pub read_only: bool,
    /// Hard-required: launch fails if missing on host. (Optional mounts are
    /// silently skipped when their host path doesn't exist.)
    pub required: bool,
    pub hint: Option<&'static str>,
}

impl MountSpec {
    /// Same path on both sides — the common case.
    fn new(path: PathBuf, read_only: bool, required: bool, hint: Option<&'static str>) -> Self {
        Self {
            src: path.clone(),
            dst: path,
            read_only,
            required,
            hint,
        }
    }

    /// Mount host `src` at container `dst` (distinct paths). Used for
    /// `--profile` auth redirects.
    fn redirected(
        src: PathBuf,
        dst: PathBuf,
        read_only: bool,
        required: bool,
        hint: Option<&'static str>,
    ) -> Self {
        Self {
            src,
            dst,
            read_only,
            required,
            hint,
        }
    }
}

/// One coding agent's registry entry. This table is the single source of
/// truth for the per-agent state layout: `mount_specs` derives the mount list
/// from it, `ensure_agent_state` pre-creates its paths, and
/// `ensure_all_agent_state` iterates its names — add an agent (or move a
/// path) here and every consumer follows. The Dockerfile's XDG parent-dir
/// pre-creation (`install -d` in src/Dockerfile) must cover the parents of
/// any nested path listed here.
struct AgentSpec {
    /// Verb name — identical to the binary name inside the image.
    name: &'static str,
    /// Home-relative state dirs, mounted RW and pre-created on first launch.
    dirs: &'static [&'static str],
    /// State FILES seeded with initial contents so the bind mount has a
    /// source to attach to (a missing source would be silently skipped,
    /// making the file ephemeral in the container).
    seed_files: &'static [(&'static str, &'static str)],
    /// The agent resolves its dirs through the XDG base-dir spec on the host
    /// (honors XDG_CONFIG_HOME / XDG_DATA_HOME), so the host-side mount
    /// sources must resolve the same way.
    xdg: bool,
}

/// Per-agent state registry. Whole-tree redirected into the profile dir under
/// `--profile`, or mounted at the standard host location by default. Listing
/// the data paths (not the binaries — those are baked into the image at
/// /usr/local/bin) is what persists credentials, history, skills, MCP config,
/// memories, and sessions across container runs.
const AGENTS: &[AgentSpec] = &[
    // claude: the .claude/ dir (sessions, memories, settings, credentials,
    // plans, tasks) plus the .claude.json config/auth-identity file beside
    // it, seeded with `{}` (parseable JSON) so claude's first load doesn't
    // choke on a zero-byte mount target.
    AgentSpec {
        name: "claude",
        dirs: &[".claude"],
        seed_files: &[(".claude.json", "{}\n")],
        xdg: false,
    },
    AgentSpec {
        name: "codex",
        dirs: &[".codex"],
        seed_files: &[],
        xdg: false,
    },
    // opencode: global config (opencode.json, themes, agents, commands) under
    // ~/.config/opencode; auth tokens + session storage under
    // ~/.local/share/opencode (auth.json, storage/). The opencode BINARY is
    // baked into the image, but its data dir is not purely inert state:
    // opencode downloads helper executables (LSP servers, ripgrep, fzf) into
    // ~/.local/share/opencode/bin, so this RW mount carries executables in
    // both directions.
    AgentSpec {
        name: "opencode",
        dirs: &[".config/opencode", ".local/share/opencode"],
        seed_files: &[],
        xdg: true,
    },
    // agy: Antigravity reuses the Gemini namespace for skills/MCP/GEMINI.md,
    // with per-host config under ~/.config/antigravity. Not known to honor
    // XDG overrides — the ~/.config path is fixed.
    AgentSpec {
        name: "agy",
        dirs: &[".gemini", ".config/antigravity"],
        seed_files: &[],
        xdg: false,
    },
    AgentSpec {
        name: "grok",
        dirs: &[".grok"],
        seed_files: &[],
        xdg: false,
    },
];

/// Flat view of every home-relative state path in the registry, paired with
/// its agent's XDG flag. Drives the mount list and the tests.
fn agent_state_paths() -> impl Iterator<Item = (&'static str, bool)> {
    AGENTS.iter().flat_map(|a| {
        a.dirs
            .iter()
            .copied()
            .chain(a.seed_files.iter().map(|(rel, _)| *rel))
            .map(move |rel| (rel, a.xdg))
    })
}

/// Root of a named profile's isolated state tree on the host.
fn profile_root(home: &Path, name: &str) -> PathBuf {
    home.join(".arbox").join("profiles").join(name)
}

/// Resolve where an agent state path actually lives on the HOST.
///
/// Under `--profile` everything roots at the profile dir (XDG overrides are
/// deliberately ignored — the profile tree is arbox's own layout). Otherwise
/// XDG-aware entries resolve through the host's XDG base dirs, falling back
/// to the literal home-relative path. The container side always uses the
/// home-relative default: no XDG vars are forwarded inside, so the
/// in-container layout stays fixed while the host source follows wherever
/// the host's own agent actually keeps its state.
fn state_source(home: &Path, rel: &str, xdg: bool, profile: Option<&str>) -> PathBuf {
    match profile {
        Some(p) => profile_root(home, p).join(rel),
        None if xdg => {
            xdg_override(rel, &|var| std::env::var_os(var)).unwrap_or_else(|| home.join(rel))
        }
        None => home.join(rel),
    }
}

/// XDG base-dir override for `rel`, if the matching env var holds an
/// absolute path (the basedir spec says relative values must be ignored).
/// None means "use the default location".
fn xdg_override(rel: &str, get: &dyn Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    let (var, suffix) = if let Some(s) = rel.strip_prefix(".config/") {
        ("XDG_CONFIG_HOME", s)
    } else if let Some(s) = rel.strip_prefix(".local/share/") {
        ("XDG_DATA_HOME", s)
    } else {
        return None;
    };
    let base = PathBuf::from(get(var)?);
    base.is_absolute().then(|| base.join(suffix))
}

/// Build the bind-mount list. With no `profile`, agent state mounts at the
/// standard host locations (shared with your host's own agents). With
/// `--profile NAME`, each agent's entire state tree is sourced from
/// `~/.arbox/profiles/NAME/` instead — auth and history move together, so a
/// second subscription stays fully self-consistent and never touches the
/// default. Non-agent mounts (workspace, Rust toolchain, ~/.gitconfig) are
/// always shared regardless of profile.
pub fn mount_specs(host: &HostContext, profile: Option<&str>) -> Vec<MountSpec> {
    let h = &host.home;
    let mut specs: Vec<MountSpec> = Vec::new();

    if let Some(workspace) = &host.workspace_root {
        specs.push(MountSpec::new(workspace.clone(), false, true, None));
        // Worktree case: the workspace's `.git` is a file pointing into a
        // separate common git dir (e.g. <main-repo>/.git). Mount it so git
        // operations resolve inside the container. Skipped for normal
        // checkouts where the common dir is `<workspace>/.git` and already
        // inside the workspace mount.
        if let Some(common) = &host.git_common_dir {
            if !common.starts_with(workspace) {
                specs.push(MountSpec::new(
                    common.clone(),
                    false,
                    true,
                    Some("git common dir not found"),
                ));
            }
        }
    }

    if !cfg!(target_family = "windows") {
        specs.extend([
            MountSpec::new(
                h.join(".cargo"),
                false,
                true,
                Some("install rustup on the host first (https://rustup.rs)"),
            ),
            MountSpec::new(
                h.join(".rustup"),
                true,
                true,
                Some("install rustup on the host first (https://rustup.rs)"),
            ),
        ]);
    }

    // Agent state dirs/files — mounted RW and optional. `ensure_agent_state`
    // pre-creates the relevant sources on first launch of each agent verb, so
    // these mounts reliably attach without any host-side prep. Under a profile
    // the source moves into the profile dir, and for XDG-aware agents it
    // follows the host's XDG base dirs; the destination (what the agent sees
    // in the container) always stays canonical.
    for (rel, xdg) in agent_state_paths() {
        let dst = h.join(rel);
        let src = state_source(h, rel, xdg, profile);
        if src == dst {
            specs.push(MountSpec::new(dst, false, false, None));
        } else {
            specs.push(MountSpec::redirected(src, dst, false, false, None));
        }
    }

    // Host's ~/.gitconfig (read-only) — git identity isn't tied to an agent
    // subscription, so it stays shared across profiles. Skipped if absent.
    specs.push(MountSpec::new(h.join(".gitconfig"), true, false, None));

    // approck's data dir (~/.local/share/approck), read-only. Optional —
    // skipped if absent.
    specs.push(MountSpec::new(
        h.join(".local").join("share").join("approck"),
        true,
        false,
        None,
    ));

    // Deliberately NOT mounted: ~/.local/bin and ~/.local/share/claude. Those
    // hold the host's own agent BINARIES (claude/codex/agy live there) — arbox
    // runs the versions baked into the image instead (bumped via `arbox
    // update`), so mounting the host copies would only let them shadow the
    // baked binary on PATH (the exact staleness bug this avoids). All agent
    // DATA lives in the state paths above.

    specs
}

/// On Windows, fix git worktree paths so they resolve correctly in the
/// container. The `.git` file in a worktree contains an absolute Windows path
/// that doesn't work inside the container. Replace it with a relative path
/// and add the container path to git's safe.directory. Returns the container
/// path if one was added (for cleanup on exit).
fn fixup_windows_worktree(host: &HostContext) -> Result<Option<String>> {
    if !cfg!(target_family = "windows") {
        return Ok(None);
    }

    let Some(workspace) = &host.workspace_root else {
        return Ok(None);
    };

    let git_file = workspace.join(".git");
    // Check if .git is a file (worktree) vs directory (normal checkout)
    if !git_file.is_file() {
        return Ok(None);
    }

    let git_content = std::fs::read_to_string(&git_file).context("reading .git file")?;

    let Some(git_dir) = git_content
        .lines()
        .find_map(|l| l.strip_prefix("gitdir: "))
        .map(|path| PathBuf::from(path).canonicalize())
        .transpose()?
    else {
        return Ok(None);
    };

    // Calculate relative path from worktree to the git common dir
    if let Some(rel_path) = pathdiff::diff_paths(git_dir, workspace) {
        let rel_str = rel_path.to_string_lossy().replace("\\", "/");
        let new_content = format!("gitdir: {}\n", rel_str);

        if git_content != new_content {
            std::fs::write(&git_file, &new_content)
                .context("writing .git file with relative path")?;
        }

        // Add the container path to git's safe.directory
        let container_path = crate::path::to_container(workspace)?;
        let mut cmd = Command::new("git");
        cmd.args([
            "config",
            "--global",
            "--add",
            "safe.directory",
            &container_path,
        ]);
        let _ = cmd.status(); // Ignore errors; this is best-effort

        return Ok(Some(container_path));
    }

    Ok(None)
}

/// Pre-create the host-side state paths an agent will write into, so the
/// bind mount has something to attach to on first run. Without this, Docker
/// silently skips missing-source mounts and the agent runs with ephemeral
/// state every launch. Each call is per-verb — only the paths the specific
/// agent's `AGENTS` entry lists are touched.
///
/// `profile` only changes WHERE these paths are rooted: the home directory
/// (or the host's XDG base dirs, for XDG-aware agents) by default, or
/// `~/.arbox/profiles/NAME/` for a named profile. The layout underneath
/// (`.claude/`, `.claude.json`, …) is identical either way, which is what
/// lets the profile dir stand in as a self-contained agent home.
fn ensure_agent_state(host: &HostContext, agent: &str, profile: Option<&str>) -> Result<()> {
    let Some(spec) = AGENTS.iter().find(|a| a.name == agent) else {
        return Ok(());
    };
    for rel in spec.dirs {
        let d = state_source(&host.home, rel, spec.xdg, profile);
        std::fs::create_dir_all(&d).with_context(|| format!("creating {}", d.display()))?;
    }
    // Seeded files (claude's .claude.json) get initial contents so the
    // agent's first load doesn't choke on a zero-byte mount target.
    for (rel, contents) in spec.seed_files {
        let f = state_source(&host.home, rel, spec.xdg, profile);
        if !f.exists() {
            std::fs::write(&f, contents).with_context(|| format!("creating {}", f.display()))?;
        }
    }
    Ok(())
}

/// Per-invocation launch options shared by every verb: the user's extra
/// bind-mount paths and the optional `--profile` name. Bundled so adding a
/// cross-cutting knob doesn't ripple through every verb function's signature.
#[derive(Default)]
pub struct Opts {
    pub rw: Vec<PathBuf>,
    pub ro: Vec<PathBuf>,
    pub profile: Option<String>,
}

/// Validate a `--profile` name before it becomes part of a filename. Reject
/// anything that could escape its directory or produce a surprising sibling.
pub fn validate_profile_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && !name.starts_with('-')
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !ok {
        bail!(
            "invalid --profile name {name:?}: use letters, digits, '-' or '_' \
             (and don't start with '-' or '.')"
        );
    }
    Ok(())
}

/// Shared skeleton for every agent verb: detect the host, demand a git
/// workspace, pre-create the agent's state paths, then run the agent's
/// binary (same name as the verb) with `injected` flags ahead of the user's
/// trailing args.
fn run_agent(
    agent: &'static str,
    injected: &[&str],
    extra: Vec<String>,
    opts: Opts,
) -> Result<ExitCode> {
    let host = host::detect()?;
    host::require_git(&host)?;
    ensure_agent_state(&host, agent, opts.profile.as_deref())?;
    let mut argv = vec![agent.to_string()];
    argv.extend(injected.iter().map(|f| f.to_string()));
    argv.extend(extra);
    run(host, argv, opts)
}

pub fn run_claude(extra: Vec<String>, opts: Opts) -> Result<ExitCode> {
    // The container IS the sandbox, so granting claude full permissions
    // inside is the correct posture.
    run_agent("claude", &["--dangerously-skip-permissions"], extra, opts)
}

pub fn run_codex(extra: Vec<String>, opts: Opts) -> Result<ExitCode> {
    run_agent(
        "codex",
        &["--dangerously-bypass-approvals-and-sandbox"],
        extra,
        opts,
    )
}

pub fn run_opencode(extra: Vec<String>, opts: Opts) -> Result<ExitCode> {
    // opencode has no approval-bypass flag to inject — its default permission
    // posture is already permissive, and tightening it lives in the user's
    // mounted ~/.config/opencode/opencode.json. Auth is file-based
    // (~/.local/share/opencode/auth.json), no keyring needed, so `opencode
    // auth login` inside the box persists via the state mount. Local
    // providers on the host (e.g. Ollama on localhost:11434) are reachable
    // on Linux, where --network host is the real host network; on Windows,
    // Docker Desktop only honors --network host with its opt-in
    // host-networking feature enabled.
    run_agent("opencode", &[], extra, opts)
}

pub fn run_agy(extra: Vec<String>, opts: Opts) -> Result<ExitCode> {
    // No documented `--dangerously-*` / `--yolo` flag for Antigravity yet —
    // forward args verbatim. The Docker boundary is still the sandbox; agy
    // itself just runs with whatever approval mode it defaults to. Note
    // that libsecret won't work inside the container (no dbus session), so
    // first-time auth typically goes through agy's SSH-style URL+code flow.
    // Under `--profile`, agy's whole ~/.gemini + ~/.config/antigravity move
    // into the profile tree, so whatever it persists there is isolated too.
    run_agent("agy", &[], extra, opts)
}

pub fn run_grok(extra: Vec<String>, opts: Opts) -> Result<ExitCode> {
    // Grok Build's safety story is its plan-mode review, not a global
    // approval-bypass flag — forward args verbatim. Auth lives in
    // ~/.grok/auth.json (file-based, no keyring dependency), which the
    // ~/.grok mount in `mount_specs` persists across runs.
    run_agent("grok", &[], extra, opts)
}

pub fn run_playwright(extra: Vec<String>, opts: Opts) -> Result<ExitCode> {
    let host = host::detect()?;
    host::require_git(&host)?;
    // `playwright` is npm-installed globally in the image. Browsers are
    // baked in at /opt/ms-playwright (PLAYWRIGHT_BROWSERS_PATH set in the
    // Dockerfile), so this works without any host-side setup.
    let mut argv = vec!["playwright".to_string()];
    argv.extend(extra);
    run(host, argv, opts)
}

pub fn run_bash(opts: Opts) -> Result<ExitCode> {
    let host = host::detect()?;
    host::require_git(&host)?;
    // No single agent verb here, so provision every file-auth agent's profile
    // token — whichever agent the user launches from the shell gets isolated.
    ensure_all_agent_state(&host, opts.profile.as_deref())?;
    run(host, vec!["/bin/bash".to_string(), "-l".to_string()], opts)
}

pub fn run_argv(argv: Vec<String>, opts: Opts) -> Result<ExitCode> {
    if argv.is_empty() {
        bail!("arbox run needs a command");
    }
    let host = host::detect()?;
    host::require_git(&host)?;
    ensure_all_agent_state(&host, opts.profile.as_deref())?;
    run(host, argv, opts)
}

/// Seed profile state for every agent. Used by the agent-agnostic verbs
/// (`bash`, `run`) so whichever agent the user launches from the shell finds
/// its profile tree ready; a no-op without a profile.
fn ensure_all_agent_state(host: &HostContext, profile: Option<&str>) -> Result<()> {
    if profile.is_some() {
        for a in AGENTS {
            ensure_agent_state(host, a.name, profile)?;
        }
    }
    Ok(())
}

fn run(host: HostContext, argv: Vec<String>, opts: Opts) -> Result<ExitCode> {
    ensure_docker_installed()?;
    host::require_supported_distro(&host)?;
    let added_safe_dir = fixup_windows_worktree(&host)?;
    let mut mounts = mount_specs(&host, opts.profile.as_deref());
    append_extra_mounts(&mut mounts, &opts.rw, false)?;
    append_extra_mounts(&mut mounts, &opts.ro, true)?;
    verify_required_mounts_exist(&mounts)?;
    let tag = image::ensure_built(&host)?;

    let mut cmd = Command::new("docker");
    cmd.args(["run", "--rm"]);
    // `-i` keeps stdin attached (needed for both interactive shells and piped
    // input). `-t` only when stdin is a real TTY — otherwise docker errors
    // with "input device is not a TTY" under `arbox run -- foo | bar`, hooks,
    // CI, etc.
    cmd.arg("-i");
    if std::io::stdin().is_terminal() {
        cmd.arg("-t");
    }
    // Distinctive uppercase hostname — `jason@ARBOX:~$` makes it obvious at
    // a glance that you're inside the sandbox shell vs. the host shell.
    // Adding `--add-host ARBOX:127.0.0.1` ensures that sudo inside the container
    // can resolve its own hostname without throwing warnings.
    cmd.args([
        "--hostname",
        "ARBOX",
        "--network",
        "host",
        "--add-host",
        "ARBOX:127.0.0.1",
    ]);
    // /dev/shm defaults to 64 MB in Docker, which is enough to crash Chromium
    // on any non-trivial page. Bump it once here so every Playwright test
    // doesn't have to remember --disable-dev-shm-usage.
    cmd.args(["--shm-size", "1g"]);
    cmd.arg("--user").arg(format!("{}:{}", host.uid, host.gid));

    cmd.arg("--workdir")
        .arg(crate::path::to_container(&host.cwd)?);
    cmd.arg("-e")
        .arg(format!("HOME={}", crate::path::to_container(&host.home)?));

    cmd.arg("-e").arg(format!("USER={}", host.username));
    cmd.arg("-e").arg(format!("TERM={}", host.term));
    cmd.arg("-e").arg("LANG=C.UTF-8");
    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        cmd.arg("-e").arg(format!("ANTHROPIC_API_KEY={key}"));
    }

    for m in &mounts {
        if !m.src.exists() {
            // Optional + missing: skip. Required + missing was caught above.
            continue;
        }

        let src = crate::path::to_mount_src(&m.src);
        let dst = crate::path::to_container(&m.dst)?;
        let mut arg = format!("type=bind,src={src},dst={dst}");
        if m.read_only {
            arg.push_str(",readonly");
        }
        cmd.arg("--mount").arg(arg);
    }

    add_wayland_clipboard(&mut cmd);

    cmd.arg(&tag);
    for a in &argv {
        cmd.arg(a);
    }

    let status = cmd
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("running `docker run`")?;

    // Clean up Windows worktree safe.directory entry after container exits
    if let Some(safe_dir_path) = added_safe_dir {
        let mut cleanup_cmd = Command::new("git");
        cleanup_cmd.args([
            "config",
            "--global",
            "--unset",
            "safe.directory",
            &safe_dir_path,
        ]);
        let _ = cleanup_cmd.status(); // Ignore errors; this is best-effort
    }

    Ok(match status.code() {
        Some(c) if (0..=255).contains(&c) => ExitCode::from(c as u8),
        _ => ExitCode::FAILURE,
    })
}

/// Expose the host's Wayland display socket so claude's image-paste flow
/// (`wl-paste --type image/png`) can read the clipboard. Wayland-only: we
/// don't mount the X11 socket. No-op when there's no Wayland session on the
/// host (e.g. headless server, X11-only desktop).
///
/// Mounts JUST the socket file — not `$XDG_RUNTIME_DIR` — so the rest of the
/// runtime dir (D-Bus session bus, gnome-keyring control socket, etc.) stays
/// on the host. We set `WAYLAND_DISPLAY` to the absolute socket path so
/// libwayland connects directly without resolving against `XDG_RUNTIME_DIR`.
fn add_wayland_clipboard(cmd: &mut Command) {
    let Ok(wd) = std::env::var("WAYLAND_DISPLAY") else {
        return;
    };
    let socket: PathBuf = if Path::new(&wd).is_absolute() {
        PathBuf::from(&wd)
    } else {
        let Some(rd) = std::env::var_os("XDG_RUNTIME_DIR") else {
            return;
        };
        PathBuf::from(rd).join(&wd)
    };
    if !socket.exists() {
        return;
    }
    let Some(socket_str) = socket.to_str() else {
        return;
    };
    cmd.arg("--mount")
        .arg(format!("type=bind,src={socket_str},dst={socket_str}"));
    cmd.arg("-e").arg(format!("WAYLAND_DISPLAY={socket_str}"));
}

/// Resolve and append user-specified `--rw`/`--ro` paths as required mounts.
/// Each path is canonicalized (so symlinks and relative paths resolve to a
/// real absolute location) and mounted at the same path on both sides.
fn append_extra_mounts(
    mounts: &mut Vec<MountSpec>,
    paths: &[PathBuf],
    read_only: bool,
) -> Result<()> {
    let flag = if read_only { "--ro" } else { "--rw" };
    for p in paths {
        let abs = p
            .canonicalize()
            .with_context(|| format!("{flag} {}: cannot resolve", p.display()))?;
        mounts.push(MountSpec::new(abs, read_only, true, None));
    }
    Ok(())
}

fn ensure_docker_installed() -> Result<()> {
    let out = Command::new("docker").arg("version").output();
    match out {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => bail!(
            "`docker version` exited with {}: {}",
            o.status,
            String::from_utf8_lossy(&o.stderr).trim()
        ),
        Err(e) => bail!(
            "`docker` is not on PATH ({e}). Install Docker first: https://docs.docker.com/engine/install/"
        ),
    }
}

fn verify_required_mounts_exist(mounts: &[MountSpec]) -> Result<()> {
    for m in mounts {
        if m.required && !m.src.exists() {
            let hint = m.hint.map(|h| format!(" — {h}")).unwrap_or_default();
            bail!(
                "required mount source {} does not exist{hint}",
                m.src.display()
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_host() -> HostContext {
        HostContext {
            uid: 1000,
            gid: 1000,
            username: "jason".to_string(),
            home: PathBuf::from("/home/jason"),
            cwd: PathBuf::from("/home/jason/code/app"),
            term: "xterm".to_string(),
            distro_id: "ubuntu".to_string(),
            distro_codename: "noble".to_string(),
            workspace_root: None,
            git_common_dir: None,
        }
    }

    /// Find the mount whose container destination is `dst`.
    fn dst<'a>(specs: &'a [MountSpec], dst: &str) -> Option<&'a MountSpec> {
        specs.iter().find(|m| m.dst.as_path() == Path::new(dst))
    }

    #[test]
    fn default_mounts_agent_state_at_home() {
        let specs = mount_specs(&fake_host(), None);
        // Every agent state path mounts at its canonical home destination,
        // same-path (shared with the host) — except that XDG-aware entries
        // legitimately redirect when the test environment itself carries
        // XDG_CONFIG_HOME/XDG_DATA_HOME overrides, so only pin those down
        // when the environment is clean.
        let xdg_env_set = std::env::var_os("XDG_CONFIG_HOME").is_some()
            || std::env::var_os("XDG_DATA_HOME").is_some();
        for (rel, xdg) in agent_state_paths() {
            let m = dst(&specs, &format!("/home/jason/{rel}"))
                .unwrap_or_else(|| panic!("missing mount for {rel}"));
            if !xdg || !xdg_env_set {
                assert_eq!(m.src, m.dst, "{rel} must be a same-path (shared) mount");
            }
        }
    }

    #[test]
    fn profile_redirects_whole_agent_tree_into_profile_dir() {
        let specs = mount_specs(&fake_host(), Some("personal"));

        // Every agent state path — dirs and the .claude.json file alike — is
        // sourced from the profile dir while the destination stays canonical.
        // XDG overrides must NOT apply under a profile.
        for (rel, _) in agent_state_paths() {
            let m = dst(&specs, &format!("/home/jason/{rel}"))
                .unwrap_or_else(|| panic!("missing mount for {rel}"));
            assert_eq!(
                m.src,
                PathBuf::from(format!("/home/jason/.arbox/profiles/personal/{rel}")),
                "{rel} must be sourced from the profile dir"
            );
        }

        // Non-agent mounts stay shared even under a profile. ~/.gitconfig is
        // mounted on every platform; ~/.cargo and ~/.rustup only on non-Windows
        // hosts (Windows bakes the toolchain into the image instead), so guard
        // those so the test doesn't panic on a Windows build.
        let mut shared = vec!["/home/jason/.gitconfig"];
        if !cfg!(target_family = "windows") {
            shared.extend(["/home/jason/.cargo", "/home/jason/.rustup"]);
        }
        for d in shared {
            let m = dst(&specs, d).unwrap();
            assert_eq!(m.src, m.dst, "{d} must stay shared across profiles");
        }
    }

    // Path::is_absolute has Windows semantics ("/xdg" isn't absolute there);
    // XDG resolution is a Unix concern, so pin its behavior on Unix only.
    #[cfg(not(target_family = "windows"))]
    #[test]
    fn xdg_override_resolution() {
        use std::ffi::OsString;

        // No env var set → no override.
        assert_eq!(xdg_override(".config/opencode", &|_| None), None);

        // Absolute XDG_DATA_HOME redirects the data dir.
        let get = |var: &str| (var == "XDG_DATA_HOME").then(|| OsString::from("/xdg/data"));
        assert_eq!(
            xdg_override(".local/share/opencode", &get),
            Some(PathBuf::from("/xdg/data/opencode"))
        );
        // A rel outside the XDG prefixes never resolves.
        assert_eq!(xdg_override(".claude", &get), None);

        // Relative base-dir values are invalid per the spec — ignored.
        let rel_get = |_: &str| Some(OsString::from("relative/dir"));
        assert_eq!(xdg_override(".config/opencode", &rel_get), None);
    }

    #[test]
    fn profile_name_validation() {
        for good in ["personal", "work", "acct-2", "a_b", "Team1"] {
            assert!(
                validate_profile_name(good).is_ok(),
                "{good} should be valid"
            );
        }
        for bad in [
            "",
            "bad/name",
            "../escape",
            ".hidden",
            "-leading",
            "has space",
        ] {
            assert!(
                validate_profile_name(bad).is_err(),
                "{bad} should be invalid"
            );
        }
    }
}
