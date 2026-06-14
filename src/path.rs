use anyhow::{bail, Result};
use std::path::Path;

/// Render `path` as the container-side path Docker should use for a bind-mount
/// *destination*, `--workdir`, and `HOME`. Identity on Unix; on Windows the
/// host path is mapped into WSL form (`C:\Users\x` -> `/mnt/c/Users/x`).
pub fn to_container(path: impl AsRef<Path>) -> Result<String> {
    let s = path.as_ref().to_string_lossy();
    if cfg!(target_family = "windows") {
        windows_to_container(s.as_ref())
    } else {
        Ok(s.into_owned())
    }
}

/// Render `path` as the bind-mount *source* Docker resolves on the host.
/// Identity on Unix; on Windows the daemon resolves the source against the
/// Windows filesystem, so the drive-letter path is kept but switched to the
/// forward slashes `docker --mount` requires (`\\?\C:\x` -> `C:/x`).
pub fn to_mount_src(path: impl AsRef<Path>) -> String {
    let s = path.as_ref().to_string_lossy();
    if cfg!(target_family = "windows") {
        windows_to_mount_src(s.as_ref())
    } else {
        s.into_owned()
    }
}

/// Strip the Windows `\\?\` verbatim prefix, if present.
fn strip_verbatim(p: &str) -> &str {
    p.strip_prefix(r"\\?\").unwrap_or(p)
}

/// `C:\Users\x` / `\\?\C:\Users\x` / `C:foo` -> `/mnt/c/...`. UNC paths can't
/// be mounted into the container this way, so they're rejected rather than
/// silently mangled (the old code turned `\\server\share` into a path with no
/// leading slash, and `C:foo` into `/mnt/cfoo`).
fn windows_to_container(p: &str) -> Result<String> {
    let p = strip_verbatim(p);

    // UNC: the original `\\server\share` or the de-verbatim'd `UNC\server\share`.
    if p.starts_with(r"\\") || p.starts_with("UNC\\") || p.starts_with("UNC/") {
        bail!("UNC paths can't be mounted into the container: {p}");
    }

    let mut chars = p.chars();
    if let (Some(drive), Some(':')) = (chars.next(), chars.next()) {
        // `chars.as_str()` is everything after the `X:` drive prefix, with no
        // byte-index slicing (so a stray multibyte lead char can't panic).
        // Tolerate absolute (`C:\x`, `C:/x`) and drive-relative (`C:x`) forms.
        let rest = chars.as_str();
        let rest = rest.strip_prefix(|c| c == '\\' || c == '/').unwrap_or(rest);
        return Ok(format!(
            "/mnt/{}/{}",
            drive.to_ascii_lowercase(),
            rest.replace('\\', "/")
        ));
    }

    // Already posix-ish (e.g. a path produced inside WSL) — normalize slashes.
    Ok(p.replace('\\', "/"))
}

/// `\\?\C:\Users\x` -> `C:/Users/x`. Keeps the drive letter; forward slashes
/// only, which is what `docker --mount type=bind,src=...` accepts on Windows.
fn windows_to_mount_src(p: &str) -> String {
    strip_verbatim(p).replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_maps_drive_paths_to_mnt() {
        assert_eq!(
            windows_to_container(r"C:\Users\x").unwrap(),
            "/mnt/c/Users/x"
        );
        assert_eq!(
            windows_to_container(r"\\?\C:\Users\x").unwrap(),
            "/mnt/c/Users/x"
        );
        assert_eq!(windows_to_container(r"D:\a\b").unwrap(), "/mnt/d/a/b");
    }

    #[test]
    fn container_inserts_separator_for_drive_relative() {
        // Regression: must be /mnt/c/foo, not /mnt/cfoo.
        assert_eq!(windows_to_container("C:foo").unwrap(), "/mnt/c/foo");
    }

    #[test]
    fn container_rejects_unc() {
        assert!(windows_to_container(r"\\server\share\repo").is_err());
        assert!(windows_to_container(r"\\?\UNC\server\share").is_err());
    }

    #[test]
    fn mount_src_keeps_drive_with_forward_slashes() {
        assert_eq!(windows_to_mount_src(r"\\?\C:\Users\x"), "C:/Users/x");
        assert_eq!(windows_to_mount_src(r"C:\a\b"), "C:/a/b");
    }
}
