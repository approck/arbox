#[cfg(target_family = "unix")]
mod unix;

#[cfg(target_family = "unix")]
pub use unix::user_info;

#[cfg(target_family = "windows")]
mod windows;

#[cfg(target_family = "windows")]
pub use windows::user_info;
