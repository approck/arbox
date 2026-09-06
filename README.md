# arbox

[![License: Apache 2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)

# Linux, macOS, and Windows. Requires Docker.

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
coding-agent setup (via mounts), and then make it equally easy to launch an
agent (claude, codex, opencode, agy, grok) inside that container with the
current git repo mounted at the same absolute path.

## About (LLM Generated)

A Docker-based agent sandbox for running Claude Code, Codex CLI, OpenCode,
Antigravity, Grok Build, and arbitrary build commands with a narrower view
of the host than a normal shell.

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
  runs the claude/codex/opencode/agy/grok versions baked into the image
  (bumped with `arbox update`), so mounting the host copies would only let
  them shadow the baked binaries on `PATH`. Only agent *state* is shared —
  see the next bullet.
- **Agent data dirs are mounted read-write** so config, credentials, history,
  memories, and sessions persist across container rebuilds: `~/.claude` +
  `~/.claude.json` (claude), `~/.codex`, `~/.config/opencode` +
  `~/.local/share/opencode` (opencode), `~/.gemini` + `~/.config/antigravity`
  (agy), and `~/.grok`. A compromised agent could modify these. Note that
  opencode's data dir is not purely inert state: opencode downloads helper
  executables (LSP servers, ripgrep, fzf) into `~/.local/share/opencode/bin`,
  so this mount carries executables in both directions — a compromised
  container agent could tamper with binaries the host's own opencode later
  runs.
- **The host Wayland display socket is mounted when available** so
  `wl-paste` works for clipboard image flows. Only the socket file is
  mounted, not the full `$XDG_RUNTIME_DIR`.
- **Audio is NOT bound unless you pass `--voice`.** With the flag, the
  container gets the host's PulseAudio/PipeWire socket and/or the ALSA
  devices under `/dev/snd` — that is a live microphone and speakers for
  whatever runs inside, so it stays opt-in and per-invocation. Passing
  `/dev/snd` also adds the device nodes' owning group (usually `audio`) as a
  supplementary group inside the container, which grants that access even if
  your host user isn't in that group. See
  [Audio](#audio---voice).
- **USB serial devices are NOT bound unless you pass `--serial`.** With the
  flag, every `/dev/ttyUSB*` and `/dev/ttyACM*` node on the host (or just the
  ones named with `--serial-dev`) is passed through with `--device`, the
  owning group (usually `dialout`) is added inside the container, and the
  host's udev database under `/run/udev` is mounted read-only so port
  enumeration can identify boards. That is a live link to whatever hardware
  is plugged in — flashing a bricked firmware is a real outcome — so it stays
  opt-in and per-invocation. See [USB serial](#usb-serial---serial).
- **Host UID/GID are mirrored** so files written from the container are owned
  by you on the host.
- **The container uses host networking** (`--network host`) because coding
  agents and package managers often need normal network behavior. Do not
  treat the network as isolated. This is passed unconditionally on every
  platform, but only does what it says on Linux: on Windows and macOS,
  Docker Desktop's host-networking mode is off by default, so `--network
  host` is silently a no-op there and `localhost` inside the container
  resolves to the Docker Desktop VM, not your machine — see the `opencode`
  row above for the concrete symptom.
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

- **Ubuntu Linux, macOS, or Windows** host with Docker. On Linux, the image is
  built from your host's Ubuntu codename so libc and toolchain behavior line
  up with the host. On macOS and Windows, the image runs `ubuntu:noble`
  inside Docker Desktop.
- **Docker Engine** on `PATH`. On macOS and Windows, Docker Desktop is
  required.
- **[rustup](https://rustup.rs)** installed on the host (Linux only). `~/.cargo`
  and `~/.rustup` must exist before launching arbox. On macOS and Windows,
  rustup is installed inside the container automatically instead — a macOS
  host's own rustup toolchain can't run inside the Linux container, so it
  isn't mounted. This also means the cargo registry/build cache is baked
  into the image rather than shared with the host: on Linux it persists in
  `~/.cargo` across every `arbox` invocation, but on macOS and Windows each
  `docker run --rm` container starts from the same baked image state, so
  `cargo build`'s downloads are re-fetched every session.
- **Git** on the host. The workspace is resolved via `git rev-parse
  --show-toplevel`.
- **For the AI agents (claude, codex, opencode, agy, grok): nothing on the
  host.** All five CLIs are baked into the image. The first time you run a
  given verb, arbox creates the agent's state paths on the host (`~/.claude`
  + `~/.claude.json` for claude, `~/.codex` for codex, `~/.config/opencode` +
  `~/.local/share/opencode` for opencode, `~/.gemini` +
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
arbox opencode                     # OpenCode TUI (local Ollama works on Linux)
arbox agy                          # Google Antigravity CLI
arbox grok                         # xAI Grok Build CLI
```

The first build can take a few minutes because the image installs common
development packages plus uv, deno, Node 22, Playwright with chromium +
firefox baked in (~700 MB just for the browsers), and all five coding
agents (claude, codex, opencode, agy, grok). Subsequent launches reuse the per-host
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
| `arbox claude [FLAGS] -- ARGS...` | Run Claude Code with `--dangerously-skip-permissions`. Binary baked into image; `~/.claude` + `~/.claude.json` mount from the host if present. With `--voice`, starts with voice mode already enabled. |
| `arbox codex  [FLAGS] -- ARGS...` | Run Codex CLI with `--dangerously-bypass-approvals-and-sandbox`. Binary baked into image; `~/.codex` mounts from the host if present. |
| `arbox opencode [FLAGS] -- ARGS...` | Run the OpenCode TUI. Binary baked into image; `~/.config/opencode` (config) and `~/.local/share/opencode` (auth in `auth.json`, sessions) mount from the host. Host-local providers like Ollama on `localhost:11434` work via host networking on Linux; on Windows and macOS, Docker Desktop reaches them only with its opt-in host-networking feature enabled. |
| `arbox agy    [FLAGS] -- ARGS...` | Run Google Antigravity's `agy` CLI. Binary baked into image; `~/.gemini` and `~/.config/antigravity` mount from the host. First-time auth uses agy's SSH-style URL+code flow since libsecret isn't reachable inside the container. |
| `arbox grok   [FLAGS] -- ARGS...` | Run xAI's Grok Build CLI. Binary baked into image; `~/.grok` mounts from the host (token lives in `~/.grok/auth.json`). |
| `arbox bash   [FLAGS]`          | Open an interactive login bash inside the container. |
| `arbox playwright [FLAGS] -- ARGS...` | Run the Playwright CLI (`test`, `codegen`, `show-report`, …). Image ships Node + Playwright + chromium + firefox. |
| `arbox run    [FLAGS] -- CMD...`  | Run a one-off command inside the container. |
| `arbox update`                  | Refresh the baked-in agents (claude, codex, opencode, agy, grok) to their latest published versions, rebuilding only the agent layers (quick — the apt/node/playwright layers stay cached). Builds the image from scratch if it doesn't exist yet. |
| `arbox update --force`          | Full clean rebuild of the entire image (`--no-cache`): re-runs apt, node, the Playwright browser downloads, everything. |
| `arbox status`                  | Show host facts, mount layout, image presence, network mode, and detected host audio and USB serial devices. Works outside a git repository (skips the workspace mount in that case). |
| `arbox clean`                   | Remove every arbox image whose tag has the current host's prefix. |

`claude`, `codex`, `opencode`, `agy`, `grok`, `playwright`, `bash`, and `run`
must be invoked from inside a git repository — they mount the git toplevel as
the workspace and `cd` into your current directory. `status`, `update`, and
`clean` do not require a repo.

### Extra bind-mount flags

`claude`, `codex`, `opencode`, `agy`, `grok`, `playwright`, `bash`, and `run`
accept zero or more `--rw <PATH>` and
`--ro <PATH>` options. Each path is canonicalized (relative paths and
symlinks resolve against the host filesystem) and mounted at the same
absolute path inside the container.

```bash
arbox bash --rw ~/scratch
arbox run --rw /tmp/build-out --ro /opt/data -- cargo build
arbox claude --rw ~/code/sibling-repo --ro ~/datasets/fixtures
```

Required to exist on the host; launches fail loudly if a path is missing.

### Audio (`--voice`)

No sound reaches the container by default. Pass `--voice` (global, on any
launch verb) to bind the host's audio hardware in — needed for microphone
input, for TTS playback, and for media tests:

```bash
arbox claude --voice          # voice mode already on; hold space to talk
arbox bash --voice            # then: rec /tmp/t.wav trim 0 3 && play /tmp/t.wav
arbox --voice run -- pactl info
```

On `arbox claude` the flag also switches Claude Code's voice mode on for that
session, so push-to-talk works immediately without running `/voice` first.
Claude Code has no voice CLI flag — voice is a setting (`voice.enabled`) that
`/voice` writes into `~/.claude/settings.json`, and that file is mounted from
the host, so toggling it inside the box would flip voice on for your host's
claude too. arbox instead passes `--settings '{"voice":{"enabled":true}}'`,
which layers onto the effective settings for that run only and leaves your
`voice.mode` (hold vs tap) preference alone. The other agents get the audio
devices but no equivalent switch.

arbox binds whichever of these the host actually has:

| Host thing                                          | How it's passed |
|-----------------------------------------------------|-----------------|
| PulseAudio / PipeWire socket (`$PULSE_SERVER`, else `$XDG_RUNTIME_DIR/pulse/native`, else `/run/user/<uid>/pulse/native`) | Bind-mounted at the same path, with `PULSE_SERVER` pointed at it. The auth cookie (`$PULSE_COOKIE`, else `~/.config/pulse/cookie`) comes along read-only when present. Only the unix-socket form of `$PULSE_SERVER` is bindable: if yours names a remote `tcp:` server, arbox treats it as "no socket" rather than silently substituting a local one. |
| ALSA devices `/dev/snd`                             | `--device /dev/snd`, plus `--group-add` for the group owning those nodes — `--user UID:GID` drops the host user's supplementary groups, so without it the mode-0660 device nodes stay unopenable. |

The socket route is the normal one on a desktop session and needs no device
access at all. `--voice` fails immediately (before any image build) when the
host has neither — a headless box, a session with no sound server running, or
a nested container without `/dev/snd`. `arbox status` reports what it can see:

```
audio:   sound server socket /run/user/1000/pulse/native (bound only with --voice)
```

The image ships the userspace side: `sox` (its `rec`/`play` front-ends are
what `/voice` records with), `alsa-utils` and `pulseaudio-utils` for poking at
devices, and an ALSA config that routes the default PCM through PulseAudio
with a fallback to real hardware, so ALSA-only callers work in either mode.

`--voice` is Linux-only. It errors out immediately on both Windows and macOS,
since Docker Desktop has no way to hand host sound devices to a Linux
container on either platform.

### USB serial (`--serial`)

No serial port reaches the container by default. Pass `--serial` (global, on
any launch verb) to bind the host's USB serial devices in — what you need to
flash and monitor a microcontroller dev board (ESP32, RP2040, STM32 Nucleo,
Arduino…) from inside the sandbox:

```bash
arbox bash --serial                          # every /dev/ttyUSB* + /dev/ttyACM*
arbox claude --serial-dev /dev/ttyACM0       # just this one board
arbox --serial-dev /dev/serial/by-id/usb-Espressif_USB_JTAG_serial_debug_unit_* run -- cargo run --release
```

`--serial` auto-detects and binds every `ttyUSB<n>` (CP210x / CH340 / FTDI
bridges) and `ttyACM<n>` (CDC-ACM: the native USB-Serial-JTAG on ESP32-S3,
C3, C6, H2, and most Arduino-style boards) node. `--serial-dev PATH`
(repeatable, implies `--serial`) narrows that to the named nodes and accepts
`/dev/serial/by-id/...` symlinks, which resolve to the real node. Legacy
`ttyS<n>` UARTs are never bound: every machine has those whether or not
anything is plugged in, and binding them would let `--serial` succeed on a
host with no board.

| Host thing                                | How it's passed |
|-------------------------------------------|-----------------|
| Each device node                          | `--device /dev/ttyUSB0`, plus `--group-add` for the group owning the node (`dialout`, read off the inode rather than assumed) — `--user UID:GID` drops the host user's supplementary groups, so without it the mode-0660 nodes stay unopenable. |
| The node's char-device major (188 for `ttyUSB`, 166 for `ttyACM`) | `--device-cgroup-rule "c 188:* rmw"`. Boards with a native USB-Serial-JTAG (S3/C3) drop off the bus when they enter download mode mid-flash. If the board comes back under the same name the existing node keeps working; if it comes back as `ttyACM1`, the rule lets you `sudo mknod /dev/ttyACM1 c 166 1` inside the container instead of relaunching. |
| udev database `/run/udev`                 | Bind-mounted read-only when present, so libudev-based port enumeration (`espflash`, anything on `serialport-rs`) sees USB vendor/product ids and can pick the board by name. Without it those tools still work, but need an explicit `--port`. |

`--serial` fails immediately (before any image build) when the host has no
matching device, and `--serial-dev` fails when a path doesn't exist or isn't
a character device. Every launch prints what it bound, on stderr so piped
`arbox run` output stays clean:

```
arbox: serial devices bound: /dev/ttyACM0, udev db /run/udev read-only
```

`arbox status` reports what it can see:

```
serial:  /dev/ttyACM0 (bound only with --serial)
```

The image ships the userspace side: `libudev-dev` (so `cargo install
espflash` builds in the box), `dfu-util` for ESP32-S2/S3 DFU flashing, and
`picocom` as a plain serial terminal. Networking is unchanged, so Wi-Fi
debugging, mDNS discovery, OTA uploads, and `espflash monitor` all behave as
they do on the host.

`--serial` is Linux-only. It errors out immediately on both Windows and macOS,
since Docker Desktop's Linux VM has no USB passthrough on either platform.

#### ESP32 toolchains

The flashing tools are ordinary cargo installs and land in `~/.cargo/bin`,
which is already shared with the container:

```bash
cargo install espflash cargo-espflash ldproxy
```

The compiler side is host work, because `~/.rustup` is mounted read-only:

- **RISC-V chips (C3, C6, H2)** build with the stock toolchain. On the host,
  `rustup target add riscv32imac-unknown-none-elf` (or `riscv32imc-` for the
  C3) and `rustup component add rust-src`; the container sees both through the
  existing mount.
- **Xtensa chips (ESP32, S2, S3)** need Espressif's forked toolchain. Run
  `espup install` on the host — it installs the `esp` toolchain into
  `~/.rustup` and the Xtensa GCC alongside it, both of which flow in through
  the read-only mount — and `source ~/export-esp.sh` inside the box before
  building, so `LIBCLANG_PATH` and `PATH` point at them.
- **`std` projects on ESP-IDF** (`esp-idf-sys`) additionally write several
  gigabytes of SDK and a Python venv under `~/.espressif`, which is not one of
  arbox's persisted mounts. Add `--rw ~/.espressif` so that survives across
  launches. Bare-metal `no_std` projects on `esp-hal` avoid this entirely.

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
| `~/.arbox/profiles/personal/.config/opencode`         | `~/.config/opencode`      |
| `~/.arbox/profiles/personal/.local/share/opencode`    | `~/.local/share/opencode` |
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

## Windows Quirks

### Git Worktrees

On Windows, git worktrees require special handling because the `.git` file contains a path reference that doesn't work inside the container. **arbox automatically handles this** by:

1. Converting the absolute Windows path in `.git` to a relative path
2. Adding the container mount path to git's `safe.directory` config on launch
3. Removing the `safe.directory` entry when the container exits

This happens transparently when you launch any arbox command from a worktree,
so no manual setup or cleanup is needed.

If you want to manually view or manage safe.directory entries:

```bash
# View all safe.directory entries
git config --global --get-all safe.directory

# Remove a specific path
git config --global --unset safe.directory /mnt/c/Users/<username>/path/to/worktree

# Remove all entries
git config --global --unset-all safe.directory
```

### User ID/Group ID

On Windows, arbox uses a hardcoded UID and GID of `1000` for container processes, since Windows does not have Unix-style user and group IDs. This value is a standard default for the first non-root user in Linux container environments.

Files created inside the container will appear to be owned by UID/GID 1000 in the container, but on the Windows host they are owned by your Windows user account as expected. This allows the container to function as a normal Linux environment while keeping host file ownership correct.

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
   added only when stdin is an interactive terminal. The host Wayland socket
   is added when the session has one, `--voice` adds the sound-server
   socket and/or `--device /dev/snd` (see [Audio](#audio---voice)), and
   `--serial` adds `--device` for each USB serial node (see
   [USB serial](#usb-serial---serial)).

## Customization

Most behavior is controlled by what's on your host:

- The image follows your host's Ubuntu codename from `/etc/os-release`.
- The container user and home directory come from `getpwuid_r`, not from
  `$USER` or `$HOME`.
- The current directory selects the git workspace to mount.
- Editing `src/Dockerfile` invalidates the cached image tag automatically;
  the next launch verb rebuilds with no extra flags.
- The agents (claude, codex, opencode, agy, grok) are pinned to `@latest` at build time,
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
