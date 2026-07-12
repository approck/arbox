use anyhow::{bail, Context, Result};
use std::hash::{DefaultHasher, Hasher};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::dockerfile::DOCKERFILE;
use crate::host::{self, HostContext};

/// How aggressively `docker build` should bypass its layer cache.
#[derive(Clone, Copy)]
enum BuildMode {
    /// Plain cached build — used by the auto-bootstrap on first launch. The
    /// agents come out at whatever version the cached layers already hold.
    Cached,
    /// Cached build, but pass a fresh `AGENT_REFRESH` token so the five agent
    /// layers (and only those) re-run and pull their latest versions. Every
    /// expensive layer above the cache-bust barrier stays cached.
    RefreshAgents,
    /// `--no-cache`: re-run every layer from scratch (apt, node, the
    /// Playwright browser downloads, everything).
    Full,
}

/// 8-hex-char fingerprint of the embedded Dockerfile bytes. Mixed into the
/// image tag so that editing the Dockerfile (different package list, bumped
/// version pin, etc.) automatically invalidates the cached image and the
/// next launch verb rebuilds. SipHash-1-3 with a fixed key is plenty for a
/// "did the bytes change" check.
fn dockerfile_hash() -> String {
    let mut h = DefaultHasher::new();
    h.write(DOCKERFILE.as_bytes());
    format!("{:08x}", h.finish() as u32)
}

/// Tag prefix shared by every arbox image for the current host (platform +
/// codename + uid). Used by `clean` to wipe stale images when the
/// Dockerfile-hash changes.
///
/// The platform segment (`win`/`nix`) matters because a Windows build and a
/// Linux build can otherwise collide: the embedded Dockerfile bytes are
/// identical (same `dockerfile_hash`) and Windows hardcodes uid 1000 + the
/// `noble` codename, but the two builds differ (Windows bakes rustup and skips
/// host-user creation via `WINDOWS_HOST=true`). Without this, a shared Docker
/// Desktop/WSL2 daemon would let `ensure_built` serve the wrong-platform image.
pub fn tag_prefix(host: &HostContext) -> String {
    let plat = if cfg!(target_family = "windows") {
        "win"
    } else {
        "nix"
    };
    format!("arbox:{plat}-{}-uid{}-", host.distro_codename, host.uid)
}

/// `arbox:<plat>-<distro_codename>-uid<uid>-<dockerfile_hash>`. Two users on
/// the same machine, two machines on different Ubuntu releases, two host
/// platforms, or two builds of different Dockerfiles all get distinct tags.
pub fn tag(host: &HostContext) -> String {
    format!("{}{}", tag_prefix(host), dockerfile_hash())
}

pub fn image_exists(tag: &str) -> Result<bool> {
    Ok(Command::new("docker")
        .args(["image", "inspect", tag])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("running `docker image inspect`")?
        .success())
}

/// Build the image if it isn't already present. Used by every launch verb so
/// the first invocation on a fresh host self-bootstraps.
pub fn ensure_built(host: &HostContext) -> Result<String> {
    let t = tag(host);
    if image_exists(&t)? {
        return Ok(t);
    }
    eprintln!("[arbox] image {t} not found — building from embedded Dockerfile…");
    build_with_args(host, &t, BuildMode::Cached)?;
    if !image_exists(&t)? {
        bail!("docker build completed but image {t} is still missing");
    }
    Ok(t)
}

/// Refresh the baked-in agents (claude, codex, opencode, agy, grok) to their
/// latest published versions by rebuilding the image's agent layers in place.
///
/// The image keeps the same tag, so launch verbs pick up the refreshed agents
/// automatically. By default only the agent layers re-run (a fresh
/// `AGENT_REFRESH` cache-bust token is passed to `docker build`); `full`
/// re-runs every layer from scratch with `--no-cache`.
pub fn update_image(full: bool) -> Result<()> {
    let host = host::detect()?;
    host::require_supported_distro(&host)?;
    let t = tag(&host);
    if full {
        eprintln!("[arbox] full rebuild of {t} (--no-cache)…");
        build_with_args(&host, &t, BuildMode::Full)
    } else {
        eprintln!("[arbox] refreshing agents in {t} to their latest versions…");
        build_with_args(&host, &t, BuildMode::RefreshAgents)
    }
}

