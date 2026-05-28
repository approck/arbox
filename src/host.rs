use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};

use crate::{git, osrelease, passwd};

/// Everything we need to know about the host to mirror it in the container.
/// Detected fresh on every invocation. Base facts (uid/gid/distro/etc.) are
/// always required; git facts are optional so commands that don't need a
/// repo (`status`, `build`, `clean`) work outside one.
#[derive(Debug, Clone)]
pub struct HostContext {
    pub uid: u32,
    pub gid: u32,
    pub username: String,
    pub home: PathBuf,
    pub cwd: PathBuf,
    pub term: String,
    pub distro_id: String,
    pub distro_codename: String,

    /// Git toplevel of `cwd`. `None` when cwd isn't inside a git repository.
    pub workspace_root: Option<PathBuf>,
    /// `git rev-parse --git-common-dir`. Always `Some` whenever `workspace_root`
    /// is `Some`.
    pub git_common_dir: Option<PathBuf>,
}

pub fn detect() -> Result<HostContext> {
    let uid = get_uid();
    let gid = get_gid();

    let (username, home) = passwd::user_info(uid)
        .with_context(|| format!("could not resolve passwd entry for uid {uid}"))?;

    let cwd = std::env::current_dir()
        .context("getting current directory")?
        .canonicalize()
        .context("canonicalizing cwd")?;

    let (distro_id, distro_codename) = if cfg!(target_family = "unix") {
        let osrel = osrelease::parse("/etc/os-release").context("reading /etc/os-release")?;
        let distro_id = osrel
            .get("ID")
            .ok_or_else(|| anyhow!("/etc/os-release has no ID="))?
            .clone();
        let distro_codename = osrel
            .get("VERSION_CODENAME")
            .ok_or_else(|| anyhow!("/etc/os-release has no VERSION_CODENAME="))?
            .clone();
        (distro_id, distro_codename)
    } else {
        ("ubuntu".to_string(), "noble".to_string())
    };

    let term = std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".to_string());

    // Git is best-effort: not all commands need a repo (status/build/clean).
    let (workspace_root, git_common_dir) = match git::toplevel(&cwd) {
        Ok(top) => {
            let common = git::common_dir(&top)?;
            (Some(top), Some(common))
        }
        Err(_) => (None, None),
    };

    Ok(HostContext {
        uid,
        gid,
        username,
        home,
        cwd,
        term,
        distro_id,
        distro_codename,
        workspace_root,
        git_common_dir,
    })
}

/// Hard gate. v1 is Ubuntu-only on purpose: the Dockerfile pins the same
/// codename, and codename-aligned glibc is what makes incremental fingerprints
/// match across host ↔ container builds.
pub fn require_supported_distro(host: &HostContext) -> Result<()> {
    if host.distro_id != "ubuntu" {
        bail!(
            "arbox currently supports Ubuntu only; detected: {}",
            host.distro_id
        );
    }
    Ok(())
}

/// Demand a git workspace for verbs that need one (claude/codex/bash/run).
/// Returns the workspace root and common git dir as a borrowed pair.
pub fn require_git<'a>(host: &'a HostContext) -> Result<(&'a Path, &'a Path)> {
    let workspace = host.workspace_root.as_deref().ok_or_else(|| {
        anyhow!(
            "arbox must be run inside a git repository (cwd: {})",
            host.cwd.display()
        )
    })?;
    // Invariant from `detect`: workspace_root.is_some() ⇒ git_common_dir.is_some().
    let common = host
        .git_common_dir
        .as_deref()
        .expect("git_common_dir is set whenever workspace_root is");

    if !host.cwd.starts_with(workspace) {
        bail!(
            "cwd {} is not inside workspace root {}",
            host.cwd.display(),
            workspace.display()
        );
    }
    Ok((workspace, common))
}

#[cfg(target_family = "unix")]
fn get_uid() -> u32 {
    unsafe { libc::getuid() }
}

#[cfg(target_family = "windows")]
fn get_uid() -> u32 {
    // Windows has no Unix-style uid system. Return 1000, a standard default uid
    // for Linux container environments that this tool mirrors users into.
    1000
}

#[cfg(target_family = "unix")]
fn get_gid() -> u32 {
    unsafe { libc::getgid() }
}

#[cfg(target_family = "windows")]
fn get_gid() -> u32 {
    // Windows has no Unix-style gid system. Return 1000, a standard default gid
    // for Linux container environments that this tool mirrors users into.
    1000
}
