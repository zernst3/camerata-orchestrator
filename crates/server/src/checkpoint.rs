//! Run checkpoints: the persisted, resumable state of a governed development run that PAUSED for
//! human review (e.g. the test-tamper guard), so the run can later RESUME by re-spawning the agent
//! from where it stopped instead of failing.
//!
//! A checkpoint captures the IDENTITY and run-specific state needed to continue: which story/UoW,
//! the worktree (repo + branch + dir) with the agent's partial work still on disk, the `base_commit`
//! the run diffs against (so a resumed run continues from the same baseline), the bounce-loop
//! `iteration` and `max_iterations` budget, and the `model`. Everything else (the decisions, the
//! grounding, the read dirs, the L3/integration bundles) is RE-DERIVED from the project + UoW + story
//! at resume time, exactly as a fresh run derives it, so the checkpoint stays small and durable.
//!
//! Persistence mirrors the other stores: in-memory `Arc<Mutex<Vec<_>>>`, optionally flushed to
//! `<data_dir>/camerata/checkpoints.json`. The pause survives an app restart because it is persisted
//! state, not a held thread.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

// The Checkpoint / NewCheckpoint domain types now live in the framework-agnostic core
// (RUST-HEADLESS-CORE-1); re-exported so `crate::checkpoint::{Checkpoint, NewCheckpoint}` call sites
// are unchanged. The CheckpointStore below (Arc<Mutex> + JSON persistence) stays in this adapter.
pub use camerata_app_core::checkpoint::{Checkpoint, NewCheckpoint};

/// Checkpoint store. In-memory by default; [`at`](Self::at) persists to
/// `<data_dir>/camerata/checkpoints.json`. `Clone` is a shallow Arc handle for `AppState`.
#[derive(Clone, Default)]
pub struct CheckpointStore {
    items: Arc<Mutex<Vec<Checkpoint>>>,
    counter: Arc<AtomicUsize>,
    path: Option<Arc<PathBuf>>,
}

impl CheckpointStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn at(path: PathBuf) -> Self {
        let items: Vec<Checkpoint> = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let max = items
            .iter()
            .filter_map(|c| c.id.strip_prefix("ckpt-"))
            .filter_map(|n| n.parse::<usize>().ok())
            .max()
            .unwrap_or(0);
        Self {
            items: Arc::new(Mutex::new(items)),
            counter: Arc::new(AtomicUsize::new(max)),
            path: Some(Arc::new(path)),
        }
    }

    fn now_rfc3339() -> String {
        chrono::Utc::now().to_rfc3339()
    }

    fn flush(&self) {
        let Some(p) = &self.path else { return };
        let Ok(items) = self.items.lock() else { return };
        if let Ok(s) = serde_json::to_string(&*items) {
            let _ = std::fs::write(p.as_ref(), s);
        }
    }

    /// Create a checkpoint, assigning a fresh `ckpt-N` id + created timestamp. Returns it.
    pub fn create(&self, new: NewCheckpoint) -> Checkpoint {
        let n = self.counter.fetch_add(1, Ordering::SeqCst) + 1;
        let c = Checkpoint {
            id: format!("ckpt-{n}"),
            story_id: new.story_id,
            run_id: new.run_id,
            escalation_id: new.escalation_id,
            pause_reason: new.pause_reason,
            repo: new.repo,
            branch: new.branch,
            worktree_dir: new.worktree_dir,
            base_commit: new.base_commit,
            iteration: new.iteration,
            max_iterations: new.max_iterations,
            model: new.model,
            project_id: new.project_id,
            created: Self::now_rfc3339(),
            resumed: None,
        };
        if let Ok(mut g) = self.items.lock() {
            g.push(c.clone());
        }
        self.flush();
        c
    }

    pub fn get(&self, id: &str) -> Option<Checkpoint> {
        self.items.lock().ok()?.iter().find(|c| c.id == id).cloned()
    }

    pub fn list(&self) -> Vec<Checkpoint> {
        self.items.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// The latest UN-resumed checkpoint for a story, if any (the one a resume would continue from).
    pub fn latest_open_for_story(&self, story_id: &str) -> Option<Checkpoint> {
        self.items
            .lock()
            .ok()?
            .iter()
            .filter(|c| c.story_id == story_id && c.resumed.is_none())
            .last()
            .cloned()
    }

    /// Stamp a checkpoint as resumed (so it is not consumed twice). Returns the updated record, or
    /// `None` for an unknown id or one already resumed.
    pub fn mark_resumed(&self, id: &str) -> Option<Checkpoint> {
        let mut guard = self.items.lock().ok()?;
        let c = guard
            .iter_mut()
            .find(|c| c.id == id && c.resumed.is_none())?;
        c.resumed = Some(Self::now_rfc3339());
        let updated = c.clone();
        drop(guard);
        self.flush();
        Some(updated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(store: &CheckpointStore, story: &str) -> Checkpoint {
        store.create(NewCheckpoint {
            story_id: story.to_string(),
            run_id: "run-1".to_string(),
            escalation_id: "esc-1".to_string(),
            pause_reason: "test-tamper".to_string(),
            repo: "me/api".to_string(),
            branch: "camerata/me-api-1".to_string(),
            worktree_dir: "/tmp/wt".to_string(),
            base_commit: "abc123".to_string(),
            iteration: 0,
            max_iterations: 3,
            model: "claude-opus-4-8".to_string(),
            project_id: Some("proj-1".to_string()),
        })
    }

    #[test]
    fn create_assigns_id_and_get_finds_it() {
        let store = CheckpointStore::new();
        let c = mk(&store, "me/api#1");
        assert_eq!(c.id, "ckpt-1");
        assert!(c.resumed.is_none());
        assert_eq!(store.get("ckpt-1").unwrap().base_commit, "abc123");
        assert!(store.get("nope").is_none());
    }

    #[test]
    fn latest_open_for_story_and_mark_resumed() {
        let store = CheckpointStore::new();
        let _c1 = mk(&store, "me/api#1");
        let c2 = mk(&store, "me/api#1"); // a second pause on the same story
        // latest_open returns the most recent un-resumed one.
        assert_eq!(store.latest_open_for_story("me/api#1").unwrap().id, c2.id);
        // mark it resumed; it is no longer "open", and can't be resumed twice.
        assert!(store.mark_resumed(&c2.id).is_some());
        assert!(store.mark_resumed(&c2.id).is_none(), "already resumed -> None");
        // now the earlier one is the latest open.
        assert_eq!(store.latest_open_for_story("me/api#1").unwrap().id, "ckpt-1");
        assert!(store.latest_open_for_story("other").is_none());
    }
}
