//! The one time move of an earlier version's state from the cache folder to the app data folder
//! (issue 342). See the parent module for the two folders.
//!
//! # What moves
//!
//! Every top level entry of the old folder except the disposable per-call files (`tmp/`) and
//! Camerata's own lock files: the saved runs (`runs/`), the CLI's scan caches and ledgers
//! (`<key>.cache.json`, `<key>.ledger.json`), the project lists beside them
//! (`project-<n>-<hash>.repos.json`) and the agent finder's rooms and traces kept outside a run.
//! They travel together because source deletion finds a checkout's files under ONE folder: a
//! cache left behind would hold client findings no deletion looks at. An entry this version
//! does not know moves too, for the same reason.
//!
//! # When
//!
//! Only when the new folder has no saved runs and the old folder has something to move, and
//! never while `CAMERATA_STATE_DIR` is set (the caller does not get here then).
//!
//! # How, and what a failure leaves
//!
//! 1. Refuse before touching anything when a run in the old folder is in use (its lock is held),
//!    when another Camerata process is still using the old folder, or when a name to move is
//!    already taken in the new folder.
//! 2. Rename each entry, `runs/` last. A rename that fails puts back every entry renamed before
//!    it, then the copy is tried.
//! 3. Copy into a staging folder inside the new folder, verify every file byte for byte, rename
//!    the staged entries into place (`runs/` last), and only then remove the old copies.
//!
//! Any failure before the old copies are removed leaves the old folder exactly as it was, and the
//! caller keeps using it. Two processes starting together are serialized by a lock in the new
//! folder; a process that keeps the old folder holds a shared lock on it for its lifetime, so no
//! later move pulls the folder out from under it.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

/// Serializes migrations between processes. Lives in the new folder.
const MIGRATION_LOCK: &str = ".state-migration.lock";
/// Held shared by every process that keeps the old folder, exclusive by a migration.
const IN_USE_LOCK: &str = ".state-dir-in-use.lock";
/// Where the copy fallback stages its copy, inside the new folder.
const STAGING: &str = ".state-migration-staging";
/// The disposable per-call files, left in the cache folder.
const DISPOSABLE: &str = "tmp";
const RUNS: &str = "runs";

/// How the entries reached the new folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveMethod {
    Renamed,
    /// Copied, verified byte for byte, then the old copy removed.
    Copied,
}

/// What [`migrate`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationOutcome {
    /// The old folder holds nothing to move. The new folder is used.
    NothingToMove,
    /// The new folder already has saved runs, so nothing moved. The new folder is used.
    NewAlreadyHasRuns { old_has_runs: bool },
    /// The entries moved. The new folder is used. `leftovers` are old copies that verified in
    /// the new folder but could not be removed from the old one.
    Moved {
        method: MoveMethod,
        entries: Vec<String>,
        leftovers: Vec<String>,
    },
    /// Nothing moved; the old folder is as it was and stays in use.
    KeptOld { reason: String },
}

impl MigrationOutcome {
    /// Whether the caller keeps using the old folder.
    pub fn keeps_old(&self) -> bool {
        matches!(self, Self::KeptOld { .. })
    }

    /// The one line the operator sees about the move, if there is anything to say.
    pub fn log_line(&self, old: &Path, new: &Path) -> Option<String> {
        let (old, new) = (old.display(), new.display());
        match self {
            Self::NothingToMove
            | Self::NewAlreadyHasRuns {
                old_has_runs: false,
            } => None,
            Self::NewAlreadyHasRuns { old_has_runs: true } => Some(format!(
                "[camerata] {old} still holds saved runs from an earlier version. They were not \
                 moved because {new} already has saved runs."
            )),
            Self::Moved {
                method, leftovers, ..
            } => {
                let how = match method {
                    MoveMethod::Renamed => "renamed",
                    MoveMethod::Copied => "copied and verified",
                };
                let rest = if leftovers.is_empty() {
                    String::new()
                } else {
                    format!(
                        " These old copies could not be removed and are still there: {}.",
                        leftovers.join(" ")
                    )
                };
                Some(format!(
                    "[camerata] Moved saved runs and inspection state from {old} to {new} \
                     ({how}).{rest}"
                ))
            }
            Self::KeptOld { reason } => Some(format!(
                "[camerata] Saved runs stay in {old} for now and were not moved to {new}: \
                 {reason}. Nothing was deleted and the move is tried again on the next start."
            )),
        }
    }
}

/// The file operations a move can fail at, injected so tests reach every failure branch.
pub(crate) trait Ops {
    fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()>;
    fn copy_file(&self, from: &Path, to: &Path) -> std::io::Result<u64>;
}

struct RealOps;

