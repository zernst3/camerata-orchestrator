//! The rules each inspection ran with, kept beside the inspection store (#560).
//!
//! A run keeps its rule snapshot in its own folder ([`crate::run_store::rule_snapshot`]), but
//! deleting a client's source removes that folder while the inspection, which every export is
//! built from, stays. So the snapshot is copied here when the inspection is recorded, content
//! addressed (`<inspections dir>/rule-snapshots/sha256-<hex>.json`, one file per distinct rule
//! library, shared by every inspection that ran on it), and the entry names it by its digest
//! ([`super::InspectionEntry::rule_digest`]). The bytes are the run's file exactly, so reading
//! them back runs every check of [`crate::run_store::rule_snapshot::decode`].
//!
//! An in-memory store (tests, no data folder) keeps the bytes in memory instead.

use std::path::PathBuf;
use std::sync::Arc;

use super::{InspectionEntry, InspectionStore};
use crate::run_store::rule_snapshot;

/// The folder inside the inspection store's folder that holds the snapshots.
pub const RULE_SNAPSHOT_DIR: &str = "rule-snapshots";

impl InspectionStore {
    /// Where the snapshot with `digest` is kept on disk; `None` for an in-memory store or a
    /// digest that is not a sha256 digest.
    pub fn rule_snapshot_path(&self, digest: &str) -> Option<PathBuf> {
        let name = rule_snapshot::file_name(digest)?;
        self.dir
            .as_ref()
            .map(|d| d.join(RULE_SNAPSHOT_DIR).join(name))
    }

    /// Keep the snapshot `bytes` under `digest`. The caller has verified them against the run's
    /// pin; a reader verifies them again on every read.
    pub fn keep_rule_snapshot(&self, digest: &str, bytes: &[u8]) -> std::io::Result<()> {
        if rule_snapshot::file_name(digest).is_none() {
            return Err(std::io::Error::other(format!(
                "the rule digest {digest} is not a sha256 digest"
            )));
        }
        match self.rule_snapshot_path(digest) {
            Some(path) => {
                if let Some(parent) = path.parent() {
                    crate::run_store::create_private_dir(parent)?;
                }
                crate::run_store::write_atomic(&path, bytes)
            }
            None => {
                self.snapshot_mem()
                    .insert(digest.to_string(), Arc::new(bytes.to_vec()));
                Ok(())
            }
        }
    }

    /// The kept snapshot bytes for `digest`, `Ok(None)` when none is kept.
    pub fn rule_snapshot_bytes(&self, digest: &str) -> std::io::Result<Option<Arc<Vec<u8>>>> {
        match self.rule_snapshot_path(digest) {
            Some(path) => match std::fs::read(&path) {
                Ok(bytes) => Ok(Some(Arc::new(bytes))),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e),
            },
            None if self.dir.is_some() => Ok(None),
            None => Ok(self.snapshot_mem().get(digest).cloned()),
        }
    }

    /// The rule digest of the newest entry of `project_id` made from the run `run_id` (an entry
    /// with no run when `run_id` is `None`), when one is recorded.
    pub fn rule_digest_for_run(&self, project_id: &str, run_id: Option<&str>) -> Option<String> {
        self.lock().get(project_id).and_then(|list| {
            list.iter()
                .rev()
                .find(|e| e.report.run_id.as_deref() == run_id)
                .and_then(|e| e.rule_digest.clone())
        })
    }

    /// Name the kept snapshot `digest` on every entry of `project_id` made from the run `run_id`.
    /// `Ok(false)` when there is no such entry.
    pub fn set_rule_digest_for_run(
        &self,
        project_id: &str,
        run_id: &str,
        digest: &str,
    ) -> std::io::Result<bool> {
        let mut guard = self.lock();
        let Some(list) = guard.get_mut(project_id) else {
            return Ok(false);
        };
        let mut changed = false;
        for e in list
            .iter_mut()
            .filter(|e| e.report.run_id.as_deref() == Some(run_id))
        {
            e.rule_digest = Some(digest.to_string());
            changed = true;
        }
        if !changed {
            return Ok(false);
        }
        self.persist_locked(project_id, list).map(|()| true)
    }

    /// Keep `rules` and name them on the latest entry of `project_id`, as if its run had kept
    /// them. Test and fixture support for an inspection built without a run; production keeps the
    /// run's own verified bytes ([`crate::AppState::record_inspection`]).
    #[doc(hidden)]
    pub fn attach_rule_snapshot(
        &self,
        project_id: &str,
        rules: &camerata_rules::RuleSet,
    ) -> std::io::Result<bool> {
        let (digest, bytes) = rule_snapshot::encode(rules);
        self.keep_rule_snapshot(&digest, &bytes)?;
        self.update_latest(project_id, |e: &mut InspectionEntry| {
            e.rule_digest = Some(digest)
        })
    }

    fn snapshot_mem(
        &self,
    ) -> std::sync::MutexGuard<'_, std::collections::BTreeMap<String, Arc<Vec<u8>>>> {
        self.snapshots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
