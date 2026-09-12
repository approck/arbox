use anyhow::{bail, Context, Result};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use crate::host::{self, HostContext};
use crate::image;

/// One bind-mount the container needs. Almost every mount lands at the same
/// absolute path on both sides (`src == dst`) — that's what makes cargo's
/// incremental fingerprints carry across the host ↔ container boundary.
///
/// The exception is `--profile NAME`: each agent's entire state tree is sourced
/// from under `~/.arbox/profiles/NAME/` while `dst` stays the canonical path
/// the agent expects (e.g. src `~/.arbox/profiles/personal/.claude` →
/// dst `~/.claude`). The agent reads/writes the standard location, unaware its
/// whole config — auth *and* history together — actually lives in the profile
/// dir, so the two can never drift out of sync the way redirecting auth alone
/// would risk.
pub struct MountSpec {
    /// Host source path. Existence is checked against this.
    pub src: PathBuf,
    /// Container destination path. Equals `src` unless this is a redirect.
    pub dst: PathBuf,
    pub read_only: bool,
    /// Hard-required: launch fails if missing on host. (Optional mounts are
    /// silently skipped when their host path doesn't exist.)
    pub required: bool,
    pub hint: Option<&'static str>,
}

impl MountSpec {
    /// Same path on both sides — the common case.
    fn new(path: PathBuf, read_only: bool, required: bool, hint: Option<&'static str>) -> Self {
        Self {
            src: path.clone(),
            dst: path,
            read_only,
            required,
            hint,
        }
    }

    /// Mount host `src` at container `dst` (distinct paths). Used for
    /// `--profile` auth redirects.
    fn redirected(
        src: PathBuf,
        dst: PathBuf,
        read_only: bool,
        required: bool,
        hint: Option<&'static str>,
    ) -> Self {
        Self {
            src,
            dst,
            read_only,
            required,
            hint,
        }
    }
}

/// One coding agent's registry entry. This table is the single source of
/// truth for the per-agent state layout: `select` resolves which of them a
/// given launch mounts, `mount_specs` derives the mount list from that, and
/// `ensure_state` pre-creates the host-side paths — add an agent (or move a
/// path) here and every consumer follows. The Dockerfile's XDG parent-dir
/// pre-creation (`install -d` in src/Dockerfile) must cover the parents of
/// any nested path listed here.
struct AgentSpec {
    /// Verb name — identical to the binary name inside the image.
    name: &'static str,
    /// Home-relative state dirs, mounted RW and pre-created on first launch.
    dirs: &'static [&'static str],
    /// State FILES seeded with initial contents so the bind mount has a
    /// source to attach to (a missing source would be silently skipped,
    /// making the file ephemeral in the container).
    seed_files: &'static [(&'static str, &'static str)],
    /// The agent resolves its dirs through the XDG base-dir spec on the host
    /// (honors XDG_CONFIG_HOME / XDG_DATA_HOME), so the host-side mount
    /// sources must resolve the same way.
    xdg: bool,
}

/// Per-agent state registry. Whole-tree redirected into the profile dir under
/// `--profile`, or mounted at the standard host location by default. Listing
/// the data paths (not the binaries — those are baked into the image at
/// /usr/local/bin) is what persists credentials, history, skills, MCP config,
/// memories, and sessions across container runs.
const AGENTS: &[AgentSpec] = &[
    // claude: the .claude/ dir (sessions, memories, settings, credentials,
    // plans, tasks) plus the .claude.json config/auth-identity file beside
    // it, seeded with `{}` (parseable JSON) so claude's first load doesn't
    // choke on a zero-byte mount target.
    AgentSpec {
        name: "claude",
        dirs: &[".claude"],
        seed_files: &[(".claude.json", "{}\n")],
        xdg: false,
    },
    AgentSpec {
        name: "codex",
        dirs: &[".codex"],
        seed_files: &[],
        xdg: false,
    },
    // opencode: global config (opencode.json, themes, agents, commands) under
    // ~/.config/opencode; auth tokens + session storage under
    // ~/.local/share/opencode (auth.json, storage/). The opencode BINARY is
    // baked into the image, but its data dir is not purely inert state:
    // opencode downloads helper executables (LSP servers, ripgrep, fzf) into
    // ~/.local/share/opencode/bin, so this RW mount carries executables in
    // both directions.
    AgentSpec {
        name: "opencode",
        dirs: &[".config/opencode", ".local/share/opencode"],
        seed_files: &[],
        xdg: true,
    },
    // agy: Antigravity reuses the Gemini namespace for skills/MCP/GEMINI.md,
    // with per-host config under ~/.config/antigravity. Not known to honor
    // XDG overrides — the ~/.config path is fixed.
    AgentSpec {
        name: "agy",
        dirs: &[".gemini", ".config/antigravity"],
        seed_files: &[],
        xdg: false,
    },
    AgentSpec {
        name: "grok",
        dirs: &[".grok"],
        seed_files: &[],
        xdg: false,
    },
];

/// Flat view of the home-relative state paths belonging to `agents`, each
/// paired with its agent's XDG flag. Drives the mount list and the tests.
fn agent_state_paths<'a>(agents: &'a [&str]) -> impl Iterator<Item = (&'static str, bool)> + 'a {
    AGENTS
        .iter()
        .filter(move |a| agents.contains(&a.name))
        .flat_map(|a| {
            a.dirs
                .iter()
                .copied()
                .chain(a.seed_files.iter().map(|(rel, _)| *rel))
                .map(move |rel| (rel, a.xdg))
        })
}

/// The `--mount-<name>` / `--no-mount-<name>` overrides, as given on the
/// command line. Empty means "whatever the verb defaults to". Names are the
/// five agents plus the tools in `TOOLS` (`wrangler`, `gh`).
#[derive(Default, Debug)]
pub struct MountOverrides {
    on: Vec<String>,
    off: Vec<String>,
}

impl MountOverrides {
    /// Record one override: `on` for `--mount-<name>`, `!on` for
    /// `--no-mount-<name>`.
    pub fn set(&mut self, name: &str, on: bool) {
        let bucket = if on { &mut self.on } else { &mut self.off };
        bucket.push(name.to_string());
    }

    /// The user's explicit choice for `name`, if they made one. `--no-mount`
    /// wins if somehow both were given, so the safe answer is the sticky one.
    fn resolve(&self, name: &str) -> Option<bool> {
        if self.off.iter().any(|a| a == name) {
            Some(false)
        } else if self.on.iter().any(|a| a == name) {
            Some(true)
        } else {
            None
        }
    }
}

/// The credential-bearing state one launch mounts: which agents' trees, and
/// which non-agent tools' config dirs (`TOOLS`) come along.
pub struct Selection {
    agents: Vec<&'static str>,
    tools: Vec<&'static str>,
}

impl Selection {
    fn has_tool(&self, name: &str) -> bool {
        self.tools.contains(&name)
    }
}

/// Resolve what a verb mounts: its own defaults, then the user's overrides.
///
/// The defaults are deliberately narrow. An agent verb passes its own name in
/// `agents` and nothing else; a tool verb (`arbox wrangler`, `arbox gh`)
/// passes its own name in `tools`; every other verb (`bash`, `run`,
/// `playwright`) passes nothing at all. So `arbox codex` never sees your
/// Claude credentials or session history, `arbox playwright test` sees no
/// credentials whatsoever, and a Cloudflare or GitHub token only reaches the
/// container when you actually run that tool. `--mount-<name>` and
/// `--no-mount-<name>` override that in either direction, on any verb.
fn select(agents: &[&str], tools: &[&str], ov: &MountOverrides) -> Selection {
    let pick = |name: &&'static str, defaults: &[&str]| {
        ov.resolve(name)
            .unwrap_or_else(|| defaults.iter().any(|d| d == name))
    };
    Selection {
        agents: AGENTS
            .iter()
            .map(|a| a.name)
            .filter(|name| pick(name, agents))
            .collect(),
        tools: TOOLS
            .iter()
            .map(|t| t.name)
            .filter(|name| pick(name, tools))
            .collect(),
    }
}

/// Root of a named profile's isolated state tree on the host.
fn profile_root(home: &Path, name: &str) -> PathBuf {
    home.join(".arbox").join("profiles").join(name)
}

/// Resolve where an agent state path actually lives on the HOST.
///
/// Under `--profile` everything roots at the profile dir (XDG overrides are
/// deliberately ignored — the profile tree is arbox's own layout). Otherwise
/// XDG-aware entries resolve through the host's XDG base dirs, falling back
/// to the literal home-relative path. The container side always uses the
/// home-relative default: no XDG vars are forwarded inside, so the
/// in-container layout stays fixed while the host source follows wherever
/// the host's own agent actually keeps its state.
fn state_source(home: &Path, rel: &str, xdg: bool, profile: Option<&str>) -> PathBuf {
    match profile {
        Some(p) => profile_root(home, p).join(rel),
        None if xdg => {
            xdg_override(rel, &|var| std::env::var_os(var)).unwrap_or_else(|| home.join(rel))
        }
        None => home.join(rel),
    }
}

/// XDG base-dir override for `rel`, if the matching env var holds an
/// absolute path (the basedir spec says relative values must be ignored).
/// None means "use the default location".
fn xdg_override(rel: &str, get: &dyn Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    let (var, suffix) = if let Some(s) = rel.strip_prefix(".config/") {
        ("XDG_CONFIG_HOME", s)
    } else {
        let s = rel.strip_prefix(".local/share/")?;
        ("XDG_DATA_HOME", s)
    };
    let base = PathBuf::from(get(var)?);
    base.is_absolute().then(|| base.join(suffix))
}

/// Env lookup handed to the host-side path resolvers, so tests can feed a
/// synthetic environment.
type EnvGet<'a> = &'a dyn Fn(&str) -> Option<std::ffi::OsString>;

/// A non-agent tool baked into the image whose login lives in one config dir
/// on the host. Like an agent's state, that dir is mounted RW only by the
/// tool's own verb (or an explicit `--mount-<name>`), because what it holds
/// is an account-wide credential no other verb has any business with. Unlike
/// agent state it is NOT profile-scoped — a Cloudflare or GitHub account
/// isn't tied to an agent subscription, and both tools carry their own
/// multi-account switching.
struct ToolSpec {
    /// Verb name — identical to the binary name inside the image.
    name: &'static str,
    /// Container-side home-relative config dir. Fixed, because the container
    /// is always Linux and arbox forwards no XDG vars into it, so the tool in
    /// the image always resolves this path under its HOME.
    container_rel: &'static str,
    /// Where that same dir lives on the HOST, following the tool's own
    /// per-platform resolution so the mount is SHARED with a host-side
    /// install rather than a second, silently diverging login.
    source: fn(&Path, EnvGet) -> PathBuf,
    /// What the credential in there is, for `arbox status` and the README.
    credential: &'static str,
}

