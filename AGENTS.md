# AI Agent Coding Guidelines

This document provides guidelines for AI agents working on this project. You must adhere to these directions.

## RESPONSE STYLE

- Be ultra concise unless asked to be verbose.
- Favor short, digestible responses.
- Never use filler like "honestly" or "my opinion".
- Act like a smart robot without a personality.
- Keep context small by keeping responses short.
- Use bulleted or numbered lists, never inline lists in prose.
- In the absence of a specific request otherwise, end work responses in this exact shape:

  ```
  ●●●● RESPONSES ●●●●

  The answer / report of what was done.

  ●●●● NEXT STEP ●●●●

  (0) Foo:
    (0a) bullet 1
    (0b) bullet 2

   Slightly longer explanation

  ●●●● ALTERNATE ●●●●   (omit this whole section when there are no real alternatives)

  (1) foo bar:
    (1a) bullet 1
    (1b) bullet 2
    (1c) (question) question 1

  (2) ...
  ```

  Headers are four round dots on each side (●●●●), with a blank line after each. The RESPONSES area holds the answer or report. Item (0) is the next logical step, with as many lettered sub-points as needed to understand it. The ALTERNATE section is OPTIONAL: include it only when genuine, distinct alternatives to (0) actually exist and you have high confidence they are worth the developer's attention. Omit the header entirely — do not write "none" — when the next step is simply the right move, when the only candidates are trivial restatements or obvious follow-on work, or when you would be padding to fill the section. When it is present, give 1-5 alternates; every alternative must have 1-5 lettered sub-points. Items and sub-points use plain parenthesized labels — (0), (0a), (1), (1a) — never markdown list markers like `0.` (parens keep the renderer from collapsing the blank lines between items). Open questions for the developer continue the letter sequence with a `(question)` annotation after the label. A blank line separates each numbered item. The explanation under item (0) is as long as needed to fully understand it, but compact and to the point.

## PREREQUISITES

Before responding to ANY request about this codebase, read the project documentation first:

1. **FIRST**: [README.md](README.md) — the security model, mount model, every flag, and how the image is built. Most questions are answered there.
2. **THEN**: [CONTRIBUTING.md](CONTRIBUTING.md) for style, test expectations, and the module layout; [SECURITY.md](SECURITY.md) for what the sandbox does and does not defend against.
3. **ONLY IF** those don't answer the question, use `rg` / `fd` / `git grep` to search `src/`.

Do NOT start with grep searches. The README is the documentation index.

## PRIORITIZE ADJACENT MARKDOWN FILES

- When reading a file `{facet}.*`, see if there is a `{facet}.md` file in the same directory. If so, read it first. It contains the documentation for the file.
- `src/Dockerfile` and `src/launch.rs` carry their documentation in long leading comment blocks. Read the block above a section before editing that section.

## CREATE OR MODIFY CODE

- Match the existing module layout: `host` collects host facts, `git` resolves workspace paths, `image` builds and inspects Docker images, `launch` orchestrates container execution, `dockerfile`/`osrelease`/`passwd`/`path` stay small and focused.
- Anything that hands the container new host access (a device, a socket, a mount) MUST follow the existing opt-in pattern: a `--flag` in `src/main.rs`, a `*Access` struct in `src/launch.rs` with `detect_*` (pure, never errors), `require_*` (bails when the flag finds nothing), `apply(&mut Command)`, and `summary()`, a line in `arbox status` (`src/image.rs`), a `README.md` security-model bullet plus a flag section, and a test asserting the exact docker flags emitted. `AudioAccess` (`--voice`) and `SerialAccess` (`--serial`) are the reference shapes.
- Remember `--user uid:gid` drops supplementary groups: any 0660 device node needs `--group-add` for its owning gid or it is unopenable inside the container.
- Nothing is mounted or bound by default beyond what the README's security model lists. Widening the default is a security-posture change: stop and ask before doing it.
- If there is no existing pattern that matches, ask the human developers before implementing new infrastructure (parsers, file walkers, config formats, image-build mechanisms).
- Comments are for *why*: invariants, gotchas, why one of several reasonable approaches was chosen. No comments restating what the code does.
- Don't add error handling for situations that can't happen. Validate only at process boundaries: host filesystem, Docker output, git output, user input.
- When a code change makes `README.md`, `SECURITY.md`, a Dockerfile comment, or a doc comment stale, fix it in the same change.
- `cargo fmt` and `cargo clippy --all-targets -- -D warnings` are REQUIRED before reporting any code change as done. Run both bare and fix every warning; do not make manual formatting-only edits.

## CREATING TEMPORARY FILES AND SCRIPTS

