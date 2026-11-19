//! Onboarding drafts: the in-flight onboarding state (scan + audit + per-repo rule
//! selection + triage dispositions) persisted to disk so a brownfield onboarding survives
//! an app restart — the architect does NOT have to re-scan to keep testing the post-scan
//! features.
//!
//! Drafts are keyed by PROJECT id: each project can have its own in-progress onboarding,
//! so opening project B never clobbers project A's draft (the UI shows a "continue or start
//! over" prompt when a project with a draft is opened). Each draft is an opaque JSON blob
//! whose shape the UI owns. A crash mid-SCAN is still unrecoverable (the scan hasn't
//! produced a draft yet); everything after the scan is sticky.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// `path` with `suffix` appended to its file name (`onboarding-draft.json` -> `.json.bak`).
fn sibling(path: &std::path::Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

/// Write `json` to `path`, first keeping the file's previous content as `<path>.bak` (one
/// previous copy, replaced on each write).
fn write_with_backup(path: &std::path::Path, json: &str) -> std::io::Result<()> {
    if path.exists() {
        let _ = std::fs::copy(path, sibling(path, ".bak"));
    }
    std::fs::write(path, json)
}

/// Persists per-project onboarding drafts to `<data_dir>/camerata/onboarding-draft.json`
/// (a `{ project_id: draft }` map), with an in-memory mirror so a session without a
/// resolvable data dir still works in-run. `Clone` is a shallow handle (shared state) so
/// it can live in the axum `AppState`.
#[derive(Clone, Default)]
pub struct DraftStore {
    path: Option<Arc<PathBuf>>,
    mem: Arc<Mutex<HashMap<String, serde_json::Value>>>,
    /// Set when the file on disk could not be read at start and was set aside; the operator is
    /// told on the next read of the draft, and it clears when the first new draft is written.
    notice: Arc<Mutex<Option<String>>>,
}

impl DraftStore {
    /// In-memory only (tests / no data dir).
    pub fn new() -> Self {
        Self::default()
    }

    /// Persist to (and rehydrate from) `path`. A file in the OLD single-draft shape (a
    /// bare draft object, pre-per-project) fails to parse as a map and is dropped — a
    /// one-time loss on upgrade (re-scan to recreate); going forward it's a map.
    pub fn at(path: PathBuf) -> Self {
        let mut notice = None;
        let mem = match std::fs::read_to_string(&path) {
            Ok(s) => {
                serde_json::from_str::<HashMap<String, serde_json::Value>>(&s).unwrap_or_else(|e| {
                    // A file we cannot read as a map is never dropped: it is set aside, so the
                    // first flush of an empty map cannot overwrite the only copy.
                    let aside = sibling(&path, ".unreadable");
                    eprintln!(
                        "[camerata-server] {} could not be read ({e}); kept untouched at {}",
                        path.display(),
                        aside.display()
                    );
                    let _ = std::fs::write(&aside, s);
                    notice = Some(format!(
                        "The saved onboarding draft file could not be read, so no earlier review \
                         was restored. The file was kept untouched at {}. Nothing was deleted.",
                        aside.display()
                    ));
                    HashMap::new()
                })
            }
            Err(_) => HashMap::new(),
        };
        Self {
            path: Some(Arc::new(path)),
            mem: Arc::new(Mutex::new(mem)),
            notice: Arc::new(Mutex::new(notice)),
        }
    }

    /// Why no draft could be restored, when the draft file on disk was unreadable at start and
    /// nothing has been saved since. `None` otherwise.
    pub fn unreadable_notice(&self) -> Option<String> {
        self.notice.lock().ok().and_then(|n| n.clone())
    }

    /// Best-effort flush of the whole map to disk; the in-memory mirror is authoritative.
    fn flush(&self) {
        let Some(p) = &self.path else { return };
        let Ok(map) = self.mem.lock() else { return };
        if let Ok(s) = serde_json::to_string(&*map) {
            let _ = write_with_backup(p.as_ref().as_path(), &s);
            if let Ok(mut n) = self.notice.lock() {
                *n = None;
            }
        }
    }

    /// The saved draft for `project_id`, or None when nothing is in progress for it.
    pub fn load(&self, project_id: &str) -> Option<serde_json::Value> {
        self.mem
            .lock()
            .ok()
            .and_then(|m| m.get(project_id).cloned())
    }

    /// Drop `project_id`'s draft and write the file; when the write fails nothing is dropped and
    /// the error is returned.
    pub fn clear_checked(&self, project_id: &str) -> std::io::Result<()> {
        let mut m = self
            .mem
            .lock()
            .map_err(|_| std::io::Error::other("draft store mutex poisoned"))?;
        let mut kept = m.clone();
        kept.remove(project_id);
        if let Some(p) = &self.path {
            let json = serde_json::to_string(&kept).map_err(std::io::Error::other)?;
            write_with_backup(p.as_ref().as_path(), &json)?;
        }
        *m = kept;
        Ok(())
    }

    /// The size in bytes of `project_id`'s saved draft as stored (0 when there is none).
    pub fn stored_bytes(&self, project_id: &str) -> u64 {
        self.load(project_id)
            .and_then(|v| serde_json::to_vec(&v).ok())
            .map_or(0, |b| b.len() as u64)
    }

    /// Replace `project_id`'s draft.
    pub fn save(&self, project_id: &str, v: serde_json::Value) {
        if let Ok(mut m) = self.mem.lock() {
            m.insert(project_id.to_string(), v);
        }
        self.flush();
    }

    /// Put a finished run's `report` into `project_id`'s draft as its `audit`, when the draft is
    /// of the same repos and holds no audit yet: the review the run produced must be reopenable
    /// even when no screen was mounted to receive it. Never replaces an audit already there.
    /// Returns whether the draft changed.
    pub fn adopt_audit(
        &self,
        project_id: &str,
        repos: &[String],
        report: serde_json::Value,
    ) -> bool {
        let changed = {
            let Ok(mut m) = self.mem.lock() else {
                return false;
            };
            let Some(draft) = m.get_mut(project_id).and_then(|d| d.as_object_mut()) else {
                return false;
            };
            let same_scan = draft
                .get("scan")
                .and_then(|s| s.get("repos"))
                .and_then(|r| r.as_array())
                .is_some_and(|r| {
                    r.len() == repos.len()
                        && r.iter()
                            .zip(repos)
                            .all(|(a, b)| a.as_str() == Some(b.as_str()))
                });
            let has_audit = draft.get("audit").is_some_and(|a| !a.is_null());
            if !same_scan || has_audit {
                false
            } else {
                draft.insert("audit".to_string(), report);
                true
            }
        };
        if changed {
            self.flush();
        }
        changed
    }

    /// Drop `project_id`'s draft (onboarding completed, or the architect started fresh).
    pub fn clear(&self, project_id: &str) {
        if let Ok(mut m) = self.mem.lock() {
            m.remove(project_id);
        }
        self.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_project_round_trip() {
        let store = DraftStore::new();
        assert!(store.load("p1").is_none());

        store.save("p1", serde_json::json!({"scan": 1}));
        store.save("p2", serde_json::json!({"scan": 2}));
        // Each project keeps its own draft; one does not clobber the other.
        assert_eq!(store.load("p1"), Some(serde_json::json!({"scan": 1})));
        assert_eq!(store.load("p2"), Some(serde_json::json!({"scan": 2})));

        // Clearing one leaves the other intact.
        store.clear("p1");
        assert!(store.load("p1").is_none());
        assert_eq!(store.load("p2"), Some(serde_json::json!({"scan": 2})));
    }
}
