# Security policy

## Reporting a vulnerability

Please **do not open a public GitHub issue** for security-sensitive reports.
Instead, email the maintainers at:

> **security@appcove.com**

Include:

- A description of the issue and its impact.
- Steps to reproduce (or a proof of concept).
- Your assessment of who is affected and under what conditions.
- Whether you'd like to be credited in the fix announcement.

We'll acknowledge receipt within a few business days and follow up with a
timeline once the issue has been triaged.

## Scope

`arbox` is a sandbox for running development tools, including LLM agents,
against your real source tree while exposing only a deliberate set of host
paths. The threat model and what's in/out of scope is documented in the
[README's Security model section](README.md#security-model). In short:

**In scope**

- Anything that lets a process inside the container modify host files outside
  the explicitly-mounted set.
- Bypasses of read-only mounts such as `~/.rustup`, `~/.local/bin`, or
  `~/.local/share/claude`.
- Anything that exposes more host IPC than the documented Wayland display
  socket mount used for clipboard support.
- Code execution on the host triggered by something that should run only
  inside the container, such as a malicious build script.
- Privilege escalation on the host that the tool's design enables.
- Vulnerabilities in `arbox` itself: command injection through Docker build
  arguments, Docker run arguments, git path handling, or user argument
  forwarding.

**Out of scope**

- Container escape vulnerabilities in Docker, runc, containerd, or the Linux
  kernel. Report those upstream.
- Risks inherent to the design and called out in the README, such as a
  compromised agent modifying the mounted project, `~/.cargo`, `~/.claude`,
  `~/.claude.json`, or `~/.codex`.
- Clipboard access through the documented Wayland socket mount when the host
  has a Wayland session.
- Anything that requires the attacker to already have host shell.
- Network access from inside the container. `arbox` intentionally uses host
  networking.

## Supported versions

The project is pre-1.0. Only the `main` branch receives security fixes. Once
tagged releases exist, this section will be updated.
