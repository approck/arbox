# check=skip=InvalidDefaultArgInFrom
# (^ MUST be line 1: BuildKit parser directives stop at the first comment-or-
# blank-or-instruction. The InvalidDefaultArgInFrom lint complains that
# HOST_DISTRO_CODENAME has no default; that's intentional — every host-
# specific value is required, so a missing build arg fails loudly instead of
# silently substituting a default.)
#
# arbox sandbox image. Built per-host (codename + uid) by `arbox build`.

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

# All four coding agents baked into the image. The host's user config dirs
# (~/.claude, ~/.claude.json, ~/.codex, ~/.gemini, ~/.config/antigravity,
# ~/.grok) are still bind-mounted from the host so credentials and history
# persist across container invocations, but the BINARIES live in the image —
# no host install needed, and the host's ~/.local/bin shadow is no longer
# relied on for agent launches.
#
# claude + codex via npm (official distribution channels). agy and grok via
# their respective install.sh scripts, run with a staged HOME so their
# self-update / download caches don't pollute /root.
ARG CLAUDE_CODE_VERSION=latest
RUN npm install -g "@anthropic-ai/claude-code@${CLAUDE_CODE_VERSION}"

ARG CODEX_VERSION=latest
RUN npm install -g "@openai/codex@${CODEX_VERSION}"

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

ARG WINDOWS_HOST=false
# Mirror the host user/group exactly.
# Most ubuntu:<codename> base images ship a default user at uid 1000;
# rename that entry instead of failing on conflict.
# Skipped for windows.
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

# Rust toolchain: conditionally baked into the image for Windows hosts,
# or mounted from the host for native Linux/Unix hosts.
# On Windows, install rustup inside the container. On Linux, mount host ~/.cargo/.rustup.
ARG CARGO_HOME=/root/.cargo
ARG RUSTUP_HOME=/root/.rustup
ENV CARGO_HOME=${CARGO_HOME}
ENV RUSTUP_HOME=${RUSTUP_HOME}
RUN if [ "${WINDOWS_HOST}" = "true" ]; then \
      curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable && \
      chmod -R 777 /usr/local/cargo /usr/local/rustup; \
    fi

# Set up PATH: include both container-installed (Windows) and host-mounted (Linux) paths.
# Windows: /usr/local/cargo/bin is used (set via CARGO_HOME above).
# Linux: host's ${HOST_HOME}/.cargo/bin is mounted and used.
ENV PATH=/usr/local/cargo/bin:/root/.cargo/bin:${HOST_HOME}/.cargo/bin:${HOST_HOME}/.local/bin:/usr/local/bin:/usr/bin:/bin

# Marker so the host-side `arbox` binary refuses to run inside its own
# container (where there's no `docker` to talk to anyway).
ENV ARBOX_INSIDE=1

# Stop Claude Code's background autoupdater check inside the sandbox.
# Neither autoupdate nor `claude update`/`claude install` will work in here
# anyway — upgrade Claude Code on the host, outside of arbox.
ENV DISABLE_AUTOUPDATER=1

USER ${HOST_USER}
WORKDIR ${HOST_HOME}
CMD ["/bin/bash", "-l"]
