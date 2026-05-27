use anyhow::{anyhow, Result};
use std::ffi::CStr;
use std::mem::MaybeUninit;
use std::path::PathBuf;
use std::ptr;

/// Look up a user's name and home dir for a UID via `getpwuid_r`.
/// Deliberately avoids `$USER` / `$HOME` env vars: they can be shadowed by the
/// caller, and we use the values to derive image tags and bind-mount paths
/// where shadowing would be a quiet correctness bug.
pub fn user_info(uid: u32) -> Result<(String, PathBuf)> {
    // Single retry with a larger buffer if the first call returns ERANGE,
    // which happens on systems with very long shells/gecos fields.
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

/// Returns Ok(Some) on success, Ok(None) if buffer was too small (ERANGE),
/// Err on any other failure.
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

    // SAFETY: getpwuid_r returned 0 with non-null result, so pwd is initialized
    // and pw_name/pw_dir point into our `buf` (kept alive for the borrow).
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_up_current_user() {
        let uid = unsafe { libc::getuid() };
        let (name, home) = user_info(uid).unwrap();
        assert!(!name.is_empty());
        assert!(home.is_absolute());
    }
}