impl Ops for RealOps {
    fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()> {
        std::fs::rename(from, to)
    }
    fn copy_file(&self, from: &Path, to: &Path) -> std::io::Result<u64> {
        std::fs::copy(from, to)
    }
}

/// Move the state an earlier version left in `old` to `new`. See the module docs.
pub fn migrate(old: &Path, new: &Path) -> MigrationOutcome {
    migrate_with(old, new, &RealOps)
}

pub(crate) fn migrate_with(old: &Path, new: &Path, ops: &dyn Ops) -> MigrationOutcome {
    migrate_holding(old, new, ops).0
}

/// The folder this process uses for the default location, decided once per `(new, old)` pair
/// and kept, so every caller in the process agrees.
pub(super) fn settled(new: &Path, old: &Path) -> PathBuf {
    static SETTLED: Mutex<Vec<(PathBuf, PathBuf, PathBuf)>> = Mutex::new(Vec::new());
    static HELD: Mutex<Vec<File>> = Mutex::new(Vec::new());
    if new == old {
        return new.to_path_buf();
    }
    let mut settled = SETTLED.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((_, _, root)) = settled.iter().find(|(n, o, _)| n == new && o == old) {
        return root.clone();
    }
    let root = if test_binary_outside_temp(new, old) {
        // A cargo test never moves the real folders of the machine it runs on: it keeps the
        // folder every earlier version used, as before this change.
        old.to_path_buf()
    } else {
        let (outcome, in_use) = migrate_holding(old, new, &RealOps);
        if let Some(line) = outcome.log_line(old, new) {
            eprintln!("{line}");
        }
        if let Some(file) = in_use {
            HELD.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(file);
        }
        if outcome.keeps_old() {
            old.to_path_buf()
        } else {
            new.to_path_buf()
        }
    };
    settled.push((new.to_path_buf(), old.to_path_buf(), root.clone()));
    root
}

fn test_binary_outside_temp(new: &Path, old: &Path) -> bool {
    let in_test = std::env::current_exe()
        .ok()
        .is_some_and(|exe| super::is_cargo_test_binary(&exe));
    let temp = std::env::temp_dir();
    in_test && !(old.starts_with(&temp) && new.starts_with(&temp))
}

/// [`migrate_with`], also returning the shared lock on the old folder when it is kept, taken
/// while the migration lock is still held so no other process can move the folder in between.
fn migrate_holding(old: &Path, new: &Path, ops: &dyn Ops) -> (MigrationOutcome, Option<File>) {
    if movable_entries(old).is_empty() {
        return (MigrationOutcome::NothingToMove, None);
    }
    if has_runs(new) {
        return (
            MigrationOutcome::NewAlreadyHasRuns {
                old_has_runs: has_runs(old),
            },
            None,
        );
    }
    if new.starts_with(old) || old.starts_with(new) {
        return kept_old(old, "one folder is inside the other".to_string());
    }
    if let Err(e) = create_private_dir(new) {
        return kept_old(old, format!("{} could not be created ({e})", new.display()));
    }
    let _migration = match lock_file(&new.join(MIGRATION_LOCK)) {
        Ok(file) => match file.lock() {
            Ok(()) => file,
            Err(e) => return kept_old(old, format!("the move could not be locked ({e})")),
        },
        Err(e) => return kept_old(old, format!("the move could not be locked ({e})")),
    };
    // Another process may have moved everything while this one waited for the lock.
    let entries = movable_entries(old);
    if entries.is_empty() {
        return (MigrationOutcome::NothingToMove, None);
    }
    if has_runs(new) {
        return (
            MigrationOutcome::NewAlreadyHasRuns {
                old_has_runs: has_runs(old),
            },
            None,
        );
    }
    let outcome = {
        let in_use = match lock_file(&old.join(IN_USE_LOCK)) {
            Ok(file) => file,
            Err(e) => return kept_old(old, format!("the old folder could not be locked ({e})")),
        };
        match in_use.try_lock() {
            Ok(()) => move_entries(old, new, &entries, ops),
            Err(std::fs::TryLockError::WouldBlock) => MigrationOutcome::KeptOld {
                reason: "another Camerata process is still using the old folder".to_string(),
            },
            Err(std::fs::TryLockError::Error(e)) => MigrationOutcome::KeptOld {
                reason: format!("the old folder could not be locked ({e})"),
            },
        }
        // The exclusive lock is released here, before the shared one is taken below.
    };
    if outcome.keeps_old() {
        let held = hold_shared(old);
        (outcome, held)
    } else {
        (outcome, None)
    }
}

/// `KeptOld` with the shared lock on the old folder taken.
fn kept_old(old: &Path, reason: String) -> (MigrationOutcome, Option<File>) {
    (MigrationOutcome::KeptOld { reason }, hold_shared(old))
}

