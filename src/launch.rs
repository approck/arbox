use anyhow::{bail, Context, Result};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use crate::host::{self, HostContext};
use crate::image;

/// One bind-mount the container needs. Mounted at the same absolute path on
/// both sides — that's what makes cargo's incremental fingerprints carry
/// across the host ↔ container boundary.
pub struct MountSpec {
    pub path: PathBuf,
    pub read_only: bool,
    /// Hard-required: launch fails if missing on host. (Optional mounts are
    /// silently skipped when their host path doesn't exist.)
    pub required: bool,
    pub hint: Option<&'static str>,
}

impl MountSpec {
    fn new(path: PathBuf, read_only: bool, required: bool, hint: Option<&'static str>) -> Self {
        Self {
            path,
            read_only,
            required,
            hint,
        }
    }
}

pub fn mount_specs(host: &HostContext) -> Vec<MountSpec> {
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
        // Agent state directories — mounted RW and optional. The agent
        // BINARIES themselves are baked into the image (claude, codex, agy,
        // grok all live at /usr/local/bin), so these mounts exist only to
        // persist credentials, history, skills, MCP config, etc. across
        // container invocations. `ensure_agent_state` (below) auto-creates
        // the relevant paths on first launch of each agent verb, so these
        // mounts are reliably attached without any host-side prep.
        MountSpec::new(h.join(".claude"), false, false, None),
        // Claude on Linux stores main config + auth state at $HOME/.claude.json
        // (a file, distinct from the .claude/ directory above).
        MountSpec::new(h.join(".claude.json"), false, false, None),
        MountSpec::new(h.join(".codex"), false, false, None),
        // Antigravity (`agy`) stores skills + MCP config + GEMINI.md under
        // ~/.gemini (the Antigravity CLI reuses the Gemini namespace);
        // per-host config under ~/.config/antigravity.
        MountSpec::new(h.join(".gemini"), false, false, None),
        MountSpec::new(h.join(".config").join("antigravity"), false, false, None),
        // Grok Build CLI stores its auth token + downloads under ~/.grok.
        MountSpec::new(h.join(".grok"), false, false, None),
        // Host's ~/.gitconfig (read-only). So git inside the container picks
        // up the user's identity, aliases, signing config, etc. Skipped if
        // absent on the host.
        MountSpec::new(h.join(".gitconfig"), true, false, None),
        // Deliberately NOT mounted: ~/.local/bin and ~/.local/share/claude.
        // Those hold the host's own agent BINARIES (~/.local/bin/{claude,
        // codex,agy} and the ~/.local/share/claude version store) — and arbox
        // runs the versions baked into the image instead, bumped via
        // `arbox update`. Mounting them would only let a host copy shadow the
        // baked binary on PATH (which is exactly the staleness bug this
        // avoids). All agent DATA — config, credentials, history, memories,
        // sessions, plans, tasks — lives in the per-agent state dirs mounted
        // above (~/.claude, ~/.claude.json, ~/.codex, ~/.gemini,
        // ~/.config/antigravity, ~/.grok), and that's what persists across
        // runs.
    ]);

    specs
}

/// Pre-create the host-side state paths an agent will write into, so the
/// bind mount has something to attach to on first run. Without this, Docker
/// silently skips missing-source mounts and the agent runs with ephemeral
/// state every launch. Each call is per-verb — only the dirs the specific
/// agent uses are touched.
fn ensure_agent_state(host: &HostContext, agent: &str) -> Result<()> {
    let h = &host.home;
    let dirs: Vec<PathBuf> = match agent {
        "claude" => vec![h.join(".claude")],
        "codex" => vec![h.join(".codex")],
        "agy" => vec![h.join(".gemini"), h.join(".config").join("antigravity")],
        "grok" => vec![h.join(".grok")],
        _ => vec![],
    };
    for d in &dirs {
        std::fs::create_dir_all(d)
            .with_context(|| format!("creating {}", d.display()))?;
    }
    // Claude's main config + auth state is a FILE next to its dir, not
    // inside it. Initialize with `{}` (parseable JSON) so claude's first
    // load doesn't choke on a zero-byte mount target.
    if agent == "claude" {
        let cj = h.join(".claude.json");
        if !cj.exists() {
            std::fs::write(&cj, "{}\n")
                .with_context(|| format!("creating {}", cj.display()))?;
        }
    }
    Ok(())
}

pub fn run_claude(extra: Vec<String>, rw: Vec<PathBuf>, ro: Vec<PathBuf>) -> Result<ExitCode> {
    let host = host::detect()?;
    host::require_git(&host)?;
    ensure_agent_state(&host, "claude")?;
    // The container IS the sandbox, so granting claude full permissions
    // inside is the correct posture.
    let mut argv = vec![
        "claude".to_string(),
        "--dangerously-skip-permissions".to_string(),
    ];
    argv.extend(extra);
    run(host, argv, rw, ro)
}

