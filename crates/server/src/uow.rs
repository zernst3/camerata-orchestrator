//! Unit of Work (UoW) — the dev-side projection of a story.
//!
//! A story carries rich tracker/product status. The UoW is what the development
//! side knows about that story: which branch the work lives on, the AI development
//! history (the record of every governed run, note, and action), and a dedicated
//! DEV status (New / InProgress / Done) shown alongside the story's own status.
//!
//! The UoW persists across sessions so switching between stories never loses dev
//! context. The store mirrors [`crate::draft::DraftStore`]: Arc<Mutex>-wrapped,
//! JSON-file-persisted, with an in-memory fallback when no data dir is resolvable.
//!
//! Note: branch + history are designed to be auto-populated by the governed run
//! (Pillar 2 — fleet execution). For now they are settable via the API endpoints;
//! the UI shows them read-only. Auto-population lands when the fleet wires in.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

// ── domain types ─────────────────────────────────────────────────────────────

/// The dev lifecycle status for a story's Unit of Work. Shown ALONGSIDE the
/// story's own tracker status — they are orthogonal: a story can be "Planned"
/// (product) while its UoW is "In Progress" (dev already started).
#[derive(Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
pub enum DevStatus {
    /// Dev work has not started for this story.
    #[default]
    New,
    /// Dev work is actively in progress.
    InProgress,
    /// Dev work is complete (code shipped / PR merged / ready for QA).
    Done,
}

impl DevStatus {
    /// Parse from the wire string the API accepts (`"new"`, `"in_progress"`, `"done"`).
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "new" => Some(Self::New),
            "in_progress" => Some(Self::InProgress),
            "done" => Some(Self::Done),
            _ => None,
        }
    }

    /// A short display label for the UI.
    pub fn label(self) -> &'static str {
        match self {
            Self::New => "New",
            Self::InProgress => "In progress",
            Self::Done => "Done",
        }
    }
}

/// A single entry in the AI development history for a UoW. Appended by the
/// governed run (Pillar 2) when it takes an action on this story's behalf; also
/// appendable via the API for manual notes.
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct HistoryEntry {
    /// RFC 3339 timestamp of the action.
    pub ts: String,
    /// A short kind tag: `"run"`, `"note"`, `"gate_deny"`, `"gate_allow"`, etc.
    pub kind: String,
    /// Human-readable description of what happened.
    pub text: String,
}

/// The Unit of Work for one story. Keyed by `story_id`.
#[derive(Clone, Default, Serialize, Deserialize, Debug)]
pub struct UnitOfWork {
    /// The story this UoW belongs to.
    pub story_id: String,
    /// The git branch this work lives on (if set). Auto-populated by the fleet;
    /// also settable via the `/api/uow/:id/branch` endpoint.
    #[serde(default)]
    pub branch: Option<String>,
    /// The dev-side status, orthogonal to the tracker story status.
    #[serde(default)]
    pub dev_status: DevStatus,
    /// The ordered AI development history: every governed run, note, and action.
    #[serde(default)]
    pub history: Vec<HistoryEntry>,
    /// The architect's sign-off on this story's governed work (issue #21), if any.
    /// `None` until an architect explicitly signs the run off. Persisted so the
    /// sign-off survives sessions and is visible alongside the dev status.
    #[serde(default)]
    pub sign_off: Option<SignOff>,
    /// RFC 3339 timestamp of the last mutation. Stamped by every mutator.
    #[serde(default)]
    pub updated: String,
}

/// An architect's explicit sign-off on a story's governed run (issue #21). Recorded
/// only by the deliberate sign-off action — Camerata never signs work off on its own.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct SignOff {
    /// RFC 3339 timestamp of when the sign-off was recorded.
    pub ts: String,
    /// Who signed off (the architect's handle/name).
    pub by: String,
    /// The run that was signed off (the provenance the architect reviewed).
    pub run_id: String,
    /// An optional note the architect attached to the sign-off.
    #[serde(default)]
    pub note: Option<String>,
}