const TOOLS: &[ToolSpec] = &[
    // wrangler: the refreshable OAuth token `wrangler login` caches. Account-
    // wide and deploy-capable (workers, d1, pages, zone, ssl_certs,
    // containers, secrets_store, …). Local development needs none of it:
    // `wrangler dev` simulates KV, R2, D1, Durable Objects and Queues on the
    // machine. Mounted RW because wrangler writes both its token and its logs
    // there.
    ToolSpec {
        name: "wrangler",
        container_rel: ".config/.wrangler",
        source: wrangler_config_source,
        credential: "your Cloudflare login",
    },
    // gh: `hosts.yml` holds the OAuth token `gh auth login` stores (when no
    // system keyring is reachable — the case inside the container, and on any
    // host where gh fell back to the file), plus `config.yml`. That token can
    // read and push every repo the account can, open PRs, and read private
    // repos, so `arbox claude` doesn't get it unless asked. Mounted RW so a
    // `gh auth login` from inside persists and token refreshes stick.
    ToolSpec {
        name: "gh",
        container_rel: ".config/gh",
        source: gh_config_source,
        credential: "your GitHub login",
    },
];

/// Where wrangler's global config dir lives on the HOST.
/// Wrangler resolves it through its vendored copy of `xdg-app-paths`:
///
///   - a pre-existing legacy `~/.wrangler` directory wins over everything;
///   - otherwise `$XDG_CONFIG_HOME/.wrangler` when that's set;
///   - otherwise `~/.config/.wrangler` on Linux,
///     `~/Library/Preferences/.wrangler` on macOS, and
///     `%APPDATA%\xdg.config\.wrangler` on Windows.
///
/// Following that exactly is what makes the mount SHARED with a host-side
/// wrangler install rather than a second, silently diverging login.
fn wrangler_config_source(
    home: &Path,
    get: &dyn Fn(&str) -> Option<std::ffi::OsString>,
) -> PathBuf {
    let legacy = home.join(".wrangler");
    if legacy.is_dir() {
        return legacy;
    }
    // A relative XDG value is invalid per the basedir spec, and would be
    // meaningless as a mount source besides.
    if let Some(base) = get("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        return base.join(".wrangler");
    }
    if cfg!(target_os = "macos") {
        home.join("Library").join("Preferences").join(".wrangler")
    } else if cfg!(target_family = "windows") {
        get("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData").join("Roaming"))
            .join("xdg.config")
            .join(".wrangler")
    } else {
        home.join(".config").join(".wrangler")
    }
}

/// Where gh's config dir lives on the HOST, following gh's own resolution
/// (`internal/config.ConfigDir`):
///
///   - `$GH_CONFIG_DIR` when set;
///   - otherwise `$XDG_CONFIG_HOME/gh` when that's set;
///   - otherwise `%AppData%\GitHub CLI` on Windows and `~/.config/gh`
///     everywhere else (macOS included — gh does not use ~/Library).
fn gh_config_source(home: &Path, get: EnvGet) -> PathBuf {
    if let Some(dir) = get("GH_CONFIG_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        return dir;
    }
    if let Some(base) = get("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        return base.join("gh");
    }
    if cfg!(target_family = "windows") {
        get("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData").join("Roaming"))
            .join("GitHub CLI")
    } else {
        home.join(".config").join("gh")
    }
}

/// Build the bind-mount list. With no `profile`, agent state mounts at the
/// standard host locations (shared with your host's own agents). With
/// `--profile NAME`, each agent's entire state tree is sourced from
/// `~/.arbox/profiles/NAME/` instead — auth and history move together, so a
/// second subscription stays fully self-consistent and never touches the
/// default. Non-agent mounts (workspace, Rust toolchain, ~/.gitconfig) are
/// always shared regardless of profile.
///
/// `sel` is the resolved `Selection` for the verb being launched — only the
/// state it names is mounted, so the verb decides what credentials the
/// container can see.
pub fn mount_specs(host: &HostContext, profile: Option<&str>, sel: &Selection) -> Vec<MountSpec> {
    let h = &host.home;
    let mut specs: Vec<MountSpec> = Vec::new();

    if let Some(workspace) = &host.workspace_root {
        specs.push(MountSpec::new(workspace.clone(), false, true, None));
        // Worktree case: the workspace's `.git` is a file pointing into a
        // separate common git dir (e.g. <main-repo>/.git). Mount it so git
        // operations resolve inside the container. Skipped for normal
        // checkouts where the common dir is `<workspace>/.git` and already
        // inside the workspace mount.
        if let Some(common) = &host.git_common_dir {
            if !common.starts_with(workspace) {
                specs.push(MountSpec::new(
                    common.clone(),
                    false,
                    true,
                    Some("git common dir not found"),
                ));
            }
        }
    }

    // Mounted from the host only where the image doesn't bake its own rustup
    // (see host::bakes_rustup / image::build_with_args).
    if !host::bakes_rustup() {
        specs.extend([
            MountSpec::new(
                h.join(".cargo"),
                false,
                true,
                Some("install rustup on the host first (https://rustup.rs)"),
            ),
            MountSpec::new(
                h.join(".rustup"),
                true,
                true,
                Some("install rustup on the host first (https://rustup.rs)"),
            ),
        ]);
    }

    // Agent state dirs/files for the SELECTED agents only — mounted RW and
    // optional. `ensure_agent_state` pre-creates their sources on launch, so
    // these mounts reliably attach without any host-side prep. Under a profile
    // the source moves into the profile dir, and for XDG-aware agents it
    // follows the host's XDG base dirs; the destination (what the agent sees
    // in the container) always stays canonical.
    for (rel, xdg) in agent_state_paths(&sel.agents) {
        let dst = h.join(rel);
        let src = state_source(h, rel, xdg, profile);
        if src == dst {
            specs.push(MountSpec::new(dst, false, false, None));
        } else {
            specs.push(MountSpec::redirected(src, dst, false, false, None));
        }
    }

    // Tool config dirs (wrangler, gh) for the SELECTED tools only — on for
    // the tool's own verb, off everywhere else, per the `TOOLS` table. The
    // destination is the container's fixed Linux path; only the source
    // follows the host's platform. Never profile-scoped.
    for tool in TOOLS.iter().filter(|t| sel.has_tool(t.name)) {
        let dst = h.join(tool.container_rel);
        let src = (tool.source)(h, &|var| std::env::var_os(var));
        specs.push(if src == dst {
            MountSpec::new(dst, false, false, None)
        } else {
            MountSpec::redirected(src, dst, false, false, None)
        });
    }

    // Host's ~/.gitconfig (read-only) — git identity isn't tied to an agent
    // subscription, so it stays shared across profiles. Skipped if absent.
    specs.push(MountSpec::new(h.join(".gitconfig"), true, false, None));

    // approck's data dir (~/.local/share/approck), read-only. Optional —
    // skipped if absent.
    specs.push(MountSpec::new(
        h.join(".local").join("share").join("approck"),
        true,
        false,
        None,
    ));

    // Deliberately NOT mounted: ~/.local/bin and ~/.local/share/claude. Those
    // hold the host's own agent BINARIES (claude/codex/agy live there) — arbox
    // runs the versions baked into the image instead (bumped via `arbox
    // update`), so mounting the host copies would only let them shadow the
    // baked binary on PATH (the exact staleness bug this avoids). All agent
    // DATA lives in the state paths above.

    specs
}

/// On Windows, fix git worktree paths so they resolve correctly in the
/// container. The `.git` file in a worktree contains an absolute Windows path
/// that doesn't work inside the container. Replace it with a relative path
/// and add the container path to git's safe.directory. Returns the container
/// path if one was added (for cleanup on exit).
fn fixup_windows_worktree(host: &HostContext) -> Result<Option<String>> {
    if !cfg!(target_family = "windows") {
        return Ok(None);
    }

    let Some(workspace) = &host.workspace_root else {
        return Ok(None);
    };

    let git_file = workspace.join(".git");
    // Check if .git is a file (worktree) vs directory (normal checkout)
    if !git_file.is_file() {
        return Ok(None);
    }

    let git_content = std::fs::read_to_string(&git_file).context("reading .git file")?;

    let Some(git_dir) = git_content
        .lines()
        .find_map(|l| l.strip_prefix("gitdir: "))
        .map(|path| PathBuf::from(path).canonicalize())
        .transpose()?
    else {
        return Ok(None);
    };

    // Calculate relative path from worktree to the git common dir
    if let Some(rel_path) = pathdiff::diff_paths(git_dir, workspace) {
        let rel_str = rel_path.to_string_lossy().replace("\\", "/");
        let new_content = format!("gitdir: {}\n", rel_str);

        if git_content != new_content {
            std::fs::write(&git_file, &new_content)
                .context("writing .git file with relative path")?;
        }

        // Add the container path to git's safe.directory
        let container_path = crate::path::to_container(workspace)?;
        let mut cmd = Command::new("git");
        cmd.args([
            "config",
            "--global",
            "--add",
            "safe.directory",
            &container_path,
        ]);
        let _ = cmd.status(); // Ignore errors; this is best-effort

        return Ok(Some(container_path));
    }

    Ok(None)
}

/// Pre-create the host-side state paths this launch will write into, so their
/// bind mounts have something to attach to on first run. Without this, Docker
/// silently skips missing-source mounts and the tool runs with ephemeral state
/// every launch — a `wrangler login` whose token vanishes on exit. Only the
/// selected paths are touched, so a verb never creates state it isn't
/// mounting.
///
/// `profile` only changes WHERE these paths are rooted: the home directory
/// (or the host's XDG base dirs, for XDG-aware agents) by default, or
/// `~/.arbox/profiles/NAME/` for a named profile. The layout underneath
/// (`.claude/`, `.claude.json`, …) is identical either way, which is what
/// lets the profile dir stand in as a self-contained agent home.
fn ensure_state(host: &HostContext, sel: &Selection, profile: Option<&str>) -> Result<()> {
    for tool in TOOLS.iter().filter(|t| sel.has_tool(t.name)) {
        // Not profile-aware: each tool's `source` mirrors wherever the host's
        // own copy of the tool keeps its config.
        let d = (tool.source)(&host.home, &|var| std::env::var_os(var));
        std::fs::create_dir_all(&d).with_context(|| format!("creating {}", d.display()))?;
    }
    for spec in AGENTS.iter().filter(|a| sel.agents.contains(&a.name)) {
        for rel in spec.dirs {
            let d = state_source(&host.home, rel, spec.xdg, profile);
            std::fs::create_dir_all(&d).with_context(|| format!("creating {}", d.display()))?;
        }
        // Seeded files (claude's .claude.json) get initial contents so the
        // agent's first load doesn't choke on a zero-byte mount target.
        for (rel, contents) in spec.seed_files {
            let f = state_source(&host.home, rel, spec.xdg, profile);
            if !f.exists() {
                std::fs::write(&f, contents)
                    .with_context(|| format!("creating {}", f.display()))?;
            }
        }
    }
    Ok(())
}

