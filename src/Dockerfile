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
# subsequent RUN steps can reference it. Used to fetch the right uv/deno
# release tarball for the host architecture.
ARG TARGETARCH

ENV DEBIAN_FRONTEND=noninteractive

# Apt package set, grouped by purpose:
#   - C/C++ build drivers + linkers: build-essential pkg-config clang
#     libclang-dev cmake mold (clang+libclang-dev are required by bindgen-
#     based crates; cmake/mold cover *-sys crates and faster linking).
#   - System libraries most non-trivial Rust crates link against: openssl,
#     sqlite, postgres, zlib, lzma.
#   - protobuf-compiler for tonic/prost.
#   - gdb for debugging.
#   - The rest are agent ergonomics: shells, search tools, network tools,
#     wayland-clipboard for image paste, python + uv (later) for PEP-723
#     scripts.
RUN apt-get update && apt-get install -y --no-install-recommends \
      build-essential pkg-config clang libclang-dev cmake mold \
      libssl-dev libsqlite3-dev libpq-dev zlib1g-dev liblzma-dev \
      protobuf-compiler \
      gdb \
      ca-certificates curl git git-lfs \
      vim nano less sudo jq unzip zip xz-utils \
      ripgrep fd-find bat tree tmux shellcheck \
      dnsutils iputils-ping \
      wl-clipboard \
      python3 python3-venv python3-pip \
      libnss3 libatk1.0-0 libatk-bridge2.0-0 libcups2 libxkbcommon0 \
      libxcomposite1 libxdamage1 libxfixes3 libxrandr2 libgbm1 \
      libpango-1.0-0 libcairo2 libasound2t64 libatspi2.0-0 libxshmfence1 \
      fonts-liberation fonts-noto-color-emoji \
    && rm -rf /var/lib/apt/lists/*

# Debian/Ubuntu rename `fd` -> `fdfind` and `bat` -> `batcat` to avoid name
# collisions with other packages. Symlink them back to the names everyone
# (and every agent) actually uses.
RUN ln -s /usr/bin/fdfind /usr/local/bin/fd \
 && ln -s /usr/bin/batcat /usr/local/bin/bat

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
ARG DENO_VERSION=2.1.6
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

# Node.js (LTS, pinned). Mirrors the uv/deno pattern: versioned tarball
# from the canonical distribution, no `curl | sh` against a moving install
# script. Override NODE_VERSION at build time to bump. 22.x is required —
# @openai/codex's npm package rejects anything older.
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
ARG PLAYWRIGHT_VERSION=1.49.0
ENV PLAYWRIGHT_BROWSERS_PATH=/opt/ms-playwright
RUN npm install -g \
      "playwright@${PLAYWRIGHT_VERSION}" \
      "@playwright/test@${PLAYWRIGHT_VERSION}" \
 && playwright install chromium firefox \
 && chmod -R a+rX /opt/ms-playwright

# Rust toolchain: conditionally baked into the image for Windows hosts, or
# mounted from the host for native Linux/Unix hosts. On Windows, install
# rustup inside the container; on Linux, host ~/.cargo/.rustup are mounted at
# runtime and this RUN is a cheap no-op. Lives ABOVE the agent cache-bust
# barrier because the toolchain download is expensive and must stay cached
# across `arbox update` runs.
ARG WINDOWS_HOST=false
ARG CARGO_HOME=/root/.cargo
ARG RUSTUP_HOME=/root/.rustup
ENV CARGO_HOME=${CARGO_HOME}
ENV RUSTUP_HOME=${RUSTUP_HOME}
RUN if [ "${WINDOWS_HOST}" = "true" ]; then \
      curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable && \
      chmod -R 777 /usr/local/cargo /usr/local/rustup; \
    fi

# Cache-bust barrier for the five agent layers below. `arbox update` passes a
# fresh value here (a unix timestamp) as a --build-arg, which invalidates this
# layer and therefore every layer after it — the claude/codex/opencode/agy/grok
# installs re-run and pull their latest published versions, while every
# expensive layer ABOVE this point (apt, uv, deno, node, the ~700 MB
# Playwright browser downloads, the Windows-host rustup toolchain) stays
# cached. The default 0 keeps ordinary cached builds (the auto-bootstrap on
# first launch, `arbox update --force`'s --no-cache pass) unaffected: the
# agents only move when you run `arbox update`. This barrier MUST stay below
# the last expensive layer and above the first agent install.
ARG AGENT_REFRESH=0
RUN echo "arbox agent refresh token: ${AGENT_REFRESH}" >/dev/null

# All five coding agents baked into the image. The host's user config dirs
# (~/.claude, ~/.claude.json, ~/.codex, ~/.config/opencode,
# ~/.local/share/opencode, ~/.gemini, ~/.config/antigravity, ~/.grok) are
# still bind-mounted from the host so credentials and history persist across
# container invocations, but the BINARIES live in the image — no host install
# needed, and the host's ~/.local/bin shadow is no longer relied on for agent
# launches.
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
# rename that entry instead of failing on conflict.
# Skipped for windows (no matching host user exists to mirror).
RUN if [ "${WINDOWS_HOST}" = "false" ]; then \
      if getent group ${HOST_GID} >/dev/null; then \
          groupmod -n ${HOST_USER} $(getent group ${HOST_GID} | cut -d: -f1); \
      else \
          groupadd -g ${HOST_GID} ${HOST_USER}; \
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

# Set up PATH: include both container-installed (Windows) and host-mounted (Linux) paths.
# Ordering matters: /usr/local/bin (where the baked-in claude/codex/opencode/
# agy/grok/playwright/node/uv/deno live) MUST come before ${HOST_HOME}/.local/bin,
# which is bind-mounted read-only from the host at runtime. If ~/.local/bin
# were first (as it once was), a stale host install like
# ~/.local/bin/claude -> ~/.local/share/claude/versions/X would shadow the
# freshly-baked binary, making `arbox update` look like a no-op. ~/.cargo/bin
# stays first so the host's rustup shims win (no agent collides with those).
# Windows: /usr/local/cargo/bin is used (set via CARGO_HOME above).
# Linux: host's ${HOST_HOME}/.cargo/bin is mounted and used.
ENV PATH=/usr/local/cargo/bin:/root/.cargo/bin:${HOST_HOME}/.cargo/bin:/usr/local/bin:${HOST_HOME}/.local/bin:/usr/bin:/bin

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
