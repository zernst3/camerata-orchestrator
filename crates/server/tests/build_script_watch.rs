//! H4: the build script that records the engine commit rebuilds the crate when the commit can
//! have changed, and not on an unrelated commit. It watches HEAD and the one ref HEAD names,
//! never the whole `refs/heads` tree. The script's pure path choice is compiled into this test.

#[path = "../build.rs"]
#[allow(dead_code)]
mod build_script;

use std::fs;
use std::path::Path;

fn on_disk(p: &Path) -> bool {
    p.exists()
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

#[test]
fn a_checkout_on_a_branch_watches_head_and_that_branchs_ref_only() {
    let dir = tempfile::tempdir().unwrap();
    let git = dir.path().join(".git");
    write(&git.join("HEAD"), "ref: refs/heads/feat/export\n");
    write(&git.join("refs/heads/feat/export"), "aaaa\n");
    write(&git.join("refs/heads/main"), "bbbb\n");
    write(&git.join("refs/heads/other/topic"), "cccc\n");
    write(&git.join("packed-refs"), "# pack-refs\n");

    let watched =
        build_script::watched_paths(&git, &git, "ref: refs/heads/feat/export\n", &on_disk);
    assert_eq!(
        watched,
        vec![git.join("HEAD"), git.join("refs/heads/feat/export")]
    );
    // Not the heads directory, and not the other branches' files.
    assert!(!watched.contains(&git.join("refs/heads")));
    assert!(!watched.contains(&git.join("refs/heads/main")));
}

#[test]
fn a_linked_checkout_reads_head_from_its_own_dir_and_the_ref_from_the_common_dir() {
    let dir = tempfile::tempdir().unwrap();
    let common = dir.path().join("repo/.git");
    let git = common.join("worktrees/agent");
    write(&git.join("HEAD"), "ref: refs/heads/work\n");
    write(&common.join("refs/heads/work"), "aaaa\n");
    let watched = build_script::watched_paths(&git, &common, "ref: refs/heads/work\n", &on_disk);
    assert_eq!(
        watched,
        vec![git.join("HEAD"), common.join("refs/heads/work")]
    );
}

#[test]
fn a_detached_head_watches_head_alone() {
    let dir = tempfile::tempdir().unwrap();
    let git = dir.path().join(".git");
    let sha = "0123456789abcdef0123456789abcdef01234567\n";
    write(&git.join("HEAD"), sha);
    write(&git.join("refs/heads/main"), "bbbb\n");
    write(&git.join("packed-refs"), "# pack-refs\n");
    let watched = build_script::watched_paths(&git, &git, sha, &on_disk);
    assert_eq!(watched, vec![git.join("HEAD")]);
}

#[test]
fn a_packed_only_branch_watches_packed_refs_and_its_own_directory() {
    let dir = tempfile::tempdir().unwrap();
    let git = dir.path().join(".git");
    write(&git.join("HEAD"), "ref: refs/heads/feat/export\n");
    write(&git.join("packed-refs"), "aaaa refs/heads/feat/export\n");
    fs::create_dir_all(git.join("refs/heads/feat")).unwrap();
    let watched =
        build_script::watched_paths(&git, &git, "ref: refs/heads/feat/export\n", &on_disk);
    assert_eq!(
        watched,
        vec![
            git.join("HEAD"),
            git.join("packed-refs"),
            git.join("refs/heads/feat")
        ]
    );
}

#[test]
fn a_path_that_does_not_exist_is_never_watched() {
    let dir = tempfile::tempdir().unwrap();
    let git = dir.path().join(".git");
    write(&git.join("HEAD"), "ref: refs/heads/gone\n");
    let watched = build_script::watched_paths(&git, &git, "ref: refs/heads/gone\n", &on_disk);
    assert_eq!(watched, vec![git.join("HEAD")]);
}
