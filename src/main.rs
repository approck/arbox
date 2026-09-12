use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

use arbox::{image, launch};

/// Every arbox option goes BEFORE the verb: `arbox --voice --mount-gh claude
/// --resume`. Everything after the verb is the verb's own command line and is
/// forwarded to it verbatim — arbox parses none of it, not even `--help`.
/// That is what makes `arbox claude --help` show claude's help and `arbox gh
/// pr create --fill` mean what it says, at the cost of arbox flags after the
/// verb no longer working: `arbox claude --voice` hands `--voice` to claude.
#[derive(Parser, Debug)]
#[command(
    name = "arbox",
    version,
    about = "Docker-based agent sandbox: a skinny chroot of the host",
    long_about = "Docker-based agent sandbox: a skinny chroot of the host.\n\n\
                  arbox options go before the verb; everything after the verb is \
                  passed to it verbatim:\n\n    \
                  arbox [OPTIONS] claude [CLAUDE ARGS...]\n    \
                  arbox --voice --mount-gh claude --resume\n    \
                  arbox --rw ~/scratch run cargo test"
)]
struct Cli {
    /// Mount HOST_PATH read-write (repeatable).
    #[arg(long = "rw", value_name = "PATH")]
    rw: Vec<PathBuf>,

    /// Mount HOST_PATH read-only (repeatable).
    #[arg(long = "ro", value_name = "PATH")]
    ro: Vec<PathBuf>,

    /// Shortcut for --rw $HOME/Desktop (fails if missing).
    #[arg(long = "desktop")]
    desktop: bool,

    /// Shortcut for --rw $HOME/Downloads (fails if missing).
    #[arg(long = "downloads")]
    downloads: bool,

    /// Bind the host's sound hardware into the container: the
    /// PulseAudio/PipeWire socket and/or the ALSA devices under /dev/snd.
    /// Off by default; fails if the host has no audio to bind. Linux only.
    ///
    /// For `arbox claude` this also turns Claude Code's voice mode on for the
    /// session (via --settings), so push-to-talk works without running /voice
    /// first and without editing your host settings.json.
    #[arg(long = "voice")]
    voice: bool,

    /// Bind the host's USB serial devices into the container:
    /// every /dev/ttyUSB* and /dev/ttyACM* node, with the owning group
    /// re-added so they're openable. For flashing and monitoring dev boards
    /// (ESP32 etc.) with espflash/esptool from inside the sandbox. Off by
    /// default; fails if the host has no such device. Linux only.
    #[arg(long = "serial")]
    serial: bool,

    /// Bind one specific serial device instead of every USB serial node
    /// (repeatable; implies --serial). Accepts the real node or a
    /// /dev/serial/by-id/... symlink.
    #[arg(long = "serial-dev", value_name = "DEV")]
    serial_dev: Vec<PathBuf>,

    /// Bind the host's Wayland display socket into the container,
    /// so processes inside can open windows on your desktop and read the
    /// clipboard (claude's image paste). This is the DEFAULT whenever the
    /// host has a Wayland session; spelling it out makes a missing session a
    /// hard error instead of a silent skip. Wayland only — the X11 socket is
    /// never mounted. Linux only.
    ///
    /// Windows render in software (Mesa llvmpipe/lavapipe) unless --gpu is
    /// also given.
    #[arg(long = "mount-wayland")]
    mount_wayland: bool,

    /// Leave the host's Wayland socket unmounted: no windows, no
    /// clipboard, no display of any kind inside the container.
    #[arg(long = "no-mount-wayland", conflicts_with = "mount_wayland")]
    no_mount_wayland: bool,

    /// Bind the host's GPU into the container: the DRM render nodes
    /// under /dev/dri (renderD*, never card*), with the owning group re-added
    /// so they're openable. Off by default — without it GL and Vulkan inside
    /// use Mesa's software rasterizers; fails if the host has no render node.
    /// Mesa GPUs (Intel, AMD, virtio) bind their render nodes; an NVIDIA GPU
    /// goes through the NVIDIA Container Toolkit (`--gpus all`) when it is
    /// installed, and otherwise the launch stops and prints what to install.
    /// Linux only.
    #[arg(long = "gpu")]
    gpu: bool,