/// What `arbox status` should show. Status isn't a launch, so there is no
/// verb to take a default from — and defaulting to nothing would make
/// `arbox --profile NAME status` print no agent mounts at all, which is
/// exactly the command you'd run to check that a profile redirects where you
/// expect. So it defaults to every agent's state, i.e. the full map of what
/// some verb could mount, with the printed note explaining that any one launch
/// mounts a subset. `--no-mount-<name>` still subtracts from it.
pub fn status_selection(ov: &MountOverrides) -> Selection {
    let all: Vec<&str> = AGENTS.iter().map(|a| a.name).collect();
    select(&all, &[], ov)
}

/// Every agent verb name, for the `arbox status` reminder that each mounts
/// its own state and nothing else.
pub fn agent_names() -> Vec<&'static str> {
    AGENTS.iter().map(|a| a.name).collect()
}

/// `(verb name, credential description)` for every tool in `TOOLS`, for the
/// `arbox status` note on what each tool verb mounts.
pub fn tool_names() -> Vec<(&'static str, &'static str)> {
    TOOLS.iter().map(|t| (t.name, t.credential)).collect()
}

/// One-line `arbox status` description of each tool config mount, in the
/// same shape as the audio and serial summaries: what would be bound, and
/// whether this invocation actually binds it. One `(name, line)` per tool.
pub fn tool_summaries(host: &HostContext, sel: &Selection) -> Vec<(&'static str, String)> {
    TOOLS
        .iter()
        .map(|t| {
            let src = (t.source)(&host.home, &|var| std::env::var_os(var));
            let line = if sel.has_tool(t.name) {
                format!("{} (rw)", src.display())
            } else {
                format!(
                    "{} (bound only with `arbox {}` or --mount-{})",
                    src.display(),
                    t.name,
                    t.name
                )
            };
            (t.name, line)
        })
        .collect()
}

/// Per-invocation launch options shared by every verb: the user's extra
/// bind-mount paths, the optional `--profile` name, the per-tool mount
/// overrides, the `--voice` / `--serial` opt-ins, and the
/// `--mount-wayland` / `--no-mount-wayland` override of the display default.
/// Bundled so adding a cross-cutting knob doesn't ripple through every verb
/// function's signature.
#[derive(Default)]
pub struct Opts {
    pub rw: Vec<PathBuf>,
    pub ro: Vec<PathBuf>,
    pub profile: Option<String>,
    /// Bind the host's sound hardware into the container (`--voice`).
    pub voice: bool,
    /// `--mount-<name>` / `--no-mount-<name>` overrides, applied on top of
    /// whatever the verb mounts by default.
    pub mounts: MountOverrides,
    /// Bind USB serial devices into the container (`--serial` /
    /// `--serial-dev`). `None` binds nothing.
    pub serial: Option<SerialRequest>,
    /// Whether to bind the host's Wayland display socket into the container,
    /// so processes inside can open windows and read the clipboard. `None`
    /// is the default: bind it when the host has one, silently skip
    /// otherwise. `Some(true)` (`--mount-wayland`) makes a missing session a
    /// hard error; `Some(false)` (`--no-mount-wayland`) never binds it.
    pub wayland: Option<bool>,
}

/// Which USB serial devices `--serial` should bind. Auto-detection binds
/// every `/dev/ttyUSB*` and `/dev/ttyACM*` node on the host; the explicit
/// form binds only the named nodes (and accepts `/dev/serial/by-id/...`
/// symlinks, which resolve to the real node).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SerialRequest {
    Auto,
    Devices(Vec<PathBuf>),
}

/// Validate a `--profile` name before it becomes part of a filename. Reject
/// anything that could escape its directory or produce a surprising sibling.
pub fn validate_profile_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && !name.starts_with('-')
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !ok {
        bail!(
            "invalid --profile name {name:?}: use letters, digits, '-' or '_' \
             (and don't start with '-' or '.')"
        );
    }
    Ok(())
}

/// Shared skeleton for every agent verb: detect the host, demand a git
/// workspace, pre-create the agent's state paths, then run the agent's
/// binary (same name as the verb) with `injected` flags ahead of the user's
/// trailing args.
fn run_agent(
    agent: &'static str,
    injected: &[&str],
    extra: Vec<String>,
    opts: Opts,
) -> Result<ExitCode> {
    let host = host::detect()?;
    host::require_git(&host)?;
    // The verb's own agent is the only default — anything else has to be
    // asked for with --mount-<name>.
    let sel = select(&[agent], &[], &opts.mounts);
    ensure_state(&host, &sel, opts.profile.as_deref())?;
    let mut argv = vec![agent.to_string()];
    argv.extend(injected.iter().map(|f| f.to_string()));
    argv.extend(extra);
    run(host, argv, opts, &sel)
}

/// Claude Code has no `--voice` flag: voice mode is a *setting*
/// (`voice.enabled`), normally toggled by the `/voice` slash command, which
/// writes it into `~/.claude/settings.json`. Since that file is bind-mounted
/// from the host, toggling it inside the box would silently flip voice on for
/// the host's own claude too. `--settings <json>` instead layers these values
/// onto the effective settings for this session only, which is exactly the
/// scope `arbox --voice` implies. Only `enabled` is set, so a user who has
/// picked `voice.mode: "tap"` keeps whatever their own settings say.
const CLAUDE_VOICE_SETTINGS: &str = r#"{"voice":{"enabled":true}}"#;

pub fn run_claude(extra: Vec<String>, opts: Opts) -> Result<ExitCode> {
    // The container IS the sandbox, so granting claude full permissions
    // inside is the correct posture.
    let mut injected = vec!["--dangerously-skip-permissions"];
    if opts.voice {
        // Injected ahead of the user's trailing args, so an explicit
        // `arbox --voice claude --settings ...` still wins.
        injected.extend(["--settings", CLAUDE_VOICE_SETTINGS]);
    }
    run_agent("claude", &injected, extra, opts)
}

pub fn run_codex(extra: Vec<String>, opts: Opts) -> Result<ExitCode> {
    run_agent(
        "codex",
        &["--dangerously-bypass-approvals-and-sandbox"],
        extra,
        opts,
    )
}

pub fn run_opencode(extra: Vec<String>, opts: Opts) -> Result<ExitCode> {
    // opencode has no approval-bypass flag to inject — its default permission
    // posture is already permissive, and tightening it lives in the user's
    // mounted ~/.config/opencode/opencode.json. Auth is file-based
    // (~/.local/share/opencode/auth.json), no keyring needed, so `opencode
    // auth login` inside the box persists via the state mount. Local
    // providers on the host (e.g. Ollama on localhost:11434) are reachable
    // on Linux, where --network host is the real host network; on Windows
    // and macOS, Docker Desktop only honors --network host with its opt-in
    // host-networking feature enabled — off by default, so localhost inside
    // the container resolves to the Docker Desktop VM instead of the host.
    run_agent("opencode", &[], extra, opts)
}

pub fn run_agy(extra: Vec<String>, opts: Opts) -> Result<ExitCode> {
    // No documented `--dangerously-*` / `--yolo` flag for Antigravity yet —
    // forward args verbatim. The Docker boundary is still the sandbox; agy
    // itself just runs with whatever approval mode it defaults to. Note
    // that libsecret won't work inside the container (no dbus session), so
    // first-time auth typically goes through agy's SSH-style URL+code flow.
    // Under `--profile`, agy's whole ~/.gemini + ~/.config/antigravity move
    // into the profile tree, so whatever it persists there is isolated too.
    run_agent("agy", &[], extra, opts)
}

pub fn run_grok(extra: Vec<String>, opts: Opts) -> Result<ExitCode> {
    // Grok Build's safety story is its plan-mode review, not a global
    // approval-bypass flag — forward args verbatim. Auth lives in
    // ~/.grok/auth.json (file-based, no keyring dependency), which the
    // ~/.grok mount in `mount_specs` persists across runs.
    run_agent("grok", &[], extra, opts)
}

pub fn run_playwright(extra: Vec<String>, opts: Opts) -> Result<ExitCode> {
    let host = host::detect()?;
    host::require_git(&host)?;
    // `playwright` is npm-installed globally in the image. Browsers are
    // baked in at /opt/ms-playwright (PLAYWRIGHT_BROWSERS_PATH set in the
    // Dockerfile), so this works without any host-side setup.
    // Nothing credential-bearing by default: a browser test run has no use
    // for your Claude, Codex, or Cloudflare credentials.
    let sel = select(&[], &[], &opts.mounts);
    ensure_state(&host, &sel, opts.profile.as_deref())?;
    let mut argv = vec!["playwright".to_string()];
    argv.extend(extra);
    run(host, argv, opts, &sel)
}

/// `arbox wrangler ...` — run the image's Cloudflare CLI against the current
/// workspace, so the host needs neither node nor wrangler installed.
///
/// This is the one verb that mounts wrangler's config dir by default: you
/// asked to run wrangler, so it gets the login it would have on the host, and
/// `wrangler login` from in here persists. Nothing else mounts it. Pass
/// `--no-mount-wrangler` for a run that must not touch your account
/// (`wrangler dev` and friends need no credential at all — the local dev
/// server simulates KV, R2, D1, Durable Objects and Queues on the machine).
pub fn run_wrangler(extra: Vec<String>, opts: Opts) -> Result<ExitCode> {
    run_tool("wrangler", extra, opts)
}

/// `arbox gh ...` — run the image's GitHub CLI against the current workspace.
///
/// The one verb that mounts gh's config dir by default, so a `gh auth login`
/// — on the host or from inside the box — carries over and persists, and
/// `git push` over HTTPS works in here when your `~/.gitconfig` names
/// `gh auth git-credential` as the credential helper. Nothing else mounts it:
/// the token is account-wide (every repo the account can push to), which is
/// exactly what `arbox claude` should not be holding by default.
pub fn run_gh(extra: Vec<String>, opts: Opts) -> Result<ExitCode> {
    run_tool("gh", extra, opts)
}

/// Shared skeleton for the tool verbs: like `run_agent`, but the default
/// mount is the tool's config dir from `TOOLS` and nothing is injected ahead
/// of the user's args.
fn run_tool(tool: &'static str, extra: Vec<String>, opts: Opts) -> Result<ExitCode> {
    let host = host::detect()?;
    host::require_git(&host)?;
    let sel = select(&[], &[tool], &opts.mounts);
    ensure_state(&host, &sel, opts.profile.as_deref())?;
    let mut argv = vec![tool.to_string()];
    argv.extend(extra);
    run(host, argv, opts, &sel)
}