pub fn run_codex(extra: Vec<String>, rw: Vec<PathBuf>, ro: Vec<PathBuf>) -> Result<ExitCode> {
    let host = host::detect()?;
    host::require_git(&host)?;
    ensure_agent_state(&host, "codex")?;
    let mut argv = vec![
        "codex".to_string(),
        "--dangerously-bypass-approvals-and-sandbox".to_string(),
    ];
    argv.extend(extra);
    run(host, argv, rw, ro)
}

pub fn run_agy(extra: Vec<String>, rw: Vec<PathBuf>, ro: Vec<PathBuf>) -> Result<ExitCode> {
    let host = host::detect()?;
    host::require_git(&host)?;
    ensure_agent_state(&host, "agy")?;
    // No documented `--dangerously-*` / `--yolo` flag for Antigravity yet —
    // forward args verbatim. The Docker boundary is still the sandbox; agy
    // itself just runs with whatever approval mode it defaults to. Note
    // that libsecret won't work inside the container (no dbus session), so
    // first-time auth typically goes through agy's SSH-style URL+code flow.
    let mut argv = vec!["agy".to_string()];
    argv.extend(extra);
    run(host, argv, rw, ro)
}

pub fn run_grok(extra: Vec<String>, rw: Vec<PathBuf>, ro: Vec<PathBuf>) -> Result<ExitCode> {
    let host = host::detect()?;
    host::require_git(&host)?;
    ensure_agent_state(&host, "grok")?;
    // Grok Build's safety story is its plan-mode review, not a global
    // approval-bypass flag — forward args verbatim. Auth lives in
    // ~/.grok/auth.json (file-based, no keyring dependency), which the
    // ~/.grok mount in `mount_specs` persists across runs.
    let mut argv = vec!["grok".to_string()];
    argv.extend(extra);
    run(host, argv, rw, ro)
}

pub fn run_playwright(extra: Vec<String>, rw: Vec<PathBuf>, ro: Vec<PathBuf>) -> Result<ExitCode> {
    let host = host::detect()?;
    host::require_git(&host)?;
    // `playwright` is npm-installed globally in the image. Browsers are
    // baked in at /opt/ms-playwright (PLAYWRIGHT_BROWSERS_PATH set in the
    // Dockerfile), so this works without any host-side setup.
    let mut argv = vec!["playwright".to_string()];
    argv.extend(extra);
    run(host, argv, rw, ro)
}

pub fn run_bash(rw: Vec<PathBuf>, ro: Vec<PathBuf>) -> Result<ExitCode> {
    let host = host::detect()?;
    host::require_git(&host)?;
    run(
        host,
        vec!["/bin/bash".to_string(), "-l".to_string()],
        rw,
        ro,
    )
}

pub fn run_argv(argv: Vec<String>, rw: Vec<PathBuf>, ro: Vec<PathBuf>) -> Result<ExitCode> {
    if argv.is_empty() {
        bail!("arbox run needs a command");
    }
    let host = host::detect()?;
    host::require_git(&host)?;
    run(host, argv, rw, ro)
}

fn run(
    host: HostContext,
    argv: Vec<String>,
    extra_rw: Vec<PathBuf>,
    extra_ro: Vec<PathBuf>,
) -> Result<ExitCode> {
    ensure_docker_installed()?;
    host::require_supported_distro(&host)?;
    let mut mounts = mount_specs(&host);
    append_extra_mounts(&mut mounts, &extra_rw, false)?;
    append_extra_mounts(&mut mounts, &extra_ro, true)?;
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
    cmd.args(["--hostname", "ARBOX", "--network", "host", "--add-host", "ARBOX:127.0.0.1"]);
    // /dev/shm defaults to 64 MB in Docker, which is enough to crash Chromium
    // on any non-trivial page. Bump it once here so every Playwright test
    // doesn't have to remember --disable-dev-shm-usage.
    cmd.args(["--shm-size", "1g"]);
    cmd.arg("--user").arg(format!("{}:{}", host.uid, host.gid));
    cmd.arg("--workdir").arg(&host.cwd);
    cmd.arg("-e").arg(format!("HOME={}", host.home.display()));
    cmd.arg("-e").arg(format!("USER={}", host.username));
    cmd.arg("-e").arg(format!("TERM={}", host.term));
    cmd.arg("-e").arg("LANG=C.UTF-8");

    for m in &mounts {
        if !m.path.exists() {
            // Optional + missing: skip. Required + missing was caught above.
            continue;
        }
        let arg = if m.read_only {
            format!(
                "type=bind,src={},dst={},readonly",
                m.path.display(),
                m.path.display()
            )
        } else {
            format!(
                "type=bind,src={},dst={}",
                m.path.display(),
                m.path.display()
            )
        };
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
        if m.required && !m.path.exists() {
            let hint = m.hint.map(|h| format!(" — {h}")).unwrap_or_default();
            bail!(
                "required mount source {} does not exist{hint}",
                m.path.display()
            );
        }
    }
    Ok(())
}