    // Per-agent state mounts. Each agent verb mounts its OWN state and
    // nothing else; every other verb (bash, run, playwright) mounts none. So
    // `arbox codex` cannot read your Claude credentials or session history,
    // and `arbox bash` starts with no agent credentials at all. These flags
    // override that in either direction, on any verb.
    /// Mount Claude Code's `~/.claude` + `~/.claude.json`, whatever the verb's default.
    #[arg(long = "mount-claude")]
    mount_claude: bool,

    /// Leave Claude Code's `~/.claude` + `~/.claude.json` unmounted, even on `arbox claude`.
    #[arg(long = "no-mount-claude", conflicts_with = "mount_claude")]
    no_mount_claude: bool,

    /// Mount Codex CLI's `~/.codex`, whatever the verb's default.
    #[arg(long = "mount-codex")]
    mount_codex: bool,

    /// Leave Codex CLI's `~/.codex` unmounted, even on `arbox codex`.
    #[arg(long = "no-mount-codex", conflicts_with = "mount_codex")]
    no_mount_codex: bool,

    /// Mount OpenCode's `~/.config/opencode` + `~/.local/share/opencode`, whatever the verb's default.
    #[arg(long = "mount-opencode")]
    mount_opencode: bool,

    /// Leave OpenCode's `~/.config/opencode` + `~/.local/share/opencode` unmounted, even on `arbox opencode`.
    #[arg(long = "no-mount-opencode", conflicts_with = "mount_opencode")]
    no_mount_opencode: bool,

    /// Mount Antigravity's `~/.gemini` + `~/.config/antigravity`, whatever the verb's default.
    #[arg(long = "mount-agy")]
    mount_agy: bool,

    /// Leave Antigravity's `~/.gemini` + `~/.config/antigravity` unmounted, even on `arbox agy`.
    #[arg(long = "no-mount-agy", conflicts_with = "mount_agy")]
    no_mount_agy: bool,

    /// Mount Grok Build's `~/.grok`, whatever the verb's default.
    #[arg(long = "mount-grok")]
    mount_grok: bool,

    /// Leave Grok Build's `~/.grok` unmounted, even on `arbox grok`.
    #[arg(long = "no-mount-grok", conflicts_with = "mount_grok")]
    no_mount_grok: bool,

    /// Mount wrangler's global config dir — the host's Cloudflare login —
    ///, whatever the verb's default. On by default for
    /// `arbox wrangler`, off for every other verb.
    #[arg(long = "mount-wrangler")]
    mount_wrangler: bool,

    /// Leave wrangler's global config dir unmounted, even on
    /// `arbox wrangler`. The local dev server needs no Cloudflare credential:
    /// it simulates KV, R2, D1, Durable Objects and Queues on the machine.
    #[arg(long = "no-mount-wrangler", conflicts_with = "mount_wrangler")]
    no_mount_wrangler: bool,

    /// Mount gh's config dir — the host's GitHub login —, whatever
    /// the verb's default. On by default for `arbox gh`, off for every other
    /// verb.
    #[arg(long = "mount-gh")]
    mount_gh: bool,

    /// Leave gh's config dir unmounted, even on `arbox gh`.
    #[arg(long = "no-mount-gh", conflicts_with = "mount_gh")]
    no_mount_gh: bool,

