# check=skip=InvalidDefaultArgInFrom
# (^ MUST be line 1: BuildKit parser directives stop at the first comment-or-
# blank-or-instruction. The InvalidDefaultArgInFrom lint complains that
# HOST_DISTRO_CODENAME has no default; that's intentional — every host-
# specific value is required, so a missing build arg fails loudly instead of
# silently substituting a default.)
#
# arbox sandbox image. Built per-host (codename + uid) by `arbox update`.

ARG HOST_DISTRO_CODENAME
FROM ubuntu:${HOST_DISTRO_CODENAME}

ARG HOST_UID
ARG HOST_GID
ARG HOST_USER
ARG HOST_HOME

# BuildKit auto-injects TARGETARCH (e.g. amd64, arm64). We re-declare it so
# subsequent RUN steps can reference it. Used to fetch the right node/uv/deno/
# bun release tarball for the host architecture.
ARG TARGETARCH

ENV DEBIAN_FRONTEND=noninteractive

# Apt, part 1 of 2: ONLY what the Playwright browser download below needs.
# Everything else is a second apt layer underneath it, so adding a dev package
# later re-runs that layer alone and leaves the ~700 MB of browsers cached.
#   - ca-certificates + curl fetch the node tarball; xz-utils unpacks it.
#   - The lib* set is Chromium's and Firefox's shared-library closure, and the
#     fonts are what keep rendered pages (and emoji) from turning into boxes.
#     These are launch-time requirements, not download-time ones, so they could
#     technically live below — they're here so the whole browser story is one
#     contiguous block, at the cost of a re-download if the list ever changes.
#     It rarely does; new browser versions ship their own bundled libraries.
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl xz-utils \
      libnss3 libatk1.0-0 libatk-bridge2.0-0 libcups2 libxkbcommon0 \
      libxcomposite1 libxdamage1 libxfixes3 libxrandr2 libgbm1 \
      libpango-1.0-0 libcairo2 libasound2t64 libatspi2.0-0 libxshmfence1 \
      fonts-liberation fonts-noto-color-emoji \
    && rm -rf /var/lib/apt/lists/*

# Node.js (LTS, pinned) — first of the downloaded toolchains, and ONLY because
# Playwright's browser fetch below needs npm. Same versioned-tarball pattern as
# uv/deno/bun further down: no `curl | sh` against a moving install script.
# Override NODE_VERSION at build time to bump. 22.x is required — @openai/codex's
# npm package rejects anything older.
ARG NODE_VERSION=22.11.0
RUN ARCH=$(case "${TARGETARCH}" in \
        amd64) echo x64 ;; \
        arm64) echo arm64 ;; \
        *)     echo "" ;; \
    esac) \
 && [ -n "${ARCH}" ] || { echo "arbox: unsupported TARGETARCH=${TARGETARCH}" >&2; exit 1; } \
 && curl -fsSL "https://nodejs.org/dist/v${NODE_VERSION}/node-v${NODE_VERSION}-linux-${ARCH}.tar.xz" \
      -o /tmp/node.tar.xz \
 && tar -xJf /tmp/node.tar.xz -C /usr/local --strip-components=1 \
 && rm -f /tmp/node.tar.xz

# Playwright (pinned) plus its bundled Chromium and Firefox baked into the
# image at /opt/ms-playwright. WebKit deliberately skipped — its system
# library list is larger and changes more often. Override PLAYWRIGHT_VERSION
# at build time to bump.
#
# This is the single most expensive layer in the image (~700 MB of browser
# downloads), so it sits as HIGH in the file as its dependencies allow: only
# the browser-prerequisite apt layer and node (which supplies the npm that
# fetches them) may precede it. Everything else — the main apt set, uv, deno,
# bun, pnpm, wrangler, rustup, the agents — goes BELOW, so bumping or adding
# any of them leaves the browsers cached. Nothing new should be inserted above
# this point without a hard dependency reason.
ARG PLAYWRIGHT_VERSION=1.49.0
ENV PLAYWRIGHT_BROWSERS_PATH=/opt/ms-playwright
RUN npm install -g \
      "playwright@${PLAYWRIGHT_VERSION}" \
      "@playwright/test@${PLAYWRIGHT_VERSION}" \
 && playwright install chromium firefox \
 && chmod -R a+rX /opt/ms-playwright

# Apt, part 2 of 2: everything that isn't a browser prerequisite, grouped by
# purpose. This is the layer that churns, which is exactly why it lives below
# the browser download.
#   - C/C++ build drivers + linkers: build-essential pkg-config clang
#     libclang-dev cmake mold (clang+libclang-dev are required by bindgen-
#     based crates; cmake/mold cover *-sys crates and faster linking).
#   - System libraries most non-trivial Rust crates link against: openssl,
#     sqlite, postgres, zlib, lzma.
#   - protobuf-compiler for tonic/prost.
#   - Database CLI clients: psql, redis-cli, mysql.
#   - gdb for debugging.
#   - Embedded / serial userspace: libudev-dev (espflash and anything on
#     serialport-rs link libudev to enumerate ports), dfu-util (ESP32-S2/S3
#     DFU flashing), picocom (a serial terminal for when `espflash monitor`
#     is the wrong shape). The device nodes themselves are NOT in the image —
#     they come in at runtime, only under `arbox --serial`. The Xtensa/RISC-V
#     Rust toolchains are host-side (`espup install` into ~/.rustup, mounted
#     read-only) and espflash/ldproxy are `cargo install`ed into ~/.cargo/bin.
#   - Audio userspace: sox (its `rec`/`play` front-ends are what claude's
#     /voice records with) plus libsox-fmt-all for the pulse/alsa/format
#     backends, alsa-utils + pulseaudio-utils to inspect devices
#     (aplay -l, pactl info) when sound misbehaves, and libasound2-plugins
#     for the ALSA -> PulseAudio bridge so ALSA-only callers still reach the
#     host's sound server. The devices and socket themselves are NOT in the
#     image — they come in at runtime, only under `arbox --voice`.
#   - GUI userspace, for Wayland windows (the host socket is mounted by
#     default; `--no-mount-wayland` withholds it):
#     libwayland-cursor0 + libwayland-egl1 complete the winit runtime closure
#     (libwayland-client0 and libxkbcommon0 are already here via wl-clipboard
#     and Chromium); libdecor-0-0 draws SDL2/pygame title bars on GNOME, which
#     has no server-side decorations; libegl1 + libgles2 + libgl1 +
#     libgl1-mesa-dri are the GL path (eframe's glow backend, Slint) and carry
#     llvmpipe; libvulkan1 + mesa-vulkan-drivers are the Vulkan path (wgpu,
#     iced, Bevy) and carry lavapipe — so windows render with NO GPU
#     passthrough, and `--gpu` is a speed-up, not a prerequisite;
#     fontconfig because winit draws its own title bar text on GNOME;
#     adwaita-icon-theme supplies the pointer cursor (invisible without a
#     theme); libdbus-1-3 because Qt (PySide6/PyQt6 wheels) links it even on
#     Wayland; libgtk-4-dev + libadwaita-1-dev so gtk-rs / relm4 / PyGObject
#     projects build; mesa-utils + vulkan-tools (eglinfo, vulkaninfo) to see
#     whether an app got hardware or software rendering. The compositor
#     socket and /dev/dri are NOT in the image — they come in at runtime,
#     the socket by default (`--no-mount-wayland` withholds), the render
#     nodes only under `--gpu`. No X11 server libs: the X11
#     socket is never mounted.
#   - unzip is what the deno and bun installs below unpack their releases with.
#   - The rest are agent ergonomics: shells, search tools, network tools,
#     wayland-clipboard for image paste, python + uv (later) for PEP-723
#     scripts.
RUN apt-get update && apt-get install -y --no-install-recommends \
      build-essential pkg-config clang libclang-dev cmake mold \
      libssl-dev libsqlite3-dev libpq-dev zlib1g-dev liblzma-dev \
      protobuf-compiler \
      postgresql-client redis-tools default-mysql-client \
      gdb \
      libudev-dev dfu-util picocom \
      git git-lfs \
      vim nano less sudo jq unzip zip \
      ripgrep fd-find bat tree tmux shellcheck \
      dnsutils iputils-ping \
      wl-clipboard \
      sox libsox-fmt-all alsa-utils pulseaudio-utils libasound2-plugins \
      libwayland-cursor0 libwayland-egl1 libdecor-0-0 \
      libegl1 libgles2 libgl1 libgl1-mesa-dri \
      libvulkan1 mesa-vulkan-drivers \
      fontconfig adwaita-icon-theme libdbus-1-3 \
      libgtk-4-dev libadwaita-1-dev \
      mesa-utils vulkan-tools \
      python3 python3-venv python3-pip \
    && rm -rf /var/lib/apt/lists/*

# Debian/Ubuntu rename `fd` -> `fdfind` and `bat` -> `batcat` to avoid name
# collisions with other packages. Symlink them back to the names everyone
# (and every agent) actually uses.
RUN ln -s /usr/bin/fdfind /usr/local/bin/fd \
 && ln -s /usr/bin/batcat /usr/local/bin/bat

# Route ALSA's "default" device through PulseAudio, so callers that only speak
# ALSA (aplay/arecord, chromium) reach the sound-server socket `arbox --voice`
# mounts — the common case, since that socket needs no /dev/snd passthrough at
# all. `fallback "sysdefault"` is what makes this safe to bake in
# unconditionally: with no socket mounted (or `--voice` passing only /dev/snd)
# the pulse connection fails and ALSA drops straight back to the real hardware,
# rather than the config turning a hardware-only box mute.
RUN install -d /etc/alsa/conf.d \
 && printf '%s\n' \
      'pcm.!default {' \
      '    type pulse' \
      '    fallback "sysdefault"' \
      '}' \
      'ctl.!default {' \
      '    type pulse' \
      '    fallback "sysdefault"' \
      '}' \
    > /etc/alsa/conf.d/99-arbox-pulse.conf

# uv (Python package + project manager). Versioned tarball direct from
# GitHub releases — no `curl | sh` against a moving install script. Override
# UV_VERSION at build time to bump. Pinned checksum is a known TODO.
ARG UV_VERSION=0.5.13
RUN ARCH=$(case "${TARGETARCH}" in \
        amd64) echo x86_64-unknown-linux-gnu ;; \
        arm64) echo aarch64-unknown-linux-gnu ;; \
        *)     echo "" ;; \
    esac) \
 && [ -n "${ARCH}" ] || { echo "arbox: unsupported TARGETARCH=${TARGETARCH}" >&2; exit 1; } \
 && curl -fsSL "https://github.com/astral-sh/uv/releases/download/${UV_VERSION}/uv-${ARCH}.tar.gz" \
      -o /tmp/uv.tar.gz \
 && tar -xzf /tmp/uv.tar.gz -C /tmp \
 && install -m 0755 "/tmp/uv-${ARCH}/uv"  /usr/local/bin/uv \
 && install -m 0755 "/tmp/uv-${ARCH}/uvx" /usr/local/bin/uvx \
 && rm -rf /tmp/uv.tar.gz "/tmp/uv-${ARCH}"

# deno (JS/TS runtime + script runner). Same pattern: versioned binary from
# GitHub releases. Override DENO_VERSION at build time to bump.
ARG DENO_VERSION=2.9.6
RUN ARCH=$(case "${TARGETARCH}" in \
        amd64) echo x86_64-unknown-linux-gnu ;; \
        arm64) echo aarch64-unknown-linux-gnu ;; \
        *)     echo "" ;; \
    esac) \
 && [ -n "${ARCH}" ] || { echo "arbox: unsupported TARGETARCH=${TARGETARCH}" >&2; exit 1; } \
 && curl -fsSL "https://github.com/denoland/deno/releases/download/v${DENO_VERSION}/deno-${ARCH}.zip" \
      -o /tmp/deno.zip \
 && unzip -q /tmp/deno.zip -d /tmp \
 && install -m 0755 /tmp/deno /usr/local/bin/deno \
 && rm -f /tmp/deno.zip /tmp/deno

# Rust toolchain: baked into the image for Windows/macOS hosts (BAKE_RUSTUP),
# or mounted from the host for native Linux hosts, where this RUN is a cheap
# no-op. Lives ABOVE the agent cache-bust barrier because the toolchain
# download is expensive and must stay cached across `arbox update` runs — but
# BELOW Playwright, so bumping the toolchain never re-fetches the browsers.
ARG WINDOWS_HOST=false
ARG BAKE_RUSTUP=false
ARG CARGO_HOME=/root/.cargo
ARG RUSTUP_HOME=/root/.rustup
ENV CARGO_HOME=${CARGO_HOME}
ENV RUSTUP_HOME=${RUSTUP_HOME}
RUN if [ "${BAKE_RUSTUP}" = "true" ]; then \
      curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable && \
      chmod -R 777 "${CARGO_HOME}" "${RUSTUP_HOME}"; \
    fi

# JS/TS package managers, the Cloudflare Workers CLI, and the GitHub CLI.
# Deliberately placed BELOW the Playwright + rustup layers so adding or
# bumping any of these never invalidates the browser download or a baked
# toolchain, and ABOVE the agent cache-bust barrier so they stay pinned
# instead of moving on every `arbox update`.

# bun (JS runtime + npm-compatible package manager, `bun install` / `bunx`).
# Same versioned-release pattern as uv/deno. NOTE: the plain linux-x64 build
# requires an AVX2-capable CPU; on older hardware swap the asset name for
# `bun-linux-x64-baseline.zip`.
ARG BUN_VERSION=1.4.2
RUN ARCH=$(case "${TARGETARCH}" in \
        amd64) echo linux-x64 ;; \
        arm64) echo linux-aarch64 ;; \
        *)     echo "" ;; \
    esac) \
 && [ -n "${ARCH}" ] || { echo "arbox: unsupported TARGETARCH=${TARGETARCH}" >&2; exit 1; } \
 && curl -fsSL "https://github.com/oven-sh/bun/releases/download/bun-v${BUN_VERSION}/bun-${ARCH}.zip" \
      -o /tmp/bun.zip \
 && unzip -q /tmp/bun.zip -d /tmp \
 && install -m 0755 "/tmp/bun-${ARCH}/bun" /usr/local/bin/bun \
 && ln -s /usr/local/bin/bun /usr/local/bin/bunx \
 && rm -rf /tmp/bun.zip "/tmp/bun-${ARCH}"

# pnpm, for repos whose lockfile is pnpm-lock.yaml. Installed as a plain
# global npm package rather than through corepack: corepack downloads the
# real package manager on first use (a network fetch at runtime, into an
# ephemeral container cache) and is no longer distributed with Node 25+.
ARG PNPM_VERSION=12.3.4
RUN npm install -g "pnpm@${PNPM_VERSION}"

# wrangler (Cloudflare Workers CLI). `wrangler dev` binds 8787 (worker) and
# 9229 (inspector), and `wrangler login` binds 8976 for its OAuth callback —
# all reachable from the host browser because arbox runs with `--network
# host` (on Windows/macOS that needs Docker Desktop's opt-in host networking).
#
# No Cloudflare credential is present except under `arbox wrangler`, the verb
# that runs this binary and mounts the config dir wrangler caches its OAuth
# token in (see `wrangler_config_source` in src/launch.rs). Every other verb
# leaves it out, which costs nothing: `wrangler dev` runs the Worker locally in
# workerd with KV, R2, D1, Durable Objects and Queues simulated on the machine,
# so Workers are developed and tested without a deploy-capable token at all.
# Note also that the image has no browser and no xdg-open, so an interactive
# login in here wants `wrangler login --device` (or `--browser false`).
# (the Wayland socket puts a display in reach, but still no browser binary.)
ARG WRANGLER_VERSION=4.130.0
RUN npm install -g "wrangler@${WRANGLER_VERSION}"

# gh (GitHub CLI). Versioned tarball from GitHub releases, same pattern as
# uv/deno/bun. Override GH_VERSION at build time to bump.
#
# No GitHub credential is present except under `arbox gh`, the verb that runs
# this binary and mounts the config dir gh stores its token in (hosts.yml —
# see the `TOOLS` table in src/launch.rs). Every other verb leaves it out. gh
# has no keyring in here, so `arbox gh auth login` falls back to the file,
# which is exactly what makes the mount persist it. With the config mounted,
# a host ~/.gitconfig that names `gh auth git-credential` as the credential
# helper makes `git push` over HTTPS work inside the box too.
ARG GH_VERSION=2.100.0
RUN ARCH=$(case "${TARGETARCH}" in \
        amd64) echo amd64 ;; \
        arm64) echo arm64 ;; \
        *)     echo "" ;; \
    esac) \
 && [ -n "${ARCH}" ] || { echo "arbox: unsupported TARGETARCH=${TARGETARCH}" >&2; exit 1; } \
 && curl -fsSL "https://github.com/cli/cli/releases/download/v${GH_VERSION}/gh_${GH_VERSION}_linux_${ARCH}.tar.gz" \
      -o /tmp/gh.tar.gz \
 && tar -xzf /tmp/gh.tar.gz -C /tmp \
 && install -m 0755 "/tmp/gh_${GH_VERSION}_linux_${ARCH}/bin/gh" /usr/local/bin/gh \
 && rm -rf /tmp/gh.tar.gz "/tmp/gh_${GH_VERSION}_linux_${ARCH}"

# Cache-bust barrier for the five agent layers below. `arbox update` passes a
# fresh value here (a unix timestamp) as a --build-arg, which invalidates this
# layer and therefore every layer after it — the claude/codex/opencode/agy/grok
# installs re-run and pull their latest published versions, while every
# expensive layer ABOVE this point (apt, node, the ~700 MB Playwright browser
# downloads, uv, deno, bun/pnpm/wrangler/gh, the baked rustup toolchain on
# Windows/macOS) stays cached. The default 0 keeps ordinary cached builds (the auto-bootstrap on
# first launch, `arbox update --force`'s --no-cache pass) unaffected: the
# agents only move when you run `arbox update`. This barrier MUST stay below
# the last expensive layer and above the first agent install.
ARG AGENT_REFRESH=0
RUN echo "arbox agent refresh token: ${AGENT_REFRESH}" >/dev/null

# All five coding agents baked into the image. Each agent's host config dir
# (~/.claude + ~/.claude.json, ~/.codex, ~/.config/opencode +
# ~/.local/share/opencode, ~/.gemini + ~/.config/antigravity, ~/.grok) is
# bind-mounted by that agent's OWN verb and no other, so credentials and
# history persist across container invocations without any agent seeing
# another's (see `select` in src/launch.rs). The BINARIES live in the image —
# no host install needed, and the host's ~/.local/bin shadow is no longer
# relied on for agent launches.
#
# claude + codex + opencode via npm (official distribution channels; the
# opencode-ai package resolves the right platform binary through its
# optionalDependencies). agy and grok via their respective install.sh
# scripts, run with a staged HOME so their self-update / download caches
# don't pollute /root.
ARG CLAUDE_CODE_VERSION=latest
RUN npm install -g "@anthropic-ai/claude-code@${CLAUDE_CODE_VERSION}"

ARG CODEX_VERSION=latest
RUN npm install -g "@openai/codex@${CODEX_VERSION}"

ARG OPENCODE_VERSION=latest
RUN npm install -g "opencode-ai@${OPENCODE_VERSION}"

# Antigravity CLI (`agy`). The install.sh accepts `--dir` for a custom
# install location; we stage the script's transient files under a temp HOME
# and drop the binary at /usr/local/bin/agy. The script's trailing
# `agy install` step (which configures shell rc files for an interactive
# user) is harmless here — it writes into the staged HOME, which is then
# deleted.
RUN HOME=/tmp/agy-stage mkdir -p /tmp/agy-stage \
 && curl -fsSL https://antigravity.google/cli/install.sh -o /tmp/agy-install.sh \
 && HOME=/tmp/agy-stage bash /tmp/agy-install.sh --dir /usr/local/bin \
 && rm -rf /tmp/agy-stage /tmp/agy-install.sh

# Grok Build CLI (`grok`). The install.sh downloads a static binary into
# $HOME/.grok/downloads and symlinks $GROK_BIN_DIR/grok at it; running as
# root it ALSO drops /usr/local/bin/grok + /usr/local/bin/agent symlinks
# pointing back into the staged HOME. We let it do all that, then resolve
# the symlink chain to copy the real binary out to a path that survives
# staged-HOME cleanup, replace the symlink with the real file, and drop
# the `agent` alias (we only expose `grok` as an arbox verb).
RUN mkdir -p /tmp/grok-stage \
 && curl -fsSL https://x.ai/cli/install.sh -o /tmp/grok-install.sh \
 && HOME=/tmp/grok-stage GROK_BIN_DIR=/tmp/grok-stage/bin \
      bash /tmp/grok-install.sh \
 && cp "$(readlink -f /usr/local/bin/grok)" /tmp/grok-bin \
 && rm -f /usr/local/bin/grok /usr/local/bin/agent \
 && mv /tmp/grok-bin /usr/local/bin/grok \
 && chmod +x /usr/local/bin/grok \
 && rm -rf /tmp/grok-stage /tmp/grok-install.sh

# Mirror the host user/group exactly.
# Most ubuntu:<codename> base images ship a default user at uid 1000;
# rename that entry instead of failing on conflict. Renaming by GID/UID also
# covers macOS hosts, whose default uid/gid (501/20) collide with the base
# image's own `ubuntu` user and `dialout` group. Two collisions to handle,
# not one: the entry AT the host's gid/uid may need renaming to HOST_USER,
# AND some unrelated entry may already own the name HOST_USER wants (e.g. a
# host user literally named "staff" colliding with the base image's own
# `staff` group at a different gid) — group/usermod -n|-l would abort the
# build with "already exists" in that case, so rename any such entry out of
# the way first.
# Skipped for windows (no matching host user exists to mirror).
RUN if [ "${WINDOWS_HOST}" = "false" ]; then \
      if getent group ${HOST_USER} >/dev/null \
         && [ "$(getent group ${HOST_USER} | cut -d: -f3)" != "${HOST_GID}" ]; then \
          groupmod -n ${HOST_USER}-arbox-old ${HOST_USER}; \
      fi && \
      if getent group ${HOST_GID} >/dev/null; then \
          groupmod -n ${HOST_USER} $(getent group ${HOST_GID} | cut -d: -f1); \
      else \
          groupadd -g ${HOST_GID} ${HOST_USER}; \
      fi && \
      if getent passwd ${HOST_USER} >/dev/null \
         && [ "$(getent passwd ${HOST_USER} | cut -d: -f3)" != "${HOST_UID}" ]; then \
          usermod -l ${HOST_USER}-arbox-old ${HOST_USER}; \
      fi && \
      if getent passwd ${HOST_UID} >/dev/null; then \
          usermod -l ${HOST_USER} -d ${HOST_HOME} -m -g ${HOST_GID} \
            $(getent passwd ${HOST_UID} | cut -d: -f1); \
      else \
          useradd -u ${HOST_UID} -g ${HOST_GID} -d ${HOST_HOME} -m -s /bin/bash ${HOST_USER}; \
      fi && \
      echo "${HOST_USER} ALL=(ALL) NOPASSWD: ALL" > /etc/sudoers.d/${HOST_USER} && \
      chmod 440 /etc/sudoers.d/${HOST_USER}; \
    fi

# Pre-create HOME and the standard XDG dirs, user-owned, on EVERY platform
# (containers run as --user ${HOST_UID}:${HOST_GID} regardless of host OS).
# Two distinct reasons:
#   1. Mount parents: when a bind-mount destination's parent dirs are
#      missing, the Docker daemon creates them ROOT-owned. ~/.config and
#      ~/.local + ~/.local/share are parents of the opencode/antigravity
#      state mounts — keep this list covering the parents of every nested
#      path in AGENTS (src/launch.rs). On Windows even ${HOST_HOME} itself
#      doesn't exist yet (the user-mirror RUN above is skipped there).
#   2. Runtime writes: agents write sibling XDG paths that are never mounted
#      (opencode puts TUI state in ~/.local/state and caches in ~/.cache);
#      those writes EACCES if the dirs come up root-owned.
# On Linux, useradd -m already created ${HOST_HOME}; install -d then merely
# re-asserts ownership on it and adds the XDG dirs.
RUN install -d -o ${HOST_UID} -g ${HOST_GID} \
      ${HOST_HOME} \
      ${HOST_HOME}/.config \
      ${HOST_HOME}/.cache \
      ${HOST_HOME}/.local \
      ${HOST_HOME}/.local/share \
      ${HOST_HOME}/.local/state

# Set up PATH: include both container-installed (Windows/macOS) and
# host-mounted (Linux) paths.
# Ordering matters: /usr/local/bin (where the baked-in claude/codex/opencode/
# agy/grok/playwright/node/uv/deno live) MUST come before ${HOST_HOME}/.local/bin,
# which is bind-mounted read-only from the host at runtime. If ~/.local/bin
# were first (as it once was), a stale host install like
# ~/.local/bin/claude -> ~/.local/share/claude/versions/X would shadow the
# freshly-baked binary, making `arbox update` look like a no-op. ~/.cargo/bin
# stays first so the host's rustup shims win (no agent collides with those).
# Windows/macOS: /usr/local/cargo/bin is used (set via CARGO_HOME above, see
# BAKE_RUSTUP). Linux: host's ${HOST_HOME}/.cargo/bin is mounted and used.
#
# /root/.cargo/bin is deliberately absent, and must not be re-added. Nothing
# ever installs there — image::build_with_args always passes an explicit
# CARGO_HOME (/usr/local/cargo when baking, ${HOST_HOME}/.cargo otherwise),
# both of which are already listed — while /root stays mode 0700 and the
# container runs as the host uid. An unreadable directory on PATH is not
# inert: POSIX execvp records EACCES when it can't traverse a PATH element
# and reports that instead of ENOENT once the search comes up empty, so any
# missing command surfaces as a permission error rather than "not found".
ENV PATH=/usr/local/cargo/bin:${HOST_HOME}/.cargo/bin:/usr/local/bin:${HOST_HOME}/.local/bin:/usr/bin:/bin

# Marker so the host-side `arbox` binary refuses to run inside its own
# container (where there's no `docker` to talk to anyway).
ENV ARBOX_INSIDE=1

# Stop Claude Code's background autoupdater AND its install-integrity checks
# inside the sandbox. The agents are baked into the image and bumped with
# `arbox update` — they are not self-managed in here, so neither
# `claude update`/`claude install` nor the startup install check apply.
#
# DISABLE_INSTALLATION_CHECKS specifically silences the recurring
#   "claude command at ~/.local/bin/claude missing or broken —
#    run claude install to repair"
# warning. The host's shared ~/.claude.json carries `installMethod: native`
# (on the host, claude was installed at ~/.local/bin), but the container runs
# the npm-baked /usr/local/bin/claude and deliberately does NOT mount the
# host's ~/.local/bin — so claude's native-install check would otherwise fire
# on every launch. (Confirmed against the 2.x binary: the check function
# early-returns when this env var is truthy.)
ENV DISABLE_AUTOUPDATER=1
ENV DISABLE_INSTALLATION_CHECKS=1

# Same posture for opencode: its version is baked into the image and bumped
# via `arbox update`, so its self-updater must not run in here. Equivalent to
# `autoupdate: false` in opencode.json, but as an env var it doesn't touch
# the user's mounted config.
ENV OPENCODE_DISABLE_AUTOUPDATE=true

USER ${HOST_USER}
WORKDIR ${HOST_HOME}
CMD ["/bin/bash", "-l"]