// ── store ─────────────────────────────────────────────────────────────────────

/// Persists a `HashMap<story_id, UnitOfWork>` to `<data_dir>/camerata/uow.json`,
/// with an in-memory mirror so a session without a resolvable data dir still works.
/// `Clone` is a shallow handle (shared `Arc`) so it can live in [`crate::AppState`].
#[derive(Clone, Default)]
pub struct UowStore {
    path: Option<Arc<PathBuf>>,
    mem: Arc<Mutex<HashMap<String, UnitOfWork>>>,
}

impl UowStore {
    /// In-memory only — no persistence (tests / no data dir).
    pub fn new() -> Self {
        Self::default()
    }

    /// Persist to (and rehydrate from) `path`.
    pub fn at(path: PathBuf) -> Self {
        let mem = if let Ok(s) = std::fs::read_to_string(&path) {
            serde_json::from_str(&s).unwrap_or_default()
        } else {
            HashMap::new()
        };
        Self {
            path: Some(Arc::new(path)),
            mem: Arc::new(Mutex::new(mem)),
        }
    }

    // ── private helpers ───────────────────────────────────────────────────────

    fn now_rfc3339() -> String {
        chrono::Utc::now().to_rfc3339()
    }

    /// Best-effort flush to disk. The in-memory state is always authoritative.
    fn flush(&self) {
        let Some(p) = &self.path else { return };
        let Ok(map) = self.mem.lock() else { return };
        if let Ok(s) = serde_json::to_string(&*map) {
            let _ = std::fs::write(p.as_ref(), s);
        }
    }

    // ── public API ────────────────────────────────────────────────────────────

    /// Return the UoW for `story_id`, creating a default one if it does not exist yet.
    pub fn get_or_create(&self, story_id: &str) -> UnitOfWork {
        let mut map = self.mem.lock().expect("uow mutex poisoned");
        map.entry(story_id.to_string())
            .or_insert_with(|| UnitOfWork {
                story_id: story_id.to_string(),
                updated: Self::now_rfc3339(),
                ..Default::default()
            })
            .clone()
    }

    /// All known UoWs, in arbitrary order.
    pub fn list(&self) -> Vec<UnitOfWork> {
        self.mem
            .lock()
            .expect("uow mutex poisoned")
            .values()
            .cloned()
            .collect()
    }

    /// Set the dev status for a story's UoW, creating it if needed.
    pub fn set_status(&self, story_id: &str, status: DevStatus) {
        let mut map = self.mem.lock().expect("uow mutex poisoned");
        let uow = map
            .entry(story_id.to_string())
            .or_insert_with(|| UnitOfWork {
                story_id: story_id.to_string(),
                ..Default::default()
            });
        uow.dev_status = status;
        uow.updated = Self::now_rfc3339();
        drop(map);
        self.flush();
    }

    /// Set (or clear) the branch for a story's UoW.
    pub fn set_branch(&self, story_id: &str, branch: Option<String>) {
        let mut map = self.mem.lock().expect("uow mutex poisoned");
        let uow = map
            .entry(story_id.to_string())
            .or_insert_with(|| UnitOfWork {
                story_id: story_id.to_string(),
                ..Default::default()
            });
        uow.branch = branch;
        uow.updated = Self::now_rfc3339();
        drop(map);
        self.flush();
    }

    /// Append an entry to the AI development history for a story's UoW.
    pub fn append_history(&self, story_id: &str, kind: &str, text: &str) {
        let mut map = self.mem.lock().expect("uow mutex poisoned");
        let uow = map
            .entry(story_id.to_string())
            .or_insert_with(|| UnitOfWork {
                story_id: story_id.to_string(),
                ..Default::default()
            });
        uow.history.push(HistoryEntry {
            ts: Self::now_rfc3339(),
            kind: kind.to_string(),
            text: text.to_string(),
        });
        uow.updated = Self::now_rfc3339();
        drop(map);
        self.flush();
    }

