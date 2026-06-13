use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;

use anyhow::anyhow;
use anyhow::Context;

pub fn user_info(uid: u32) -> anyhow::Result<(String, PathBuf)> {
    _ = uid; // done to keep the interface identical to the unix counterpart

    let username = get_username()?;
    let home_dir = std::env::home_dir().context("get home directory")?;
    Ok((username.to_string_lossy().to_string(), home_dir))
}

fn get_username() -> anyhow::Result<OsString> {
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn GetUserNameW(lpBuffer: *mut u16, pcbBuffer: *mut u32) -> i32;
    }

    const SIZE: u32 = 256;

    unsafe {
        let mut buf: Vec<u16> = vec![0u16; SIZE as usize];
        let mut size: u32 = SIZE;

        if GetUserNameW(buf.as_mut_ptr(), &mut size) != 0 {
            let len = usize::try_from(size.saturating_sub(1))?;
            let name = OsString::from_wide(&buf[..len]);
            Ok(name)
        } else {
            Err(anyhow!("GetUserNameW failed"))
        }
    }
}
