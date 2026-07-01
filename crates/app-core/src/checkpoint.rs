//! Run-checkpoint domain types: the persisted, resumable state of a governed development run that
//! PAUSED for human review (e.g. the test-tamper guard), so the run can later RESUME by re-spawning
//! the agent from where it stopped instead of failing.
//!
//! A checkpoint captures the IDENTITY and run-specific state needed to continue: which story/UoW,
//! the worktree (repo + branch + dir) with the agent's partial work still on disk, the `base_commit`
//! the run diffs against (so a resumed run continues from the same baseline), the bounce-loop
//! `iteration` and `max_iterations` budget, and the `model`. Everything else (the decisions, the
//! grounding, the read dirs, the L3/integration bundles) is RE-DERIVED from the project + UoW + story
//! at resume time, exactly as a fresh run derives it, so the checkpoint stays small and durable.
//!
//! These are framework-agnostic data shapes (RUST-HEADLESS-CORE-1). The `CheckpointStore` that
//! persists them (`Arc<Mutex<Vec<_>>>` + optional JSON flush) lives in the server adapter.

use serde::{Deserialize, Serialize};

/// One persisted, resumable run checkpoint.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Checkpoint {
    /// `ckpt-N`.
    pub id: String,
    /// The UoW this run is for.
    pub story_id: String,
    /// The original run id (for history/provenance; the resume spawns a fresh run id).
    pub run_id: String,
    /// The review that gates the resume (the human's decision unblocks it).
    pub escalation_id: String,
    /// Why it paused (e.g. "test-tamper").
    pub pause_reason: String,
    /// The primary repo (`owner/repo`).
    pub repo: String,
    /// The UoW branch the work is on.
    pub branch: String,
    /// The worktree directory on disk (the agent's partial work is here).
    pub worktree_dir: String,
    /// The commit the run diffs against; a resumed run reuses it so the diff continues from the
    /// same baseline rather than re-baselining and losing the agent's prior work in the comparison.
    pub base_commit: String,
    /// The bounce-loop iteration the run had reached when it paused.
    pub iteration: usize,
    /// The loop-guard budget.
    pub max_iterations: usize,
    /// The model the run's agent used.
    pub model: String,
    /// The owning project (for re-deriving config on resume).
    #[serde(default)]
    pub project_id: Option<String>,
    pub created: String,
    /// Set when a resume has consumed this checkpoint (so it is not resumed twice).
    #[serde(default)]
    pub resumed: Option<String>,
}

/// The inputs to create a checkpoint (everything except the assigned id + timestamps).
#[derive(Clone, Debug)]
pub struct NewCheckpoint {
    pub story_id: String,
    pub run_id: String,
    pub escalation_id: String,
    pub pause_reason: String,
    pub repo: String,
    pub branch: String,
    pub worktree_dir: String,
    pub base_commit: String,
    pub iteration: usize,
    pub max_iterations: usize,
    pub model: String,
    pub project_id: Option<String>,
}
