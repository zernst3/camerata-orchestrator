//! Onboarding draft: the in-flight onboarding state (scan + audit + per-repo rule
//! selection + triage dispositions) persisted to disk so a brownfield onboarding survives
//! an app restart — the architect does NOT have to re-scan to keep testing the post-scan
//! features. There is ONE current draft (onboarding is one active flow at a time); it is an
//! opaque JSON blob whose shape the UI owns. A crash mid-SCAN is still unrecoverable (the
//! scan hasn't produced a draft yet); everything after the scan is sticky.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Persists the current onboarding draft to `<data_dir>/camerata/onboarding-draft.json`,
/// with an in-memory mirror so a session without a resolvable data dir still works in-run.
/// `Clone` is a shallow handle (shared state) so it can live in the axum `AppState`.
#[derive(Clone, Default)]
pub struct DraftStore {
    path: Option<Arc<PathBuf>>,
    mem: Arc<Mutex<Option<serde_json::Value>>>,
}

impl DraftStore {
    /// In-memory only (tests / no data dir).
    pub fn new() -> Self {
        Self::default()
    }

    /// Persist to (and rehydrate from) `path`.
    pub fn at(path: PathBuf) -> Self {
        Self { path: Some(Arc::new(path)), mem: Arc::new(Mutex::new(None)) }
    }

    /// The saved draft, or None when nothing is in progress.
    pub fn load(&self) -> Option<serde_json::Value> {
        if let Some(p) = &self.path {
            if let Ok(s) = std::fs::read_to_string(p.as_ref().as_path()) {
                if let Ok(v) = serde_json::from_str(&s) {
                    return Some(v);
                }
            }
        }
        self.mem.lock().ok().and_then(|m| m.clone())
    }

    /// Replace the current draft (best-effort write; the in-memory mirror always updates).
    pub fn save(&self, v: serde_json::Value) {
        if let Some(p) = &self.path {
            if let Ok(s) = serde_json::to_string(&v) {
                let _ = std::fs::write(p.as_ref().as_path(), s);
            }
        }
        if let Ok(mut m) = self.mem.lock() {
            *m = Some(v);
        }
    }

    /// Drop the draft (onboarding completed, or the architect started fresh).
    pub fn clear(&self) {
        if let Some(p) = &self.path {
            let _ = std::fs::remove_file(p.as_ref().as_path());
        }
        if let Ok(mut m) = self.mem.lock() {
            *m = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_round_trip() {
        let store = DraftStore::new();
        assert!(store.load().is_none());
        store.save(serde_json::json!({"scan": 1}));
        assert_eq!(store.load(), Some(serde_json::json!({"scan": 1})));
        store.clear();
        assert!(store.load().is_none());
    }
}
