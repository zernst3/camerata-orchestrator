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

/// Rebuild when the commit this build names can have changed, and not otherwise. Watches HEAD
/// itself (a branch switch, or a detached checkout moving) and the ONE ref file HEAD points at
/// (a new commit on the current branch), never the whole `refs/heads` tree, so a commit on an
/// unrelated branch does not rebuild this crate. Linked checkouts keep HEAD in their own git dir
/// and branch refs in the common dir, so both are asked for. Only existing paths are watched:
/// cargo treats a missing one as changed on every build.
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
    let head = std::fs::read_to_string(git_dir.join("HEAD")).unwrap_or_default();
    for path in watched_paths(&git_dir, &common_dir, &head, &|p: &Path| p.exists()) {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

/// The paths whose change can move the commit `HEAD` names. `head` is the text of
/// `<git_dir>/HEAD`; `exists` says whether a path is on disk (injected so the choice is testable
/// without a repository).
///
/// - `HEAD` always.
/// - A symbolic `HEAD` (`ref: refs/heads/main`) adds the one ref file it names, from the common
///   dir. When that ref exists only in `packed-refs` (no loose file yet), `packed-refs` is
///   watched instead, plus the ref's own directory so the loose file a new commit creates is
///   seen.
/// - A detached `HEAD` holds the SHA itself, so `HEAD` alone is enough.
///
/// Only paths that exist are returned.
pub fn watched_paths(
    git_dir: &Path,
    common_dir: &Path,
    head: &str,
    exists: &dyn Fn(&Path) -> bool,
) -> Vec<PathBuf> {
    let mut candidates = vec![git_dir.join("HEAD")];
    if let Some(reference) = head.trim().strip_prefix("ref:").map(str::trim) {
        let loose = common_dir.join(reference);
        if exists(&loose) {
            candidates.push(loose);
        } else {
            candidates.push(common_dir.join("packed-refs"));
            if let Some(dir) = loose.parent() {
                candidates.push(dir.to_path_buf());
            }
        }
    }
    candidates.into_iter().filter(|p| exists(p)).collect()
}
