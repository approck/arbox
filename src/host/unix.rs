#![cfg(target_family = "unix")]

use std::ffi::{CStr, OsString};
use std::mem::MaybeUninit;
use std::path::PathBuf;
use std::ptr;

use anyhow::{anyhow, Context, Result};

use super::OsInfo;
use super::UserInfoQuery;
use super::UserInfoQueryExtra;

impl UserInfoQuery {
    pub fn unix() -> Result<UserInfoQuery> {
        let uid = unsafe { libc::getuid() };
        let gid = unsafe { libc::getgid() };
        let (name, home) = get_name_and_home(uid)?;
        Ok(Self {
            username: OsString::from(name),
            home,
            extra: UserInfoQueryExtra::Unix { uid, gid },
        })
    }
}

impl OsInfo {
    pub fn get_unix() -> Result<Self> {
        let uid = unsafe { libc::getuid() };
        let gid = unsafe { libc::getgid() };
        let osrel = crate::osrelease::parse("/etc/os-release")
            .context("reading /etc/os-release")?;
        let distro_id = osrel
            .get("ID")
            .ok_or_else(|| anyhow!("/etc/os-release has no ID="))?
            .clone();
        let distro_codename = osrel
            .get("VERSION_CODENAME")
            .ok_or_else(|| anyhow!("/etc/os-release has no VERSION_CODENAME="))?
            .clone();
        Ok(Self::Unix { uid, gid, distro_id, distro_codename })
    }
}

fn get_name_and_home(uid: u32) -> Result<(String, PathBuf)> {
    for size in [4096usize, 65536] {
        match try_lookup(uid, size)? {
            Some(pair) => return Ok(pair),
            None => continue,
        }
    }
    Err(anyhow!(
        "getpwuid_r returned ERANGE even with 64K buffer for uid {uid}"
    ))
}

fn try_lookup(uid: u32, bufsize: usize) -> Result<Option<(String, PathBuf)>> {
    let mut buf = vec![0u8; bufsize];
    let mut pwd = MaybeUninit::<libc::passwd>::uninit();
    let mut result: *mut libc::passwd = ptr::null_mut();

    let rc = unsafe {
        libc::getpwuid_r(
            uid as libc::uid_t,
            pwd.as_mut_ptr(),
            buf.as_mut_ptr() as *mut libc::c_char,
            buf.len(),
            &mut result,
        )
    };

    if rc == libc::ERANGE {
        return Ok(None);
    }
    if rc != 0 {
        return Err(anyhow!("getpwuid_r failed for uid {uid}: errno {rc}"));
    }
    if result.is_null() {
        return Err(anyhow!("no passwd entry for uid {uid}"));
    }

    let pwd = unsafe { pwd.assume_init() };
    let name = unsafe { CStr::from_ptr(pwd.pw_name) }
        .to_str()
        .map_err(|_| anyhow!("passwd pw_name is not utf-8"))?
        .to_string();
    let home = unsafe { CStr::from_ptr(pwd.pw_dir) }
        .to_str()
        .map_err(|_| anyhow!("passwd pw_dir is not utf-8"))?;
    if home.is_empty() {
        return Err(anyhow!("passwd entry for {name} has no home dir"));
    }
    Ok(Some((name, PathBuf::from(home))))
}
