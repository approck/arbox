use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

use arbox::{image, launch};

#[derive(Parser, Debug)]
#[command(
    name = "arbox",
    version,
    about = "Docker-based agent sandbox: a skinny chroot of the host"
)]
struct Cli {
    /// Mount HOST_PATH read-write at the same path inside the container.
    /// Repeatable. Global — placeable before or after the subcommand.
    #[arg(long = "rw", value_name = "PATH", global = true)]
    rw: Vec<PathBuf>,

    /// Mount HOST_PATH read-only at the same path inside the container.
    /// Repeatable. Global — placeable before or after the subcommand.
    #[arg(long = "ro", value_name = "PATH", global = true)]
    ro: Vec<PathBuf>,

    /// Shortcut for `--rw $HOME/Desktop`. Useful when you want to drop
    /// screenshots or scratch files between host and container. Mount fails
    /// loudly if $HOME/Desktop doesn't exist.
    #[arg(long = "desktop", global = true)]
    desktop: bool,

    /// Shortcut for `--rw $HOME/Downloads`. Useful for handing artifacts
    /// (built binaries, captured traces, etc.) between host and container.
    /// Mount fails loudly if $HOME/Downloads doesn't exist.
    #[arg(long = "downloads", global = true)]
    downloads: bool,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run `claude` inside the sandbox (passes --dangerously-skip-permissions).
    /// All trailing args are forwarded to claude:
    ///   `arbox claude --resume`, `arbox claude "describe this repo"`.
    Claude {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Run `codex` inside the sandbox
    /// (passes --dangerously-bypass-approvals-and-sandbox). All trailing args
    /// are forwarded to codex.
    Codex {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Drop into an interactive bash login shell inside the sandbox.
    Bash,
    /// Run the Playwright CLI inside the sandbox (image ships node +
    /// playwright + chromium + firefox + the system libs they link
    /// against). Examples: `arbox playwright test`, `arbox playwright
    /// codegen https://example.com`, `arbox playwright show-report`.
    Playwright {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Run an arbitrary command inside the sandbox: `arbox run -- cargo test`.
    Run {
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
    /// Remove every arbox image whose tag has the current host's prefix.
    Clean,
}

fn main() -> ExitCode {
    if std::env::var_os("ARBOX_INSIDE").is_some() {
        eprintln!("arbox is the host-side orchestrator; it cannot run inside its own container.");
        eprintln!("Exit this shell and run `arbox` from your host.");
        return ExitCode::FAILURE;
    }
    let cli = Cli::parse();
    let mut rw = cli.rw;
    if cli.desktop || cli.downloads {
        let Some(home) = std::env::var_os("HOME") else {
            eprintln!("error: --desktop/--downloads require $HOME to be set");
            return ExitCode::FAILURE;
        };
        let home = PathBuf::from(home);
        if cli.desktop {
            rw.push(home.join("Desktop"));
        }
        if cli.downloads {
            rw.push(home.join("Downloads"));
        }
    }
    match dispatch(cli.cmd, rw, cli.ro) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn dispatch(cmd: Cmd, rw: Vec<PathBuf>, ro: Vec<PathBuf>) -> Result<ExitCode> {
    match cmd {
        Cmd::Claude { args } => launch::run_claude(args, rw, ro),
        Cmd::Codex { args } => launch::run_codex(args, rw, ro),
        Cmd::Bash => launch::run_bash(rw, ro),
        Cmd::Playwright { args } => launch::run_playwright(args, rw, ro),
        Cmd::Run { cmd } => launch::run_argv(cmd, rw, ro),
        Cmd::Build { force, no_cache } => {
            image::build_image(force, no_cache).map(|_| ExitCode::SUCCESS)
        }
        Cmd::Status => image::print_status().map(|_| ExitCode::SUCCESS),
        Cmd::Clean => image::clean().map(|_| ExitCode::SUCCESS),
    }
}