- Place them in the root directory of the project in the `TEMP/*` folder (gitignored). Create `TEMP/` if missing.
- DO NOT USE `/tmp` because it is inaccessible outside of the container.
- Use `uv` + `python3` instead of bash for advanced operations.
- **Code reviews (`/review`, review skill, reviewer subagent):** always write review notes, summaries, diffs, and related scratch artifacts under `./TEMP/` (e.g. `TEMP/review-<id>.md`). Never use `/tmp`, `$TMPDIR`, or repo-root `TEMP-*` filenames for review output.

## NEVER USE `find`

- Use `fd`, `git ls-files`, `git grep`, `rg` instead.

## Error handling

- Errors use `anyhow`. Convert foreign errors with `.context(...)` / `.with_context(|| ...)` naming the offending input (a path, a flag, a command), not hand-rolled `map_err` + `anyhow!`.
- NEVER conceal a `Result` error by defaulting it away: `.unwrap_or_default()`, `.ok()`, `.unwrap_or(...)` on a fallible operation silently converts a failure into a wrong answer. Propagate with `?`; where a degraded fallback is genuinely intended (an optional mount that is missing, a detection probe on a host without the hardware), say why in a comment.
- No `.unwrap()` and no numeric `as` casts in new code. Pick types that don't need converting. `.unwrap_or(...)` / `.unwrap_or_else(...)` are fine.
- A flag the user passed explicitly (`--voice`, `--serial`, ...) that finds nothing to bind is a hard error before any image build, never a silent launch.

## Specific Instructions

- NEVER add brand-new external dependencies without explicit approval, but you may search out ideal dependencies to suggest. The dependency list in `Cargo.toml` is deliberately tiny.
- If a suitable dependency already exists in `Cargo.toml`, prefer reusing it over writing a custom parser or formatter.
- Do NOT hand-roll support for standard formats just to avoid asking for dependency approval.
- Never delete files or functions without explicit approval.
- Never look at `LOCAL.toml`, `LOCAL.env`, or `LOCAL.json` files.
- Never run a live `docker build` / `docker run` / `arbox <verb>` on your own initiative. Unit tests do not need Docker; end-to-end verification is the developer's to run.
- The Dockerfile is embedded in the binary from `src/Dockerfile`. Keep the Playwright browser layer (apt part 1 + browser download) untouched when adding packages: new apt packages go in the second apt layer so the ~700 MB browser layer stays cached.
- Downloaded toolchains in the Dockerfile are pinned versioned tarballs with a checksum or a pinned npm version. No `curl | sh` against a moving install script.
- The image is built from the host's Ubuntu codename on Linux. Do not assume a fixed codename in package names or paths.

## Naming Conventions

- **Rust**:
  - `snake_case` for variables and function names.
  - `CamelCase` for types (structs, enums, traits).
  - `ALL_CAPS` for constants.
- **CLI flags**: `--kebab-case`, global (`global = true`) when they apply to every launch verb, and paired `--mount-<name>` / `--no-mount-<name>` for state-mount overrides.
- **`kind` is forbidden as an identifier** (field, parameter, `*_kind` suffix): use `variant` for enum discriminators and a concrete domain word (`source`, `mode`, ...) everywhere else.

## Approved Commands

- `cargo check`: fast type check while iterating.
- `cargo test`: the unit suite. Runs without Docker.
- `cargo clippy --all-targets -- -D warnings`: REQUIRED after every code change. Stricter than `cargo check`; a green check does not guarantee clippy passes.
- `cargo fmt`: REQUIRED after every code change. `cargo fmt --check` is what CI runs.
- `cargo build --release`: final build before a PR.
- Run every build/check/test command bare. Never pipe it to `head`, `tail`, or `grep`; the full output must be visible.
- `rg <pattern>` for finding code. `fd <pattern>` / `git ls-files` for finding files.

## FORBIDDEN COMMANDS

- `find` → use `git ls-files` or `fd` instead.
- Any command that launches a container from this repo's own code (`arbox ...`, `cargo run -- ...`, `docker run ...`) unless the user explicitly asks.

## Tests

- Add tests for anything in `host.rs`, `git.rs`, `osrelease.rs`, `passwd.rs`, or `path.rs` that does parsing or path manipulation.
- Add tests for any change that expands the mount model, adds docker flags, or materially changes image tags. The docker argument list is the contract: assert it exactly, as the `serial_apply_*` and audio tests do.
- Don't add tests that require a live Docker daemon.

## Commit messages

- Imperative mood, ~70 chars or less for the subject.
- Body explains *why* only if it isn't obvious from the diff.
- Never commit until the developer has approved the message and file list.
