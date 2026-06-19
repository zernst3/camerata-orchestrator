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

/// Persists per-project onboarding drafts to `<data_dir>/camerata/onboarding-draft.json`
/// (a `{ project_id: draft }` map), with an in-memory mirror so a session without a
/// resolvable data dir still works in-run. `Clone` is a shallow handle (shared state) so
/// it can live in the axum `AppState`.
#[derive(Clone, Default)]
pub struct DraftStore {
    path: Option<Arc<PathBuf>>,
    mem: Arc<Mutex<HashMap<String, serde_json::Value>>>,
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
        let mem = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<HashMap<String, serde_json::Value>>(&s).ok())
            .unwrap_or_default();
        Self { path: Some(Arc::new(path)), mem: Arc::new(Mutex::new(mem)) }
    }

    /// Best-effort flush of the whole map to disk; the in-memory mirror is authoritative.
    fn flush(&self) {
        let Some(p) = &self.path else { return };
        let Ok(map) = self.mem.lock() else { return };
        if let Ok(s) = serde_json::to_string(&*map) {
            let _ = std::fs::write(p.as_ref().as_path(), s);
        }
    }

    /// The saved draft for `project_id`, or None when nothing is in progress for it.
    pub fn load(&self, project_id: &str) -> Option<serde_json::Value> {
        self.mem.lock().ok().and_then(|m| m.get(project_id).cloned())
    }

    /// Replace `project_id`'s draft.
    pub fn save(&self, project_id: &str, v: serde_json::Value) {
        if let Ok(mut m) = self.mem.lock() {
            m.insert(project_id.to_string(), v);
        }
        self.flush();
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