    /// Record an architect's sign-off on a story's governed run (issue #21). Sets the
    /// `sign_off` and also appends a `sign_off` history entry so the act shows in the
    /// AI development timeline. Returns the updated UoW. Camerata never calls this on
    /// its own — it is driven solely by the explicit sign-off action.
    pub fn sign_off(
        &self,
        story_id: &str,
        by: &str,
        run_id: &str,
        note: Option<&str>,
    ) -> UnitOfWork {
        let now = Self::now_rfc3339();
        let sign_off = SignOff {
            ts: now.clone(),
            by: by.to_string(),
            run_id: run_id.to_string(),
            note: note.map(|s| s.to_string()),
        };
        let history_text = match note.filter(|n| !n.trim().is_empty()) {
            Some(n) => format!("Run {run_id} signed off by {by}: {n}"),
            None => format!("Run {run_id} signed off by {by}"),
        };
        let updated = {
            let mut map = self.mem.lock().expect("uow mutex poisoned");
            let uow = map
                .entry(story_id.to_string())
                .or_insert_with(|| UnitOfWork {
                    story_id: story_id.to_string(),
                    ..Default::default()
                });
            uow.sign_off = Some(sign_off);
            uow.history.push(HistoryEntry {
                ts: now.clone(),
                kind: "sign_off".to_string(),
                text: history_text,
            });
            uow.updated = now;
            uow.clone()
        };
        self.flush();
        updated
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_get_or_create_set_status_list() {
        let store = UowStore::new();

        // get_or_create returns a default UoW for a new story id.
        let uow = store.get_or_create("CAM-1");
        assert_eq!(uow.story_id, "CAM-1");
        assert_eq!(uow.dev_status, DevStatus::New);
        assert!(uow.branch.is_none());
        assert!(uow.history.is_empty());

        // set_status mutates the stored UoW.
        store.set_status("CAM-1", DevStatus::InProgress);
        let uow2 = store.get_or_create("CAM-1");
        assert_eq!(uow2.dev_status, DevStatus::InProgress);

        // list returns all created UoWs.
        store.get_or_create("CAM-2");
        let all = store.list();
        assert_eq!(all.len(), 2);
        let cam1 = all
            .iter()
            .find(|u| u.story_id == "CAM-1")
            .expect("CAM-1 in list");
        assert_eq!(cam1.dev_status, DevStatus::InProgress);

        // set_status to Done.
        store.set_status("CAM-1", DevStatus::Done);
        assert_eq!(store.get_or_create("CAM-1").dev_status, DevStatus::Done);
    }

    #[test]
    fn set_branch_and_append_history() {
        let store = UowStore::new();

        store.set_branch("S-99", Some("feature/my-work".to_string()));
        assert_eq!(
            store.get_or_create("S-99").branch.as_deref(),
            Some("feature/my-work")
        );

        store.append_history("S-99", "run", "Governed run completed — 3 allow, 0 deny");
        let uow = store.get_or_create("S-99");
        assert_eq!(uow.history.len(), 1);
        assert_eq!(uow.history[0].kind, "run");
        assert!(uow.history[0].text.contains("Governed run"));

        // Clearing the branch.
        store.set_branch("S-99", None);
        assert!(store.get_or_create("S-99").branch.is_none());
    }

    #[test]
    fn sign_off_records_and_appends_history() {
        let store = UowStore::new();
        // No sign-off until the explicit action.
        assert!(store.get_or_create("CAM-21").sign_off.is_none());

        let uow = store.sign_off("CAM-21", "zach", "run-3", Some("LGTM, gate held"));
        let so = uow.sign_off.as_ref().expect("signed off");
        assert_eq!(so.by, "zach");
        assert_eq!(so.run_id, "run-3");
        assert_eq!(so.note.as_deref(), Some("LGTM, gate held"));

        // The sign-off is also recorded in the history timeline.
        assert!(uow
            .history
            .iter()
            .any(|h| h.kind == "sign_off" && h.text.contains("run-3")));

        // Persisted: a fresh get reflects it.
        let again = store.get_or_create("CAM-21");
        assert!(again.sign_off.is_some());
    }
}
