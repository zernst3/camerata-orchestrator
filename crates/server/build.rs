//! Build script: records the git commit this binary was built from as `CAMERATA_GIT_COMMIT`, so
//! every export can name the exact engine that produced it (README, `manifest.json`,
//! `json/inspection.json`, the PDF). Read in the crate through `option_env!`, so a build outside a git
//! checkout (a source tarball, a vendored copy) simply records no commit rather than failing.
//!
//! A release pipeline can set `CAMERATA_GIT_COMMIT` itself; that value wins over `git`.
//!
//! Also records `CAMERATA_GIT_DIRTY` (`1` when the tree had uncommitted changes, `0` when clean),
//! read through `option_env!`, reported on `/api/health` next to the commit and saved with every run
//! (a client inspection refuses to start on a dirty build, and a clean inspection is not continued
//! on a dirty one). The script reruns when a source file of this package or of a workspace crate it
//! depends on changes (the rule library lives inside `crates/rules`), so an edit made after a
//! clean build flips the flag at the next build, while an edit in a crate this package does not
//! use leaves it alone (a rerun recompiles the package). The lists below are checked against
//! `cargo metadata` by `crates/server/tests/build_watch_list.rs`.
//!
//! The app (`crates/ui`) includes this file by path as its own build script, so both binaries
//! name their build the same way (the app refuses to run against a server from another build).

use std::path::{Path, PathBuf};
use std::process::Command;

pub fn main() {
    println!("cargo:rerun-if-env-changed=CAMERATA_GIT_COMMIT");
    watch_sources();
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
    if let Some(status) = git(&["status", "--porcelain"]) {
        let dirty = if status.is_empty() { "0" } else { "1" };
        println!("cargo:rustc-env=CAMERATA_GIT_DIRTY={dirty}");
    }
    watch_head();
}

/// The workspace crates (folder names under `crates/`) the server's normal dependency graph
/// contains, itself included.
pub const SERVER_CRATES: &[&str] = &[
    "agent",
    "api-types",
    "app-core",
    "checks",
    "core",
    "fleet",
    "gateway",
    "intake",
    "liveness",
    "llm",
    "orchestrator",
    "persistence",
    "rules",
    "scaffold",
    "secrets",
    "server",
    "ui-core",
    "worktracker",
];

/// The same for the desktop app (it does not depend on the server crate).
pub const UI_CRATES: &[&str] = &["api-types", "ui", "ui-core", "worktracker"];

/// The crate folders whose sources can change what package `pkg` is built from; `None` for a
/// package this build script does not know, which then watches all of `crates/`.
pub fn watched_crates(pkg: &str) -> Option<&'static [&'static str]> {
    match pkg {
        "camerata-server" => Some(SERVER_CRATES),
        "camerata-ui" => Some(UI_CRATES),
        _ => None,
    }
}

/// Rerun when a source of this build's own crates changes, so the dirty flag follows edits made
/// after a clean build. Paths are relative to the package folder (`crates/<name>`).
fn watch_sources() {
    let pkg = std::env::var("CARGO_PKG_NAME").unwrap_or_default();
    match watched_crates(&pkg) {
        Some(crates) => {
            for c in crates {
                println!("cargo:rerun-if-changed=../{c}");
            }
        }
        None => println!("cargo:rerun-if-changed=.."),
    }
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
///   watched instead, plus the ref's own directory (or its nearest existing ancestor down to
///   `refs/heads`) so the loose file a new commit creates is seen.
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
            // The ref's own directory, or when git has removed it (a packed `feat/x` after
            // `git gc` leaves no empty `refs/heads/feat`) the nearest ancestor that still exists,
            // stopping at `refs/heads`: the next commit recreates the loose ref there.
            let stop = common_dir.join("refs").join("heads");
            let mut dir = loose.parent();
            while let Some(d) = dir {
                if exists(d) {
                    candidates.push(d.to_path_buf());
                    break;
                }
                if d == stop {
                    break;
                }
                dir = d.parent();
            }
        }
    }
    candidates.into_iter().filter(|p| exists(p)).collect()
}
