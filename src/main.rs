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
    /// Mount HOST_PATH read-write (repeatable, global).
    #[arg(long = "rw", value_name = "PATH", global = true)]
    rw: Vec<PathBuf>,

    /// Mount HOST_PATH read-only (repeatable, global).
    #[arg(long = "ro", value_name = "PATH", global = true)]
    ro: Vec<PathBuf>,

    /// Shortcut for --rw $HOME/Desktop (fails if missing).
    #[arg(long = "desktop", global = true)]
    desktop: bool,

    /// Shortcut for --rw $HOME/Downloads (fails if missing).
    #[arg(long = "downloads", global = true)]
    downloads: bool,

    /// Use a named auth profile (global). Sources each agent's ENTIRE state
    /// tree (auth + history + memories + sessions + settings) from
    /// `~/.arbox/profiles/NAME/` instead of the standard host locations, so a
    /// second subscription runs fully self-contained and never touches your
    /// default login. Auth and history always match because they live in one
    /// tree. The default (no --profile) shares your normal host locations.
    #[arg(long = "profile", value_name = "NAME", global = true)]
    profile: Option<String>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run Claude Code (--dangerously-skip-permissions injected).
    ///
    /// All trailing args are forwarded to claude:
    ///   `arbox claude --resume`, `arbox claude "describe this repo"`.
    Claude {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Run Codex CLI (approval-bypass flag injected).
    ///
    /// Passes --dangerously-bypass-approvals-and-sandbox. All trailing args
    /// are forwarded to codex.
    Codex {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Run the OpenCode (`opencode`) TUI.
    ///
    /// The binary is baked into the image; ~/.config/opencode (config,
    /// themes, agents) and ~/.local/share/opencode (auth.json, sessions)
    /// mount from the host. No approval-bypass flag exists or is needed —
    /// opencode defaults to permissive permissions. Host-local providers
    /// (e.g. Ollama on localhost:11434) work because the container uses
    /// host networking. All trailing args forwarded.
    Opencode {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Run Google Antigravity's `agy` CLI.
    ///
    /// The binary is baked into the image; ~/.gemini and
    /// ~/.config/antigravity mount from the host for credential / skill /
    /// MCP persistence. First-time auth uses agy's SSH-style URL+code flow
    /// because libsecret isn't available inside the container. All
    /// trailing args forwarded.
    Agy {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Run xAI's Grok Build (`grok`) CLI.
    ///
    /// The binary is baked into the image; ~/.grok mounts from the host
    /// for auth (token in ~/.grok/auth.json) and download cache. All
    /// trailing args forwarded — grok's safety story is plan-mode review,
    /// not an approval-bypass flag.
    Grok {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Interactive bash login shell inside the sandbox.
    Bash,
    /// Run the Playwright CLI (test, codegen, show-report, …).
    ///
    /// Image ships node + playwright + chromium + firefox + the system
    /// libs they link against. Examples: `arbox playwright test`,
    /// `arbox playwright codegen https://example.com`,
    /// `arbox playwright show-report`.
    Playwright {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Run an arbitrary command: `arbox run -- cargo test`.
    Run {
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        cmd: Vec<String>,
    },
    /// Refresh the baked-in agents (claude, codex, opencode, agy, grok) to
    /// their latest published versions, rebuilding the image in place.
    ///
    /// By default only the five agent layers re-run, so it's quick — the
    /// apt/uv/deno/node/playwright layers stay cached. Use --force for a full
    /// clean rebuild of the entire image (re-runs apt, node, the ~700 MB
    /// Playwright browser downloads, everything).
    ///
    /// (Also reachable as `arbox build`.)
    #[command(alias = "build")]
    Update {
        /// Full clean rebuild of the whole image instead of just the agents.
        #[arg(long)]
        force: bool,
    },
    /// Show host facts, image presence, and mount layout.
    Status,
    /// Remove every arbox image for this host.
    Clean,
}

fn main() -> ExitCode {
    if std::env::var_os("ARBOX_INSIDE").is_some() {
        eprintln!("arbox is the host-side orchestrator; it cannot run inside its own container.");
        eprintln!("Exit this shell and run `arbox` from your host.");
        return ExitCode::FAILURE;
    }
    let cli = Cli::parse();
    if let Some(p) = &cli.profile {
        if let Err(e) = launch::validate_profile_name(p) {
            eprintln!("error: {e:#}");
            return ExitCode::FAILURE;
        }
    }
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
    let opts = launch::Opts {
        rw,
        ro: cli.ro,
        profile: cli.profile,
    };
    match dispatch(cli.cmd, opts) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn dispatch(cmd: Cmd, opts: launch::Opts) -> Result<ExitCode> {
    match cmd {
        Cmd::Claude { args } => launch::run_claude(args, opts),
        Cmd::Codex { args } => launch::run_codex(args, opts),
        Cmd::Opencode { args } => launch::run_opencode(args, opts),
        Cmd::Agy { args } => launch::run_agy(args, opts),
        Cmd::Grok { args } => launch::run_grok(args, opts),
        Cmd::Bash => launch::run_bash(opts),
        Cmd::Playwright { args } => launch::run_playwright(args, opts),
        Cmd::Run { cmd } => launch::run_argv(cmd, opts),
        Cmd::Update { force } => image::update_image(force).map(|_| ExitCode::SUCCESS),
        Cmd::Status => image::print_status(opts.profile.as_deref()).map(|_| ExitCode::SUCCESS),
        Cmd::Clean => image::clean().map(|_| ExitCode::SUCCESS),
    }
}
