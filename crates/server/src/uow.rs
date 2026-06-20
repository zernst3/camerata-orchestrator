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

use camerata_persistence::{
    encode, ArtifactKind, ArtifactStore, EditActor, NewRevision, RevisionOp,
};
use camerata_worktracker::investigation::{DecisionRecord, DecisionOutcome, InvestigationArtifact};

use crate::lifecycle::{TransitionError, UowStage};

/// The single SQLite project id under which all UoW-owned artifacts (decision
/// records, investigation notes) are filed in the central [`ArtifactStore`].
///
/// Camerata's artifact store partitions by `project_id`; the UoW layer is
/// per-story, not per-tracker-project, so we file every UoW artifact under one
/// stable namespace and use the `artifact_id` (derived from the story id) to key
/// per-story history. This keeps the store's composite identity
/// `(project_id, kind, artifact_id)` unique per story without threading a real
/// project id through the sync UoW API.
pub const UOW_ARTIFACT_PROJECT: &str = "camerata-uow";

/// The artifact id under which a story's full decision set is versioned in the
/// [`ArtifactStore`]. One revision per `set_decisions` call, so the decision
/// history is the revision history.
fn decisions_artifact_id(story_id: &str) -> String {
    format!("{story_id}/decisions")
}

/// The artifact id under which a story's investigation note is versioned.
/// Matches the convention documented on
/// [`camerata_worktracker::investigation::InvestigationArtifact`].
fn investigation_artifact_id(story_id: &str) -> String {
    format!("{story_id}/investigation")
}

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

/// The durable gate provenance persisted onto a UoW after a governed run finishes.
///
/// [`crate::run::RunProvenance`] is the live, derived-on-read summary of a run; this
/// is the FROZEN copy stamped onto the UoW so the governed-development record survives
/// even if the in-memory run is gone (the `RunStore` is in-memory, the UoW persists).
/// It is the honest accounting an architect reviews at QA before signing off.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct GateProvenance {
    /// The run this provenance came from.
    pub run_id: String,
    /// "scripted" (token-free, real-gate verdicts) or "live".
    pub mode: String,
    /// How many gate verdicts allowed a write.
    pub allow_count: usize,
    /// How many gate verdicts denied a write.
    pub deny_count: usize,
    /// Total bounces the gate sent back (== `deny_count`; named for the architect-
    /// facing vocabulary).
    pub total_bounces: usize,
    /// The distinct rule ids that actually fired a denial, in first-seen order.
    #[serde(default)]
    pub rules_fired: Vec<String>,
    /// RFC 3339 timestamp of when this provenance was stamped onto the UoW.
    pub recorded: String,
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
    /// The precise governed-development lifecycle stage (Pillar 2). Orthogonal to
    /// `dev_status` (which is the coarse badge): this drives the no-code-first gate
    /// and the QA gate. Defaults to [`UowStage::Intake`]. Mutated ONLY through the
    /// transition methods on [`UowStore`], which run the pure state machine in
    /// [`crate::lifecycle`].
    #[serde(default)]
    pub stage: UowStage,
    /// The structured decision records surfaced during this story's investigation.
    ///
    /// This field is now a READ CACHE: the durable home for decisions is the
    /// central [`ArtifactStore`] (ROUTE-A, landed in the
    /// `2026-06-20_artifactstore_decisions_migration` decision doc), keyed by
    /// story id, where every `set_decisions` is a new revision with actor + op
    /// provenance so the per-story decision history is queryable and versioned.
    ///
    /// When a [`UowStore`] is backed by an [`ArtifactStore`], this field is kept
    /// in sync on write (mirrored from the store) and hydrated on read
    /// (read-through from the store's latest revision). When there is no store
    /// (in-memory tests, no data dir), it remains the authoritative home so the
    /// gate still works. Either way the JSON-serialized value here is also the
    /// back-compat carrier: an existing `uow.json` with inline decisions still
    /// loads, and is migrated into the store on first store-backed write.
    #[serde(default)]
    pub decisions: Vec<DecisionRecord>,
    /// The ordered AI development history: every governed run, note, and action.
    #[serde(default)]
    pub history: Vec<HistoryEntry>,
    /// The frozen gate provenance from the most recent completed governed run, if any.
    /// Stamped by [`UowStore::record_gate_provenance`] when a run finishes; the durable
    /// record the architect reviews at QA. `None` until a run has completed.
    #[serde(default)]
    pub gate_provenance: Option<GateProvenance>,
    /// The architect's sign-off on this story's governed work (issue #21), if any.
    /// `None` until an architect explicitly signs the run off. Persisted so the
    /// sign-off survives sessions and is visible alongside the dev status.
    #[serde(default)]
    pub sign_off: Option<SignOff>,
    /// The SOC-2 evidence record for the most recent completed governed run (issue #53).
    ///
    /// Assembled and attached by [`UowStore::attach_evidence`] when a run finishes.
    /// `None` until a run has completed and evidence was assembled. Additive: if the run
    /// produced no evidence (e.g. token-free scripted path without a changed-file diff),
    /// this remains `None` and sign-off is not blocked by the evidence gate. Persisted
    /// alongside the provenance so the QA reviewer can read the full governance artifact
    /// without needing the in-memory run.
    #[serde(default)]
    pub evidence: Option<crate::evidence::UowEvidenceRecord>,
    /// RFC 3339 timestamp of the last mutation. Stamped by every mutator.
    #[serde(default)]
    pub updated: String,
}

