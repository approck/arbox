use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::ptr;

use anyhow::anyhow;

pub fn user_info(uid: u32) -> anyhow::Result<(String, PathBuf)> {
    _ = uid; // done to keep the interface identical to the unix counterpart

    let username = get_username()?;
    let home_dir = get_home_dir()?.canonicalize()?;
    Ok((username.to_string_lossy().to_string(), home_dir))
}

fn get_username() -> anyhow::Result<OsString> {
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn GetUserNameW(lpBuffer: *mut u16, pcbBuffer: *mut u32) -> i32;
    }

    unsafe {
        let mut buf: Vec<u16> = vec![0; 256];
        let mut size: u32 = buf.len() as u32;

        if GetUserNameW(buf.as_mut_ptr(), &mut size) != 0 {
            let len = (size.saturating_sub(1)) as usize;
            let name = OsString::from_wide(&buf[..len]);
            Ok(name)
        } else {
            Err(anyhow!("GetUserNameW failed"))
        }
    }
}

fn get_home_dir() -> anyhow::Result<PathBuf> {
    #[allow(clippy::upper_case_acronyms)]
    #[repr(C)]
    struct GUID {
        data1: u32,
        data2: u16,
        data3: u16,
        data4: [u8; 8],
    }

    #[link(name = "shell32")]
    #[link(name = "ole32")]
    unsafe extern "system" {
        fn SHGetKnownFolderPath(
            rfid: *const GUID,
            dwFlags: u32,
            hToken: *mut core::ffi::c_void,
            ppszPath: *mut *mut u16,
        ) -> i32;

        fn CoTaskMemFree(pv: *mut core::ffi::c_void);
    }

    // FOLDERID_Profile = user home directory
    // See: https://learn.microsoft.com/en-us/windows/win32/shell/knownfolderid
    const FOLDERID_PROFILE: GUID = GUID {
        data1: 0x5E6C858F,
        data2: 0x0E22,
        data3: 0x4760,
        data4: [0x9A, 0xFE, 0xEA, 0x33, 0x17, 0xB6, 0x71, 0x73],
    };

    unsafe {
        let mut path_ptr: *mut u16 = ptr::null_mut();

        let hr = SHGetKnownFolderPath(&FOLDERID_PROFILE, 0, ptr::null_mut(), &mut path_ptr);

        if hr != 0 {
            return Err(anyhow!("SHGetKnownFolderPath failed: HRESULT={}", hr));
        }

        // find null terminator with bounds (Windows paths are typically < 32K)
        let mut len = 0;
        const MAX_PATH: usize = 32768;
        while len < MAX_PATH && *path_ptr.add(len) != 0 {
            len += 1;
        }
        if len >= MAX_PATH {
            return Err(anyhow!("path string too long or missing null terminator"));
        }

        let slice = std::slice::from_raw_parts(path_ptr, len);
        let path = OsString::from_wide(slice);

        CoTaskMemFree(path_ptr as *mut _);

        Ok(PathBuf::from(path))
    }
}