pub fn run_bash(extra: Vec<String>, opts: Opts) -> Result<ExitCode> {
    let host = host::detect()?;
    host::require_git(&host)?;
    // Nothing by default. Launching an agent from this shell means asking for
    // its state explicitly: `arbox --mount-claude bash`. Without that, the
    // agent starts unauthenticated and writes throwaway state inside the
    // container, which is the intended shape — the shell is not a blanket
    // grant of every credential you own.
    let sel = select(&[], &[], &opts.mounts);
    ensure_state(&host, &sel, opts.profile.as_deref())?;
    let mut argv = vec!["/bin/bash".to_string(), "-l".to_string()];
    argv.extend(extra);
    run(host, argv, opts, &sel)
}

pub fn run_argv(argv: Vec<String>, opts: Opts) -> Result<ExitCode> {
    if argv.is_empty() {
        bail!("arbox run needs a command");
    }
    let host = host::detect()?;
    host::require_git(&host)?;
    // Same posture as `bash`: nothing mounted unless asked for by name.
    let sel = select(&[], &[], &opts.mounts);
    ensure_state(&host, &sel, opts.profile.as_deref())?;
    run(host, argv, opts, &sel)
}

fn run(host: HostContext, argv: Vec<String>, opts: Opts, sel: &Selection) -> Result<ExitCode> {
    ensure_docker_installed()?;
    host::require_supported_distro(&host)?;
    // Resolved before the image build so a bad `--voice` fails in a second
    // rather than after a multi-minute bootstrap.
    let audio = opts.voice.then(|| require_audio(&host)).transpose()?;
    let serial = opts.serial.as_ref().map(require_serial).transpose()?;
    let wayland = match opts.wayland {
        Some(true) => Some(require_wayland(&host)?),
        Some(false) => None,
        None => Some(detect_wayland(&host)).filter(|w| !w.is_empty()),
    };
    if let Some(serial) = &serial {
        // stderr, so `arbox run -- foo | bar` pipelines stay clean.
        eprintln!("arbox: {}", serial.launch_note());
    }
    let added_safe_dir = fixup_windows_worktree(&host)?;
    let mut mounts = mount_specs(&host, opts.profile.as_deref(), sel);
    append_extra_mounts(&mut mounts, &opts.rw, false)?;
    append_extra_mounts(&mut mounts, &opts.ro, true)?;
    verify_required_mounts_exist(&mounts)?;
    let tag = image::ensure_built(&host)?;

    let mut cmd = Command::new("docker");
    cmd.args(["run", "--rm"]);
    // `-i` keeps stdin attached (needed for both interactive shells and piped
    // input). `-t` only when stdin is a real TTY — otherwise docker errors
    // with "input device is not a TTY" under `arbox run -- foo | bar`, hooks,
    // CI, etc.
    cmd.arg("-i");
    if std::io::stdin().is_terminal() {
        cmd.arg("-t");
    }
    // Distinctive uppercase hostname — `jason@ARBOX:~$` makes it obvious at
    // a glance that you're inside the sandbox shell vs. the host shell.
    // Adding `--add-host ARBOX:127.0.0.1` ensures that sudo inside the container
    // can resolve its own hostname without throwing warnings.
    cmd.args([
        "--hostname",
        "ARBOX",
        "--network",
        "host",
        "--add-host",
        "ARBOX:127.0.0.1",
    ]);
    // /dev/shm defaults to 64 MB in Docker, which is enough to crash Chromium
    // on any non-trivial page. Bump it once here so every Playwright test
    // doesn't have to remember --disable-dev-shm-usage.
    cmd.args(["--shm-size", "1g"]);
    cmd.arg("--user").arg(format!("{}:{}", host.uid, host.gid));

    cmd.arg("--workdir")
        .arg(crate::path::to_container(&host.cwd)?);
    cmd.arg("-e")
        .arg(format!("HOME={}", crate::path::to_container(&host.home)?));

    cmd.arg("-e").arg(format!("USER={}", host.username));
    cmd.arg("-e").arg(format!("TERM={}", host.term));
    cmd.arg("-e").arg("LANG=C.UTF-8");
    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        cmd.arg("-e").arg(format!("ANTHROPIC_API_KEY={key}"));
    }

    for m in &mounts {
        if !m.src.exists() {
            // Optional + missing: skip. Required + missing was caught above.
            continue;
        }

        let src = crate::path::to_mount_src(&m.src);
        let dst = crate::path::to_container(&m.dst)?;
        let mut arg = format!("type=bind,src={src},dst={dst}");
        if m.read_only {
            arg.push_str(",readonly");
        }
        cmd.arg("--mount").arg(arg);
    }

    if let Some(wayland) = &wayland {
        wayland.apply(&mut cmd, host.uid, host.gid);
    }
    if let Some(audio) = &audio {
        audio.apply(&mut cmd);
    }
    if let Some(serial) = &serial {
        serial.apply(&mut cmd);
    }

    cmd.arg(&tag);
    for a in &argv {
        cmd.arg(a);
    }

    let status = cmd
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("running `docker run`")?;

    // Clean up Windows worktree safe.directory entry after container exits
    if let Some(safe_dir_path) = added_safe_dir {
        let mut cleanup_cmd = Command::new("git");
        cleanup_cmd.args([
            "config",
            "--global",
            "--unset",
            "safe.directory",
            &safe_dir_path,
        ]);
        let _ = cleanup_cmd.status(); // Ignore errors; this is best-effort
    }

    Ok(match status.code() {
        Some(c) if (0..=255).contains(&c) => ExitCode::from(c as u8),
        _ => ExitCode::FAILURE,
    })
}

/// The host display plumbing handed to the container: the compositor's
/// socket, which is everything a Wayland client needs to open windows and
/// read the clipboard (claude's image-paste flow runs `wl-paste --type
/// image/png` over it). Bound by DEFAULT whenever the host has a Wayland
/// session — image paste and GUI work are everyday needs — and withheld with
/// `--no-mount-wayland`; `--mount-wayland` spells the default out and makes
/// a missing session a hard error. Wayland-only — the X11 socket is never
/// mounted, because X11 has no client isolation (any X client can read every
/// other window's input and pixels), whereas a Wayland client sees only its
/// own surfaces, which is what makes a default-on display tolerable.
///
/// Mounts JUST the socket file — not `$XDG_RUNTIME_DIR` — so the rest of the
/// host runtime dir (D-Bus session bus, gnome-keyring control socket, …) stays
/// on the host. `WAYLAND_DISPLAY` is set to the absolute socket path so
/// libwayland connects directly without resolving against `XDG_RUNTIME_DIR`.
/// The container gets its own private tmpfs as `XDG_RUNTIME_DIR` instead:
/// GTK, Qt and SDL insist on a writable, user-owned, mode-0700 runtime dir
/// (for their own sockets and shm files), and without one they warn or fail
/// to start. winit-based apps (egui, iced, wgpu, Bevy, Slint) need neither.
pub struct WaylandAccess {
    /// The compositor socket on the host, bind-mounted at the same path.
    socket: Option<PathBuf>,
}

/// Container-side `XDG_RUNTIME_DIR` when the Wayland socket is bound. Not
/// `/run/user/<uid>`: that is where the host socket usually lives, and the
/// socket's bind mount must not be stacked on top of a tmpfs at its own
/// parent.
const CONTAINER_RUNTIME_DIR: &str = "/run/arbox-runtime";

impl WaylandAccess {
    fn is_empty(&self) -> bool {
        self.socket.is_none()
    }

    /// Append the docker flags that carry this access into the container.
    /// `uid`/`gid` own the runtime tmpfs, matching `--user`.
    fn apply(&self, cmd: &mut Command, uid: u32, gid: u32) {
        let Some(sock) = self.socket.as_ref().and_then(|p| p.to_str()) else {
            return;
        };
        cmd.arg("--mount")
            .arg(format!("type=bind,src={sock},dst={sock}"));
        cmd.arg("-e").arg(format!("WAYLAND_DISPLAY={sock}"));
        cmd.arg("--tmpfs").arg(format!(
            "{CONTAINER_RUNTIME_DIR}:rw,nosuid,nodev,mode=0700,uid={uid},gid={gid}"
        ));
        cmd.arg("-e")
            .arg(format!("XDG_RUNTIME_DIR={CONTAINER_RUNTIME_DIR}"));
    }

    /// One-line description for `arbox status`.
    pub fn summary(&self) -> String {
        match &self.socket {
            Some(s) => format!(
                "socket {} (bound by default; --no-mount-wayland to withhold)",
                s.display()
            ),
            None => "no Wayland session detected on host".to_string(),
        }
    }
}

/// Where the host's Wayland socket is, per the protocol's own rules: an
/// absolute `$WAYLAND_DISPLAY` is the socket path itself, a relative one
/// resolves against `$XDG_RUNTIME_DIR`. `None` when either is missing —
/// a headless host or an X11-only desktop.
fn wayland_socket_path(get: &dyn Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    let wd = PathBuf::from(get("WAYLAND_DISPLAY")?);
    if wd.is_absolute() {
        return Some(wd);
    }
    Some(PathBuf::from(get("XDG_RUNTIME_DIR")?).join(wd))
}

/// What display the host currently offers. Pure detection — no error when
/// there's nothing, since `arbox status` reports the empty case too.
///
/// Linux-only by construction: Docker Desktop's Linux VM on Windows and
/// macOS has no path to a host compositor (neither host runs one), so
/// detection reports empty there rather than promising a socket
/// `--mount-wayland` then can't bind.
pub fn detect_wayland(_host: &HostContext) -> WaylandAccess {
    if !cfg!(target_os = "linux") {
        return WaylandAccess { socket: None };
    }
    let socket = wayland_socket_path(&|var| std::env::var_os(var)).filter(|p| p.exists());
    WaylandAccess { socket }
}

/// `detect_wayland`, but for the explicit `--mount-wayland` path where
/// finding nothing is a hard error — the user asked for a display by name,
/// so silently launching without one would just move the failure to the
/// first window. (The default path skips silently instead: a headless host
/// must still be able to run `arbox claude`.)
fn require_wayland(host: &HostContext) -> Result<WaylandAccess> {
    let wayland = detect_wayland(host);
    if wayland.is_empty() {
        if cfg!(target_os = "linux") {
            bail!(
                "--mount-wayland: no Wayland socket found — expected $WAYLAND_DISPLAY to name \
                 one (relative to $XDG_RUNTIME_DIR, or absolute). A headless host, an ssh \
                 session, or an X11-only desktop will all look like this; the X11 socket is \
                 deliberately not supported."
            );
        }
        bail!(
            "--mount-wayland is Linux-only: Docker Desktop's Linux VM has no path to a host \
             compositor on Windows or macOS"
        );
    }
    Ok(wayland)
}

