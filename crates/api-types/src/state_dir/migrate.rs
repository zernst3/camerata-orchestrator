//! STUB: the red half of issue 342.

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveMethod {
    Renamed,
    Copied,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationOutcome {
    NothingToMove,
    NewAlreadyHasRuns { old_has_runs: bool },
    Moved { method: MoveMethod, entries: Vec<String>, leftovers: Vec<String> },
    KeptOld { reason: String },
}

pub(crate) trait Ops {
    fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()>;
    fn copy_file(&self, from: &Path, to: &Path) -> std::io::Result<u64>;
}

pub fn migrate(old: &Path, new: &Path) -> MigrationOutcome {
    let _ = (old, new);
    MigrationOutcome::NothingToMove
}

pub(crate) fn migrate_with(old: &Path, new: &Path, ops: &dyn Ops) -> MigrationOutcome {
    let _ = (old, new, ops);
    MigrationOutcome::NothingToMove
}

#[cfg(test)]
#[path = "migrate_tests.rs"]
mod tests;
