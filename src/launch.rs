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

impl std::fmt::Display for MountSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mode = if self.read_only { "ro" } else { "rw" };
        let exists = self.path.exists();
        let suffix = match (exists, self.required) {
            (true, _) => "",
            (false, true) => "  [missing — required]",
            (false, false) => "  [missing — skipped]",
        };
        write!(f, "  {} ({mode}){suffix}", self.path.display())?;

        Ok(())
    }
}

impl HostContext {
    pub fn mount_specs(self: &HostContext) -> Vec<MountSpec> {
        let h = &self.home;
        let mut specs: Vec<MountSpec> = Vec::new();

        if let Some(workspace) = &self.workspace_root {
            specs.push(MountSpec::new(workspace.clone(), false, true, None));
            // Worktree case: the workspace's `.git` is a file pointing into a
            // separate common git dir (e.g. <main-repo>/.git). Mount it so git
            // operations resolve inside the container. Skipped for normal
            // checkouts where the common dir is `<workspace>/.git` and already
            // inside the workspace mount.
            if let Some(common) = &self.git_common_dir {
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
            MountSpec::new(h.join(".codex"), false, false, None),
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

        // Claude on Linux stores main config + auth state at $HOME/.claude.json
        // (a file, distinct from the .claude/ directory above). Docker doesn't support
        // binding individual files on Windows, so skip it there.
        #[cfg(target_family = "unix")]
        specs.push(MountSpec::new(h.join(".claude.json"), false, false, None));

        specs
    }
}

/// Strip UNC prefix (\\?\) from Windows canonicalized paths so Docker can use them.
/// Docker mounts don't understand UNC paths on Windows.
fn normalize_for_docker(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(stripped) = s.strip_prefix(r"\\?\") {
        PathBuf::from(stripped)
    } else {
        p.to_path_buf()
    }
}

/// Stage host .claude.json into the mounted .claude dir so the container can pick it up.
/// Best-effort: if the source doesn't exist or the copy fails, we continue silently.
#[cfg(target_family = "windows")]
fn stage_claude_json_in(home: &std::path::Path) {
    let src = home.join(".claude.json");
    let dst = home.join(".claude").join(".claude.json");
    if src.exists() {
        let _ = std::fs::copy(&src, &dst);
    }
}

/// After the container exits, promote .claude/.claude.json (written by the entrypoint)
/// back to the canonical host location. Best-effort.
#[cfg(target_family = "windows")]
fn stage_claude_json_out(home: &std::path::Path) {
    let src = home.join(".claude").join(".claude.json");
    let dst = home.join(".claude.json");
    if src.exists() {
        let _ = std::fs::copy(&src, &dst);
    }
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
    let host = HostContext::detect()?;
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
    let host = HostContext::detect()?;
    host::require_git(&host)?;
    require_agent_dotfiles(&host, "codex")?;
    let mut argv = vec![
        "codex".to_string(),
        "--dangerously-bypass-approvals-and-sandbox".to_string(),
    ];
    argv.extend(extra);
    run(host, argv, rw, ro)
}

pub fn run_bash(rw: Vec<PathBuf>, ro: Vec<PathBuf>) -> Result<ExitCode> {
    let host = HostContext::detect()?;
    host::require_git(&host)?;
    #[cfg(target_family = "unix")]
    let argv = vec!["/bin/bash".to_string(), "-l".to_string()];
    #[cfg(target_family = "windows")]
    let argv = {
        let shell = std::env::var("SHELL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| r"C:\Program Files\Git\bin\bash.exe".to_string());
        vec![shell, "-l".to_string()]
    };
    run(host, argv, rw, ro)
}

pub fn run_argv(argv: Vec<String>, rw: Vec<PathBuf>, ro: Vec<PathBuf>) -> Result<ExitCode> {
    if argv.is_empty() {
        bail!("arbox run needs a command");
    }
    let host = HostContext::detect()?;
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
    let mut mounts = host.mount_specs();
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

    match (&host.user, &host.os) {
        (host::UserInfo::Unix { username, uid, gid }, host::OsInfo::Unix { .. }) => {
            host::require_supported_distro(&host)?;
            // Distinctive uppercase hostname — `jason@ARBOX:~$` makes it obvious at
            // a glance that you're inside the sandbox shell vs. the host shell.
            cmd.args(["--hostname", "ARBOX", "--network", "host"]);
            cmd.arg("--user")
                .arg(format!("{}:{}", uid, gid));
            cmd.arg("--workdir").arg(&host.cwd);
            cmd.arg("-e").arg(format!("HOME={}", host.home.display()));
            cmd.arg("-e")
                .arg(format!("USER={}", username.to_string_lossy()));
            cmd.arg("-e").arg(format!("TERM={}", host.term));
            cmd.arg("-e").arg("LANG=C.UTF-8");
        }
        (host::UserInfo::Windows { username }, host::OsInfo::Windows { .. }) => {
            ensure_docker_windows_mode()?;
            host::require_hyperv_capable(&host)?;

            cmd.arg("--isolation=hyperv");

            let hostname = std::env::var("COMPUTERNAME")
                .unwrap_or_else(|_| "ARBOX".to_string());
            cmd.args(["--hostname", &hostname]);

            // Windows containers use NAT — no --network host
            cmd.arg("--workdir").arg(&host.cwd);
            cmd.arg("-e").arg(format!("USERNAME={}", username.to_string_lossy()));
            cmd.arg("-e").arg(format!("USERPROFILE={}", host.home.display()));
            cmd.arg("-e").arg(format!("HOME={}", host.home.display()));

            // Semicolon-separated Windows PATH
            let cargo_bin = host.home.join(".cargo").join("bin");
            let local_bin = host.home.join(".local").join("bin");
            let path = format!(
                "{};{};C:\\tools\\bin;C:\\Program Files\\Git\\cmd;C:\\Program Files\\Git\\bin;C:\\Windows\\System32;C:\\Windows",
                cargo_bin.display(),
                local_bin.display()
            );
            cmd.arg("-e").arg(format!("PATH={}", path));
        }
        _ => unreachable!(),
    };

    for m in &mounts {
        if !m.path.exists() {
            // Optional + missing: skip. Required + missing was caught above.
            continue;
        }
        // Docker on Windows doesn't support binding individual files, skip them.
        // The mount spec should already be Unix-only for file paths.
        if m.path.is_file() {
            continue;
        }
        let normalized = normalize_for_docker(&m.path);
        let arg = if m.read_only {
            format!(
                "type=bind,src={},dst={},readonly",
                normalized.display(),
                normalized.display()
            )
        } else {
            format!(
                "type=bind,src={},dst={}",
                normalized.display(),
                normalized.display()
            )
        };
        cmd.arg("--mount").arg(arg);
    }

    add_wayland_clipboard(&mut cmd);

    cmd.arg(&tag);
    for a in &argv {
        cmd.arg(a);
    }

    #[cfg(target_family = "windows")]
    stage_claude_json_in(&host.home);

    let status = cmd
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("running `docker run`")?;

    #[cfg(target_family = "windows")]
    stage_claude_json_out(&host.home);

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

fn ensure_docker_windows_mode() -> Result<()> {
    let out = Command::new("docker")
        .args(["info", "--format", "{{.OSType}}"])
        .output()
        .context("running `docker info`")?;
    let os_type = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if os_type != "windows" {
        bail!(
            "Docker is in {} containers mode; switch to Windows containers mode \
             (right-click Docker tray → \"Switch to Windows containers\")",
            os_type
        );
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