    /// Use a named auth profile. Sources each agent's ENTIRE state
    /// tree (auth + history + memories + sessions + settings) from
    /// `~/.arbox/profiles/NAME/` instead of the standard host locations, so a
    /// second subscription runs fully self-contained and never touches your
    /// default login. Auth and history always match because they live in one
    /// tree. The default (no --profile) shares your normal host locations.
    #[arg(long = "profile", value_name = "NAME")]
    profile: Option<String>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run Claude Code (--dangerously-skip-permissions injected).
    ///
    /// Everything after `claude` is forwarded to claude verbatim:
    ///   `arbox claude --resume`, `arbox claude "describe this repo"`.
    /// With `arbox --voice claude`, claude also starts with voice mode enabled.
    #[command(disable_help_flag = true)]
    Claude {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Run Codex CLI (approval-bypass flag injected).
    ///
    /// Passes --dangerously-bypass-approvals-and-sandbox. Everything after
    /// `codex` is forwarded to codex verbatim.
    #[command(disable_help_flag = true)]
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
    /// (e.g. Ollama on localhost:11434) work on Linux via host networking;
    /// on Windows, Docker Desktop reaches them only with its opt-in
    /// host-networking feature enabled. Everything after `opencode` is
    /// forwarded verbatim.
    #[command(disable_help_flag = true)]
    Opencode {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Run Google Antigravity's `agy` CLI.
    ///
    /// The binary is baked into the image; ~/.gemini and
    /// ~/.config/antigravity mount from the host for credential / skill /
    /// MCP persistence. First-time auth uses agy's SSH-style URL+code flow
    /// because libsecret isn't available inside the container. Everything
    /// after `agy` is forwarded verbatim.
    #[command(disable_help_flag = true)]
    Agy {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Run xAI's Grok Build (`grok`) CLI.
    ///
    /// The binary is baked into the image; ~/.grok mounts from the host
    /// for auth (token in ~/.grok/auth.json) and download cache. Everything
    /// after `grok` is forwarded verbatim — grok's safety story is plan-mode
    /// review, not an approval-bypass flag.
    #[command(disable_help_flag = true)]
    Grok {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Interactive bash login shell inside the sandbox.
    ///
    /// Everything after `bash` is forwarded to `bash -l` verbatim:
    /// `arbox bash -c 'cargo test'`.
    #[command(disable_help_flag = true)]
    Bash {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Run the Playwright CLI (test, codegen, show-report, …).
    ///
    /// Image ships node + playwright + chromium + firefox + the system
    /// libs they link against. Examples: `arbox playwright test`,
    /// `arbox playwright codegen https://example.com`,
    /// `arbox playwright show-report`. Everything after `playwright` is
    /// forwarded verbatim.
    #[command(disable_help_flag = true)]
    Playwright {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Run the Cloudflare Workers CLI: `arbox wrangler dev`.
    ///
    /// Runs the wrangler baked into the image against the current workspace,
    /// so the host needs neither node nor wrangler installed. This verb — and
    /// only this verb — mounts wrangler's config dir, so your `wrangler login`
    /// carries over and persists. `wrangler dev` itself needs no Cloudflare
    /// credential: it runs the Worker locally in workerd with KV, R2, D1,
    /// Durable Objects and Queues simulated on the machine. Everything after
    /// `wrangler` is forwarded verbatim.
    #[command(disable_help_flag = true)]
    Wrangler {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Run the GitHub CLI: `arbox gh pr create`.
    ///
    /// Runs the gh baked into the image against the current workspace. This
    /// verb — and only this verb — mounts gh's config dir, so your `gh auth
    /// login` carries over and persists, and `git push` over HTTPS works
    /// when your ~/.gitconfig uses `gh auth git-credential`. Everything after
    /// `gh` is forwarded verbatim. Log in from inside with `arbox gh auth
    /// login` (the device-code flow; there is no browser in the box).
    #[command(disable_help_flag = true)]
    Gh {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Run an arbitrary command: `arbox run cargo test`.
    ///
    /// Everything after `run` is the command line, verbatim.
    #[command(disable_help_flag = true)]
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
    /// Install the NVIDIA Container Toolkit on this Ubuntu host (runs apt
    /// via sudo), so `arbox --gpu` can hand an NVIDIA GPU to the container.
    ///
    /// Also reachable as choice (2) of the menu `--gpu` shows when the NVIDIA
    /// driver is loaded but the toolkit is missing. Uses Ubuntu's own package
    /// when the release carries it, else adds NVIDIA's apt repository. Prints
    /// every sudo step before running it. Does not re-launch anything: re-run
    /// your `arbox --gpu ...` command afterwards.
    #[command(name = "install-nvidia-container-toolkit")]
    InstallNvidiaContainerToolkit,
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
    // An explicit device list is a narrower request than the bare flag, so it
    // wins when both are given.
    let serial = if !cli.serial_dev.is_empty() {
        Some(launch::SerialRequest::Devices(cli.serial_dev))
    } else if cli.serial {
        Some(launch::SerialRequest::Auto)
    } else {
        None
    };
    // Table-driven so adding an agent means one row here, not a new branch.
    let mut mounts = launch::MountOverrides::default();
    for (name, on, off) in [
        ("claude", cli.mount_claude, cli.no_mount_claude),
        ("codex", cli.mount_codex, cli.no_mount_codex),
        ("opencode", cli.mount_opencode, cli.no_mount_opencode),
        ("agy", cli.mount_agy, cli.no_mount_agy),
        ("grok", cli.mount_grok, cli.no_mount_grok),
        ("wrangler", cli.mount_wrangler, cli.no_mount_wrangler),
        ("gh", cli.mount_gh, cli.no_mount_gh),
    ] {
        if on || off {
            mounts.set(name, on);
        }
    }
    let opts = launch::Opts {
        rw,
        ro: cli.ro,
        profile: cli.profile,
        voice: cli.voice,
        serial,
        wayland: match (cli.mount_wayland, cli.no_mount_wayland) {
            (true, _) => Some(true),
            (_, true) => Some(false),
            _ => None,
        },
        gpu: cli.gpu,
        mounts,
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
        Cmd::Bash { args } => launch::run_bash(args, opts),
        Cmd::Playwright { args } => launch::run_playwright(args, opts),
        Cmd::Wrangler { args } => launch::run_wrangler(args, opts),
        Cmd::Gh { args } => launch::run_gh(args, opts),
        Cmd::Run { cmd } => launch::run_argv(cmd, opts),
        Cmd::Update { force } => image::update_image(force).map(|_| ExitCode::SUCCESS),
        Cmd::InstallNvidiaContainerToolkit => launch::install_nvidia_container_toolkit(),
        Cmd::Status => {
            image::print_status(opts.profile.as_deref(), &opts.mounts).map(|_| ExitCode::SUCCESS)
        }
        Cmd::Clean => image::clean().map(|_| ExitCode::SUCCESS),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(argv: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("arbox").chain(argv.iter().copied()))
            .unwrap_or_else(|e| panic!("{argv:?}: {e}"))
    }

    fn passthrough(cmd: Cmd) -> Vec<String> {
        match cmd {
            Cmd::Claude { args }
            | Cmd::Codex { args }
            | Cmd::Opencode { args }
            | Cmd::Agy { args }
            | Cmd::Grok { args }
            | Cmd::Bash { args }
            | Cmd::Playwright { args }
            | Cmd::Wrangler { args }
            | Cmd::Gh { args } => args,
            Cmd::Run { cmd } => cmd,
            other => panic!("not a pass-through verb: {other:?}"),
        }
    }

    /// The contract: arbox options before the verb, and everything after the
    /// verb — flags, `--help`, arbox's own option names — belongs to the verb.
    #[test]
    fn options_before_verb_and_verbatim_after() {
        let cli = parse(&["--voice", "--mount-gh", "claude", "--resume", "--voice"]);
        assert!(cli.voice);
        assert!(cli.mount_gh);
        assert_eq!(passthrough(cli.cmd), ["--resume", "--voice"]);

        // An arbox option after the verb is NOT an arbox option any more.
        let cli = parse(&["claude", "--voice"]);
        assert!(!cli.voice);
        assert_eq!(passthrough(cli.cmd), ["--voice"]);

        // `--help` after a pass-through verb goes to the tool, not to clap.
        assert_eq!(passthrough(parse(&["claude", "--help"]).cmd), ["--help"]);
        assert_eq!(passthrough(parse(&["gh", "-h"]).cmd), ["-h"]);
        assert_eq!(
            passthrough(parse(&["gh", "pr", "create", "--fill"]).cmd),
            ["pr", "create", "--fill"]
        );
        assert_eq!(
            passthrough(parse(&["bash", "-c", "cargo test"]).cmd),
            ["-c", "cargo test"]
        );
        assert_eq!(
            passthrough(parse(&["run", "cargo", "test", "--", "--nocapture"]).cmd),
            ["cargo", "test", "--", "--nocapture"]
        );
    }

    /// A leading `--` right after the verb is still accepted (older docs and
    /// scripts use it) and is swallowed by clap rather than forwarded.
    #[test]
    fn leading_double_dash_still_works() {
        assert_eq!(
            passthrough(parse(&["run", "--", "cargo", "test"]).cmd),
            ["cargo", "test"]
        );
        assert_eq!(
            passthrough(parse(&["claude", "--", "--resume"]).cmd),
            ["--resume"]
        );
    }

    /// Top-level help and the non-pass-through verbs keep clap's own help.
    #[test]
    fn arbox_help_still_reachable() {
        use clap::error::ErrorKind;
        for argv in [
            vec!["--help"],
            vec!["update", "--help"],
            vec!["status", "-h"],
        ] {
            let err = Cli::try_parse_from(std::iter::once("arbox").chain(argv.iter().copied()))
                .expect_err("help should short-circuit");
            assert_eq!(err.kind(), ErrorKind::DisplayHelp, "{argv:?}");
        }
        let cli = parse(&["update", "--force"]);
        assert!(matches!(cli.cmd, Cmd::Update { force: true }));
    }
}
