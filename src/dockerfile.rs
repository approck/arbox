/// The Dockerfile is embedded at compile time. The build context is empty
/// otherwise — every host-specific value flows through `--build-arg`.
pub const DOCKERFILE_UBUNTU: &str = include_str!("Dockerfile.ubuntu");
pub const DOCKERFILE_WINDOWS: &str = include_str!("Dockerfile.windows");

/// Windows container entrypoint script for staging .claude.json through the mounted
/// .claude directory (since Docker on Windows doesn't support binding individual files).
pub const ENTRYPOINT_PS1: &str = include_str!("entrypoint.ps1");
