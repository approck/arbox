use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

use arbox::{image, launch};

/// Extra bind-mount flags shared by every launch verb. Each option is
/// repeatable; the source path is canonicalized at launch time and mounted
/// at the same absolute path inside the container.
#[derive(Args, Debug, Default)]
struct MountFlags {
    /// Mount HOST_PATH read-write at the same path inside the container.
    #[arg(long = "rw", value_name = "PATH")]
    rw: Vec<PathBuf>,
    /// Mount HOST_PATH read-only at the same path inside the container.
    #[arg(long = "ro", value_name = "PATH")]
    ro: Vec<PathBuf>,
}

#[derive(Parser, Debug)]
#[command(
    name = "arbox",
    version,
    about = "Docker-based agent sandbox: a skinny chroot of the host"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run `claude` inside the sandbox (passes --dangerously-skip-permissions).
    Claude {
        #[command(flatten)]
        mounts: MountFlags,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Run `codex` inside the sandbox (passes --dangerously-bypass-approvals-and-sandbox).
    Codex {
        #[command(flatten)]
        mounts: MountFlags,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Drop into an interactive bash login shell inside the sandbox.
    Bash {
        #[command(flatten)]
        mounts: MountFlags,
    },
    /// Run an arbitrary command inside the sandbox: `arbox run -- cargo test`.
    Run {
        #[command(flatten)]
        mounts: MountFlags,
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        cmd: Vec<String>,
    },
    /// Build (or rebuild) the sandbox image for the current host.
    Build {
        /// Force a rebuild even if the image already exists.
        #[arg(long)]
        force: bool,
        /// Pass --no-cache to docker build.
        #[arg(long)]
        no_cache: bool,
    },
    /// Print detected host facts, image presence, and mount layout.
    Status,
    /// Remove the local sandbox image.
    Clean,
}

fn main() -> ExitCode {
    if std::env::var_os("ARBOX_INSIDE").is_some() {
        eprintln!("arbox is the host-side orchestrator; it cannot run inside its own container.");
        eprintln!("Exit this shell and run `arbox` from your host.");
        return ExitCode::FAILURE;
    }
    let cli = Cli::parse();
    match dispatch(cli.cmd) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn dispatch(cmd: Cmd) -> Result<ExitCode> {
    match cmd {
        Cmd::Claude { args, mounts } => launch::run_claude(args, mounts.rw, mounts.ro),
        Cmd::Codex { args, mounts } => launch::run_codex(args, mounts.rw, mounts.ro),
        Cmd::Bash { mounts } => launch::run_bash(mounts.rw, mounts.ro),
        Cmd::Run { cmd, mounts } => launch::run_argv(cmd, mounts.rw, mounts.ro),
        Cmd::Build { force, no_cache } => {
            image::build_image(force, no_cache).map(|_| ExitCode::SUCCESS)
        }
        Cmd::Status => image::print_status().map(|_| ExitCode::SUCCESS),
        Cmd::Clean => image::clean().map(|_| ExitCode::SUCCESS),
    }
}