fn hold_shared(old: &Path) -> Option<File> {
    let file = lock_file(&old.join(IN_USE_LOCK)).ok()?;
    file.try_lock_shared().ok()?;
    Some(file)
}

/// The checks that need the locks, then the rename and, when it fails, the copy.
fn move_entries(old: &Path, new: &Path, entries: &[String], ops: &dyn Ops) -> MigrationOutcome {
    if let Some(run) = run_in_use(old) {
        return MigrationOutcome::KeptOld {
            reason: format!("the saved run {run} is in use by another process"),
        };
    }
    for name in entries {
        let dest = new.join(name);
        if std::fs::symlink_metadata(&dest).is_err() {
            continue;
        }
        // An empty runs folder holds no runs (a listing may have created it).
        if name == RUNS && dest.is_dir() && std::fs::remove_dir(&dest).is_ok() {
            continue;
        }
        return MigrationOutcome::KeptOld {
            reason: format!("{} already exists", dest.display()),
        };
    }
    let rename_failure = match rename_all(old, new, entries, ops) {
        Ok(()) => {
            return MigrationOutcome::Moved {
                method: MoveMethod::Renamed,
                entries: entries.to_vec(),
                leftovers: Vec::new(),
            }
        }
        Err(Undo::RolledBack(e)) => e,
        Err(Undo::Stuck(reason)) => return MigrationOutcome::KeptOld { reason },
    };
    match copy_verify_place(old, new, entries, ops) {
        Ok(()) => MigrationOutcome::Moved {
            method: MoveMethod::Copied,
            entries: entries.to_vec(),
            leftovers: remove_old_copies(old, entries),
        },
        Err(Undo::RolledBack(e)) => MigrationOutcome::KeptOld {
            reason: format!("renaming failed ({rename_failure}) and copying failed ({e})"),
        },
        Err(Undo::Stuck(reason)) => MigrationOutcome::KeptOld { reason },
    }
}

/// Why a step failed, and whether everything it did was undone.
enum Undo {
    /// Everything was put back: the folders are as they were before the step.
    RolledBack(String),
    /// Something could not be put back. The old folder still holds every original.
    Stuck(String),
}

/// Rename `entries` from `from` to `to` in order; on a failure, rename the done ones back.
fn rename_all(from: &Path, to: &Path, entries: &[String], ops: &dyn Ops) -> Result<(), Undo> {
    for (i, name) in entries.iter().enumerate() {
        if let Err(e) = ops.rename(&from.join(name), &to.join(name)) {
            let failure = format!("{name}: {e}");
            for done in entries[..i].iter().rev() {
                if let Err(back) = ops.rename(&to.join(done), &from.join(done)) {
                    return Err(Undo::Stuck(format!(
                        "moving {failure} failed and {} could not be put back in {} ({back})",
                        done,
                        from.display()
                    )));
                }
            }
            return Err(Undo::RolledBack(failure));
        }
    }
    Ok(())
}

/// Copy every entry into the staging folder, verify it, and rename it into place. The old
/// folder is only read.
fn copy_verify_place(
    old: &Path,
    new: &Path,
    entries: &[String],
    ops: &dyn Ops,
) -> Result<(), Undo> {
    let staging = new.join(STAGING);
    // A staging folder left by an interrupted attempt holds only copies: the originals are
    // still in the old folder, which nothing removes until a copy is fully in place.
    if std::fs::symlink_metadata(&staging).is_ok() {
        std::fs::remove_dir_all(&staging)
            .map_err(|e| Undo::RolledBack(format!("an earlier staging folder remains ({e})")))?;
    }
    let staged = (|| -> std::io::Result<()> {
        create_private_dir(&staging)?;
        for name in entries {
            copy_tree(&old.join(name), &staging.join(name), ops)?;
            verify_tree(&old.join(name), &staging.join(name))?;
        }
        Ok(())
    })();
    if let Err(e) = staged {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(Undo::RolledBack(e.to_string()));
    }
    match rename_all(&staging, new, entries, ops) {
        Ok(()) => {
            let _ = std::fs::remove_dir(&staging);
            Ok(())
        }
        Err(Undo::RolledBack(e)) => {
            let _ = std::fs::remove_dir_all(&staging);
            Err(Undo::RolledBack(e))
        }
        Err(stuck) => Err(stuck),
    }
}

/// Remove the old copies of entries that are verified in place in the new folder. What cannot
/// be removed is returned, for the log line.
fn remove_old_copies(old: &Path, entries: &[String]) -> Vec<String> {
    let mut leftovers = Vec::new();
    for name in entries {
        let path = old.join(name);
        let removed = match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(&path),
            Ok(_) => std::fs::remove_file(&path),
            Err(_) => Ok(()),
        };
        if removed.is_err() {
            leftovers.push(path.display().to_string());
        }
    }
    leftovers
}