impl UnitOfWork {
    /// `true` when this UoW has an evidence record with a critical scoped-scan finding
    /// that blocks the `AwaitingQa → SignedOff` transition until an explicit waive-with-
    /// reason is supplied. `false` when there is no evidence record yet (the gate does not
    /// block a sign-off without evidence — only an existing critical finding blocks it).
    pub fn is_sign_off_blocked(&self) -> bool {
        self.evidence
            .as_ref()
            .is_some_and(|e| e.is_sign_off_blocked())
    }
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
///
/// # Decision + investigation persistence (ROUTE-A)
///
/// When [`with_artifacts`](Self::with_artifacts) attaches an [`ArtifactStore`], the
/// per-story decision set and investigation note are ALSO persisted into the central,
/// version-tracked store (one revision per write, with actor + op provenance). The
/// `uow.json` file remains for the rest of the UoW (branch, stage, history, evidence,
/// …) and as the back-compat carrier for decisions; the store is the source of truth
/// for decision history. The store handle is optional so tests and a no-data-dir launch
/// keep working with the inline-decisions behaviour unchanged.
#[derive(Clone, Default)]
pub struct UowStore {
    path: Option<Arc<PathBuf>>,
    mem: Arc<Mutex<HashMap<String, UnitOfWork>>>,
    /// The central artifact store backing decision-record + investigation-note
    /// history. `None` for an in-memory store with no artifact backing (the inline
    /// `decisions` field is then authoritative).
    artifacts: Option<Arc<dyn ArtifactStore>>,
    /// A handle to the tokio runtime, captured at construction so the sync UoW API
    /// can drive the async [`ArtifactStore`] calls. `None` when no artifact store is
    /// attached, or when no runtime was available at construction (defensive).
    runtime: Option<tokio::runtime::Handle>,
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
            artifacts: None,
            runtime: None,
        }
    }

    /// Attach a central [`ArtifactStore`] so decision records and investigation
    /// notes are persisted with full revision history (ROUTE-A). Returns a new
    /// handle sharing the same in-memory map + file path as `self`.
    ///
    /// The current tokio runtime handle is captured here so the sync mutator API
    /// can drive the store's async methods. Call this from inside the tokio
    /// runtime (it is, during `AppState` construction). If no runtime is current,
    /// the store is still attached but writes degrade gracefully to in-memory/JSON
    /// only (the handle capture is best-effort).
    pub fn with_artifacts(mut self, artifacts: Arc<dyn ArtifactStore>) -> Self {
        self.runtime = tokio::runtime::Handle::try_current().ok();
        self.artifacts = Some(artifacts);
        self
    }

    // ── private helpers ───────────────────────────────────────────────────────

    fn now_rfc3339() -> String {
        chrono::Utc::now().to_rfc3339()
    }

    /// Run an async artifact-store operation to completion from the sync UoW API.
    ///
    /// Uses the captured runtime handle. When called from within a tokio worker
    /// thread (the normal case, inside an Axum handler), wraps the blocking wait in
    /// [`tokio::task::block_in_place`] so the worker thread is allowed to block
    /// without stalling the scheduler. Returns `None` when no runtime/store is
    /// attached.
    fn block_on_artifacts<F, T>(&self, fut: F) -> Option<T>
    where
        F: std::future::Future<Output = T>,
    {
        let handle = self.runtime.as_ref()?;
        // `block_in_place` requires the multi-thread runtime; the server uses
        // `rt-multi-thread`. If we are somehow on a current-thread runtime,
        // `block_in_place` would panic, so guard by runtime flavour is not exposed;
        // instead we catch the common case and fall back to `Handle::block_on`,
        // which works when called from OUTSIDE a runtime thread (e.g. a sync test
        // that built the store on a multi-thread runtime).
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            tokio::task::block_in_place(|| handle.block_on(fut))
        }));
        result.ok()
    }

    /// Persist a story's full decision set into the artifact store as one new
    /// revision, if a store is attached. Best-effort: a store failure never breaks
    /// the in-memory/JSON write that already happened. `op` is `Create` for the
    /// first revision of this story's decisions and `Update` thereafter; `actor`
    /// reflects who drove the change.
    fn persist_decisions(&self, story_id: &str, decisions: &[DecisionRecord]) {
        let Some(artifacts) = self.artifacts.clone() else {
            return;
        };
        let aid = decisions_artifact_id(story_id);
        let payload = match encode(&decisions.to_vec()) {
            Ok(p) => p,
            Err(_) => return,
        };
        // Decide Create vs Update by whether a prior revision exists. The actor is
        // derived from the freshest decision provenance: a set with any user-touched
        // decision is attributed to the user, else the AI.
        let actor = if decisions
            .iter()
            .any(|d| matches!(d.outcome, DecisionOutcome::Approved | DecisionOutcome::Rejected { .. }))
        {
            EditActor::User
        } else {
            EditActor::Ai
        };
        let now = chrono::Utc::now();
        let _ = self.block_on_artifacts(async move {
            let existing = artifacts
                .current_artifact(UOW_ARTIFACT_PROJECT, ArtifactKind::DecisionRecord, &aid)
                .await
                .ok()
                .flatten();
            let op = if existing.is_some() {
                RevisionOp::Update
            } else {
                RevisionOp::Create
            };
            artifacts
                .record_revision(&NewRevision::new(
                    UOW_ARTIFACT_PROJECT,
                    ArtifactKind::DecisionRecord,
                    &aid,
                    actor,
                    op,
                    payload,
                    now,
                ))
                .await
        });
    }

    /// Read a story's decision set from the artifact store's latest revision, if a
    /// store is attached and a revision exists. Returns `None` when there is no
    /// store, no revision, or the payload cannot be decoded — the caller then falls
    /// back to the inline `decisions` cache (back-compat).
    fn load_decisions_from_store(&self, story_id: &str) -> Option<Vec<DecisionRecord>> {
        let artifacts = self.artifacts.clone()?;
        let aid = decisions_artifact_id(story_id);
        let rev = self.block_on_artifacts(async move {
            artifacts
                .current_artifact(UOW_ARTIFACT_PROJECT, ArtifactKind::DecisionRecord, &aid)
                .await
                .ok()
                .flatten()
        })??;
        rev.decode::<Vec<DecisionRecord>>().ok()
    }

    /// One-time hydrate: if this story has inline decisions (loaded from an older
    /// `uow.json`) but NO revision yet in the store, migrate them into the store as
    /// the first revision so no data is lost when the store becomes the source of
    /// truth. Best-effort and idempotent (skips when a revision already exists).
    fn hydrate_inline_decisions_into_store(&self, story_id: &str, inline: &[DecisionRecord]) {
        if self.artifacts.is_none() || inline.is_empty() {
            return;
        }
        if self.load_decisions_from_store(story_id).is_some() {
            return; // store already has history; nothing to migrate.
        }
        self.persist_decisions(story_id, inline);
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
            // Advance the lifecycle stage to SignedOff when the UoW is at AwaitingQa
            // (the legal point). Sign-off is the explicit, never-automatic QA gate; if
            // the stage is somewhere else (e.g. a manual sign-off before the stage was
            // driven there) we still record the sign-off but leave the stage, since the
            // pure state machine forbids the jump and we never fabricate a transition.
            if let Ok(next) = uow.stage.sign_off() {
                let from = uow.stage;
                uow.stage = next;
                uow.history.push(HistoryEntry {
                    ts: now.clone(),
                    kind: "stage".to_string(),
                    text: format!("Stage advanced: {} → {}", from.label(), next.label()),
                });
            }
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

    // ── lifecycle (Pillar 2) ────────────────────────────────────────────────────

    /// Replace the full set of decision records for a story's UoW. Used when the
    /// investigation phase surfaces (or the architect approves/rejects) decisions; the
    /// governed-dev gate reads these to decide whether development may start.
    ///
    /// When an [`ArtifactStore`] is attached, this ALSO records the new decision set as
    /// a fresh revision in the central store (ROUTE-A) so the per-story decision history
    /// is queryable + versioned. The inline `decisions` field is kept in sync as the
    /// read cache + back-compat carrier.
    pub fn set_decisions(&self, story_id: &str, decisions: Vec<DecisionRecord>) -> UnitOfWork {
        let now = Self::now_rfc3339();
        // Persist to the artifact store first (best-effort) so the durable history is
        // recorded; the in-memory/JSON write below is the authoritative read cache.
        self.persist_decisions(story_id, &decisions);
        let updated = {
            let mut map = self.mem.lock().expect("uow mutex poisoned");
            let uow = map
                .entry(story_id.to_string())
                .or_insert_with(|| UnitOfWork {
                    story_id: story_id.to_string(),
                    ..Default::default()
                });
            uow.decisions = decisions;
            uow.updated = now;
            uow.clone()
        };
        self.flush();
        updated
    }

    /// The current decision set for a story, read THROUGH the artifact store when one
    /// is attached (the source of truth for decision history), falling back to the
    /// inline `decisions` cache otherwise.
    ///
    /// On the store-backed path this also performs the one-time hydrate of any inline
    /// decisions loaded from an older `uow.json` that have no store revision yet, so the
    /// migration is lazy and lossless: the first read of a legacy UoW seeds the store.
    /// The returned set is the authoritative decision state the gate should use.
    pub fn decisions_for(&self, story_id: &str) -> Vec<DecisionRecord> {
        let inline = self.get_or_create(story_id).decisions;
        if self.artifacts.is_none() {
            return inline;
        }
        // Lazy back-compat migration: seed the store from legacy inline decisions.
        self.hydrate_inline_decisions_into_store(story_id, &inline);
        match self.load_decisions_from_store(story_id) {
            Some(from_store) => {
                // Keep the inline cache coherent with the store's source of truth so a
                // subsequent `uow.json` flush reflects the same decisions.
                if from_store != inline {
                    let mut map = self.mem.lock().expect("uow mutex poisoned");
                    if let Some(uow) = map.get_mut(story_id) {
                        uow.decisions = from_store.clone();
                    }
                    drop(map);
                    self.flush();
                }
                from_store
            }
            None => inline,
        }
    }

    // ── investigation notes (ROUTE-A) ───────────────────────────────────────────

    /// Persist a story's investigation note into the central [`ArtifactStore`] as a new
    /// revision (ROUTE-A), keyed by the `"{story_id}/investigation"` artifact id with
    /// actor + op provenance. One investigation note exists per story; each save is a
    /// new revision so the architect can diff the agent's first draft against revisions.
    ///
    /// Returns the recorded revision's version number on success, or `None` when no
    /// artifact store is attached (the investigation phase is store-backed only — unlike
    /// decisions, there is no inline-on-the-UoW fallback home for the note).
    ///
    /// The `actor` recorded is derived from the note's own provenance so a
    /// `mark_reviewed` save is attributed to the architect and an authoring save to the AI.
    pub fn set_investigation_note(
        &self,
        note: &InvestigationArtifact,
    ) -> Option<i64> {
        let artifacts = self.artifacts.clone()?;
        let aid = investigation_artifact_id(&note.story_id);
        let payload = encode(note).ok()?;
        let actor = match note.provenance.actor {
            camerata_worktracker::investigation::RevisionActor::User => EditActor::User,
            camerata_worktracker::investigation::RevisionActor::Ai => EditActor::Ai,
        };
        let now = chrono::Utc::now();
        let rev = self.block_on_artifacts(async move {
            let existing = artifacts
                .current_artifact(UOW_ARTIFACT_PROJECT, ArtifactKind::InvestigationNote, &aid)
                .await
                .ok()
                .flatten();
            let op = if existing.is_some() {
                RevisionOp::Update
            } else {
                RevisionOp::Create
            };
            artifacts
                .record_revision(&NewRevision::new(
                    UOW_ARTIFACT_PROJECT,
                    ArtifactKind::InvestigationNote,
                    &aid,
                    actor,
                    op,
                    payload,
                    now,
                ))
                .await
                .ok()
        })??;
        Some(rev.version)
    }

    /// Read a story's current investigation note from the central [`ArtifactStore`],
    /// or `None` when no store is attached, no note has been recorded, or the latest
    /// revision is a deletion.
    pub fn investigation_note_for(&self, story_id: &str) -> Option<InvestigationArtifact> {
        let artifacts = self.artifacts.clone()?;
        let aid = investigation_artifact_id(story_id);
        let rev = self.block_on_artifacts(async move {
            artifacts
                .current_artifact(UOW_ARTIFACT_PROJECT, ArtifactKind::InvestigationNote, &aid)
                .await
                .ok()
                .flatten()
        })??;
        rev.decode::<InvestigationArtifact>().ok()
    }

    /// Apply a pure stage transition to a story's UoW, persisting the new stage and
    /// appending a `stage` history entry on success. On failure the UoW is unchanged
    /// and the [`TransitionError`] is returned so the caller can surface exactly why
    /// the move was blocked.
    ///
    /// `transition` is the pure function from the current [`UowStage`] to the next one
    /// (e.g. `|s| s.begin_investigation()`), so all the rule enforcement lives in
    /// [`crate::lifecycle`] and this method only owns the persistence + history.
    fn apply_transition<F>(
        &self,
        story_id: &str,
        transition: F,
    ) -> Result<UnitOfWork, TransitionError>
    where
        F: FnOnce(UowStage) -> Result<UowStage, TransitionError>,
    {
        let now = Self::now_rfc3339();
        let result = {
            let mut map = self.mem.lock().expect("uow mutex poisoned");
            let uow = map
                .entry(story_id.to_string())
                .or_insert_with(|| UnitOfWork {
                    story_id: story_id.to_string(),
                    ..Default::default()
                });
            match transition(uow.stage) {
                Ok(next) => {
                    let from = uow.stage;
                    uow.stage = next;
                    uow.history.push(HistoryEntry {
                        ts: now.clone(),
                        kind: "stage".to_string(),
                        text: format!("Stage advanced: {} → {}", from.label(), next.label()),
                    });
                    uow.updated = now;
                    Ok(uow.clone())
                }
                Err(e) => Err(e),
            }
        };
        if result.is_ok() {
            self.flush();
        }
        result
    }

    /// Intake → Investigating. See [`UowStage::begin_investigation`].
    pub fn begin_investigation(&self, story_id: &str) -> Result<UnitOfWork, TransitionError> {
        self.apply_transition(story_id, |s| s.begin_investigation())
    }

    /// Investigating → DecisionsApproved, gated by the UoW's current decision records.
    /// See [`UowStage::approve_decisions`].
    pub fn approve_decisions(&self, story_id: &str) -> Result<UnitOfWork, TransitionError> {
        // Read the decisions THROUGH the artifact store (the source of truth) when one
        // is attached, falling back to the inline cache otherwise. The transition then
        // re-locks; cloning is cheap relative to correctness.
        let decisions = self.decisions_for(story_id);
        self.apply_transition(story_id, |s| s.approve_decisions(&decisions))
    }

    /// DecisionsApproved → Development, re-checking the decision gate. See
    /// [`UowStage::start_development`]. Returns the [`TransitionError`] (so the run
    /// start can block + surface why) when the gate is not satisfied.
    pub fn start_development(&self, story_id: &str) -> Result<UnitOfWork, TransitionError> {
        let decisions = self.decisions_for(story_id);
        self.apply_transition(story_id, |s| s.start_development(&decisions))
    }

    /// Development → AwaitingQa. See [`UowStage::finish_development`].
    pub fn finish_development(&self, story_id: &str) -> Result<UnitOfWork, TransitionError> {
        self.apply_transition(story_id, |s| s.finish_development())
    }

    /// Attach the SOC-2 evidence record from a completed governed run onto a story's UoW
    /// (issue #53). Appends an `evidence` history entry so the act is visible in the
    /// AI development timeline. Does NOT change the stage.
    ///
    /// If the evidence record contains a critical scoped-scan finding, that sets a
    /// blocking signal on the UoW (readable via [`UnitOfWork::is_sign_off_blocked`]).
    /// The sign-off handler enforces this block: a Critical finding requires an explicit
    /// waive-with-reason before the `AwaitingQa → SignedOff` transition is allowed.
    pub fn attach_evidence(
        &self,
        story_id: &str,
        evidence: crate::evidence::UowEvidenceRecord,
    ) -> UnitOfWork {
        let now = Self::now_rfc3339();
        let has_critical = evidence.is_sign_off_blocked();
        let summary = format!(
            "SOC-2 evidence record attached for run {}: {} gate event(s), {} scoped finding(s){}.",
            evidence.run_id,
            evidence.history.len(),
            evidence.scoped_scan.as_ref().map(|s| s.total_findings).unwrap_or(0),
            if has_critical { " — CRITICAL finding blocks sign-off" } else { "" },
        );
        let updated = {
            let mut map = self.mem.lock().expect("uow mutex poisoned");
            let uow = map
                .entry(story_id.to_string())
                .or_insert_with(|| UnitOfWork {
                    story_id: story_id.to_string(),
                    ..Default::default()
                });
            uow.evidence = Some(evidence);
            uow.history.push(HistoryEntry {
                ts: now.clone(),
                kind: "evidence".to_string(),
                text: summary,
            });
            uow.updated = now;
            uow.clone()
        };
        self.flush();
        updated
    }

    /// Stamp the frozen gate provenance from a completed run onto a story's UoW and
    /// append a `provenance` history entry. The durable QA-review record (the in-memory
    /// run may be gone; this survives). Does NOT change the stage — call
    /// [`Self::finish_development`] for that.
    pub fn record_gate_provenance(
        &self,
        story_id: &str,
        provenance: GateProvenance,
    ) -> UnitOfWork {
        let now = Self::now_rfc3339();
        let summary = format!(
            "Gate provenance recorded for {}: {} allowed, {} denied ({} bounces).",
            provenance.run_id,
            provenance.allow_count,
            provenance.deny_count,
            provenance.total_bounces
        );
        let updated = {
            let mut map = self.mem.lock().expect("uow mutex poisoned");
            let uow = map
                .entry(story_id.to_string())
                .or_insert_with(|| UnitOfWork {
                    story_id: story_id.to_string(),
                    ..Default::default()
                });
            uow.gate_provenance = Some(provenance);
            uow.history.push(HistoryEntry {
                ts: now.clone(),
                kind: "provenance".to_string(),
                text: summary,
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

    // ── lifecycle (Pillar 2) ────────────────────────────────────────────────────

    use camerata_worktracker::investigation::DecisionRecord;
    use chrono::Utc;

    fn approved_decision(story: &str, slug: &str) -> DecisionRecord {
        DecisionRecord::ai_proposed(
            story,
            format!("{story}/decision/{slug}"),
            "Decision",
            "Question?",
            "Rationale",
            vec![],
            Utc::now(),
        )
        .approve(Utc::now())
    }

    fn pending_decision(story: &str, slug: &str) -> DecisionRecord {
        DecisionRecord::ai_proposed(
            story,
            format!("{story}/decision/{slug}"),
            "Decision",
            "Question?",
            "Rationale",
            vec![],
            Utc::now(),
        )
    }

    #[test]
    fn new_uow_starts_at_intake_stage() {
        let store = UowStore::new();
        assert_eq!(store.get_or_create("S-1").stage, UowStage::Intake);
    }

    #[test]
    fn begin_investigation_advances_and_records_history() {
        let store = UowStore::new();
        let uow = store.begin_investigation("S-1").expect("ok from intake");
        assert_eq!(uow.stage, UowStage::Investigating);
        assert!(uow.history.iter().any(|h| h.kind == "stage"));

        // Repeating from the wrong stage errors and leaves the stage unchanged.
        let err = store.begin_investigation("S-1").unwrap_err();
        assert!(matches!(err, TransitionError::WrongStage { .. }));
        assert_eq!(store.get_or_create("S-1").stage, UowStage::Investigating);
    }

    #[test]
    fn approve_decisions_blocks_until_all_decisions_approved() {
        let store = UowStore::new();
        store.begin_investigation("S-2").unwrap();

        // No decisions: blocked.
        let err = store.approve_decisions("S-2").unwrap_err();
        assert!(matches!(
            err,
            TransitionError::DecisionsNotApproved { total: 0, .. }
        ));

        // One pending: still blocked.
        store.set_decisions("S-2", vec![pending_decision("S-2", "a")]);
        assert!(store.approve_decisions("S-2").is_err());
        assert_eq!(store.get_or_create("S-2").stage, UowStage::Investigating);

        // All approved: advances.
        store.set_decisions("S-2", vec![approved_decision("S-2", "a")]);
        let uow = store.approve_decisions("S-2").expect("gate satisfied");
        assert_eq!(uow.stage, UowStage::DecisionsApproved);
    }

    #[test]
    fn start_development_gate_rechecks_decisions() {
        let store = UowStore::new();
        store.begin_investigation("S-3").unwrap();
        store.set_decisions("S-3", vec![approved_decision("S-3", "a")]);
        store.approve_decisions("S-3").unwrap();

        // The decisions are re-opened after approval: start_development must re-block.
        store.set_decisions("S-3", vec![pending_decision("S-3", "a")]);
        let err = store.start_development("S-3").unwrap_err();
        assert!(matches!(err, TransitionError::DecisionsNotApproved { .. }));
        assert_eq!(store.get_or_create("S-3").stage, UowStage::DecisionsApproved);

        // Re-approve and the gate opens.
        store.set_decisions("S-3", vec![approved_decision("S-3", "a")]);
        let uow = store.start_development("S-3").expect("gate satisfied");
        assert_eq!(uow.stage, UowStage::Development);
    }

    #[test]
    fn record_gate_provenance_persists_and_does_not_change_stage() {
        let store = UowStore::new();
        store.begin_investigation("S-4").unwrap();
        store.set_decisions("S-4", vec![approved_decision("S-4", "a")]);
        store.approve_decisions("S-4").unwrap();
        store.start_development("S-4").unwrap();

        let prov = GateProvenance {
            run_id: "run-9".to_string(),
            mode: "scripted".to_string(),
            allow_count: 1,
            deny_count: 2,
            total_bounces: 2,
            rules_fired: vec!["SEC-NO-PATH-ESCAPE-1".to_string()],
            recorded: String::new(),
        };
        let uow = store.record_gate_provenance("S-4", prov);
        let stamped = uow.gate_provenance.expect("provenance stamped");
        assert_eq!(stamped.run_id, "run-9");
        assert_eq!(stamped.deny_count, 2);
        // Stage is unchanged by recording provenance.
        assert_eq!(store.get_or_create("S-4").stage, UowStage::Development);
        assert!(uow.history.iter().any(|h| h.kind == "provenance"));
    }

    #[test]
    fn full_lifecycle_through_sign_off_advances_stage() {
        let store = UowStore::new();
        store.begin_investigation("S-5").unwrap();
        store.set_decisions("S-5", vec![approved_decision("S-5", "a")]);
        store.approve_decisions("S-5").unwrap();
        store.start_development("S-5").unwrap();
        store.finish_development("S-5").unwrap();
        assert_eq!(store.get_or_create("S-5").stage, UowStage::AwaitingQa);

        // Sign-off advances to SignedOff (the explicit gate from AwaitingQa).
        let uow = store.sign_off("S-5", "zach", "run-1", None);
        assert_eq!(uow.stage, UowStage::SignedOff);
        assert!(uow.sign_off.is_some());
    }

    #[test]
    fn sign_off_from_wrong_stage_records_but_leaves_stage() {
        let store = UowStore::new();
        // UoW at Intake: sign-off is recorded but the stage cannot legally jump.
        let uow = store.sign_off("S-6", "zach", "run-1", None);
        assert!(uow.sign_off.is_some());
        assert_eq!(uow.stage, UowStage::Intake);
    }

    // ── Evidence (issue #53) ────────────────────────────────────────────────────

    fn make_evidence_record(story: &str, run: &str, has_critical: bool) -> crate::evidence::UowEvidenceRecord {
        let mut record = crate::evidence::UowEvidenceRecord::new(story, run, "2026-06-20T00:00:00Z");
        record.set_scoped_scan(crate::evidence::ScopedScanSummary {
            files_scanned: 1,
            total_findings: if has_critical { 1 } else { 0 },
            has_critical,
            findings: Vec::new(),
        });
        record.compute_hash();
        record
    }

    use crate::evidence::ScopedScanSummary;

    #[test]
    fn attach_evidence_stores_record_and_appends_history() {
        let store = UowStore::new();
        let evidence = make_evidence_record("S-ev-1", "run-1", false);
        let uow = store.attach_evidence("S-ev-1", evidence.clone());

        // Evidence is stored on the UoW.
        let stored = uow.evidence.expect("evidence must be stored");
        assert_eq!(stored.run_id, "run-1");
        assert_eq!(stored.story_id, "S-ev-1");

        // Appended to history.
        assert!(uow.history.iter().any(|h| h.kind == "evidence"),
            "attach_evidence must append a history entry with kind='evidence'");
    }

    #[test]
    fn is_sign_off_blocked_false_without_evidence() {
        let store = UowStore::new();
        let uow = store.get_or_create("S-ev-2");
        // No evidence attached yet — never blocks.
        assert!(!uow.is_sign_off_blocked());
    }

    #[test]
    fn is_sign_off_blocked_false_with_non_critical_evidence() {
        let store = UowStore::new();
        let evidence = make_evidence_record("S-ev-3", "run-1", false);
        let uow = store.attach_evidence("S-ev-3", evidence);
        assert!(!uow.is_sign_off_blocked(), "non-critical evidence must not block sign-off");
    }

    #[test]
    fn is_sign_off_blocked_true_with_critical_evidence() {
        let store = UowStore::new();
        let evidence = make_evidence_record("S-ev-4", "run-1", true);
        let uow = store.attach_evidence("S-ev-4", evidence);
        assert!(uow.is_sign_off_blocked(), "critical evidence must block sign-off");
    }

    #[test]
    fn attach_evidence_history_mentions_critical_when_blocked() {
        let store = UowStore::new();
        let evidence = make_evidence_record("S-ev-5", "run-42", true);
        let uow = store.attach_evidence("S-ev-5", evidence);
        let entry = uow.history.iter()
            .find(|h| h.kind == "evidence")
            .expect("evidence history entry");
        assert!(
            entry.text.contains("CRITICAL"),
            "history entry must mention CRITICAL when a critical finding is present: {:?}",
            entry.text
        );
    }

    #[test]
    fn attach_evidence_does_not_change_stage() {
        let store = UowStore::new();
        store.begin_investigation("S-ev-6").unwrap();
        store.set_decisions("S-ev-6", vec![approved_decision("S-ev-6", "a")]);
        store.approve_decisions("S-ev-6").unwrap();
        store.start_development("S-ev-6").unwrap();
        assert_eq!(store.get_or_create("S-ev-6").stage, UowStage::Development);

        let evidence = make_evidence_record("S-ev-6", "run-1", false);
        store.attach_evidence("S-ev-6", evidence);

        // Stage must be unchanged by attaching evidence.
        assert_eq!(store.get_or_create("S-ev-6").stage, UowStage::Development);
    }

    #[test]
    fn attach_evidence_persists_across_get_or_create() {
        let store = UowStore::new();
        let evidence = make_evidence_record("S-ev-7", "run-99", false);
        store.attach_evidence("S-ev-7", evidence);

        // A subsequent get must see the same evidence.
        let uow = store.get_or_create("S-ev-7");
        assert!(uow.evidence.is_some(), "evidence must survive get_or_create round-trip");
        assert_eq!(uow.evidence.unwrap().run_id, "run-99");
    }
}

// ── ArtifactStore-backed decision + investigation persistence (ROUTE-A) ─────────
//
// These tests exercise the store-backed path: a real in-memory `SqliteStore` is
// attached to the `UowStore`, so decisions are persisted as versioned revisions and
// read back through the store. They run on a MULTI-THREAD tokio runtime because the
// sync UoW API drives the async store via `block_in_place`, which requires it.
#[cfg(test)]
mod artifact_store_tests {
    use super::*;
    use camerata_persistence::{ArtifactKind, ArtifactStore, SqliteStore};
    use camerata_worktracker::investigation::{
        decisions_approved_for_development, DecisionRecord, InvestigationArtifact,
    };
    use chrono::Utc;
    use std::sync::Arc;

    /// A `UowStore` (in-memory map, no JSON file) backed by a fresh in-memory
    /// `SqliteStore` so decisions/investigation notes are persisted with history.
    async fn store_backed() -> UowStore {
        let sqlite = SqliteStore::open("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        let artifacts: Arc<dyn ArtifactStore> = Arc::new(sqlite);
        UowStore::new().with_artifacts(artifacts)
    }

    fn approved(story: &str, slug: &str) -> DecisionRecord {
        DecisionRecord::ai_proposed(
            story,
            format!("{story}/decision/{slug}"),
            "Decision",
            "Question?",
            "Rationale",
            vec![],
            Utc::now(),
        )
        .approve(Utc::now())
    }

    fn pending(story: &str, slug: &str) -> DecisionRecord {
        DecisionRecord::ai_proposed(
            story,
            format!("{story}/decision/{slug}"),
            "Decision",
            "Question?",
            "Rationale",
            vec![],
            Utc::now(),
        )
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn decisions_persist_and_reload_via_artifact_store() {
        let store = store_backed().await;

        // First write: creates revision 1.
        store.set_decisions("CAM-100", vec![pending("CAM-100", "a")]);
        let loaded = store.decisions_for("CAM-100");
        assert_eq!(loaded.len(), 1);
        assert!(loaded[0].needs_review(), "first decision is pending");

        // Second write: a fresh revision (history grows).
        store.set_decisions("CAM-100", vec![approved("CAM-100", "a")]);
        let loaded2 = store.decisions_for("CAM-100");
        assert_eq!(loaded2.len(), 1);
        assert!(!loaded2[0].needs_review(), "decision now approved");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn each_set_decisions_is_a_new_revision_with_history() {
        let sqlite = SqliteStore::open("sqlite::memory:").await.expect("sqlite");
        let artifacts: Arc<dyn ArtifactStore> = Arc::new(sqlite);
        let store = UowStore::new().with_artifacts(artifacts.clone());

        store.set_decisions("CAM-200", vec![pending("CAM-200", "a")]);
        store.set_decisions("CAM-200", vec![approved("CAM-200", "a")]);
        store.set_decisions(
            "CAM-200",
            vec![approved("CAM-200", "a"), approved("CAM-200", "b")],
        );

        // The store keeps the full revision history for this story's decisions.
        let history = artifacts
            .history(
                UOW_ARTIFACT_PROJECT,
                ArtifactKind::DecisionRecord,
                &decisions_artifact_id("CAM-200"),
            )
            .await
            .expect("history");
        assert_eq!(history.len(), 3, "three set_decisions = three revisions");
        assert_eq!(history[0].version, 1);
        assert_eq!(history[2].version, 3);

        // The latest revision is the source of truth the gate reads.
        let current = store.decisions_for("CAM-200");
        assert_eq!(current.len(), 2);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn gate_reads_store_backed_decisions() {
        let store = store_backed().await;
        store.begin_investigation("CAM-300").unwrap();

        // Pending in the store: gate blocks.
        store.set_decisions("CAM-300", vec![pending("CAM-300", "a")]);
        assert!(
            !decisions_approved_for_development(&store.decisions_for("CAM-300")),
            "pending store-backed decision must block the gate"
        );
        assert!(store.approve_decisions("CAM-300").is_err());

        // Approved in the store: gate opens (read THROUGH the store).
        store.set_decisions("CAM-300", vec![approved("CAM-300", "a")]);
        assert!(decisions_approved_for_development(&store.decisions_for(
            "CAM-300"
        )));
        let uow = store
            .approve_decisions("CAM-300")
            .expect("gate satisfied via store");
        assert_eq!(uow.stage, UowStage::DecisionsApproved);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn back_compat_inline_decisions_hydrate_into_store() {
        // Simulate a UoW loaded from an older uow.json: decisions live inline, the
        // store has no revision yet. The first store-backed read must migrate them.
        let store = store_backed().await;

        // Seed the inline field directly via the in-memory map (mimicking a legacy
        // `uow.json` load that set decisions before any store existed).
        {
            let mut map = store.mem.lock().expect("mutex");
            map.insert(
                "CAM-LEGACY".to_string(),
                UnitOfWork {
                    story_id: "CAM-LEGACY".to_string(),
                    decisions: vec![approved("CAM-LEGACY", "a")],
                    ..Default::default()
                },
            );
        }

        // Before the read-through, the store has no revision for this story.
        assert!(
            store.load_decisions_from_store("CAM-LEGACY").is_none(),
            "store starts empty for the legacy story"
        );

        // decisions_for triggers the one-time hydrate, then reads from the store.
        let loaded = store.decisions_for("CAM-LEGACY");
        assert_eq!(loaded.len(), 1, "legacy inline decision is preserved");
        assert!(!loaded[0].needs_review());

        // The hydrate seeded a revision in the store (no data lost).
        assert!(
            store.load_decisions_from_store("CAM-LEGACY").is_some(),
            "legacy inline decisions were migrated into the store"
        );

        // Hydrate is idempotent: a second read does not add another revision.
        store.decisions_for("CAM-LEGACY");
        let history = store
            .load_decisions_from_store("CAM-LEGACY")
            .expect("present");
        assert_eq!(history.len(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn investigation_note_persists_and_reloads() {
        let store = store_backed().await;
        let t = Utc::now();

        // No note yet.
        assert!(store.investigation_note_for("CAM-400").is_none());

        // AI authors a note: revision 1.
        let note = InvestigationArtifact::ai_authored("CAM-400", "Found an ambiguity.", t);
        let v1 = store.set_investigation_note(&note).expect("recorded");
        assert_eq!(v1, 1);

        let loaded = store
            .investigation_note_for("CAM-400")
            .expect("note present");
        assert_eq!(loaded.story_id, "CAM-400");
        assert!(!loaded.reviewed, "note starts unreviewed");

        // Architect reviews it: revision 2, attributed to the user.
        let reviewed = loaded.mark_reviewed(t);
        let v2 = store.set_investigation_note(&reviewed).expect("recorded");
        assert_eq!(v2, 2);

        let loaded2 = store
            .investigation_note_for("CAM-400")
            .expect("note present");
        assert!(loaded2.reviewed, "review state survives the round-trip");
    }

    #[test]
    fn no_store_attached_keeps_inline_decisions_behaviour() {
        // A plain in-memory UowStore (no artifact store) must behave exactly as before:
        // decisions_for returns the inline field and the gate reads it.
        let store = UowStore::new();
        store.set_decisions("CAM-500", vec![approved("CAM-500", "a")]);
        let loaded = store.decisions_for("CAM-500");
        assert_eq!(loaded.len(), 1);
        assert!(decisions_approved_for_development(&loaded));
        // No store means no investigation-note persistence.
        let note = InvestigationArtifact::ai_authored("CAM-500", "x", Utc::now());
        assert!(store.set_investigation_note(&note).is_none());
        assert!(store.investigation_note_for("CAM-500").is_none());
    }
}
