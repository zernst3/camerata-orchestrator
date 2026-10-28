//! Real-git fixtures shared by the unit tests and the integration tests of this crate.
//!
//! Source deletion, re-fetch, the delta rescan and the finding-context reader all drive the
//! system `git`, and their tests need real repositories: a working clone, a bare "remote" it
//! pushes to (reached by a `file://` URL, so nothing touches the network), commits, branches,
//! dirty trees. This module is the ONE place those are built.
//!
//! It is compiled into the library (hidden from the docs) rather than `#[cfg(test)]`, because
//! integration tests under `tests/` link the library built WITHOUT `cfg(test)`. Nothing in the
//! product calls it. Every helper takes a directory the CALLER owns (a `TempDir`); none of them
//! ever writes outside it, and none reads or changes the user's git configuration (commits are
//! made with explicit `-c user.*` identity, so a machine with no global identity works).

use std::path::{Path, PathBuf};
use std::process::Command;

/// Run `git <args>` in `dir`, panicking with git's stderr when it fails. Returns trimmed stdout.
pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args([
            "-c",
            "user.name=Camerata Test",
            "-c",
            "user.email=test@camerata.local",
        ])
        .args([
            "-c",
            "init.defaultBranch=main",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .expect("git is installed");
    assert!(
        out.status.success(),
        "git {args:?} failed in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Write `contents` to `dir/rel`, creating parent directories.
pub fn write_file(dir: &Path, rel: &str, contents: &str) {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent dir");
    }
    std::fs::write(path, contents).expect("write fixture file");
}

/// Stage everything and commit it; returns the new commit's full sha.
pub fn commit_all(dir: &Path, message: &str) -> String {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", message]);
    git(dir, &["rev-parse", "HEAD"])
}

/// `git init` on branch `main` in `dir` (created if missing) with one commit containing
/// `files` (`(relative path, contents)`). Returns the commit sha. No remote.
pub fn init_repo(dir: &Path, files: &[(&str, &str)]) -> String {
    std::fs::create_dir_all(dir).expect("create repo dir");
    git(dir, &["init", "-q", "-b", "main"]);
    for (rel, contents) in files {
        write_file(dir, rel, contents);
    }
    commit_all(dir, "initial")
}

/// Like [`init_repo`] but with `origin` pointing at `origin_url` (never contacted).
pub fn init_repo_with_origin(dir: &Path, origin_url: &str, files: &[(&str, &str)]) -> String {
    let sha = init_repo(dir, files);
    git(dir, &["remote", "add", "origin", origin_url]);
    sha
}

/// A bare repository at `base/remote.git` seeded with `files` on `main` (through a throwaway
/// working clone at `base/seed`). Returns `(bare_path, file_url, head_sha)`. The returned
/// `file://` URL is what a clone/fetch under test is pointed at.
pub fn bare_remote(base: &Path, files: &[(&str, &str)]) -> (PathBuf, String, String) {
    let bare = base.join("remote.git");
    std::fs::create_dir_all(&bare).expect("create bare dir");
    git(&bare, &["init", "-q", "--bare", "-b", "main"]);
    let seed = base.join("seed");
    let sha = init_repo(&seed, files);
    git(&seed, &["remote", "add", "origin", &bare.to_string_lossy()]);
    git(&seed, &["push", "-q", "origin", "main"]);
    let url = format!("file://{}", bare.display());
    (bare, url, sha)
}

/// Push another commit to the bare remote created by [`bare_remote`] (through `base/seed`).
/// Returns the new head sha.
pub fn push_commit(base: &Path, rel: &str, contents: &str, message: &str) -> String {
    let seed = base.join("seed");
    write_file(&seed, rel, contents);
    let sha = commit_all(&seed, message);
    git(&seed, &["push", "-q", "origin", "main"]);
    sha
}

/// Clone `url` into `dest` and rewrite `origin` to `origin_url` afterwards, so the working
/// copy LOOKS like a clone of a GitHub repository (`https://github.com/acme/app.git`) while its
/// objects came from the local bare remote. Returns the checked-out head sha.
pub fn clone_as(url: &str, dest: &Path, origin_url: &str) -> String {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).expect("create clone parent");
    }
    let parent = dest.parent().expect("dest has a parent");
    git(parent, &["clone", "-q", url, &dest.to_string_lossy()]);
    git(dest, &["remote", "set-url", "origin", origin_url]);
    git(dest, &["rev-parse", "HEAD"])
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn fixtures_build_a_remote_a_clone_and_a_newer_commit() {
        let tmp = TempDir::new().unwrap();
        let (_bare, url, first) = bare_remote(tmp.path(), &[("a.txt", "one\n")]);
        let clone = tmp.path().join("clone");
        let head = clone_as(&url, &clone, "https://github.com/acme/app.git");
        assert_eq!(head, first);
        assert_eq!(
            git(&clone, &["config", "--get", "remote.origin.url"]),
            "https://github.com/acme/app.git"
        );
        let second = push_commit(tmp.path(), "b.txt", "two\n", "second");
        assert_ne!(first, second);
    }
}
