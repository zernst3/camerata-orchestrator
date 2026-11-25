//! The one time move of saved runs from the cache folder to the app data folder. Every failure
//! is injected through [`Ops`], so each branch runs on any machine (root included, where file
//! modes cannot make a write fail).
#![allow(clippy::unwrap_used)]

use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::*;

/// The real file operations, with failures switched on per test.
#[derive(Default)]
struct Faulty {
    /// Fail every rename whose source is inside this folder (a different volume, say).
    fail_rename_from: Option<PathBuf>,
    /// Fail the rename of exactly this source.
    fail_rename_of: Option<PathBuf>,
    /// Fail the copy of the Nth file (1 based).
    fail_copy_at: Option<usize>,
    copies: Cell<usize>,
}

impl Ops for Faulty {
    fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()> {
        let blocked = self
            .fail_rename_from
            .as_ref()
            .is_some_and(|d| from.starts_with(d))
            || self.fail_rename_of.as_deref() == Some(from);
        if blocked {
            return Err(std::io::Error::other("injected rename failure"));
        }
        std::fs::rename(from, to)
    }
    fn copy_file(&self, from: &Path, to: &Path) -> std::io::Result<u64> {
        let n = self.copies.get() + 1;
        self.copies.set(n);
        if self.fail_copy_at == Some(n) {
            return Err(std::io::Error::other("injected copy failure"));
        }
        std::fs::copy(from, to)
    }
}

struct Fixture {
    _root: tempfile::TempDir,
    old: PathBuf,
    new: PathBuf,
}

