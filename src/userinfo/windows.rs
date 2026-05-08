use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::ptr;

use anyhow::anyhow;
use anyhow::Result;

use super::OsInfo;
use super::UserInfo;
use super::UserInfoExtra;

impl UserInfo {
    pub fn get_windows() -> Result<Self> {
        let username = get_username()?;
        let home = get_home_dir()?;
        Ok(Self {
            username,
            home,
            extra: UserInfoExtra::Windows,
        })
    }
}

#[link(name = "advapi32")]
unsafe extern "system" {
    fn GetUserNameW(lpBuffer: *mut u16, pcbBuffer: *mut u32) -> i32;
}

pub fn get_username() -> Result<OsString> {
    unsafe {
        let mut size: u32 = 0;

        GetUserNameW(std::ptr::null_mut(), &mut size);

        let mut buf: Vec<u16> = vec![0; size as usize];

        if GetUserNameW(buf.as_mut_ptr(), &mut size) != 0 {
            let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            let name = OsString::from_wide(&buf[..len]);

            Ok(name)
        } else {
            Err(anyhow!("GetUserNameW failed"))
        }
    }
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

#[repr(C)]
struct GUID {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

// FOLDERID_Profile = user home directory
// See: https://learn.microsoft.com/en-us/windows/win32/shell/knownfolderid
const FOLDERID_PROFILE: GUID = GUID {
    data1: 0x5E6C858F,
    data2: 0x0E22,
    data3: 0x4760,
    data4: [0x9A, 0xFE, 0xEA, 0x33, 0x17, 0xB6, 0x71, 0x73],
};

pub fn get_home_dir() -> Result<PathBuf> {
    unsafe {
        let mut path_ptr: *mut u16 = ptr::null_mut();

        let hr = SHGetKnownFolderPath(&FOLDERID_PROFILE, 0, ptr::null_mut(), &mut path_ptr);

        if hr != 0 {
            return Err(anyhow!("SHGetKnownFolderPath failed: HRESULT={}", hr));
        }

        // find null terminator
        let mut len = 0;
        while *path_ptr.add(len) != 0 {
            len += 1;
        }

        let slice = std::slice::from_raw_parts(path_ptr, len);
        let path = OsString::from_wide(slice);

        CoTaskMemFree(path_ptr as *mut _);

        Ok(PathBuf::from(path))
    }
}

#[derive(Debug)]
#[repr(C)]
#[allow(non_snake_case)]
/// https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-osversioninfoexw
struct OSVERSIONINFOEXW {
    dwOSVersionInfoSize: u32,
    dwMajorVersion: u32,
    dwMinorVersion: u32,
    dwBuildNumber: u32,
    dwPlatformId: u32,
    szCSDVersion: [u16; 128],
    wServicePackMajor: u16,
    wServicePackMinor: u16,
    wSuiteMask: u16,
    wProductType: u8,
    wReserved: u8,
}

#[link(name = "ntdll")]
extern "system" {
    fn RtlGetVersion(info: *mut OSVERSIONINFOEXW) -> i32;
}

impl OsInfo {
    pub fn get_windows() -> Result<Self> {
        let mut info = OSVERSIONINFOEXW {
            dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOEXW>() as u32,
            dwMajorVersion: 0,
            dwMinorVersion: 0,
            dwBuildNumber: 0,
            dwPlatformId: 0,
            szCSDVersion: [0u16; 128],
            wServicePackMajor: 0,
            wServicePackMinor: 0,
            wSuiteMask: 0,
            wProductType: 0,
            wReserved: 0,
        };

        unsafe {
            if RtlGetVersion(&mut info) != 0 {
                return Err(anyhow!("RtlGetVersion failed"));
            }
        }

        let svc_pack = if let Some(last) = info
            .szCSDVersion
            .iter()
            .copied()
            .enumerate()
            .find_map(|(i, val)| (val == 0).then_some(i))
        {
            (last > 0).then_some(String::from_utf16_lossy(&info.szCSDVersion[0..last]))
        } else {
            Some(String::from_utf16_lossy(&info.szCSDVersion))
        };

        Ok(Self::Windows {
            major: info.dwMajorVersion,
            minor: info.dwMinorVersion,
            build: info.dwBuildNumber,
            platform_id: info.dwPlatformId,
            svc_pack,
            product_type: info.wProductType,
            suite_mask: info.wSuiteMask,
        })
    }
}
