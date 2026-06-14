# PLAN — Windows support follow-ups

Tracking the remaining fixes surfaced by the code review of the windows-support
work (`git diff origin/master HEAD` as of the `feat: add windows support` merge).

## Status legend
- ✅ done / committed
- ⬜ not started
- 🟡 reviewable on Linux, but only fully verifiable on a Windows + Docker Desktop host

---

## Done

- ✅ **Linux staleness regression** (`c5f79c2`) — restored PATH ordering so
  `/usr/local/bin` precedes `${HOST_HOME}/.local/bin`; stopped mounting
  `~/.local/bin` and `~/.local/share/claude` (they hold the host's own
  claude/codex/agy binaries and were shadowing the baked-in ones); dropped the
  unused `~/.approck` mount; mounted `~/.local/share/approck` read-only.
- ✅ **Phase 1 — image tag + path translation** (`416bb2d`)
  - Platform segment (`win`/`nix`) in `image::tag_prefix` so Windows and Linux
    builds can't alias on a shared Docker Desktop/WSL2 daemon.
  - `path::to_container` + `path::to_mount_src` helpers (identity on Unix);
    removed the inline `cfg!(windows)` branches and duplicated `\\?\` strip.
  - `to_mount_src` emits forward slashes for `docker --mount`; UNC paths
    rejected with a clear error; `C:foo` → `/mnt/c/foo` (was `/mnt/cfoo`).
  - Pure unit tests for the conversions (run on any platform).
  - Guarded the `.cargo`/`.rustup` profile-test assertions for Windows builds.

---

## Phase 2 — Windows worktree correctness (highest remaining value)

The single most important remaining fix: today arbox **permanently rewrites the
user's worktree `.git` file** and **leaks `git config safe.directory` entries**.

### 2a. ⬜🟡 Restore the worktree `.git` file on exit (review finding #2)
**Where:** `src/launch.rs` — `fixup_windows_worktree` + the cleanup block in `run()`.

**Problem:** `fixup_windows_worktree` overwrites the worktree's `.git`
(`gitdir: <absolute windows path>` → relative) and never restores it. The
README and the function doc-comment both promise transparent cleanup.

**Approach:** replace the `Option<String>` return with an RAII guard.

```rust
struct WorktreeFixup {
    git_file: PathBuf,
    original: String,   // original .git contents, restored on drop
    safe_dir: String,   // container path added to safe.directory
}

impl Drop for WorktreeFixup {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.git_file, &self.original);
        let _ = Command::new("git")
            .args(["config", "--global", "--unset-all", "safe.directory", &self.safe_dir])
            .status();
    }
}
```

`fixup_windows_worktree` returns `Result<Option<WorktreeFixup>>`; `run()` binds
it (`let _fixup = fixup_windows_worktree(&host)?;`) so Drop runs on normal exit
**and on the early `?`-returns after it** (see 2b).

**Residual:** Drop does not run on `SIGKILL` / hard Ctrl-C. Document that in the
function comment and keep the README's manual-cleanup snippet as the escape
hatch.

### 2b. ⬜🟡 Stop leaking / double-adding `safe.directory` (review finding #3)
**Where:** same function + the post-`docker run` cleanup block.

**Problems:**
1. `--add` runs on every launch (duplicates accumulate).
2. Cleanup uses `--unset` (singular) — errors with "multiple values" once
   duplicated, so cleanup then removes nothing, permanently.
3. The current cleanup lives *after* `cmd.status()`, so any early `?`-return
   between `fixup_windows_worktree` and there (e.g. `verify_required_mounts_exist`,
   `image::ensure_built`, a failed `docker run`) leaks the entry.

**Fix:**
- Before adding, check `git config --global --get-all safe.directory` and skip
  if the path is already present.
- Use `--unset-all` (not `--unset`) on removal.
- Move teardown into the `Drop` from 2a so it can't be skipped by an early
  return.

**Verification:** compiles + clippy on Linux; behavior needs a Windows host with
a real git worktree. Manual check: `git config --global --get-all safe.directory`
before/after a launch, and after a deliberately-failed launch (e.g. break the
build) to confirm no leak.

---

## Phase 3 — Windows polish (low priority, defer until Windows testing)

### 3a. ⬜ `arbox status` shows untranslated paths on Windows (finding #11)
**Where:** `src/image.rs` — `print_status`.
Now trivial with the Phase 1 helpers: render the mount source/dest through
`path::to_mount_src` / `path::to_container` so `status` matches what `run()`
actually passes to Docker. Keep the host-form columns too if useful; just don't
let `status` claim `C:\...` when the container sees `/mnt/c/...`.

### 3b. ⬜🟡 `GetUserNameW` never retries on a too-small buffer (finding #8)
**Where:** `src/passwd/windows.rs` — `get_username`.
On a `0` return, check `GetLastError() == ERROR_INSUFFICIENT_BUFFER` and retry
once using the `size` the call wrote back, instead of failing outright. Low
likelihood (needs a >255-char account name) but cheap to make correct.

### 3c. ⬜🟡 Dockerfile `USER`/`WORKDIR` reference an uncreated user on Windows (finding #9)
**Where:** `src/Dockerfile` (lines ~242–243) + `src/image.rs` build-args.
On a Windows build the user-creation `RUN` is skipped, so `USER ${HOST_USER}` /
`WORKDIR ${HOST_HOME}` point at a user/home that doesn't exist. Benign today
(arbox always passes `--user 1000:1000` and `--workdir`), so this is defensive.
Option: pass a `RUNTIME_USER` build-arg that is the host user on Linux and
`ubuntu` (uid 1000 in noble) on Windows; use it for `USER`/`WORKDIR`.

### 3d. ⬜🟡 Cross-drive worktree silently breaks (finding #6/#8)
**Where:** `src/launch.rs` — `fixup_windows_worktree`.
When the worktree and its git common dir are on different drives,
`pathdiff::diff_paths` returns `None` and the fixup silently no-ops, leaving an
absolute Windows `gitdir:` that won't resolve in the container. At minimum
`log()`/eprintln a clear warning instead of silently doing nothing.

---

## To confirm (not a bug, needs a product decision)

### ⬜ `ANTHROPIC_API_KEY` passthrough scope
**Where:** `src/launch.rs` — `run()` (the `if let Ok(key) = std::env::var(...)`).
The windows PR forwards the host's `ANTHROPIC_API_KEY` into **every** verb
(bash/grok/codex/agy), on all platforms. If that's intended, leave it. If the
key should only reach claude, gate it (or drop it and rely on mounted
credentials). One-line change either way.

---

## Suggested order
1. Phase 2 (2a + 2b together — one RAII guard covers both). Highest value:
   stops mutating users' repos and polluting global gitconfig.
2. Phase 3a (`print_status`) — quick win now that the helpers exist.
3. Phase 3b–3d — batch when someone can actually run on Windows.
4. Resolve the `ANTHROPIC_API_KEY` question.

All Phase 2/3 code is build-/clippy-/test-clean-able on Linux; items marked 🟡
need a Windows + Docker Desktop host for behavioral verification.
