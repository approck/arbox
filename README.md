# arbox

[![License: Apache 2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)

# Designed for Ubuntu Linux. Requires Docker.

## LLM Produced Code Notice

This was produced by LLM coding assistance under the direction of humans.
It may be rife with errors, omissions, and bad design decisions.
Use with caution and review the code before trusting it.

## Why?

Rust development is unique in that it has excellent cross-platform tooling
and does not need containerization for sane development. It would be a shame
to move to a full containerized development workflow just for sandbox
support.

The goal of this project is to make it easy to build a host-shaped Docker
container with the same uid, gid, Ubuntu codename, Rust toolchain, and
claude/codex setup (via mounts), and then make it equally easy to launch
claude or codex inside that container with the current git repo mounted at
the same absolute path.

## About (LLM Generated)

A Docker-based agent sandbox for running Claude Code, Codex CLI, and
arbitrary build commands with a narrower view of the host than a normal
shell.

`arbox` builds a per-host Ubuntu image from an embedded Dockerfile, mirrors
your host uid/gid, bind-mounts the current git workspace at the same path,
and runs the requested command as you. The point is fast re-entry into a
host-shaped environment where edits still appear in your normal editor, but
the agent sees only the explicit mounts.

This is intentionally closer to a skinny chroot than to a hardened VM. It is
useful against accidents, prompt injection, and many dependency-script
mistakes; it is not a defense against Docker/container escape vulnerabilities
or a process that you intentionally gave access to your mounted credentials.

## Security model

- **The current git workspace is mounted read-write** at the same absolute
  path inside the container. Edits made by the agent are real host edits.
- **Git worktree common dirs are mounted read-write** when they live outside
  the workspace, so normal git operations keep working in worktrees.
- **Host `~/.rustup` is mounted read-only**, so rustup toolchain payloads
  are not writable from inside the container.
- **Host `~/.cargo` is mounted read-write.** This keeps cargo registry cache,
  config, and installed command shims shared with the host, but it also means
  a compromised process can modify files under `~/.cargo`, including
  `~/.cargo/bin`. Treat this as a convenience tradeoff, not a hard boundary.
- **Host `~/.gitconfig` is mounted read-only** when present, so git inside
  the container picks up your identity, aliases, and signing config.
- **`~/.local/bin` and `~/.local/share/claude` are deliberately NOT mounted.**
  Those hold the host's own agent binaries and claude version store; arbox
  runs the claude/codex/agy/grok versions baked into the image (bumped with
  `arbox update`), so mounting the host copies would only let them shadow the
  baked binaries on `PATH`. Only agent *state* is shared — see the next bullet.
- **Agent data dirs are mounted read-write** so config, credentials, history,
  memories, and sessions persist across container rebuilds: `~/.claude` +
  `~/.claude.json` (claude), `~/.codex`, `~/.gemini` + `~/.config/antigravity`
  (agy), and `~/.grok`. A compromised agent could modify these.
- **The host Wayland display socket is mounted when available** so
  `wl-paste` works for clipboard image flows. Only the socket file is
  mounted, not the full `$XDG_RUNTIME_DIR`.
- **Host UID/GID are mirrored** so files written from the container are owned
  by you on the host.
- **The container uses host networking** (`--network host`) because coding
  agents and package managers often need normal network behavior. Do not
  treat the network as isolated.
- **`/dev/shm` is bumped to 1 GB.** Docker's 64 MB default crashes Chromium
  on non-trivial pages; the bump removes the need for
  `--disable-dev-shm-usage` on every Playwright launch.
- **Inside the container, agents run with `--dangerously-skip-permissions` /
  `--dangerously-bypass-approvals-and-sandbox`.** This is intentional: the
  Docker boundary and explicit mount list are the sandbox.

The host kernel still trusts Docker and the container runtime. This project
defends against common development-agent accidents and many malicious
project-level scripts, not against a determined attacker with a container
escape or host shell access.

## Requirements

- **Ubuntu Linux** host. The image is built from your host's Ubuntu codename
  so libc and toolchain behavior line up with the host.
- **Docker Engine** on `PATH`.
- **[rustup](https://rustup.rs)** installed on the host. `~/.cargo` and
  `~/.rustup` must exist before launching arbox.
- **Git** on the host. The workspace is resolved via `git rev-parse
  --show-toplevel`.
- **For the AI agents (claude, codex, agy, grok): nothing on the host.**
  All four CLIs are baked into the image. The first time you run a given
  verb, arbox creates the agent's state paths on the host (`~/.claude` +
  `~/.claude.json` for claude, `~/.codex` for codex, `~/.gemini` +
  `~/.config/antigravity` for agy, `~/.grok` for grok) and bind-mounts them
  in so credentials and history persist across subsequent runs.

## Install

From source:

```bash
git clone https://github.com/approck/arbox
cd arbox
cargo install --path .
```

This drops `arbox` into `~/.cargo/bin`. Make sure that's on your `PATH`.

## Quick start

```bash
cd ~/code/some-rust-project
arbox status                       # inspect detected host facts and mounts
arbox update                       # refresh agents to latest; auto-builds if missing
arbox bash                         # interactive bash, project auto-mounted
arbox run -- cargo test            # one-off command
arbox claude                       # Claude Code, project auto-mounted
arbox codex                        # Codex CLI, project auto-mounted
arbox agy                          # Google Antigravity CLI
arbox grok                         # xAI Grok Build CLI
```

The first build can take a few minutes because the image installs common
development packages plus uv, deno, Node 22, Playwright with chromium +
firefox baked in (~700 MB just for the browsers), and all four coding
agents (claude, codex, agy, grok). Subsequent launches reuse the per-host
image tag, which is `arbox:<ubuntu-codename>-uid<uid>-<dockerfile-hash>`.
The Dockerfile-content hash is the trailing 8 hex chars; editing the
embedded Dockerfile changes the hash, which makes the next launch verb
notice the missing tag and rebuild automatically. `arbox clean` wipes every
image with your host's prefix, including stale ones from earlier Dockerfile
revisions.

The Dockerfile is multi-arch via BuildKit's `TARGETARCH`. amd64 (x86_64) and
arm64 (aarch64) hosts both work; other architectures fail the build with a
clear message.

## Commands

| Command                         | Description |
|---------------------------------|-------------|
| `arbox claude [FLAGS] -- ARGS...` | Run Claude Code with `--dangerously-skip-permissions`. Binary baked into image; `~/.claude` + `~/.claude.json` mount from the host if present. |
| `arbox codex  [FLAGS] -- ARGS...` | Run Codex CLI with `--dangerously-bypass-approvals-and-sandbox`. Binary baked into image; `~/.codex` mounts from the host if present. |
| `arbox agy    [FLAGS] -- ARGS...` | Run Google Antigravity's `agy` CLI. Binary baked into image; `~/.gemini` and `~/.config/antigravity` mount from the host. First-time auth uses agy's SSH-style URL+code flow since libsecret isn't reachable inside the container. |
| `arbox grok   [FLAGS] -- ARGS...` | Run xAI's Grok Build CLI. Binary baked into image; `~/.grok` mounts from the host (token lives in `~/.grok/auth.json`). |
| `arbox bash   [FLAGS]`          | Open an interactive login bash inside the container. |
| `arbox playwright [FLAGS] -- ARGS...` | Run the Playwright CLI (`test`, `codegen`, `show-report`, …). Image ships Node + Playwright + chromium + firefox. |
| `arbox run    [FLAGS] -- CMD...`  | Run a one-off command inside the container. |
| `arbox update`                  | Refresh the baked-in agents (claude, codex, agy, grok) to their latest published versions, rebuilding only the agent layers (quick — the apt/node/playwright layers stay cached). Builds the image from scratch if it doesn't exist yet. |
| `arbox update --force`          | Full clean rebuild of the entire image (`--no-cache`): re-runs apt, node, the Playwright browser downloads, everything. |
| `arbox status`                  | Show host facts, mount layout, image presence, and network mode. Works outside a git repository (skips the workspace mount in that case). |
| `arbox clean`                   | Remove every arbox image whose tag has the current host's prefix. |

`claude`, `codex`, `agy`, `grok`, `bash`, and `run` must be invoked from
inside a git repository — they mount the git toplevel as the workspace and
`cd` into your current directory. `status`, `update`, and `clean` do not
require a repo.

### Extra bind-mount flags

`claude`, `codex`, `agy`, `grok`, `bash`, and `run` accept zero or more `--rw <PATH>` and
`--ro <PATH>` options. Each path is canonicalized (relative paths and
symlinks resolve against the host filesystem) and mounted at the same
absolute path inside the container.

```bash
arbox bash --rw ~/scratch
arbox run --rw /tmp/build-out --ro /opt/data -- cargo build
arbox claude --rw ~/code/sibling-repo --ro ~/datasets/fixtures
```

Required to exist on the host; launches fail loudly if a path is missing.

### Auth profiles (`--profile`)

By default each agent uses its standard host location, so arbox shares your
normal login, history, and memories with the host. Pass `--profile <NAME>`
(global, on any launch verb) to run a *second* subscription concurrently under
a fully self-contained state tree — handy when you want one box on your work
plan and another on a personal plan at the same time.

A profile sources each agent's **entire state tree** from
`~/.arbox/profiles/<NAME>/` while the container still sees the canonical path,
so auth and history always travel together and can never drift apart:

| Host source (profile `personal`)                      | Mounted in container as   |
|-------------------------------------------------------|---------------------------|
| `~/.arbox/profiles/personal/.claude`                  | `~/.claude`               |
| `~/.arbox/profiles/personal/.claude.json`             | `~/.claude.json`          |
| `~/.arbox/profiles/personal/.codex`                   | `~/.codex`                |
| `~/.arbox/profiles/personal/.gemini`                  | `~/.gemini`               |
| `~/.arbox/profiles/personal/.config/antigravity`      | `~/.config/antigravity`   |
| `~/.arbox/profiles/personal/.grok`                    | `~/.grok`                 |

Everything inside that tree — credentials, sessions (so `--resume` works),
memories, settings, MCP config — is isolated to the profile and consistent
with its own auth. The trade-off is intentional: a profile does **not** share
memories or sessions with the default or with other profiles. Non-agent mounts
(the workspace, the Rust toolchain, and read-only `~/.gitconfig`) stay shared
regardless of profile, since git identity isn't tied to a subscription.

The profile tree is created on first launch of each agent verb (its dirs are
made and `~/.arbox/profiles/<NAME>/.claude.json` is seeded with `{}` so the
bind mounts attach); then `/login` inside that box populates it.

```bash
arbox claude --profile personal          # first run: log in inside the box
arbox --profile personal claude --resume # resumes that profile's own sessions
arbox --profile personal status          # show the redirected mounts
```

`agy` is keyring / URL-code based, but because its whole `~/.gemini` and
`~/.config/antigravity` move into the profile tree, whatever it persists there
is isolated too. The default profile is unaffected — it keeps using your
standard host locations.

## How it works

1. `host::detect()` reads UID/GID, passwd username/home, current directory,
   `$TERM`, and `/etc/os-release`. It also tries to resolve the git toplevel
   and common dir, but tolerates failure so non-launch verbs work outside a
   repo.
2. `host::require_supported_distro()` rejects non-Ubuntu hosts for now;
   launch verbs additionally call `host::require_git()` to demand a workspace.
3. `image::ensure_built()` derives the image tag from the host codename, uid,
   and an 8-char hash of the embedded Dockerfile bytes
   (`arbox:<codename>-uid<uid>-<hash>`). When the Dockerfile changes, the tag
   changes, so missing-image detection automatically triggers a rebuild on
   the next launch.
4. The Dockerfile starts from `ubuntu:<host-codename>`, installs common
   development tools plus pinned uv and deno binaries (architecture chosen
   from BuildKit's `TARGETARCH`), bakes in the coding agents, mirrors the
   host user/group, and orders `PATH` so `/usr/local/bin` (the baked agents)
   wins over the host-mounted `~/.local/bin` — while `~/.cargo/bin` stays
   first for the rustup shims.
5. `launch::mount_specs()` builds the explicit bind-mount list for the
   workspace, git worktree metadata, Rust toolchain, and agent state dirs
   (config, credentials, history, sessions). Agent *binaries* are never
   mounted — they come from the image. With `--profile NAME` each agent's state
   tree is instead sourced from `~/.arbox/profiles/NAME/` (see
   [Auth profiles](#auth-profiles---profile)). User-supplied `--rw`/`--ro`
   paths are appended after canonicalization.
6. `docker run --rm -i --network host --user UID:GID --workdir <cwd>` runs
   the selected command with host-shaped paths and inherited stdio. `-t` is
   added only when stdin is an interactive terminal.

## Customization

Most behavior is controlled by what's on your host:

- The image follows your host's Ubuntu codename from `/etc/os-release`.
- The container user and home directory come from `getpwuid_r`, not from
  `$USER` or `$HOME`.
- The current directory selects the git workspace to mount.
- Editing `src/Dockerfile` invalidates the cached image tag automatically;
  the next launch verb rebuilds with no extra flags.
- The agents (claude, codex, agy, grok) are pinned to `@latest` at build time,
  but Docker's layer cache keys on the literal `RUN` command — not on what
  `@latest` resolves to — so an unchanged Dockerfile keeps serving whatever
  agent versions were current when the layer was first built. `arbox update`
  busts just the agent layers (via the `AGENT_REFRESH` build-arg) to pull the
  newest versions; `arbox update --force` rebuilds the whole image from
  scratch.
- Add ad-hoc directories with `--rw`/`--ro` per-invocation; permanent
  additions belong in `launch::mount_specs`.

## Development

```bash
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

The unit tests do not require Docker. End-to-end behavior requires an Ubuntu
host with Docker and the expected host config directories.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Bug reports and PRs welcome.

For security issues, see [SECURITY.md](SECURITY.md) and please don't open a
public issue.

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) and
[NOTICE](NOTICE).

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in this work shall be licensed as above, without any
additional terms or conditions.