fn copy_tree(src: &Path, dst: &Path, ops: &dyn Ops) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(src)?;
    let kind = meta.file_type();
    if kind.is_symlink() {
        let target = std::fs::read_link(src)?;
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&target, dst)
        }
        #[cfg(not(unix))]
        {
            let _ = (target, dst);
            Err(std::io::Error::other(format!(
                "{} is a link, which this platform cannot copy",
                src.display()
            )))
        }
    } else if kind.is_dir() {
        std::fs::create_dir(dst)?;
        let mut names: Vec<_> = std::fs::read_dir(src)?
            .map(|e| e.map(|e| e.file_name()))
            .collect::<Result<_, _>>()?;
        names.sort();
        for name in names {
            copy_tree(&src.join(&name), &dst.join(&name), ops)?;
        }
        // Last, so a folder that is not writable by its owner is still filled first.
        std::fs::set_permissions(dst, meta.permissions())
    } else {
        ops.copy_file(src, dst).map(|_| ())
    }
}

/// `Ok` when `dst` holds exactly what `src` holds: the same names, links and bytes.
fn verify_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    let differs = |what: &str| {
        Err(std::io::Error::other(format!(
            "the copy of {} does not match ({what})",
            src.display()
        )))
    };
    let (a, b) = (
        std::fs::symlink_metadata(src)?,
        std::fs::symlink_metadata(dst)?,
    );
    if a.file_type().is_symlink() {
        if !b.file_type().is_symlink() || std::fs::read_link(src)? != std::fs::read_link(dst)? {
            return differs("link");
        }
        return Ok(());
    }
    if a.is_dir() {
        if !b.is_dir() {
            return differs("not a folder");
        }
        let names = |p: &Path| -> std::io::Result<Vec<std::ffi::OsString>> {
            let mut n: Vec<_> = std::fs::read_dir(p)?
                .map(|e| e.map(|e| e.file_name()))
                .collect::<Result<_, _>>()?;
            n.sort();
            Ok(n)
        };
        let listed = names(src)?;
        if listed != names(dst)? {
            return differs("entries");
        }
        for name in listed {
            verify_tree(&src.join(&name), &dst.join(&name))?;
        }
        return Ok(());
    }
    if !b.is_file() || a.len() != b.len() {
        return differs("size");
    }
    let (mut fa, mut fb) = (File::open(src)?, File::open(dst)?);
    let (mut ba, mut bb) = (vec![0u8; 64 * 1024], vec![0u8; 64 * 1024]);
    loop {
        let n = read_full(&mut fa, &mut ba)?;
        let m = read_full(&mut fb, &mut bb)?;
        if n != m || ba[..n] != bb[..m] {
            return differs("bytes");
        }
        if n == 0 {
            return Ok(());
        }
    }
}

/// Read until `buf` is full or the file ends.
fn read_full(file: &mut File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

/// The top level entries of `old` to move, `runs` last. Empty when `old` does not exist.
fn movable_entries(old: &Path) -> Vec<String> {
    let Ok(read) = std::fs::read_dir(old) else {
        return Vec::new();
    };
    let mut names: Vec<String> = read
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n != DISPOSABLE && !n.starts_with(".state-"))
        .collect();
    names.sort_by_key(|n| (n == RUNS, n.clone()));
    names
}

/// True when `root/runs` holds at least one saved run.
fn has_runs(root: &Path) -> bool {
    std::fs::read_dir(root.join(RUNS))
        .is_ok_and(|read| read.flatten().any(|e| e.path().join("run.json").is_file()))
}

/// The id of a run under `old` whose lock is held by a live process, if any. A lock that cannot
/// be probed counts as held: moving a run out from under its worker is the one thing to avoid.
fn run_in_use(old: &Path) -> Option<String> {
    let read = std::fs::read_dir(old.join(RUNS)).ok()?;
    for entry in read.flatten() {
        let lock = entry.path().join("lock.json");
        if std::fs::symlink_metadata(&lock).is_err() {
            continue;
        }
        let held = match File::open(&lock) {
            Ok(file) => match file.try_lock_shared() {
                Ok(()) => {
                    let _ = file.unlock();
                    false
                }
                Err(_) => true,
            },
            Err(_) => true,
        };
        if held {
            return Some(entry.file_name().to_string_lossy().into_owned());
        }
    }
    None
}

fn lock_file(path: &Path) -> std::io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// Create `dir` and its missing parents; `dir` itself, when created here, is private to the
/// owner. An existing folder keeps its mode.
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    match builder.create(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && dir.is_dir() => Ok(()),
        other => other,
    }
}

#[cfg(test)]
#[path = "migrate_tests.rs"]
mod tests;
