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
        // Agent dotfiles are NOT marked required at the mount-list level —
        // `arbox bash` and `arbox run` shouldn't fail just because the user
        // hasn't run claude/codex on the host. Verbs that actually launch an
        // agent enforce existence themselves via `require_agent_dotfiles`.
        MountSpec::new(h.join(".claude"), false, false, None),
        // Claude on Linux stores main config + auth state at $HOME/.claude.json
        // (a file, distinct from the .claude/ directory above). This is what
        // makes claude's credentials persist across container invocations.
        MountSpec::new(h.join(".claude.json"), false, false, None),
        MountSpec::new(h.join(".codex"), false, false, None),
        // Host's ~/.gitconfig (read-only). So git inside the container picks
        // up the user's identity, aliases, signing config, etc. Skipped if
        // absent on the host.
        MountSpec::new(h.join(".gitconfig"), true, false, None),
        // Optional: where claude/codex binaries live on this host. Skipped
        // silently when absent (e.g. on a host that uses a different layout).
        MountSpec::new(h.join(".local").join("bin"), true, false, None),
        MountSpec::new(
            h.join(".local").join("share").join("claude"),
            true,
            false,
            None,
        ),
    ]);

    specs
}

/// Per-verb pre-flight: error out if the dotfiles a specific agent needs
/// aren't present on the host. Bash and `run` skip this check entirely.
fn require_agent_dotfiles(host: &HostContext, agent: &str) -> Result<()> {
    let needs: &[(&str, PathBuf, &str)] = match agent {
        "claude" => &[
            (
                "~/.claude",
                host.home.join(".claude"),
                "run `claude` once on the host to set up its config dir",
            ),
            (
                "~/.claude.json",
                host.home.join(".claude.json"),
                "run `claude` once on the host to create ~/.claude.json",
            ),
        ][..],
        "codex" => &[(
            "~/.codex",
            host.home.join(".codex"),
            "run `codex` once on the host to authenticate first",
        )][..],
        _ => &[][..],
    };
    for (label, path, hint) in needs {
        if !path.exists() {
            bail!("{label} does not exist — {hint}");
        }
    }
    Ok(())
}

pub fn run_claude(extra: Vec<String>, rw: Vec<PathBuf>, ro: Vec<PathBuf>) -> Result<ExitCode> {
    let host = host::detect()?;
    host::require_git(&host)?;
    require_agent_dotfiles(&host, "claude")?;
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
    require_agent_dotfiles(&host, "codex")?;
    let mut argv = vec![
        "codex".to_string(),
        "--dangerously-bypass-approvals-and-sandbox".to_string(),
    ];
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
    cmd.args(["--hostname", "ARBOX", "--network", "host"]);
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
