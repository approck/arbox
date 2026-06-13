use std::path::Path;

/// Convert a Windows path to WSL format for Docker (/mnt/c/...)
pub fn to_wsl(path: impl AsRef<Path>) -> String {
    let mut p = path.as_ref().to_string_lossy().to_string();

    // Strip \\?\ prefix if present
    const PREFIX: &str = "\\\\?\\";
    if p.starts_with(PREFIX) {
        p = p[4..].to_string();
    }

    // Convert Windows drive letter to WSL format
    if p.len() > 1 && p.chars().nth(1) == Some(':') {
        if let Some(drive) = p.chars().next() {
            p = format!(
                "/mnt/{}{}",
                drive.to_ascii_lowercase(),
                &p[2..].replace('\\', "/")
            );
        }
    }

    p
}
