# Contributing to arbox

Thanks for considering a contribution. This is a small project; the bar for
accepting changes is "clear win, doesn't add complexity, passes local checks."

## Ground rules

- **Apache 2.0 license:** by submitting a contribution, you agree it will be
  licensed under Apache 2.0 (see [LICENSE](LICENSE)). No CLA.
- **One concern per PR.** A bug fix, a feature, or a refactor; not all three.
- **Discuss large changes first.** Open an issue before sinking time into
  anything that touches the embedded Dockerfile, the mount model, or the
  security posture.

## Setup

```bash
git clone https://github.com/appcove/arbox
cd arbox
cargo build
cargo test
```

You can develop and run the unit test suite without Docker installed. To
exercise end-to-end behavior you'll need Docker and an Ubuntu host with the
expected Rust, Claude, and Codex config directories.

## Before opening a PR

Run the same checks maintainers will run during review:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
```

## Style

- **No comments on what code does** when the names already say it. Comments
  are for *why*: invariants, gotchas, why we picked one of several reasonable
  approaches.
- **Don't add error handling for situations that can't happen.** Internal
  invariants don't need run-time checks; only validate at process boundaries
  such as the host filesystem, Docker output, git output, and user input.
- **Prefer editing over adding.** New files, new modules, and new
  abstractions all need to earn their keep.
- **Match the existing module layout.** `host` collects host facts, `git`
  resolves workspace paths, `image` builds and inspects Docker images,
  `launch` orchestrates container execution, and small parser modules stay
  focused.

## Tests

Add tests for:

- Anything in `host.rs`, `git.rs`, `osrelease.rs`, or `passwd.rs` that does
  parsing or path manipulation.
- Any change that expands the mount model or materially changes image tags.
- Any parser or command-construction behavior that can be exercised without
  launching Docker.

Don't add tests that require a live Docker daemon unless the project grows a
separate opt-in integration test harness. Keep that boundary exercised by
hand for now.

## Commit messages

- Imperative mood, ~70 chars or less for the subject.
- Body explains *why* if it isn't obvious from the diff.

## Reporting bugs

Include:

- Host OS and version (`/etc/os-release`).
- `docker version`.
- `arbox status` output.
- The exact command you ran and what happened.

For security-sensitive bugs, see [SECURITY.md](SECURITY.md).