fn write(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

/// An old cache folder as a released build leaves it, and a new folder that already holds the
/// cockpit's own data (it is the same folder as the app data root).
fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let old = root.path().join("cache").join("camerata");
    let new = root.path().join("data").join("camerata");
    write(&old.join("runs/r-1/run.json"), r#"{"run_id":"r-1"}"#);
    write(&old.join("runs/r-1/calls/abc.json"), "[1,2,3]");
    write(&old.join("runs/r-2/run.json"), r#"{"run_id":"r-2"}"#);
    write(&old.join("web-0011aabb.cache.json"), "cache");
    write(&old.join("web-0011aabb.ledger.json"), "ledger");
    write(&old.join("project-2-deadbeef.repos.json"), "[]");
    write(&old.join("finder-traces/t.json"), "trace");
    write(&old.join("tmp/system-prompts/1-0-1/prompt.txt"), "prompt");
    write(&new.join("projects.json"), "[]");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(old.join("runs/r-1"), std::fs::Permissions::from_mode(0o700))
            .unwrap();
    }
    Fixture {
        _root: root,
        old,
        new,
    }
}

/// Every file and folder under `root` with its bytes, Camerata's own lock files left out.
fn snapshot(root: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Option<Vec<u8>>>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(".state-") {
                continue;
            }
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if path.is_dir() {
                out.insert(rel, None);
                walk(root, &path, out);
            } else {
                out.insert(rel, Some(std::fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

const MOVED: [&str; 6] = [
    "runs/r-1/run.json",
    "runs/r-1/calls/abc.json",
    "runs/r-2/run.json",
    "web-0011aabb.cache.json",
    "web-0011aabb.ledger.json",
    "project-2-deadbeef.repos.json",
];

fn assert_moved(fx: &Fixture, before: &BTreeMap<String, Option<Vec<u8>>>) {
    let after = snapshot(&fx.new);
    for rel in MOVED.iter().chain(["finder-traces/t.json"].iter()) {
        assert_eq!(
            after.get(*rel),
            before.get(*rel),
            "{rel} arrived in the new folder with the same bytes"
        );
        assert!(!fx.old.join(rel).exists(), "{rel} left the old folder");
    }
    assert_eq!(
        std::fs::read_to_string(fx.new.join("projects.json")).unwrap(),
        "[]",
        "the data already in the new folder is untouched"
    );
    assert!(
        fx.old.join("tmp/system-prompts/1-0-1/prompt.txt").exists(),
        "the disposable per call files stay in the cache folder"
    );
    assert!(!fx.new.join("tmp").exists());
    assert!(
        !fx.new.join(".state-migration-staging").exists(),
        "no staging folder is left behind"
    );
}

#[test]
fn runs_and_bookkeeping_move_by_rename() {
    let fx = fixture();
    let before = snapshot(&fx.old);
    let out = migrate(&fx.old, &fx.new);
    match &out {
        MigrationOutcome::Moved {
            method, leftovers, ..
        } => {
            assert_eq!(*method, MoveMethod::Renamed);
            assert!(leftovers.is_empty(), "{leftovers:?}");
        }
        other => panic!("{other:?}"),
    }
    assert_moved(&fx, &before);
}

/// Two volumes (a rename that cannot cross them): copy, verify, then remove the old copy.
#[test]
fn a_rename_that_fails_falls_back_to_copy_then_verify() {
    let fx = fixture();
    let before = snapshot(&fx.old);
    let ops = Faulty {
        fail_rename_from: Some(fx.old.clone()),
        ..Faulty::default()
    };
    let out = migrate_with(&fx.old, &fx.new, &ops);
    match &out {
        MigrationOutcome::Moved {
            method, leftovers, ..
        } => {
            assert_eq!(*method, MoveMethod::Copied);
            assert!(leftovers.is_empty(), "{leftovers:?}");
        }
        other => panic!("{other:?}"),
    }
    assert_moved(&fx, &before);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(fx.new.join("runs/r-1"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o700,
            "a run folder stays private after a copy"
        );
    }
}

fn assert_old_intact(fx: &Fixture, before: &BTreeMap<String, Option<Vec<u8>>>) {
    assert_eq!(
        &snapshot(&fx.old),
        before,
        "the old folder is exactly as it was"
    );
    assert!(
        !fx.new.join("runs").exists(),
        "the new folder holds no runs, so the next start tries again"
    );
    assert!(!fx.new.join("web-0011aabb.cache.json").exists());
    assert!(!fx.new.join(".state-migration-staging").exists());
}

#[test]
fn a_failed_copy_leaves_the_old_folder_intact_and_keeps_it() {
    let fx = fixture();
    let before = snapshot(&fx.old);
    let ops = Faulty {
        fail_rename_from: Some(fx.old.clone()),
        fail_copy_at: Some(3),
        ..Faulty::default()
    };
    let out = migrate_with(&fx.old, &fx.new, &ops);
    assert!(matches!(out, MigrationOutcome::KeptOld { .. }), "{out:?}");
    assert_old_intact(&fx, &before);
}

/// The runs folder is renamed last; when it fails, every entry renamed before it goes back
/// before the copy is tried, and when the copy fails too nothing has moved.
#[test]
fn a_rename_that_fails_part_way_is_put_back_before_anything_else() {
    let fx = fixture();
    let before = snapshot(&fx.old);
    let ops = Faulty {
        fail_rename_of: Some(fx.old.join("runs")),
        fail_copy_at: Some(1),
        ..Faulty::default()
    };
    let out = migrate_with(&fx.old, &fx.new, &ops);
    assert!(matches!(out, MigrationOutcome::KeptOld { .. }), "{out:?}");
    assert_old_intact(&fx, &before);
}

#[test]
fn nothing_moves_when_the_new_folder_already_has_runs() {
    let fx = fixture();
    write(&fx.new.join("runs/r-9/run.json"), r#"{"run_id":"r-9"}"#);
    let (old_before, new_before) = (snapshot(&fx.old), snapshot(&fx.new));
    let out = migrate(&fx.old, &fx.new);
    assert_eq!(
        out,
        MigrationOutcome::NewAlreadyHasRuns { old_has_runs: true }
    );
    assert_eq!(snapshot(&fx.old), old_before);
    assert_eq!(snapshot(&fx.new), new_before);
}

#[test]
fn nothing_moves_when_the_old_folder_is_missing_or_holds_only_disposable_files() {
    let root = tempfile::tempdir().unwrap();
    let old = root.path().join("cache/camerata");
    let new = root.path().join("data/camerata");
    assert_eq!(migrate(&old, &new), MigrationOutcome::NothingToMove);
    write(&old.join("tmp/system-prompts/x/prompt.txt"), "p");
    assert_eq!(migrate(&old, &new), MigrationOutcome::NothingToMove);
    assert!(old.join("tmp/system-prompts/x/prompt.txt").exists());
}

/// A run another process is working on right now (an earlier build, say) is not moved out from
/// under it.
#[test]
fn a_run_in_use_is_not_moved() {
    let fx = fixture();
    let lock_path = fx.old.join("runs/r-2/lock.json");
    write(&lock_path, "{}");
    let held = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    held.try_lock().unwrap();
    let before = snapshot(&fx.old);
    let out = migrate(&fx.old, &fx.new);
    assert!(matches!(out, MigrationOutcome::KeptOld { .. }), "{out:?}");
    assert_old_intact(&fx, &before);
    drop(held);
}

#[test]
fn a_name_already_taken_in_the_new_folder_stops_the_move_before_it_starts() {
    let fx = fixture();
    write(&fx.new.join("web-0011aabb.ledger.json"), "someone else's");
    let before = snapshot(&fx.old);
    let out = migrate(&fx.old, &fx.new);
    assert!(matches!(out, MigrationOutcome::KeptOld { .. }), "{out:?}");
    assert_eq!(snapshot(&fx.old), before);
    assert_eq!(
        std::fs::read_to_string(fx.new.join("web-0011aabb.ledger.json")).unwrap(),
        "someone else's"
    );
    assert!(!fx.new.join("runs").exists());
}

/// An empty `runs` folder in the new location (created by a listing, say) holds no runs and does
/// not block the move.
#[test]
fn an_empty_runs_folder_in_the_new_location_does_not_block_the_move() {
    let fx = fixture();
    std::fs::create_dir_all(fx.new.join("runs")).unwrap();
    let before = snapshot(&fx.old);
    let out = migrate(&fx.old, &fx.new);
    assert!(matches!(out, MigrationOutcome::Moved { .. }), "{out:?}");
    assert_moved(&fx, &before);
}
