/// The Dockerfile is embedded at compile time. The build context is empty
/// otherwise — every host-specific value flows through `--build-arg`.
pub const DOCKERFILE: &str = include_str!("Dockerfile");