/// The host sound plumbing `--voice` hands to the container. Nothing here is
/// bound without the flag: audio is opt-in because it widens the sandbox from
/// "files you named" to "the machine's microphone and speakers", which is a
/// real capability to give an agent (claude's `/voice`, TTS playback,
/// Playwright media tests) and a real thing to withhold by default.
///
/// Two independent paths, either or both of which may be present:
///   - the PulseAudio/PipeWire native socket, which is how a desktop session
///     normally exposes audio and is the route that needs no device access at
///     all — just a unix socket the server already arbitrates;
///   - the raw ALSA character devices under `/dev/snd`, for hosts with no
///     sound server (headless boxes, minimal sessions) or apps that insist on
///     talking to the hardware directly.
pub struct AudioAccess {
    /// Native sound-server socket on the host, bind-mounted at the same path.
    pulse_socket: Option<PathBuf>,
    /// PulseAudio auth cookie. Servers that don't run `auth-anonymous` reject
    /// a cookie-less client, so carry it in read-only when the host has one.
    pulse_cookie: Option<PathBuf>,
    /// `Some` when the host has `/dev/snd` at all, carrying the supplementary
    /// gids that own those nodes (`audio`, typically 29). `--user uid:gid`
    /// drops every supplementary group the host user has, so without re-adding
    /// these the mode-0660 root:audio device nodes are unopenable inside the
    /// container even though `--device` exposed them. `Some(vec![])` is a real
    /// state — nodes owned outright by root — and still gets the passthrough:
    /// whether the devices exist and who may open them are separate questions,
    /// and only the first one decides whether `--voice` has anything to bind.
    alsa: Option<Vec<u32>>,
}

impl AudioAccess {
    fn is_empty(&self) -> bool {
        self.pulse_socket.is_none() && self.alsa.is_none()
    }

    /// Append the docker flags that carry this access into the container.
    fn apply(&self, cmd: &mut Command) {
        if let Some(sock) = self.pulse_socket.as_ref().and_then(|p| p.to_str()) {
            cmd.arg("--mount")
                .arg(format!("type=bind,src={sock},dst={sock}"));
            // Absolute server address, so nothing inside depends on
            // XDG_RUNTIME_DIR (which arbox deliberately does not forward).
            cmd.arg("-e").arg(format!("PULSE_SERVER=unix:{sock}"));
            // SoX's compiled-in default device is ALSA; without this `rec` and
            // `play` would bypass the socket we just mounted and fail on hosts
            // where /dev/snd isn't also passed through.
            cmd.arg("-e").arg("AUDIODRIVER=pulseaudio");
        }
        if let Some(cookie) = self.pulse_cookie.as_ref().and_then(|p| p.to_str()) {
            cmd.arg("--mount")
                .arg(format!("type=bind,src={cookie},dst={cookie},readonly"));
            cmd.arg("-e").arg(format!("PULSE_COOKIE={cookie}"));
        }
        if let Some(gids) = &self.alsa {
            // Directory form: docker expands it to every device node beneath.
            cmd.args(["--device", "/dev/snd"]);
            for gid in gids {
                cmd.arg("--group-add").arg(gid.to_string());
            }
        }
    }

    /// One-line description for `arbox status`.
    pub fn summary(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(s) = &self.pulse_socket {
            parts.push(format!("sound server socket {}", s.display()));
        }
        if self.alsa.is_some() {
            parts.push("ALSA devices /dev/snd".to_string());
        }
        if parts.is_empty() {
            "none detected on host".to_string()
        } else {
            format!("{} (bound only with --voice)", parts.join(" + "))
        }
    }
}

/// What audio the host currently offers. Pure detection — no error when
/// there's nothing, since `arbox status` reports the empty case too.
///
/// Linux-only by construction: on Windows and macOS, Docker Desktop's Linux
/// VM has no path to the host's sound devices at all, so detection always
/// reports empty there rather than reporting a socket/device `--voice` then
/// can't actually use (`require_audio` and `arbox status` must agree on
/// this, or status can promise audio `--voice` refuses).
pub fn detect_audio(host: &HostContext) -> AudioAccess {
    if !cfg!(target_os = "linux") {
        return AudioAccess {
            pulse_socket: None,
            pulse_cookie: None,
            alsa: None,
        };
    }
    let env = |var: &str| std::env::var_os(var);
    let pulse_socket = pulse_socket_path(host.uid, &env).filter(|p| p.exists());
    let pulse_cookie = pulse_socket
        .is_some()
        .then(|| pulse_cookie_path(&host.home, &env))
        .flatten()
        .filter(|p| p.exists());
    AudioAccess {
        pulse_socket,
        pulse_cookie,
        alsa: alsa_devices(),
    }
}

/// `detect_audio`, but for the `--voice` path where finding nothing is a hard
/// error — the user asked for sound explicitly, so silently launching a mute
/// container would just move the failure to the first `rec` invocation.
fn require_audio(host: &HostContext) -> Result<AudioAccess> {
    let audio = detect_audio(host);
    if audio.is_empty() {
        if cfg!(target_os = "linux") {
            bail!(
                "--voice: no host audio found — expected a PulseAudio/PipeWire socket at \
                 $XDG_RUNTIME_DIR/pulse/native (or a unix: address in $PULSE_SERVER) or \
                 ALSA devices at /dev/snd. A headless host, a session without a running \
                 sound server, a container without /dev/snd passed through, or a \
                 $PULSE_SERVER naming a remote tcp: server will all look like this."
            );
        }
        bail!(
            "--voice is Linux-only: Docker Desktop's Linux VM has no path to the host's \
             sound devices on Windows or macOS"
        );
    }
    Ok(audio)
}

/// Where the host's sound-server socket lives. A set `$PULSE_SERVER` settles
/// it either way — that's the host user's own override — and only when it's
/// unset does this fall back to `pulse/native` under the runtime dir. With
/// neither, there's no socket. `$XDG_RUNTIME_DIR` is preferred but
/// not required — it's routinely unset in ssh sessions and cron-like contexts
/// where `/run/user/<uid>` is nonetheless there and live.
fn pulse_socket_path(
    uid: u32,
    get: &dyn Fn(&str) -> Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    if let Some(server) = get("PULSE_SERVER") {
        // Explicit override: honor it or bind nothing. Falling back to the
        // runtime-dir socket here would quietly point the container at a
        // *different* server than the host user picked. A remote `tcp:` server
        // isn't carried in — arbox doesn't forward `PULSE_SERVER` for its own
        // sake, so there'd be nothing inside the container to act on it, and
        // remote playback isn't what `--voice` is for.
        return parse_pulse_server(server.to_str()?);
    }
    let runtime = match get("XDG_RUNTIME_DIR").map(PathBuf::from) {
        Some(d) if d.is_absolute() => d,
        _ => PathBuf::from(format!("/run/user/{uid}")),
    };
    Some(runtime.join("pulse").join("native"))
}

/// First mountable unix socket in a PulseAudio server string. The value is a
/// whitespace-separated *list* of addresses, each optionally prefixed with a
/// `{machine-id}` block (that's what `pax11publish` and the X11 property put
/// there), and a bare absolute path counts as a unix address just like the
/// explicit `unix:` form. `None` for anything unmountable — a `tcp:` address,
/// a relative path, an empty list.
fn parse_pulse_server(server: &str) -> Option<PathBuf> {
    server.split_whitespace().find_map(|entry| {
        let addr = match entry.split_once('}') {
            Some((prefix, rest)) if prefix.starts_with('{') => rest,
            _ => entry,
        };
        let path = addr.strip_prefix("unix:").unwrap_or(addr);
        Path::new(path).is_absolute().then(|| PathBuf::from(path))
    })
}

/// The PulseAudio auth cookie: `$PULSE_COOKIE`, else the standard
/// `~/.config/pulse/cookie`.
fn pulse_cookie_path(
    home: &Path,
    get: &dyn Fn(&str) -> Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    match get("PULSE_COOKIE").map(PathBuf::from) {
        Some(p) if p.is_absolute() => Some(p),
        Some(_) => None,
        None => Some(home.join(".config").join("pulse").join("cookie")),
    }
}

/// `Some(group owners of the /dev/snd nodes, deduped)`, or `None` when the
/// host has no ALSA devices to pass through. The gids are read off the
/// filesystem rather than assuming `audio` is gid 29 — distros and container
/// bases disagree — and an empty vec just means root owns the nodes outright,
/// which is still a `/dev/snd` worth passing through.
#[cfg(target_family = "unix")]
fn alsa_devices() -> Option<Vec<u32>> {
    use std::os::unix::fs::MetadataExt;

    let nodes: Vec<_> = std::fs::read_dir("/dev/snd")
        .ok()?
        .flatten()
        .filter_map(|e| e.metadata().ok())
        // Skip the `by-id`/`by-path` subdirectories; only the character
        // devices themselves carry the ownership that matters.
        .filter(|m| !m.is_dir())
        .collect();
    if nodes.is_empty() {
        // The directory exists but holds no device nodes — a nested container
        // with an empty /dev/snd. Nothing to bind, so don't claim otherwise.
        return None;
    }
    let mut gids: Vec<u32> = nodes
        .iter()
        .map(|m| m.gid())
        // root already owns everything the container's root-owned mounts need;
        // adding gid 0 as a supplementary group would hand out far more.
        .filter(|g| *g != 0)
        .collect();
    gids.sort_unstable();
    gids.dedup();
    Some(gids)
}

#[cfg(target_family = "windows")]
fn alsa_devices() -> Option<Vec<u32>> {
    None
}

/// The USB serial plumbing `--serial` hands to the container, for flashing
/// and monitoring microcontroller dev boards (ESP32 and friends) from inside
/// the sandbox. Nothing here is bound without the flag: a serial port is a
/// live link to whatever hardware is plugged in, which is a real capability
/// to give an agent and a real thing to withhold by default.
///
/// Docker's `--device` alone isn't enough, for the same reason as `/dev/snd`:
/// `--user uid:gid` drops the host user's supplementary groups, so the
/// mode-0660 `root:dialout` nodes are unopenable inside the container until
/// the owning gid is re-added with `--group-add`.
pub struct SerialAccess {
    /// Resolved character-device nodes on the host, bound at the same path.
    devices: Vec<PathBuf>,
    /// Supplementary gids owning those nodes (`dialout`, typically 20),
    /// deduped, root excluded.
    gids: Vec<u32>,
    /// Character-device majors of the bound nodes (188 for `ttyUSB`, 166 for
    /// `ttyACM`), deduped. Each becomes a wildcard `--device-cgroup-rule`, so
    /// a board that re-enumerates mid-flash (the ESP32-S3/C3 USB-Serial-JTAG
    /// drops off the bus when it enters download mode) stays reachable: if it
    /// comes back under the same name the existing node keeps working, and if
    /// it comes back under a new minor the user can `sudo mknod` it inside the
    /// container without relaunching.
    majors: Vec<u32>,
    /// The host udev database (`/run/udev`), bind-mounted read-only when
    /// present so libudev-based port enumeration (espflash, serialport-rs)
    /// can read USB vendor/product ids and identify boards by name. Without
    /// it those tools still work, but only with an explicit `--port`.
    udev: Option<PathBuf>,
}

