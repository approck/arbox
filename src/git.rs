use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Resolve the git toplevel for `dir`. Errors if `dir` isn't inside a git repo.
pub fn toplevel(dir: &Path) -> Result<PathBuf> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("running `git rev-parse --show-toplevel`")?;
    if !out.status.success() {
        bail!(
            "arbox must be run inside a git repository (cwd: {})",
            dir.display()
        );
    }
    let s = String::from_utf8(out.stdout)
        .context("git toplevel is not utf-8")?
        .trim()
        .to_string();
    Ok(PathBuf::from(s).canonicalize()?)
}

/// Resolve the **common** git dir — the one that holds objects, refs, etc.
///
/// In a normal checkout this is just `<toplevel>/.git`. In a git **worktree**
/// the workspace's `.git` is a *file* containing `gitdir: <main>/.git/worktrees/<name>`,
/// and the common dir is `<main>/.git/`, which lives outside the workspace.
/// `git rev-parse --git-common-dir` resolves either case authoritatively.
pub fn common_dir(dir: &Path) -> Result<PathBuf> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--git-common-dir"])
        .output()
        .context("running `git rev-parse --git-common-dir`")?;
    if !out.status.success() {
        bail!(
            "git rev-parse --git-common-dir failed in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let s = String::from_utf8(out.stdout)
        .context("git common dir output is not utf-8")?
        .trim()
        .to_string();
    let p = PathBuf::from(&s);
    let abs = if p.is_absolute() { p } else { dir.join(p) };
    abs.canonicalize()
        .with_context(|| format!("canonicalizing git common dir `{s}` from {}", dir.display()))
}
