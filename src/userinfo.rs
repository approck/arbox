#[cfg(target_family = "unix")]
mod ubuntu;

#[cfg(target_family = "windows")]
mod windows;

use anyhow::Result;
use std::ffi::OsString;
use std::path::PathBuf;

/// Complete user + platform information
#[derive(Debug, Clone)]
pub struct UserInfo {
    pub username: OsString,
    pub home: PathBuf,
    pub extra: UserInfoExtra,
}

impl UserInfo {
    pub fn get() -> Result<Self> {
        #[cfg(target_family = "unix")]
        {
            ubuntu::UserInfo::get()
        }
        #[cfg(target_family = "windows")]
        {
            Self::get_windows()
        }
    }
}

#[derive(Debug, Clone)]
pub enum UserInfoExtra {
    Unix { uid: u32, gid: u32 },
    Windows,
}

/// Platform-specific OS context
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