impl SerialAccess {
    fn none() -> Self {
        Self {
            devices: Vec::new(),
            gids: Vec::new(),
            majors: Vec::new(),
            udev: None,
        }
    }

    /// Build the access set for already-validated device nodes, reading the
    /// owning gids and device majors off the filesystem rather than assuming
    /// `dialout` is gid 20 or that every board is a `ttyUSB` — distros and
    /// drivers disagree.
    fn for_devices(devices: Vec<PathBuf>) -> Self {
        if devices.is_empty() {
            return Self::none();
        }
        let mut gids = Vec::new();
        let mut majors = Vec::new();
        for dev in &devices {
            if let Some((gid, major)) = device_owner_and_major(dev) {
                // root already owns everything the container's root-owned
                // mounts need; adding gid 0 as a supplementary group would
                // hand out far more.
                if gid != 0 {
                    gids.push(gid);
                }
                majors.push(major);
            }
        }
        gids.sort_unstable();
        gids.dedup();
        majors.sort_unstable();
        majors.dedup();
        let udev = Some(PathBuf::from("/run/udev")).filter(|p| p.is_dir());
        Self {
            devices,
            gids,
            majors,
            udev,
        }
    }

    fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }

    /// Append the docker flags that carry this access into the container.
    fn apply(&self, cmd: &mut Command) {
        for dev in &self.devices {
            cmd.arg("--device").arg(dev);
        }
        for gid in &self.gids {
            cmd.arg("--group-add").arg(gid.to_string());
        }
        for major in &self.majors {
            cmd.arg("--device-cgroup-rule")
                .arg(format!("c {major}:* rmw"));
        }
        if let Some(udev) = self.udev.as_ref().and_then(|p| p.to_str()) {
            cmd.arg("--mount")
                .arg(format!("type=bind,src={udev},dst={udev},readonly"));
        }
    }

    fn device_list(&self) -> String {
        let list: Vec<String> = self
            .devices
            .iter()
            .map(|d| d.display().to_string())
            .collect();
        list.join(" ")
    }

    /// One-line description for `arbox status`.
    pub fn summary(&self) -> String {
        if self.devices.is_empty() {
            return "none detected on host".to_string();
        }
        format!("{} (bound only with --serial)", self.device_list())
    }

    /// What a launch under `--serial` is actually handing over, printed once
    /// at startup: auto-detection can pick up more than the board the user
    /// had in mind, and a re-enumerated node name is the first thing to check
    /// when a flash fails.
    fn launch_note(&self) -> String {
        let udev = match &self.udev {
            Some(p) => format!(", udev db {} read-only", p.display()),
            None => String::new(),
        };
        format!("serial devices bound: {}{udev}", self.device_list())
    }
}

/// Is `name` a USB serial device node? Matches the two Linux drivers dev
/// boards show up under: `ttyUSB<n>` (usb-serial bridges — CP210x, CH340,
/// FTDI) and `ttyACM<n>` (CDC-ACM, which is what a native USB-Serial-JTAG
/// peripheral or an Arduino-style board presents). Not `ttyS<n>` — those are
/// the legacy UARTs every machine has whether or not anything is plugged in,
/// and binding them would make `--serial` succeed on a host with no board.
fn is_usb_serial_name(name: &str) -> bool {
    ["ttyUSB", "ttyACM"].iter().any(|prefix| {
        name.strip_prefix(prefix)
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    })
}

/// What USB serial devices the host currently offers. Pure detection — no
/// error when there's nothing, since `arbox status` reports the empty case.
///
/// Linux-only by construction, like `detect_audio`: Docker Desktop's Linux VM
/// on Windows and macOS has no USB passthrough, so detection reports empty
/// there rather than promising devices `--serial` then can't bind.
pub fn detect_serial() -> SerialAccess {
    if !cfg!(target_os = "linux") {
        return SerialAccess::none();
    }
    let mut devices: Vec<PathBuf> = std::fs::read_dir("/dev")
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_str().is_some_and(is_usb_serial_name))
        .map(|e| e.path())
        .filter(|p| is_char_device(p))
        .collect();
    devices.sort();
    SerialAccess::for_devices(devices)
}

/// `detect_serial` for the `--serial` path, where finding nothing is a hard
/// error — the user asked for a board explicitly, so launching without one
/// would just move the failure to the first `espflash` invocation. With
/// `--serial-dev`, every named path must resolve to a character device.
fn require_serial(request: &SerialRequest) -> Result<SerialAccess> {
    if !cfg!(target_os = "linux") {
        bail!(
            "--serial is Linux-only: Docker Desktop's Linux VM has no USB passthrough on \
             Windows or macOS"
        );
    }
    let serial = match request {
        SerialRequest::Auto => detect_serial(),
        SerialRequest::Devices(paths) => {
            let mut devices = Vec::with_capacity(paths.len());
            for p in paths {
                // Canonicalize so `/dev/serial/by-id/usb-...` symlinks turn
                // into the real node docker's `--device` needs.
                let abs = p
                    .canonicalize()
                    .with_context(|| format!("--serial-dev {}: cannot resolve", p.display()))?;
                if !is_char_device(&abs) {
                    bail!(
                        "--serial-dev {}: not a character device (resolved to {})",
                        p.display(),
                        abs.display()
                    );
                }
                if !devices.contains(&abs) {
                    devices.push(abs);
                }
            }
            SerialAccess::for_devices(devices)
        }
    };
    if serial.is_empty() {
        bail!(
            "--serial: no USB serial devices found — expected /dev/ttyUSB* (CP210x/CH340/FTDI \
             bridges) or /dev/ttyACM* (native USB-Serial-JTAG on ESP32-S3/C3/C6/H2). Plug the \
             board in, or name a device explicitly with --serial-dev PATH."
        );
    }
    Ok(serial)
}

#[cfg(target_family = "unix")]
fn is_char_device(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    std::fs::metadata(path).is_ok_and(|m| m.file_type().is_char_device())
}

#[cfg(target_family = "windows")]
fn is_char_device(_path: &Path) -> bool {
    false
}

/// `(owning gid, device major)` of a character-device node.
#[cfg(target_os = "linux")]
fn device_owner_and_major(path: &Path) -> Option<(u32, u32)> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(path).ok()?;
    Some((m.gid(), libc::major(m.rdev())))
}

#[cfg(not(target_os = "linux"))]
fn device_owner_and_major(_path: &Path) -> Option<(u32, u32)> {
    None
}

/// Resolve and append user-specified `--rw`/`--ro` paths as required mounts.
/// Each path is canonicalized (so symlinks and relative paths resolve to a
/// real absolute location) and mounted at the same path on both sides.
fn append_extra_mounts(
    mounts: &mut Vec<MountSpec>,
    paths: &[PathBuf],
    read_only: bool,
) -> Result<()> {
    let flag = if read_only { "--ro" } else { "--rw" };
    for p in paths {
        let abs = p
            .canonicalize()
            .with_context(|| format!("{flag} {}: cannot resolve", p.display()))?;
        mounts.push(MountSpec::new(abs, read_only, true, None));
    }
    Ok(())
}

fn ensure_docker_installed() -> Result<()> {
    let out = Command::new("docker").arg("version").output();
    match out {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => bail!(
            "`docker version` exited with {}: {}",
            o.status,
            String::from_utf8_lossy(&o.stderr).trim()
        ),
        Err(e) => bail!(
            "`docker` is not on PATH ({e}). Install Docker first: https://docs.docker.com/engine/install/"
        ),
    }
}

