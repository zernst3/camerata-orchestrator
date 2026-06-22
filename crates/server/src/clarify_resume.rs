//! Phase 3b: the agent→run pause/resume channel's persisted CONTEXT.
//!
//! When a gated agent (investigation, and later dev) raises a structured clarifying
//! question via the gateway's `ask_clarification` tool, the run PAUSES at a checkpoint:
//! the question is posted into the 3a [`crate::clarify::ClarificationStore`] and the run
//! transitions to [`crate::run::RunStatus::AwaitingClarification`]. To RESUME on answer,
//! the server must re-spawn the SAME gated agent (same governed role, same worktree, gate
//! intact) with the prior task context plus the asked question plus the user's answer.
//!
//! That re-spawn needs enough context to rebuild the agent. This module persists it,
//! keyed by the clarification id, with the SAME disk-backed flush-on-mutate pattern the
//! 3a clarify store uses ([`crate::clarify::ClarificationStore::at`]) — so a pause point
//! survives a restart and the run can still resume after the process is bounced.
//!
//! The resume is NOT a blocking long-poll: the agent subprocess has already exited at the
//! pause (a question is its last act before STOPping). On answer, a fresh gated agent is
//! spawned with the accumulated context. Persisting the context here is what makes the
//! pause durable and the resume reconstructable.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

/// The kind of gated run that paused on a clarification. Drives which runner the resume
/// path re-spawns. Investigation is the first wired phase; the enum makes adding the
/// dev-phase resume a closed, explicit choice rather than a stringly-typed branch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PausedPhase {
    /// The single-agent INVESTIGATION runner.
    Investigation,
}

/// Everything needed to RE-SPAWN the same gated agent and continue, persisted at the
/// pause point. Keyed (in the store) by the clarification id the run is parked on.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClarifyResumeContext {
    /// The run that paused (so the resume re-uses the same run timeline).
    pub run_id: String,
    /// The story / UoW the run governs (the clarify-store key, and the UoW to update).
    pub story_id: String,
    /// Human title carried for rebuilding the agent prompt on resume.
    pub story_title: String,
    /// Story description carried for rebuilding the agent prompt on resume.
    pub story_desc: String,
    /// The model id the paused agent ran on — the resume re-uses the same one.
    pub model: String,
    /// Which gated runner paused (drives which resume path runs).
    pub phase: PausedPhase,
    /// The ORIGINAL task prompt the agent was running when it asked. The resume prompt is
    /// this plus the asked question plus the human's answer, so the re-spawned agent has
    /// the full prior context (we re-spawn fresh rather than long-poll a hung process).
    pub original_task: String,
    /// The exact question text the agent asked (echoed back into the resume prompt).
    pub asked_question: String,
}

/// Disk-backed store of resume contexts, keyed by clarification id. Mirrors
/// [`crate::clarify::ClarificationStore`]: in-memory by default ([`Self::new`]); when
/// constructed via [`Self::at`] it rehydrates from and flushes to a JSON file so a pause
/// point's resume context survives a restart.
#[derive(Clone, Default)]
pub struct ClarifyResumeStore {
    items: Arc<Mutex<HashMap<String, ClarifyResumeContext>>>,
    path: Option<Arc<PathBuf>>,
}

impl ClarifyResumeStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Persist to (and rehydrate from) `path`. Open pause points' resume contexts survive
    /// a restart, so a run parked on a clarification can still resume after a bounce.
    pub fn at(path: PathBuf) -> Self {
        let items: HashMap<String, ClarifyResumeContext> = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self {
            items: Arc::new(Mutex::new(items)),
            path: Some(Arc::new(path)),
        }
    }

    /// Write the current map to the backing file, if persistent. Best-effort (the
    /// in-memory state stays authoritative). Must NOT be called holding the lock.
    fn flush(&self) {
        let Some(p) = &self.path else { return };
        let Ok(map) = self.items.lock() else { return };
        if let Ok(s) = serde_json::to_string(&*map) {
            let _ = std::fs::write(p.as_ref(), s);
        }
    }

    /// Record the resume context for a clarification id (the pause checkpoint). Overwrites
    /// any prior context for the same id. Flushes if persistent.
    pub fn put(&self, clar_id: &str, ctx: ClarifyResumeContext) {
        if let Ok(mut guard) = self.items.lock() {
            guard.insert(clar_id.to_string(), ctx);
        }
        self.flush();
    }

    /// Fetch the resume context for a clarification id, if one is parked there.
    pub fn get(&self, clar_id: &str) -> Option<ClarifyResumeContext> {
        self.items.lock().ok()?.get(clar_id).cloned()
    }

    /// Remove (consume) the resume context for a clarification id once the resume has been
    /// kicked, so it cannot be double-resumed. Returns the removed context, if any.
    pub fn take(&self, clar_id: &str) -> Option<ClarifyResumeContext> {
        let removed = {
            let mut guard = self.items.lock().ok()?;
            guard.remove(clar_id)
        };
        if removed.is_some() {
            self.flush();
        }
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ClarifyResumeContext {
        ClarifyResumeContext {
            run_id: "run-1".into(),
            story_id: "CAM-7".into(),
            story_title: "Add export".into(),
            story_desc: "Members CSV export.".into(),
            model: "claude-opus-4-8".into(),
            phase: PausedPhase::Investigation,
            original_task: "Analyze the story.".into(),
            asked_question: "Include archived members?".into(),
        }
    }

    #[test]
    fn put_get_take_round_trip() {
        let store = ClarifyResumeStore::new();
        store.put("clar-1", ctx());
        let got = store.get("clar-1").expect("present");
        assert_eq!(got.run_id, "run-1");
        assert_eq!(got.phase, PausedPhase::Investigation);

        // Take consumes it: a second take is None (no double-resume).
        let taken = store.take("clar-1").expect("present");
        assert_eq!(taken.story_id, "CAM-7");
        assert!(store.take("clar-1").is_none());
        assert!(store.get("clar-1").is_none());
    }

    #[test]
    fn persistence_survives_reopen() {
        let dir = std::env::temp_dir().join(format!(
            "cam-resume-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("clarify-resume.json");

        {
            let store = ClarifyResumeStore::at(path.clone());
            store.put("clar-9", ctx());
        }

        // Reopen at the same path: the parked resume context survived the restart.
        let reopened = ClarifyResumeStore::at(path.clone());
        let got = reopened.get("clar-9").expect("survived restart");
        assert_eq!(got.run_id, "run-1");
        assert_eq!(got.asked_question, "Include archived members?");
        assert_eq!(got.original_task, "Analyze the story.");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
