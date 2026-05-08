mod unix;
mod windows;

use anyhow::{anyhow, bail, Context, Result};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::git;

/// Everything we need to know about the host to mirror it in the container.
/// Detected fresh on every invocation. Base facts (uid/gid/distro/etc.) are
/// always required; git facts are optional so commands that don't need a
/// repo (`status`, `build`, `clean`) work outside one.
#[derive(Debug, Clone)]
pub struct HostContext {
    pub user: UserInfo,
    pub home: PathBuf,
    pub cwd: PathBuf,
    pub term: String,
    /// Git toplevel of `cwd`. `None` when cwd isn't inside a git repository.
    pub workspace_root: Option<PathBuf>,
    /// `git rev-parse --git-common-dir`. Always `Some` whenever `workspace_root`
    /// is `Some`.
    pub git_common_dir: Option<PathBuf>,
    pub os: OsInfo,
}

#[derive(Debug, Clone)]
pub enum UserInfo {
    Unix {
        username: OsString,
        uid: u32,
        gid: u32,
    },
    Windows {
        username: OsString,
    },
}

/// Complete user + platform information
#[derive(Debug, Clone)]
pub struct UserInfoQuery {
    pub username: OsString,
    pub home: PathBuf,
    pub extra: UserInfoQueryExtra,
}

#[derive(Debug, Clone)]
pub enum UserInfoQueryExtra {
    Unix { uid: u32, gid: u32 },
    Windows,
}

impl UserInfoQuery {
    fn get() -> Result<Self> {
        #[cfg(target_family = "unix")]
        {
            Self::unix()
        }
        #[cfg(target_family = "windows")]
        {
            Self::windows()
        }
    }
}

#[derive(Debug, Clone)]
pub enum OsInfo {
    Unix {
        uid: u32,
        gid: u32,
        distro_id: String,
        distro_codename: String,
    },
    Windows {
        major: u32,
        minor: u32,
        build: u32,
        platform_id: u32,
        svc_pack: Option<String>,
        product_type: u8,
        suite_mask: u16,
    },
}

impl OsInfo {
    pub fn get() -> Result<Self> {
        #[cfg(target_family = "unix")]
        {
            Self::get_unix()
        }
        #[cfg(target_family = "windows")]
        {
            Self::get_windows()
        }
    }
}


impl HostContext {
    pub fn detect() -> Result<Self> {
        let user_info_query = UserInfoQuery::get()?;
        let user = match user_info_query.extra {
            UserInfoQueryExtra::Unix { uid, gid } => UserInfo::Unix {
                username: user_info_query.username,
                uid,
                gid,
            },
            UserInfoQueryExtra::Windows => UserInfo::Windows {
                username: user_info_query.username,
            },
        };

        let os = OsInfo::get()?;

        let cwd = std::env::current_dir()
            .context("getting current directory")?
            .canonicalize()
            .context("canonicalizing cwd")?;

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
            user,
            home: user_info_query.home,
            cwd,
            term,
            workspace_root,
            git_common_dir,
            os,
        })
    }
}

impl std::fmt::Display for HostContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.os {
            OsInfo::Unix {
                uid,
                gid,
                distro_id,
                distro_codename,
            } => {
                writeln!(f, "host(unix):")?;
                writeln!(f, "  {:<16} {}", "uid:", uid)?;
                writeln!(f, "  {:<16} {}", "gid:", gid)?;
                writeln!(f, "  {:<16} unix", "family:")?;
                writeln!(f, "  {:<16} {} {}", "distro:", distro_id, distro_codename,)?;
            }
            OsInfo::Windows {
                major,
                minor,
                build,
                platform_id,
                svc_pack,
                product_type,
                suite_mask,
            } => {
                writeln!(f, "host(windows):")?;
                writeln!(
                    f,
                    "  {:<16} {}.{}",
                    "version:", major, minor
                )?;
                writeln!(f, "  {:<16} {}", "build:", build)?;
                writeln!(f, "  {:<16} {}", "platform_id:", platform_id)?;
                if let Some(s) = svc_pack {
                    writeln!(f, "  {:<16} {}", "service pack:", s)?;
                } else {
                    writeln!(f, "  {:<16} None", "service pack:")?;
                }
                writeln!(f, "  {:<16} {}", "product_type:", product_type)?;
                writeln!(f, "  {:<16} 0x{:04x}", "suite_mask:", suite_mask)?;
            }
        }
        let username = match &self.user {
            UserInfo::Unix { username, .. } | UserInfo::Windows { username } => username,
        };
        writeln!(f, "  {:<16} {}", "username:", username.display())?;
        writeln!(f, "  {:<16} {}", "home:", self.home.display())?;
        match &self.workspace_root {
            Some(w) => writeln!(f, "  {:<16} {}", "workspace root:", w.display())?,
            None => writeln!(f, "  {:<16} (not in a git repository)", "workspace root:")?,
        }
        if let Some(c) = &self.git_common_dir {
            if !c.starts_with(self.workspace_root.as_deref().unwrap_or(&self.cwd)) {
                writeln!(f, "  {:<16} {} (worktree)", "git common dir:", c.display())?;
            }
        }
        writeln!(f, "  {:<16} {}", "cwd:", self.cwd.display())?;
        write!(f, "  {:<16} {}", "term:", self.term)?;

        Ok(())
    }
}

/// Hard gate. v1 is Ubuntu-only on purpose: the Dockerfile pins the same
/// codename, and codename-aligned glibc is what makes incremental fingerprints
/// match across host ↔ container builds.
pub fn require_supported_distro(host: &HostContext) -> Result<()> {
    match &host.os {
        OsInfo::Unix { distro_id, .. } => {
            if distro_id != "ubuntu" {
                bail!(
                    "arbox currently supports Ubuntu only; detected: {}",
                    distro_id
                );
            }
        }
        OsInfo::Windows { .. } => {}
    }
    Ok(())
}

/// Require a Windows edition that supports Hyper-V (Pro / Enterprise / Education / Server).
/// Home editions cannot enable Hyper-V, which is required for the container sandbox.
pub fn require_hyperv_capable(host: &HostContext) -> Result<()> {
    if let OsInfo::Windows { suite_mask, .. } = &host.os {
        const VER_SUITE_PERSONAL: u16 = 0x0200;
        if suite_mask & VER_SUITE_PERSONAL != 0 {
            bail!(
                "arbox requires a Windows edition that supports Hyper-V \
                 (Pro, Enterprise, or Education). \
                 Windows Home does not support Hyper-V isolation."
            );
        }
    }
    Ok(())
}


/// Demand a git workspace for verbs that need one (claude/codex/bash/run).
/// Returns the workspace root and common git dir as a borrowed pair.
pub fn require_git(host: &HostContext) -> Result<(&Path, &Path)> {
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

    let workspace_canon = workspace
        .canonicalize()
        .context("canonicalizing workspace root")?;
    if !host.cwd.starts_with(&workspace_canon) {
        bail!(
            "cwd {} is not inside workspace root {}",
            host.cwd.display(),
            workspace.display()
        );
    }
    Ok((workspace, common))
}