fn verify_required_mounts_exist(mounts: &[MountSpec]) -> Result<()> {
    for m in mounts {
        if m.required && !m.src.exists() {
            let hint = m.hint.map(|h| format!(" — {h}")).unwrap_or_default();
            bail!(
                "required mount source {} does not exist{hint}",
                m.src.display()
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_host() -> HostContext {
        HostContext {
            uid: 1000,
            gid: 1000,
            username: "jason".to_string(),
            home: PathBuf::from("/home/jason"),
            cwd: PathBuf::from("/home/jason/code/app"),
            term: "xterm".to_string(),
            distro_id: "ubuntu".to_string(),
            distro_codename: "noble".to_string(),
            workspace_root: None,
            git_common_dir: None,
        }
    }

    /// Find the mount whose container destination is `dst`.
    fn dst<'a>(specs: &'a [MountSpec], dst: &str) -> Option<&'a MountSpec> {
        specs.iter().find(|m| m.dst.as_path() == Path::new(dst))
    }

    /// Selection covering every agent, as `--mount-<each>` would produce.
    fn all_selected() -> Selection {
        Selection {
            agents: agent_names(),
            tools: Vec::new(),
        }
    }

    #[test]
    fn default_mounts_agent_state_at_home() {
        let sel = all_selected();
        let all = sel.agents.clone();
        let specs = mount_specs(&fake_host(), None, &sel);
        // Every agent state path mounts at its canonical home destination,
        // same-path (shared with the host) — except that XDG-aware entries
        // legitimately redirect when the test environment itself carries
        // XDG_CONFIG_HOME/XDG_DATA_HOME overrides, so only pin those down
        // when the environment is clean.
        let xdg_env_set = std::env::var_os("XDG_CONFIG_HOME").is_some()
            || std::env::var_os("XDG_DATA_HOME").is_some();
        for (rel, xdg) in agent_state_paths(&all) {
            let m = dst(&specs, &format!("/home/jason/{rel}"))
                .unwrap_or_else(|| panic!("missing mount for {rel}"));
            if !xdg || !xdg_env_set {
                assert_eq!(m.src, m.dst, "{rel} must be a same-path (shared) mount");
            }
        }
    }

    #[test]
    fn profile_redirects_whole_agent_tree_into_profile_dir() {
        let sel = all_selected();
        let all = sel.agents.clone();
        let specs = mount_specs(&fake_host(), Some("personal"), &sel);

        // Every agent state path — dirs and the .claude.json file alike — is
        // sourced from the profile dir while the destination stays canonical.
        // XDG overrides must NOT apply under a profile.
        for (rel, _) in agent_state_paths(&all) {
            let m = dst(&specs, &format!("/home/jason/{rel}"))
                .unwrap_or_else(|| panic!("missing mount for {rel}"));
            assert_eq!(
                m.src,
                PathBuf::from(format!("/home/jason/.arbox/profiles/personal/{rel}")),
                "{rel} must be sourced from the profile dir"
            );
        }

        // Non-agent mounts stay shared even under a profile. ~/.gitconfig is
        // mounted on every platform; ~/.cargo and ~/.rustup only where the
        // image doesn't bake its own rustup, so guard those so the test
        // doesn't panic elsewhere.
        let mut shared = vec!["/home/jason/.gitconfig"];
        if !host::bakes_rustup() {
            shared.extend(["/home/jason/.cargo", "/home/jason/.rustup"]);
        }
        for d in shared {
            let m = dst(&specs, d).unwrap();
            assert_eq!(m.src, m.dst, "{d} must stay shared across profiles");
        }
    }

    // Path::is_absolute has Windows semantics ("/xdg" isn't absolute there);
    // XDG resolution is a Unix concern, so pin its behavior on Unix only.
    #[cfg(not(target_family = "windows"))]
    #[test]
    fn xdg_override_resolution() {
        use std::ffi::OsString;

        // No env var set → no override.
        assert_eq!(xdg_override(".config/opencode", &|_| None), None);

        // Absolute XDG_DATA_HOME redirects the data dir.
        let get = |var: &str| (var == "XDG_DATA_HOME").then(|| OsString::from("/xdg/data"));
        assert_eq!(
            xdg_override(".local/share/opencode", &get),
            Some(PathBuf::from("/xdg/data/opencode"))
        );
        // A rel outside the XDG prefixes never resolves.
        assert_eq!(xdg_override(".claude", &get), None);

        // Relative base-dir values are invalid per the spec — ignored.
        let rel_get = |_: &str| Some(OsString::from("relative/dir"));
        assert_eq!(xdg_override(".config/opencode", &rel_get), None);
    }

    #[test]
    fn agent_verb_mounts_only_its_own_state() {
        // The point of the whole scheme: `arbox codex` must not carry Claude's
        // credentials or session history, and vice versa.
        let sel = select(&["codex"], &[], &MountOverrides::default());
        assert_eq!(sel.agents, vec!["codex"]);

        let specs = mount_specs(&fake_host(), None, &sel);
        assert!(dst(&specs, "/home/jason/.codex").is_some());
        for foreign in [
            "/home/jason/.claude",
            "/home/jason/.claude.json",
            "/home/jason/.config/opencode",
            "/home/jason/.local/share/opencode",
            "/home/jason/.gemini",
            "/home/jason/.config/antigravity",
            "/home/jason/.grok",
        ] {
            assert!(
                dst(&specs, foreign).is_none(),
                "{foreign} must not be mounted for `arbox codex`"
            );
        }
        // Non-agent mounts are unaffected by the selection.
        assert!(dst(&specs, "/home/jason/.gitconfig").is_some());
    }

    #[test]
    fn non_agent_verbs_mount_no_agent_state() {
        let sel = select(&[], &[], &MountOverrides::default());
        assert!(sel.agents.is_empty());
        let specs = mount_specs(&fake_host(), None, &sel);
        for (rel, _) in agent_state_paths(&agent_names()) {
            assert!(
                dst(&specs, &format!("/home/jason/{rel}")).is_none(),
                "{rel} must not be mounted by bash/run/playwright"
            );
        }
    }

    #[test]
    fn mount_overrides_win_in_both_directions() {
        // --mount-claude on a verb that defaults to nothing.
        let mut ov = MountOverrides::default();
        ov.set("claude", true);
        assert_eq!(select(&[], &[], &ov).agents, vec!["claude"]);

        // --no-mount-claude on `arbox claude` itself.
        let mut ov = MountOverrides::default();
        ov.set("claude", false);
        assert!(select(&["claude"], &[], &ov).agents.is_empty());

        // Additive: the verb's own agent plus one asked for by name.
        let mut ov = MountOverrides::default();
        ov.set("codex", true);
        assert_eq!(
            select(&["claude"], &[], &ov).agents,
            vec!["claude", "codex"]
        );
    }

    #[test]
    fn tool_verb_default_and_overrides() {
        for tool in ["wrangler", "gh"] {
            // The tool's own verb mounts it; every other verb doesn't — not
            // even the other tool's verb.
            assert!(select(&[], &[tool], &MountOverrides::default()).has_tool(tool));
            assert!(!select(&["claude"], &[], &MountOverrides::default()).has_tool(tool));
            let other = if tool == "gh" { "wrangler" } else { "gh" };
            assert!(!select(&[], &[other], &MountOverrides::default()).has_tool(tool));

            // --mount-<tool> turns it on elsewhere; --no-mount-<tool> turns it
            // off on the tool's own verb.
            let mut ov = MountOverrides::default();
            ov.set(tool, true);
            assert!(select(&["claude"], &[], &ov).has_tool(tool));
            let mut ov = MountOverrides::default();
            ov.set(tool, false);
            assert!(!select(&[], &[tool], &ov).has_tool(tool));
        }
    }

    // gh's resolution is env-driven; pin the Unix branch (the Windows one
    // reads %APPDATA%, and Path::is_absolute has Windows semantics).
    #[cfg(not(target_family = "windows"))]
    #[test]
    fn gh_config_follows_host_resolution() {
        use std::ffi::OsString;

        let home = Path::new("/home/jason");

        // Nothing set → ~/.config/gh, same as the container side.
        assert_eq!(
            gh_config_source(home, &|_| None),
            PathBuf::from("/home/jason/.config/gh")
        );

        // GH_CONFIG_DIR wins outright, over XDG_CONFIG_HOME too.
        let both = |var: &str| match var {
            "GH_CONFIG_DIR" => Some(OsString::from("/opt/ghcfg")),
            "XDG_CONFIG_HOME" => Some(OsString::from("/xdg/cfg")),
            _ => None,
        };
        assert_eq!(gh_config_source(home, &both), PathBuf::from("/opt/ghcfg"));

        // XDG_CONFIG_HOME moves it; relative values are ignored per the spec.
        let xdg = |var: &str| (var == "XDG_CONFIG_HOME").then(|| OsString::from("/xdg/cfg"));
        assert_eq!(gh_config_source(home, &xdg), PathBuf::from("/xdg/cfg/gh"));
        let rel = |var: &str| (var == "XDG_CONFIG_HOME").then(|| OsString::from("cfg"));
        assert_eq!(
            gh_config_source(home, &rel),
            PathBuf::from("/home/jason/.config/gh")
        );
    }

    // Wrangler's own resolution is per-platform; pin the Linux branch (and the
    // XDG override, whose is_absolute check has Windows semantics) on Unix.
    #[cfg(not(target_family = "windows"))]
    #[test]
    fn wrangler_config_follows_host_resolution() {
        use std::ffi::OsString;

        let home = Path::new("/home/jason");

        // Nothing set → the platform default beside the other XDG config dirs.
        assert_eq!(
            wrangler_config_source(home, &|_| None),
            PathBuf::from("/home/jason/.config/.wrangler")
        );

        // XDG_CONFIG_HOME moves it; a relative value is ignored per the spec.
        let xdg = |var: &str| (var == "XDG_CONFIG_HOME").then(|| OsString::from("/xdg/cfg"));
        assert_eq!(
            wrangler_config_source(home, &xdg),
            PathBuf::from("/xdg/cfg/.wrangler")
        );
        let rel = |var: &str| (var == "XDG_CONFIG_HOME").then(|| OsString::from("cfg"));
        assert_eq!(
            wrangler_config_source(home, &rel),
            PathBuf::from("/home/jason/.config/.wrangler")
        );
    }

    #[test]
    fn tool_config_is_not_mounted_by_other_verbs() {
        // Only the tool's own verb (or an explicit --mount-<tool>) carries a
        // Cloudflare or GitHub credential, under a profile or otherwise.
        for profile in [None, Some("personal")] {
            let sel = select(&[], &[], &MountOverrides::default());
            let specs = mount_specs(&fake_host(), profile, &sel);
            for path in ["/home/jason/.config/.wrangler", "/home/jason/.config/gh"] {
                assert!(
                    dst(&specs, path).is_none(),
                    "{path} must stay out of the mount list by default"
                );
            }
        }
    }

    #[test]
    fn tool_verb_mounts_at_the_container_linux_path() {
        // The destination is fixed even when the host source is elsewhere
        // (macOS/Windows, an XDG override, or a legacy ~/.wrangler), and it
        // stays shared under a profile.
        for (tool, path) in [
            ("wrangler", "/home/jason/.config/.wrangler"),
            ("gh", "/home/jason/.config/gh"),
        ] {
            for profile in [None, Some("personal")] {
                let sel = select(&[], &[tool], &MountOverrides::default());
                let specs = mount_specs(&fake_host(), profile, &sel);
                let m = dst(&specs, path).unwrap_or_else(|| panic!("missing {tool} config mount"));
                assert!(!m.read_only, "{tool} must be able to write its token");
                assert!(!m.required, "a host without {tool} state must still launch");
                assert!(
                    !m.src.starts_with("/home/jason/.arbox/profiles"),
                    "{tool} config must stay shared across profiles"
                );
            }
        }
    }

    // Wayland socket resolution is env-driven and Linux-only; pin it on Unix
    // (Path::is_absolute has Windows semantics).
    #[cfg(not(target_family = "windows"))]
    #[test]
    fn wayland_socket_resolution() {
        use std::ffi::OsString;

        // No WAYLAND_DISPLAY → no session, whatever XDG_RUNTIME_DIR says.
        let xdg_only =
            |var: &str| (var == "XDG_RUNTIME_DIR").then(|| OsString::from("/run/user/1000"));
        assert_eq!(wayland_socket_path(&xdg_only), None);

        // Relative name resolves against the runtime dir …
        let both = |var: &str| match var {
            "WAYLAND_DISPLAY" => Some(OsString::from("wayland-0")),
            "XDG_RUNTIME_DIR" => Some(OsString::from("/run/user/1000")),
            _ => None,
        };
        assert_eq!(
            wayland_socket_path(&both),
            Some(PathBuf::from("/run/user/1000/wayland-0"))
        );
        // … and without one there is nothing to resolve against.
        let rel_only = |var: &str| (var == "WAYLAND_DISPLAY").then(|| OsString::from("wayland-0"));
        assert_eq!(wayland_socket_path(&rel_only), None);

        // An absolute value is the socket itself; the runtime dir is ignored.
        let abs = |var: &str| {
            (var == "WAYLAND_DISPLAY").then(|| OsString::from("/run/user/1000/wayland-1"))
        };
        assert_eq!(
            wayland_socket_path(&abs),
            Some(PathBuf::from("/run/user/1000/wayland-1"))
        );
    }

    /// The docker flags are the contract: the socket at its own path,
    /// WAYLAND_DISPLAY absolute, and a private user-owned runtime tmpfs.
    #[test]
    fn wayland_apply_emits_socket_env_and_runtime_tmpfs() {
        let access = WaylandAccess {
            socket: Some(PathBuf::from("/run/user/1000/wayland-0")),
        };
        let mut cmd = Command::new("docker");
        access.apply(&mut cmd, 1000, 1000);
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "--mount",
                "type=bind,src=/run/user/1000/wayland-0,dst=/run/user/1000/wayland-0",
                "-e",
                "WAYLAND_DISPLAY=/run/user/1000/wayland-0",
                "--tmpfs",
                "/run/arbox-runtime:rw,nosuid,nodev,mode=0700,uid=1000,gid=1000",
                "-e",
                "XDG_RUNTIME_DIR=/run/arbox-runtime",
            ]
        );

        // Nothing detected: nothing emitted, not even the runtime dir.
        let none = WaylandAccess { socket: None };
        let mut cmd = Command::new("docker");
        none.apply(&mut cmd, 1000, 1000);
        assert_eq!(cmd.get_args().count(), 0);
        assert!(none.is_empty());
    }

    #[test]
    fn wayland_summary_reports_empty_and_populated() {
        let none = WaylandAccess { socket: None };
        assert_eq!(none.summary(), "no Wayland session detected on host");
        let some = WaylandAccess {
            socket: Some(PathBuf::from("/run/user/1000/wayland-0")),
        };
        assert_eq!(
            some.summary(),
            "socket /run/user/1000/wayland-0 (bound by default; --no-mount-wayland to withhold)"
        );
    }

    // Path::is_absolute has Windows semantics, and the whole audio path is
    // Linux-only, so pin this on Unix.
    #[cfg(not(target_family = "windows"))]
    #[test]
    fn pulse_socket_resolution() {
        use std::ffi::OsString;

        // Nothing set → the standard runtime-dir location for the uid.
        assert_eq!(
            pulse_socket_path(1000, &|_| None),
            Some(PathBuf::from("/run/user/1000/pulse/native"))
        );

        // XDG_RUNTIME_DIR moves it; a relative value is ignored per the spec.
        let xdg = |var: &str| (var == "XDG_RUNTIME_DIR").then(|| OsString::from("/run/u"));
        assert_eq!(
            pulse_socket_path(1000, &xdg),
            Some(PathBuf::from("/run/u/pulse/native"))
        );
        let rel = |var: &str| (var == "XDG_RUNTIME_DIR").then(|| OsString::from("run/u"));
        assert_eq!(
            pulse_socket_path(7, &rel),
            Some(PathBuf::from("/run/user/7/pulse/native"))
        );

        // PULSE_SERVER wins, but only in its mountable unix-socket form.
        let unix = |var: &str| (var == "PULSE_SERVER").then(|| OsString::from("unix:/tmp/pa.sock"));
        assert_eq!(
            pulse_socket_path(1000, &unix),
            Some(PathBuf::from("/tmp/pa.sock"))
        );
        let tcp = |var: &str| (var == "PULSE_SERVER").then(|| OsString::from("tcp:10.0.0.2:4713"));
        assert_eq!(pulse_socket_path(1000, &tcp), None);
    }

    // PULSE_SERVER is an address *list*, and the forms below all show up in
    // the wild — a bare path, a `{machine-id}` prefix, several entries where
    // only one is mountable.
    #[cfg(not(target_family = "windows"))]
    #[test]
    fn pulse_server_address_list_parsing() {
        assert_eq!(
            parse_pulse_server("unix:/tmp/pa.sock"),
            Some(PathBuf::from("/tmp/pa.sock"))
        );
        assert_eq!(
            parse_pulse_server("/run/user/1000/pulse/native"),
            Some(PathBuf::from("/run/user/1000/pulse/native"))
        );
        assert_eq!(
            parse_pulse_server("{f00dcafe}unix:/run/user/1000/pulse/native"),
            Some(PathBuf::from("/run/user/1000/pulse/native"))
        );
        // First mountable entry wins; unmountable ones are skipped, not fatal.
        assert_eq!(
            parse_pulse_server("tcp:10.0.0.2:4713 unix:/tmp/pa.sock"),
            Some(PathBuf::from("/tmp/pa.sock"))
        );
        assert_eq!(parse_pulse_server("tcp:10.0.0.2:4713"), None);
        assert_eq!(parse_pulse_server("unix:rel/pa.sock"), None);
        assert_eq!(parse_pulse_server(""), None);
    }

    #[cfg(not(target_family = "windows"))]
    #[test]
    fn pulse_cookie_resolution() {
        use std::ffi::OsString;

        let home = Path::new("/home/jason");
        assert_eq!(
            pulse_cookie_path(home, &|_| None),
            Some(PathBuf::from("/home/jason/.config/pulse/cookie"))
        );
        let set = |var: &str| (var == "PULSE_COOKIE").then(|| OsString::from("/etc/pa-cookie"));
        assert_eq!(
            pulse_cookie_path(home, &set),
            Some(PathBuf::from("/etc/pa-cookie"))
        );
        // A relative override is not usable as a bind source — no cookie.
        let rel = |var: &str| (var == "PULSE_COOKIE").then(|| OsString::from("pa-cookie"));
        assert_eq!(pulse_cookie_path(home, &rel), None);
    }

    #[test]
    fn audio_summary_reports_empty_and_populated() {
        let none = AudioAccess {
            pulse_socket: None,
            pulse_cookie: None,
            alsa: None,
        };
        assert!(none.is_empty());
        assert_eq!(none.summary(), "none detected on host");

        let both = AudioAccess {
            pulse_socket: Some(PathBuf::from("/run/user/1000/pulse/native")),
            pulse_cookie: None,
            alsa: Some(vec![29]),
        };
        assert!(!both.is_empty());
        assert!(both.summary().contains("/run/user/1000/pulse/native"));
        assert!(both.summary().contains("/dev/snd"));

        // /dev/snd owned outright by root: no group to add, but the devices
        // are there and `--voice` must still count and bind them.
        let root_owned = AudioAccess {
            pulse_socket: None,
            pulse_cookie: None,
            alsa: Some(Vec::new()),
        };
        assert!(!root_owned.is_empty());
        assert!(root_owned.summary().contains("/dev/snd"));
    }

    #[test]
    fn usb_serial_name_matching() {
        for yes in ["ttyUSB0", "ttyUSB12", "ttyACM0", "ttyACM3"] {
            assert!(is_usb_serial_name(yes), "{yes} should match");
        }
        // Legacy UARTs, bare prefixes, and lookalikes must not.
        for no in [
            "ttyS0", "ttyUSB", "ttyACM", "ttyUSB0a", "ttyAMA0", "tty0", "usbmon0",
        ] {
            assert!(!is_usb_serial_name(no), "{no} should not match");
        }
    }

    #[test]
    fn serial_summary_reports_empty_and_populated() {
        let none = SerialAccess::none();
        assert!(none.is_empty());
        assert_eq!(none.summary(), "none detected on host");

        let two = SerialAccess {
            devices: vec![PathBuf::from("/dev/ttyUSB0"), PathBuf::from("/dev/ttyACM0")],
            gids: vec![20],
            majors: vec![166, 188],
            udev: None,
        };
        assert!(!two.is_empty());
        assert_eq!(
            two.summary(),
            "/dev/ttyUSB0 /dev/ttyACM0 (bound only with --serial)"
        );
        assert_eq!(
            two.launch_note(),
            "serial devices bound: /dev/ttyUSB0 /dev/ttyACM0"
        );

        let with_udev = SerialAccess {
            udev: Some(PathBuf::from("/run/udev")),
            ..two
        };
        assert_eq!(
            with_udev.launch_note(),
            "serial devices bound: /dev/ttyUSB0 /dev/ttyACM0, udev db /run/udev read-only"
        );
    }

    /// The docker flags are the contract: every node as `--device`, every
    /// owning group re-added, a wildcard cgroup rule per major, and the udev
    /// db read-only when present.
    #[test]
    fn serial_apply_emits_device_group_and_cgroup_flags() {
        let access = SerialAccess {
            devices: vec![PathBuf::from("/dev/ttyUSB0"), PathBuf::from("/dev/ttyACM0")],
            gids: vec![20],
            majors: vec![166, 188],
            udev: Some(PathBuf::from("/run/udev")),
        };
        let mut cmd = Command::new("docker");
        access.apply(&mut cmd);
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "--device",
                "/dev/ttyUSB0",
                "--device",
                "/dev/ttyACM0",
                "--group-add",
                "20",
                "--device-cgroup-rule",
                "c 166:* rmw",
                "--device-cgroup-rule",
                "c 188:* rmw",
                "--mount",
                "type=bind,src=/run/udev,dst=/run/udev,readonly",
            ]
        );

        // Root-owned nodes and no udev db: devices still bind, nothing else.
        let bare = SerialAccess {
            devices: vec![PathBuf::from("/dev/ttyACM0")],
            gids: vec![],
            majors: vec![166],
            udev: None,
        };
        let mut cmd = Command::new("docker");
        bare.apply(&mut cmd);
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "--device",
                "/dev/ttyACM0",
                "--device-cgroup-rule",
                "c 166:* rmw"
            ]
        );
    }

    /// An explicit `--serial-dev` naming something that isn't a device node
    /// (or doesn't exist) must fail before any image build, not surface as a
    /// docker error.
    #[cfg(target_os = "linux")]
    #[test]
    fn serial_dev_rejects_non_devices() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("not-a-tty");
        std::fs::write(&plain, b"").unwrap();
        let err = require_serial(&SerialRequest::Devices(vec![plain.clone()]))
            .err()
            .expect("a regular file is not a serial device");
        assert!(
            err.to_string().contains("not a character device"),
            "{err:#}"
        );

        let missing = dir.path().join("ttyUSB9");
        let err = require_serial(&SerialRequest::Devices(vec![missing]))
            .err()
            .expect("a missing path cannot be bound");
        assert!(err.to_string().contains("cannot resolve"), "{err:#}");
    }

    /// `/dev/null` is a character device on every Linux box, so it stands in
    /// for a board here: the node binds, its major is read off the inode, and
    /// duplicates (a symlink and its target) collapse to one `--device`.
    #[cfg(target_os = "linux")]
    #[test]
    fn serial_dev_resolves_symlinks_and_dedups() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("by-id-style-link");
        std::os::unix::fs::symlink("/dev/null", &link).unwrap();
        let access = require_serial(&SerialRequest::Devices(vec![
            link,
            PathBuf::from("/dev/null"),
        ]))
        .unwrap();
        assert_eq!(access.devices, vec![PathBuf::from("/dev/null")]);
        let rdev = std::fs::metadata("/dev/null").unwrap().rdev();
        assert_eq!(access.majors, vec![libc::major(rdev)]);
        // /dev/null is root:root — no supplementary group to hand out.
        assert!(access.gids.is_empty());
    }

    #[test]
    fn profile_name_validation() {
        for good in ["personal", "work", "acct-2", "a_b", "Team1"] {
            assert!(
                validate_profile_name(good).is_ok(),
                "{good} should be valid"
            );
        }
        for bad in [
            "",
            "bad/name",
            "../escape",
            ".hidden",
            "-leading",
            "has space",
        ] {
            assert!(
                validate_profile_name(bad).is_err(),
                "{bad} should be invalid"
            );
        }
    }
}
