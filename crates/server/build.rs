//! Build script: records the git commit this binary was built from as `CAMERATA_GIT_COMMIT`, so
//! every export can name the exact engine that produced it (README, `manifest.json`,
//! `findings.json`, the PDF). Read in the crate through `option_env!`, so a build outside a git
//! checkout (a source tarball, a vendored copy) simply records no commit rather than failing.
//!
//! A release pipeline can set `CAMERATA_GIT_COMMIT` itself; that value wins over `git`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=CAMERATA_GIT_COMMIT");
    if let Ok(commit) = std::env::var("CAMERATA_GIT_COMMIT") {
        if is_commit(&commit) {
            println!("cargo:rustc-env=CAMERATA_GIT_COMMIT={commit}");
        }
        return;
    }
    let Some(commit) = git(&["rev-parse", "HEAD"]).filter(|c| is_commit(c)) else {
        return;
    };
    println!("cargo:rustc-env=CAMERATA_GIT_COMMIT={commit}");
    watch_head();
}

/// A full 40-character lowercase hex SHA-1 (or 64 for SHA-256 repositories). Anything else is
/// not recorded: a wrong commit is worse than none.
fn is_commit(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64)
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    Some(text.trim().to_string())
}

/// Rebuild when HEAD moves: on a branch switch (HEAD itself), on a new commit to a branch (a
/// file under `refs/heads`, watched as a directory so a ref that is created by the commit is
/// seen too), and when refs are packed. Worktrees keep HEAD in their own git dir and refs in the
/// common dir, so both are asked for. Only existing paths are watched: cargo treats a missing
/// one as changed on every build.
fn watch_head() {
    let (Some(git_dir), Some(common_dir)) = (
        git(&["rev-parse", "--absolute-git-dir"]).map(PathBuf::from),
        git(&["rev-parse", "--git-common-dir"]).map(PathBuf::from),
    ) else {
        return;
    };
    let common_dir = if common_dir.is_absolute() {
        common_dir
    } else {
        std::env::current_dir().map_or(common_dir.clone(), |cwd| cwd.join(common_dir))
    };
    for path in [
        git_dir.join("HEAD"),
        common_dir.join("packed-refs"),
        common_dir.join("refs").join("heads"),
    ] {
        rerun_if_changed(&path);
    }
}

fn rerun_if_changed(path: &Path) {
    if path.exists() {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}