/// Seconds since the Unix epoch, used as the `AGENT_REFRESH` cache-bust token.
/// Any value distinct from the previous build invalidates the agent layers; a
/// monotonically-increasing timestamp guarantees that.
fn refresh_token() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn build_with_args(host: &HostContext, t: &str, mode: BuildMode) -> Result<()> {
    let dir = tempfile::tempdir().context("creating build context tempdir")?;
    let dockerfile_path = dir.path().join("Dockerfile");
    std::fs::write(&dockerfile_path, DOCKERFILE)
        .with_context(|| format!("writing {}", dockerfile_path.display()))?;

    let home_str = crate::path::to_container(&host.home)?;

    let mut cmd = Command::new("docker");
    cmd.arg("build")
        .arg("--tag")
        .arg(t)
        .arg("--build-arg")
        .arg(format!("HOST_DISTRO_CODENAME={}", host.distro_codename))
        .arg("--build-arg")
        .arg(format!("HOST_UID={}", host.uid))
        .arg("--build-arg")
        .arg(format!("HOST_GID={}", host.gid))
        .arg("--build-arg")
        .arg(format!("HOST_USER={}", host.username))
        .arg("--build-arg")
        .arg(format!("HOST_HOME={}", home_str));
    if cfg!(target_family = "windows") {
        cmd.args(["--build-arg", "WINDOWS_HOST=true"])
            .args(["--build-arg", "CARGO_HOME=/usr/local/cargo"])
            .args(["--build-arg", "RUSTUP_HOME=/usr/local/rustup"]);
    } else {
        cmd.args(["--build-arg", &format!("CARGO_HOME={}/.cargo", home_str)])
            .args(["--build-arg", &format!("RUSTUP_HOME={}/.rustup", home_str)]);
    }
    match mode {
        BuildMode::Cached => {}
        BuildMode::RefreshAgents => {
            cmd.arg("--build-arg")
                .arg(format!("AGENT_REFRESH={}", refresh_token()));
        }
        BuildMode::Full => {
            cmd.arg("--no-cache");
        }
    }
    cmd.arg("-f").arg(&dockerfile_path);
    cmd.arg(dir.path());
    cmd.stdout(Stdio::inherit()).stderr(Stdio::inherit());

    let status = cmd.status().context("running `docker build`")?;
    if !status.success() {
        bail!("docker build failed with {status}");
    }
    Ok(())
}

/// Remove every arbox image for the current host. This includes the
/// currently-tagged image and any stale ones from earlier Dockerfile
/// revisions (whose tags differ only in the dockerfile_hash suffix).
pub fn clean() -> Result<()> {
    let host = host::detect()?;
    let prefix = tag_prefix(&host);

    let listing = Command::new("docker")
        .args(["images", "--format", "{{.Repository}}:{{.Tag}}", "arbox"])
        .output()
        .context("running `docker images` to enumerate arbox tags")?;
    if !listing.status.success() {
        bail!(
            "`docker images` failed: {}",
            String::from_utf8_lossy(&listing.stderr).trim()
        );
    }
    let stdout = String::from_utf8_lossy(&listing.stdout);
    let to_remove: Vec<&str> = stdout
        .lines()
        .map(|l| l.trim())
        .filter(|l| l.starts_with(&prefix))
        .collect();

    if to_remove.is_empty() {
        eprintln!("[arbox] no arbox images for this host present");
        return Ok(());
    }

    eprintln!("[arbox] removing {} image(s):", to_remove.len());
    for t in &to_remove {
        eprintln!("  {t}");
    }
    let mut cmd = Command::new("docker");
    cmd.arg("rmi");
    for t in &to_remove {
        cmd.arg(t);
    }
    let status = cmd
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("running `docker rmi`")?;
    if !status.success() {
        bail!("docker rmi failed with {status}");
    }
    Ok(())
}

pub fn print_status(profile: Option<&str>) -> Result<()> {
    // Always print whatever we managed to detect, even on unsupported hosts.
    let host = match host::detect() {
        Ok(h) => h,
        Err(e) => {
            println!("host detection failed: {e:#}");
            return Ok(());
        }
    };

    println!("host:");
    println!("  uid:                {}", host.uid);
    println!("  gid:                {}", host.gid);
    println!("  username:           {}", host.username);
    println!("  home:               {}", host.home.display());
    println!(
        "  distro:             {} {}",
        host.distro_id, host.distro_codename
    );
    match &host.workspace_root {
        Some(w) => println!("  workspace root:     {}", w.display()),
        None => println!("  workspace root:     (not in a git repository)"),
    }
    if let Some(c) = &host.git_common_dir {
        if !c.starts_with(host.workspace_root.as_deref().unwrap_or(&host.cwd)) {
            println!("  git common dir:     {} (worktree)", c.display());
        }
    }
    println!("  cwd:                {}", host.cwd.display());
    println!("  term:               {}", host.term);
    match profile {
        Some(p) => println!("  profile:            {p}"),
        None => println!("  profile:            (default — standard auth locations)"),
    }

    if host.distro_id != "ubuntu" {
        println!();
        println!(
            "unsupported distro: {} (arbox supports ubuntu only)",
            host.distro_id
        );
        return Ok(());
    }

    println!("mounts (host -> container path):");
    for m in crate::launch::mount_specs(&host, profile) {
        let mode = if m.read_only { "ro" } else { "rw" };
        let exists = m.src.exists();
        let suffix = match (exists, m.required) {
            (true, _) => "",
            (false, true) => "  [missing — required]",
            (false, false) => "  [missing — skipped]",
        };
        // Most mounts land at the same path on both sides; show the redirect
        // arrow only for the profile auth files where src != dst.
        if m.src == m.dst {
            println!("  {} ({mode}){suffix}", m.src.display());
        } else {
            println!(
                "  {} -> {} ({mode}){suffix}",
                m.src.display(),
                m.dst.display()
            );
        }
    }

    let t = tag(&host);
    println!("image:");
    if image_exists(&t)? {
        println!("  tag:    {t}");
        println!("  status: present");
    } else {
        println!("  tag:    {t}");
        println!("  status: not built — run `arbox update` (or any launch verb) to build it");
    }
    println!("network: host");
    Ok(())
}
