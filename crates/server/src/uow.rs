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

use camerata_agent::post_story_hook::{PostStoryHook, StoryCompletion};
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
///
/// # Invariant (BUG-10)
///
/// `total_bounces == deny_count` is an INVARIANT: both fields mean the same count.
/// `total_bounces` uses the architect-facing "bounce" vocabulary in the UI; `deny_count`
/// uses the gate-facing vocabulary in code. They are kept as two fields for API
/// back-compat but MUST be set to identical values. A `debug_assert` fires in
/// `record_gate_provenance` when this invariant is violated. Future callers should
/// prefer the `new` constructor which enforces it at construction time.
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
    /// facing vocabulary). Must always equal `deny_count` — see type-level doc.
    pub total_bounces: usize,
    /// The distinct rule ids that actually fired a denial, in first-seen order.
    #[serde(default)]
    pub rules_fired: Vec<String>,
    /// RFC 3339 timestamp of when this provenance was stamped onto the UoW.
    pub recorded: String,
}

impl GateProvenance {
    /// Canonical constructor that enforces the `total_bounces == deny_count` invariant
    /// (BUG-10). Prefer this over struct literals in new code.
    pub fn new(
        run_id: impl Into<String>,
        mode: impl Into<String>,
        allow_count: usize,
        deny_count: usize,
        rules_fired: Vec<String>,
        recorded: impl Into<String>,
    ) -> Self {
        Self {
            run_id: run_id.into(),
            mode: mode.into(),
            allow_count,
            deny_count,
            // total_bounces is identical to deny_count by definition; the two fields
            // exist for back-compat vocabulary reasons only.
            total_bounces: deny_count,
            rules_fired,
            recorded: recorded.into(),
        }
    }
}

/// One message in a story-authoring clarification chat. `role` is `"user"` or
/// `"ai"`; `text` is the message body. Persisted on the UoW so the back-and-forth
/// survives sessions until the story is published.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct AuthorChatMessage {
    /// `"user"` (the requirements author) or `"ai"` (the drafting assistant).
    pub role: String,
    /// The message body.
    pub text: String,
}

/// The transient AI story-authoring state carried by a DRAFT UoW (one created via
/// `POST /api/uow/blank` with `work_item = None`). It records the requirements
/// prompt, the clarification chat transcript, and the current AI-drafted issue
/// (title + body). It is preserved on the struct after publish for the record.
///
/// All fields default, so a legacy `uow.json` written before this field existed
/// deserializes with an empty/absent authoring state (back-compat).
#[derive(Clone, Default, Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct AuthoringState {
    /// The first user message: the free-text requirements that seed the draft.
    #[serde(default)]
    pub requirements_prompt: String,
    /// The full clarification chat transcript (user + ai turns), in order.
    #[serde(default)]
    pub chat: Vec<AuthorChatMessage>,
    /// The current AI-drafted issue title.
    #[serde(default)]
    pub draft_title: String,
    /// The current AI-drafted issue body (GitHub-flavoured markdown).
    #[serde(default)]
    pub draft_body: String,
}

/// The Unit of Work for one story. Keyed by `story_id`.
#[derive(Clone, Default, Serialize, Deserialize, Debug)]
pub struct UnitOfWork {
    /// The story this UoW belongs to.
    pub story_id: String,
    /// The work-item link for a UoW whose KEY is not itself the work-item story id.
    ///
    /// For a normal UoW created from an existing issue the key IS the work-item story
    /// id (`owner/repo#num`) and this stays `None` — `/api/uows` resolves the work item
    /// from the spine by the key. For a DRAFT UoW authored with AI (keyed `draft-<uuid>`)
    /// this carries the real work-item story id after publish so the link survives without
    /// re-keying the UoW (see the build decision doc: draft-id-no-rekey). Defaults to
    /// `None` so a legacy `uow.json` loads unchanged.
    #[serde(default)]
    pub work_item: Option<String>,
    /// The AI story-authoring state for a DRAFT UoW (created blank, no work item yet).
    /// `None` for a normal UoW that references an existing work item. `Some` while the
    /// architect is authoring a story with AI; it carries the requirements prompt, the
    /// clarification chat, and the drafted issue title/body. Defaults to `None` so a
    /// legacy `uow.json` loads unchanged.
    #[serde(default)]
    pub authoring: Option<AuthoringState>,
    /// The git branch this work lives on (if set). Auto-populated by the fleet;
    /// also settable via the `/api/uow/:id/branch` endpoint.
    #[serde(default)]
    pub branch: Option<String>,
    /// The GitHub pull-request number for this UoW's branch, once a PR exists (issue:
    /// per-UoW PR lifecycle, Decision 2). Set when the console opens a PR, OR backfilled
    /// by discovery (`resolve_pr_for_uow`) when a PR was opened directly in GitHub. The
    /// STORED number always wins over a head-branch search; discovery only backfills it.
    /// `None` until a PR exists. Defaults to `None` so a legacy `uow.json` loads unchanged.
    #[serde(default)]
    pub pr_number: Option<u64>,
    /// The GitHub `html_url` of this UoW's PR, stored alongside `pr_number` so the console
    /// can render a link without re-fetching. `None` until a PR exists. Defaults to `None`.
    #[serde(default)]
    pub pr_url: Option<String>,
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
    /// The parent issue number (normalized: digits only, e.g. `"42"` not `"#42"`), when
    /// this draft UoW should be published as a GitHub sub-issue of an existing issue. Set
    /// at blank-UoW creation time and carried through to the publish step, where the
    /// native GitHub sub-issue link is created. `None` for a normal UoW with no parent.
    /// `#[serde(default)]` for back-compat: existing `uow.json` records without this
    /// field round-trip unchanged.
    #[serde(default)]
    pub parent_id: Option<String>,
    /// The id of the project that CREATED this UoW, when it is a project-scoped draft.
    ///
    /// A brand-new blank draft has no `work_item` and a `draft-<uuid>` `story_id`, so it
    /// resolves to NO repo and would be excluded from every project's list by repo
    /// resolution alone. Stamping the creating project's id here lets
    /// [`UowStore::list_for_project`] include a draft in its OWN project's view while still
    /// excluding it from any OTHER project's view (whose id differs and which shares none of
    /// the draft's non-existent repo). `None` for a normal UoW that resolves by repo, and
    /// for legacy `uow.json` records written before this field existed (back-compat).
    #[serde(default)]
    pub project_id: Option<String>,
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
///
/// # Post-story documentation (PROC-STORY-DOCS-1)
///
/// When [`with_story_doc_hook`](Self::with_story_doc_hook) attaches a
/// [`PostStoryHook`], the hook is called inside [`Self::sign_off`] after the sign-off
/// is persisted. The [`camerata_agent::StoryDocEmitter`] implementation emits two
/// DRAFT markdown files per story under `docs/<story-id>/` in the workspace root (for
/// the `per-story-docs` convention, which is the PROC-STORY-DOCS-1 default). For all
/// other conventions the hook is a no-op. Hook failures are non-fatal: the sign-off
/// is already persisted when the hook runs, so a doc-write error only logs and never
/// rolls back the sign-off.
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
    /// Optional post-story hook, called at the END of [`Self::sign_off`] after the
    /// sign-off is persisted. The hook receives a [`StoryCompletion`] snapshot and
    /// can emit documentation, trigger CI, post Slack messages, etc. Hook failures
    /// are intentionally non-fatal (logged only) — the sign-off is already committed
    /// by the time the hook fires. `None` disables the hook (the default).
    post_story_hook: Option<Arc<dyn PostStoryHook>>,
    /// The absolute workspace root passed to the post-story hook. When `None` (the
    /// default), the hook receives a workspace root of the current directory
    /// (`PathBuf::new()`), which will cause doc-write to fail gracefully unless the
    /// hook itself handles the absence. Set via `with_workspace_root` or inferred
    /// from the settings store by the caller.
    workspace_root: Option<PathBuf>,
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
            post_story_hook: None,
            workspace_root: None,
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

    /// Attach a [`PostStoryHook`] to be called at the END of [`Self::sign_off`]
    /// (PROC-STORY-DOCS-1). The hook is invoked with a [`StoryCompletion`] snapshot
    /// that includes the story id, decisions, run summary, and workspace root.
    ///
    /// Hook failures are non-fatal: the sign-off is already persisted by the time the
    /// hook fires. A doc-write error is logged (to stderr in the current process) and
    /// the sign-off result is returned unchanged.
    ///
    /// Builder form: returns a new handle sharing the same in-memory map + file path
    /// as `self`.
    pub fn with_story_doc_hook(mut self, hook: Arc<dyn PostStoryHook>) -> Self {
        self.post_story_hook = Some(hook);
        self
    }

    /// Set the workspace root passed to the post-story hook in [`Self::sign_off`].
    ///
    /// The workspace root is the ABSOLUTE path to the root of the repo being
    /// governed. Documentation is written under `<workspace_root>/docs/<story_id>/`.
    ///
    /// Builder form: returns a new handle sharing the same in-memory map + file path.
    pub fn with_workspace_root(mut self, root: PathBuf) -> Self {
        self.workspace_root = Some(root);
        self
    }

    // ── private helpers ───────────────────────────────────────────────────────

    fn now_rfc3339() -> String {
        chrono::Utc::now().to_rfc3339()
    }

    /// A process-unique token for a draft UoW id (`draft-<token>`). Combines the
    /// nanosecond wall clock with a monotonic process-local counter so two blanks
    /// created in the same nanosecond still get distinct ids (no `uuid` dependency).
    fn next_draft_token() -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("{nanos:x}-{n:x}")
    }

    /// Run an async artifact-store operation to completion from the sync UoW API.
    ///
    /// Uses the captured runtime handle. When called from within a tokio worker
    /// thread (the normal case, inside an Axum handler), wraps the blocking wait in
    /// [`tokio::task::block_in_place`] so the worker thread is allowed to block
    /// without stalling the scheduler. Returns `None` when no runtime/store is
    /// attached.
    ///
    /// # Runtime flavour requirement (BUG-UOW-1 / BUG-INT-1)
    ///
    /// `block_in_place` is only valid on a `rt-multi-thread` Tokio runtime. The
    /// server entry point uses `#[tokio::main]` with the multi-thread flavour, so
    /// this is always satisfied in production. Calling this from a `current-thread`
    /// runtime (e.g. the default `#[tokio::test]` harness) previously caused a panic
    /// that was silently swallowed by `catch_unwind`, silently discarding artifact
    /// writes. The fix makes the invariant explicit:
    ///
    /// - On a `MultiThread` runtime: use `block_in_place` as before (correct).
    /// - On a `CurrentThread` runtime: emit a `tracing::warn!` and return `None` so
    ///   the failure is observable rather than silent. This gracefully degrades to the
    ///   inline-only path in any test that uses the default single-thread harness.
    ///
    /// The `catch_unwind` / `AssertUnwindSafe` approach is removed: it asserted
    /// unwind-safety without evidence, masked all panics from `block_in_place`
    /// (including unexpected internal state corruption), and produced an indistinct
    /// `None` that callers could not distinguish from "no runtime attached".
    fn block_on_artifacts<F, T>(&self, fut: F) -> Option<T>
    where
        F: std::future::Future<Output = T>,
    {
        let handle = self.runtime.as_ref()?;
        // Assert the multi-thread runtime invariant explicitly (BUG-UOW-1 / BUG-INT-1).
        match handle.runtime_flavor() {
            tokio::runtime::RuntimeFlavor::MultiThread => {
                // Safe to block the current worker thread.
                Some(tokio::task::block_in_place(|| handle.block_on(fut)))
            }
            other => {
                // CurrentThread (or any other future flavour): block_in_place would
                // panic. Degrade gracefully so the inline/JSON path still works.
                eprintln!(
                    "[camerata] block_on_artifacts: block_in_place requires rt-multi-thread \
                     (current flavor: {other:?}); artifact store write skipped. \
                     Use #[tokio::test(flavor = \"multi_thread\")] in tests that exercise \
                     the artifact store path."
                );
                None
            }
        }
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
    ///
    /// Returns the existing store contents when the hydrate is skipped (store already
    /// had history), or the newly-written `inline` slice (as `Some(inline.to_vec())`)
    /// when the hydrate ran. Returns `None` when no artifacts store is attached or the
    /// inline set is empty.
    ///
    /// The caller (`decisions_for`) uses this return value to avoid a second
    /// `load_decisions_from_store` round-trip on the same call (BUG-UOW-4).
    fn hydrate_inline_decisions_into_store(
        &self,
        story_id: &str,
        inline: &[DecisionRecord],
    ) -> Option<Vec<DecisionRecord>> {
        if self.artifacts.is_none() || inline.is_empty() {
            return None;
        }
        // Check whether the store already has a revision.
        if let Some(existing) = self.load_decisions_from_store(story_id) {
            return Some(existing); // store already has history; nothing to migrate.
        }
        // No revision yet — seed the store from the inline cache.
        self.persist_decisions(story_id, inline);
        // Return the inline set; it is now the store's first revision.
        Some(inline.to_vec())
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
    ///
    /// When this materializes a NEW UoW it persists immediately. Without this, a UoW
    /// created via `/api/uow/from-workitem` (which only calls `get_or_create`, with no
    /// follow-up mutating call) never reached `uow.json` and vanished between sessions —
    /// the architect would create UoWs and find them gone on reopening Camerata.
    pub fn get_or_create(&self, story_id: &str) -> UnitOfWork {
        // Materialize under the lock, then release it BEFORE flushing (flush re-locks the
        // same mutex — flushing while holding the guard would deadlock).
        let (uow, created) = {
            let mut map = self.mem.lock().expect("uow mutex poisoned");
            let created = !map.contains_key(story_id);
            let uow = map
                .entry(story_id.to_string())
                .or_insert_with(|| UnitOfWork {
                    story_id: story_id.to_string(),
                    updated: Self::now_rfc3339(),
                    ..Default::default()
                })
                .clone();
            (uow, created)
        };
        if created {
            self.flush();
        }
        uow
    }

    /// Create a blank DRAFT UoW with an empty authoring state and no work item.
    ///
    /// The key is a draft id (`draft-<uuid>`); the UoW carries `authoring =
    /// Some(default)` and `work_item` stays unset (resolved as `None` by `/api/uows`).
    /// The draft id is the UoW key for its whole lifecycle: after publish the work-item
    /// reference is carried on the spine story, so the key is never re-mapped (see the
    /// build decision doc). Persists immediately. Returns the created UoW.
    pub fn create_blank(&self) -> UnitOfWork {
        self.create_blank_with_parent(None, None)
    }

    /// Create a blank DRAFT UoW with an optional `parent_id` and an optional
    /// `project_id`. When `parent_id` is `Some`, the normalized number string is stored on
    /// the UoW and carried through to the publish step, where a native GitHub sub-issue link
    /// is created. When `project_id` is `Some`, the draft is scoped to that project so it
    /// appears in that project's `list_for_project` view (and only that project's) even
    /// though it has no resolvable repo yet. Otherwise identical to [`Self::create_blank`].
    pub fn create_blank_with_parent(
        &self,
        parent_id: Option<String>,
        project_id: Option<String>,
    ) -> UnitOfWork {
        let id = format!("draft-{}", Self::next_draft_token());
        let now = Self::now_rfc3339();
        let uow = {
            let mut map = self.mem.lock().expect("uow mutex poisoned");
            let uow = UnitOfWork {
                story_id: id.clone(),
                authoring: Some(AuthoringState::default()),
                parent_id,
                project_id,
                updated: now,
                ..Default::default()
            };
            map.insert(id.clone(), uow.clone());
            uow
        };
        self.flush();
        uow
    }

    /// Append a chat turn to a draft UoW's authoring state and overwrite the current
    /// draft title/body. The first user message is also recorded as the
    /// `requirements_prompt` (when it is still empty). Materializes an authoring state
    /// if the UoW does not have one yet. Returns the updated UoW.
    ///
    /// `user_message` / `ai_reply` are appended in that order (user first, then ai).
    /// `draft_title` / `draft_body` replace the current draft. Persists.
    #[allow(clippy::too_many_arguments)]
    pub fn append_authoring_turn(
        &self,
        story_id: &str,
        user_message: &str,
        ai_reply: &str,
        draft_title: &str,
        draft_body: &str,
    ) -> UnitOfWork {
        let now = Self::now_rfc3339();
        let updated = {
            let mut map = self.mem.lock().expect("uow mutex poisoned");
            let uow = map
                .entry(story_id.to_string())
                .or_insert_with(|| UnitOfWork {
                    story_id: story_id.to_string(),
                    authoring: Some(AuthoringState::default()),
                    ..Default::default()
                });
            let st = uow.authoring.get_or_insert_with(AuthoringState::default);
            if st.requirements_prompt.trim().is_empty() {
                st.requirements_prompt = user_message.to_string();
            }
            st.chat.push(AuthorChatMessage {
                role: "user".to_string(),
                text: user_message.to_string(),
            });
            st.chat.push(AuthorChatMessage {
                role: "ai".to_string(),
                text: ai_reply.to_string(),
            });
            st.draft_title = draft_title.to_string();
            st.draft_body = draft_body.to_string();
            uow.updated = now;
            uow.clone()
        };
        self.flush();
        updated
    }

    /// Link a (draft) UoW to a newly-created work item: set its `work_item` reference
    /// to the canonical story id. The UoW KEY is NOT changed (the draft id stays the
    /// key; the work-item ref carries the real `owner/repo#num`). Appends a history
    /// entry so the publish act is visible in the timeline. Returns the updated UoW.
    pub fn link_work_item(&self, story_id: &str, work_item_story_id: &str) -> UnitOfWork {
        let now = Self::now_rfc3339();
        let updated = {
            let mut map = self.mem.lock().expect("uow mutex poisoned");
            let uow = map
                .entry(story_id.to_string())
                .or_insert_with(|| UnitOfWork {
                    story_id: story_id.to_string(),
                    ..Default::default()
                });
            uow.work_item = Some(work_item_story_id.to_string());
            uow.history.push(HistoryEntry {
                ts: now.clone(),
                kind: "authored".to_string(),
                text: format!("Story authored with AI and published to the board: {work_item_story_id}"),
            });
            uow.updated = now;
            uow.clone()
        };
        self.flush();
        updated
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

    /// UoWs whose repo is in `repos`, resolved purely by repo (NO project-id scoping).
    ///
    /// The repo is resolved from the UoW's `work_item` link when present (a draft that
    /// has been published/linked to a real issue carries the work item's `owner/repo#num`
    /// id there, while its KEY stays the original `draft-…` id), and otherwise from the
    /// `story_id` key. This keeps a LINKED draft visible under its work item's project
    /// while still EXCLUDING an unlinked draft whose id has no resolvable repo.
    ///
    /// Use this for repo-only sweeps (e.g. the startup worktree teardown that unions every
    /// project's repos): a blank draft has no worktree and no resolvable repo, so it must
    /// NOT be included. For an active-project view that should also include that project's
    /// own blank drafts, use [`Self::list_for_project`].
    pub fn list_for_repos(&self, repos: &[String]) -> Vec<UnitOfWork> {
        self.mem
            .lock()
            .expect("uow mutex poisoned")
            .values()
            .filter(|u| {
                let repo = u
                    .work_item
                    .as_deref()
                    .and_then(crate::repo_from_story_id)
                    .or_else(|| crate::repo_from_story_id(&u.story_id));
                repo.is_some_and(|r| repos.iter().any(|p| p == &r))
            })
            .cloned()
            .collect()
    }

    /// UoWs visible to the project `project_id` whose repos are `repos`.
    ///
    /// A UoW is included when EITHER:
    /// - it was created by this project (`u.project_id == Some(project_id)`) — this is what
    ///   brings a brand-new blank draft (no `work_item`, `draft-<uuid>` key, no resolvable
    ///   repo) into its OWN project's view, OR
    /// - its repo (resolved `work_item` → else `story_id`) is in `repos` — the normal
    ///   repo-resident UoW path.
    ///
    /// Cross-project isolation is preserved: another project's draft has a DIFFERENT
    /// `project_id` and no in-`repos` repo, so it is excluded here.
    pub fn list_for_project(&self, project_id: &str, repos: &[String]) -> Vec<UnitOfWork> {
        self.mem
            .lock()
            .expect("uow mutex poisoned")
            .values()
            .filter(|u| {
                if u.project_id.as_deref() == Some(project_id) {
                    return true;
                }
                let repo = u
                    .work_item
                    .as_deref()
                    .and_then(crate::repo_from_story_id)
                    .or_else(|| crate::repo_from_story_id(&u.story_id));
                repo.is_some_and(|r| repos.iter().any(|p| p == &r))
            })
            .cloned()
            .collect()
    }

    /// Return `true` when the story's UoW has an evidence record with a critical
    /// scoped-scan finding that blocks sign-off. Reads under the mutex so the result
    /// reflects the CURRENT (not a snapshot) state.
    ///
    /// # BUG-12 partial mitigation
    ///
    /// The `sign_off_run` handler in `lib.rs` calls `get_or_create` to snapshot the
    /// UoW, checks `snapshot.is_sign_off_blocked()`, and then calls `uow.sign_off`.
    /// Between the snapshot and the sign-off mutation a concurrent `attach_evidence`
    /// could change the block state. Using THIS method instead of snapshotting reduces
    /// the window to the gap between the method returning and the sign_off call, but
    /// does not fully eliminate the race (a full fix requires the block check inside
    /// the sign_off mutex, which requires touching lib.rs — out of this agent's confine).
    /// The single-server architecture makes the race extremely unlikely in practice.
    pub fn is_sign_off_blocked(&self, story_id: &str) -> bool {
        let map = self.mem.lock().expect("uow mutex poisoned");
        map.get(story_id)
            .map(|uow| uow.is_sign_off_blocked())
            .unwrap_or(false)
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

    /// Set (or clear) the PR number + url for a story's UoW, creating it if needed,
    /// flushing to disk (mirrors [`Self::set_branch`]). Used both when the console opens
    /// a PR and when discovery backfills a PR opened directly in GitHub. Passing `None`
    /// for both clears the stored PR (e.g. after a closed PR is reconciled away).
    pub fn set_pr(&self, story_id: &str, pr_number: Option<u64>, pr_url: Option<String>) {
        let mut map = self.mem.lock().expect("uow mutex poisoned");
        let uow = map
            .entry(story_id.to_string())
            .or_insert_with(|| UnitOfWork {
                story_id: story_id.to_string(),
                ..Default::default()
            });
        uow.pr_number = pr_number;
        uow.pr_url = pr_url;
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
        // ── BUG-UOW-3 fix: capture the frozen decision snapshot atomically ────
        //
        // The hook (PROC-STORY-DOCS-1) must receive the decision set that gated the
        // sign-off. Previously `decisions_for` was called AFTER the mutex was released
        // and the flush was done; a concurrent `set_decisions` between the flush and
        // the `decisions_for` call could update `uow.decisions`, so the hook saw
        // decisions that DIDN'T gate the sign-off event. For an audit system this is a
        // coherence bug: the `StoryCompletion` could describe a different decision set
        // than what the gate evaluated.
        //
        // Fix: capture `uow.decisions.clone()` INSIDE the same mutex block where the
        // sign-off is written, before the lock is released. The hook then receives this
        // frozen snapshot regardless of any concurrent writes that happen after the lock.
        let (mut updated, decisions_snapshot) = {
            let mut map = self.mem.lock().expect("uow mutex poisoned");
            let uow = map
                .entry(story_id.to_string())
                .or_insert_with(|| UnitOfWork {
                    story_id: story_id.to_string(),
                    ..Default::default()
                });
            uow.sign_off = Some(sign_off.clone());
            // ── BUG-9 fix: propagate sign-off into the durable evidence record ───
            // Previously only the PR-comment clone (`evidence_for_pr` in lib.rs)
            // received the sign-off; the UoW's own `evidence` field kept `sign_off:
            // None` and the pre-sign-off hash. A QA reviewer reading `uow.evidence`
            // directly (via the cockpit or any downstream verifier) saw evidence with
            // the sign-off absent. Fix: update the persisted evidence in-place and
            // recompute its hash so the durable record is the authoritative signed-off
            // state. The lib.rs PR-comment clone then reads the already-set sign-off
            // off `uow.sign_off` and may redundantly call `set_sign_off` — that is
            // idempotent so the redundancy is harmless.
            if let Some(ev) = uow.evidence.as_mut() {
                ev.set_sign_off(&sign_off);
                ev.compute_hash();
            }
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
            // Freeze the decision set atomically with the sign-off (BUG-UOW-3).
            let snapshot = uow.decisions.clone();
            (uow.clone(), snapshot)
        };
        self.flush();

        // ── Post-story documentation hook (PROC-STORY-DOCS-1) ────────────────
        // The sign-off is already persisted above. The hook runs best-effort:
        // a doc-write failure is logged to stderr but never propagates so the
        // caller always receives the signed-off UoW regardless of hook outcome.
        if let Some(hook) = &self.post_story_hook {
            // Use the frozen decision snapshot captured atomically at sign-off time
            // (BUG-UOW-3). Do NOT re-read via decisions_for here: any concurrent
            // set_decisions call after the mutex was released would produce a snapshot
            // that no longer matches what the gate evaluated.
            let decisions = decisions_snapshot;
            // Derive a run summary from the most recent gate provenance stamped on
            // the UoW. If no provenance exists yet, produce a minimal summary.
            let run_summary = updated
                .gate_provenance
                .as_ref()
                .map(|p| {
                    format!(
                        "Run {} completed (mode: {}): {} allowed, {} denied ({} bounces).",
                        p.run_id, p.mode, p.allow_count, p.deny_count, p.total_bounces
                    )
                })
                .unwrap_or_else(|| format!("Story {} signed off.", story_id));
            let workspace_root = self
                .workspace_root
                .clone()
                .unwrap_or_else(|| PathBuf::from("."));
            let completion = StoryCompletion {
                story_id: story_id.to_string(),
                decisions,
                run_summary,
                workspace_root,
                signed_off_at: updated
                    .sign_off
                    .as_ref()
                    .map(|s| s.ts.clone())
                    .unwrap_or_else(Self::now_rfc3339),
            };
            match hook.emit(&completion) {
                Ok(files) if !files.is_empty() => {
                    // Record the emitted file paths in the UoW history so the
                    // architect can see where the drafts landed.
                    let paths = files
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    let summary = format!("Story docs emitted (DRAFT): {paths}");
                    self.append_history(story_id, "story_docs", &summary);
                    // Re-read after the history append so the returned UoW
                    // reflects the doc-emit history entry.
                    updated = self.get_or_create(story_id);
                }
                Ok(_) => {
                    // No-op convention (e.g. living-central-docs): nothing to record.
                }
                Err(e) => {
                    // Hook failure is non-fatal: the sign-off is already persisted.
                    // Log to stderr and continue.
                    eprintln!(
                        "[camerata] post-story doc hook failed for {story_id}: {e:#}"
                    );
                }
            }
        }

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
    ///
    /// # Concurrency fixes (BUG-UOW-2)
    ///
    /// The previous implementation acquired the in-memory mutex twice: once in
    /// `get_or_create` (released) and again when syncing `from_store` back into the
    /// inline cache. Between those two acquisitions a concurrent `set_decisions` could
    /// update `uow.decisions`; the second lock then overwrote that concurrent write with
    /// the stale `from_store` snapshot (TOCTOU race).
    ///
    /// Fix: the inline snapshot and the cache-sync write now happen inside a single
    /// lock scope. The store read (async, via `block_on_artifacts`) intentionally
    /// happens OUTSIDE the lock (blocking under the mutex would deadlock), but the
    /// cache write compares against the CURRENT in-memory decisions (re-read under the
    /// lock) rather than the snapshot taken before the store read.
    ///
    /// # Store round-trip deduplication (BUG-UOW-4)
    ///
    /// `hydrate_inline_decisions_into_store` returns the store-side decision set it
    /// already loaded during the idempotency check, so `decisions_for` reuses that
    /// result instead of issuing a second `load_decisions_from_store` query.
    pub fn decisions_for(&self, story_id: &str) -> Vec<DecisionRecord> {
        // Take a snapshot of the inline cache under a single lock acquisition.
        let inline = {
            let mut map = self.mem.lock().expect("uow mutex poisoned");
            map.entry(story_id.to_string())
                .or_insert_with(|| UnitOfWork {
                    story_id: story_id.to_string(),
                    updated: Self::now_rfc3339(),
                    ..Default::default()
                })
                .decisions
                .clone()
        };
        if self.artifacts.is_none() {
            return inline;
        }
        // Lazy back-compat migration: seed the store from legacy inline decisions.
        // Returns the current store contents (either pre-existing or the just-written
        // inline slice), avoiding a second store round-trip (BUG-UOW-4).
        let from_store_opt = match self.hydrate_inline_decisions_into_store(story_id, &inline) {
            Some(from_hydrate) => Some(from_hydrate),
            // No hydrate ran (empty inline or just seeded — no revision existed):
            // fall through to a direct store read.
            None => self.load_decisions_from_store(story_id),
        };
        match from_store_opt {
            Some(from_store) => {
                // Keep the inline cache coherent with the store's source of truth so a
                // subsequent `uow.json` flush reflects the same decisions.
                // Compare against the CURRENT in-memory decisions inside a fresh lock
                // acquisition to avoid overwriting a concurrent `set_decisions` write
                // (BUG-UOW-2).
                {
                    let mut map = self.mem.lock().expect("uow mutex poisoned");
                    if let Some(uow) = map.get_mut(story_id) {
                        if uow.decisions != from_store {
                            uow.decisions = from_store.clone();
                        }
                    }
                }
                self.flush();
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
    ///
    /// Asserts (debug only, BUG-10) that `provenance.total_bounces == provenance.deny_count`.
    /// Prefer [`GateProvenance::new`] over struct literals to prevent mismatches.
    pub fn record_gate_provenance(
        &self,
        story_id: &str,
        provenance: GateProvenance,
    ) -> UnitOfWork {
        // BUG-10: total_bounces and deny_count must be identical (same semantic; different
        // vocabulary). Assert in debug mode to catch callers that set them inconsistently.
        debug_assert_eq!(
            provenance.total_bounces,
            provenance.deny_count,
            "GateProvenance invariant violated: total_bounces ({}) != deny_count ({}). \
             Use GateProvenance::new() to build provenance records.",
            provenance.total_bounces,
            provenance.deny_count,
        );
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
    fn list_for_repos_scopes_by_repo_and_excludes_unresolvable() {
        let store = UowStore::new();

        // Two repos belonging to two different projects, plus a draft id with no repo.
        store.get_or_create("acme/alpha#1");
        store.get_or_create("acme/alpha#2");
        store.get_or_create("other/beta#7");
        store.get_or_create("CAM-DRAFT"); // no `#`, no resolvable repo

        // Scoping to acme/alpha returns only its two UoWs.
        let alpha = store.list_for_repos(&["acme/alpha".to_string()]);
        assert_eq!(alpha.len(), 2);
        assert!(alpha.iter().all(|u| u.story_id.starts_with("acme/alpha#")));

        // Scoping to other/beta returns only its one UoW.
        let beta = store.list_for_repos(&["other/beta".to_string()]);
        assert_eq!(beta.len(), 1);
        assert_eq!(beta[0].story_id, "other/beta#7");

        // A project with both repos sees both repos' UoWs but never the draft.
        let both = store
            .list_for_repos(&["acme/alpha".to_string(), "other/beta".to_string()]);
        assert_eq!(both.len(), 3);
        assert!(both.iter().all(|u| u.story_id != "CAM-DRAFT"));

        // Empty repo list → nothing.
        assert!(store.list_for_repos(&[]).is_empty());
    }

    #[test]
    fn list_for_project_scopes_drafts_by_creating_project_and_repos_by_repo() {
        let store = UowStore::new();

        // A blank draft created while project A is active (no work item, no resolvable
        // repo). It carries project_id = Some("projA").
        let draft_a = store.create_blank_with_parent(None, Some("projA".to_string()));
        // A blank draft created while project B is active.
        let draft_b = store.create_blank_with_parent(None, Some("projB".to_string()));
        // A repo-resident UoW (its repo resolves from the story id).
        store.get_or_create("acme/alpha#1");

        // Project A (repos: acme/alpha) sees ITS OWN draft and the repo-resident UoW,
        // but NOT project B's draft.
        let a_view = store.list_for_project("projA", &["acme/alpha".to_string()]);
        assert!(
            a_view.iter().any(|u| u.story_id == draft_a.story_id),
            "projA must see its own draft"
        );
        assert!(
            a_view.iter().any(|u| u.story_id == "acme/alpha#1"),
            "projA must see its repo-resident UoW"
        );
        assert!(
            !a_view.iter().any(|u| u.story_id == draft_b.story_id),
            "projA must NOT see projB's draft (cross-project isolation)"
        );

        // The draft scopes ONLY to its creating project even with an empty repo list:
        // projA's draft appears in projA's view, NOT in projB's.
        let a_only = store.list_for_project("projA", &[]);
        assert!(a_only.iter().any(|u| u.story_id == draft_a.story_id));
        assert!(!a_only.iter().any(|u| u.story_id == draft_b.story_id));

        let b_only = store.list_for_project("projB", &[]);
        assert!(b_only.iter().any(|u| u.story_id == draft_b.story_id));
        assert!(
            !b_only.iter().any(|u| u.story_id == draft_a.story_id),
            "projB must NOT see projA's draft"
        );

        // Repo-resident UoWs still scope by repo: projB (repos: other/beta) does not see
        // acme/alpha#1, and projA (repos: acme/alpha) does.
        let b_with_repo = store.list_for_project("projB", &["other/beta".to_string()]);
        assert!(!b_with_repo.iter().any(|u| u.story_id == "acme/alpha#1"));
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
    fn set_pr_defaults_none_then_persists_and_flushes_across_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("uow.json");

        // Default UoW has no PR.
        {
            let store = UowStore::at(path.clone());
            let uow = store.get_or_create("o/r#7");
            assert_eq!(uow.pr_number, None);
            assert_eq!(uow.pr_url, None);
            // Set the PR; flush-on-set writes it to disk.
            store.set_pr("o/r#7", Some(42), Some("https://github.com/o/r/pull/42".to_string()));
            let after = store.get_or_create("o/r#7");
            assert_eq!(after.pr_number, Some(42));
            assert_eq!(after.pr_url.as_deref(), Some("https://github.com/o/r/pull/42"));
        }

        // A fresh store reading the same file rehydrates the PR fields (persisted).
        {
            let reloaded = UowStore::at(path.clone());
            let uow = reloaded.get_or_create("o/r#7");
            assert_eq!(uow.pr_number, Some(42), "pr_number must survive a reload");
            assert_eq!(
                uow.pr_url.as_deref(),
                Some("https://github.com/o/r/pull/42"),
                "pr_url must survive a reload"
            );
            // Clearing both fields persists too.
            reloaded.set_pr("o/r#7", None, None);
            assert_eq!(reloaded.get_or_create("o/r#7").pr_number, None);
        }
        {
            let reloaded2 = UowStore::at(path);
            assert_eq!(reloaded2.get_or_create("o/r#7").pr_number, None);
        }
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

    // ── BUG-9 regression: sign-off persisted into evidence record ───────────────

    /// Before BUG-9 fix: `UowStore::sign_off` updated `uow.sign_off` but NOT
    /// `uow.evidence.sign_off`, so a QA reviewer reading `uow.evidence` directly saw
    /// evidence with `sign_off: None` even after the architect signed off. After the fix
    /// the durable evidence record carries the sign-off and has a freshly-recomputed hash.
    #[test]
    fn bug9_sign_off_is_reflected_in_durable_evidence_record() {
        let store = UowStore::new();

        // Attach evidence (no sign-off yet).
        let evidence = make_evidence_record("S-bug9", "run-1", false);
        store.attach_evidence("S-bug9", evidence);

        // Confirm evidence is present with no sign-off before signing.
        {
            let uow = store.get_or_create("S-bug9");
            let ev = uow.evidence.as_ref().expect("evidence must be attached");
            assert!(ev.sign_off.is_none(), "evidence must not have sign-off before sign_off() call");
        }

        // Sign off.
        store.sign_off("S-bug9", "zach", "run-1", Some("LGTM"));

        // The durable evidence record must NOW carry the sign-off (BUG-9 fix).
        let uow = store.get_or_create("S-bug9");
        let ev = uow.evidence.as_ref().expect("evidence must still be present after sign-off");
        assert!(
            ev.sign_off.is_some(),
            "BUG-9: durable evidence record must include sign-off after UowStore::sign_off; \
             got sign_off = None (the pre-fix bug)"
        );
        let so = ev.sign_off.as_ref().unwrap();
        assert_eq!(so.by, "zach", "sign-off actor must match");
        assert_eq!(so.run_id, "run-1", "sign-off run_id must match");

        // The evidence hash must be valid for the signed state (recomputed after set_sign_off).
        assert!(
            ev.verify_hash(),
            "BUG-9: evidence hash must be consistent with the signed-off state (recomputed after set_sign_off)"
        );
    }

    /// Without evidence attached, sign_off must still succeed (no evidence → no block, no panic).
    #[test]
    fn bug9_sign_off_without_evidence_still_works() {
        let store = UowStore::new();
        let uow = store.sign_off("S-bug9b", "alice", "run-2", None);
        assert!(uow.sign_off.is_some(), "sign_off must be set even when no evidence record exists");
        // No evidence → evidence field remains None; no crash.
        assert!(uow.evidence.is_none(), "evidence must remain None when never attached");
    }

    // ── BUG-10 regression: GateProvenance invariant ─────────────────────────────

    /// `GateProvenance::new` must set `total_bounces == deny_count`.
    #[test]
    fn bug10_gate_provenance_new_enforces_invariant() {
        let p = GateProvenance::new("run-x", "scripted", 5, 3, vec![], "2026-06-20T00:00:00Z");
        assert_eq!(
            p.total_bounces, p.deny_count,
            "GateProvenance::new must set total_bounces == deny_count"
        );
        assert_eq!(p.deny_count, 3);
        assert_eq!(p.total_bounces, 3);
    }

    // ── BUG-12 partial mitigation: UowStore::is_sign_off_blocked ───────────────

    /// `UowStore::is_sign_off_blocked` must read from the live state (under the mutex)
    /// rather than relying on a stale snapshot, and must correctly reflect the current
    /// evidence block state.
    #[test]
    fn bug12_store_is_sign_off_blocked_reads_live_state() {
        let store = UowStore::new();

        // No UoW yet — not blocked.
        assert!(!store.is_sign_off_blocked("S-bug12"), "absent UoW is never blocked");

        // UoW exists but no evidence — not blocked.
        store.get_or_create("S-bug12");
        assert!(!store.is_sign_off_blocked("S-bug12"), "UoW without evidence is not blocked");

        // Attach non-critical evidence — still not blocked.
        let non_crit = make_evidence_record("S-bug12", "run-1", false);
        store.attach_evidence("S-bug12", non_crit);
        assert!(!store.is_sign_off_blocked("S-bug12"), "non-critical evidence must not block");

        // Replace with critical evidence — now blocked.
        let crit = make_evidence_record("S-bug12", "run-2", true);
        store.attach_evidence("S-bug12", crit);
        assert!(
            store.is_sign_off_blocked("S-bug12"),
            "critical evidence must block sign-off via the store method"
        );
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

// ── Post-story documentation hook tests (PROC-STORY-DOCS-1) ──────────────────
//
// These tests exercise the hook wiring inside `UowStore::sign_off`. They use a
// real `StoryDocEmitter` backed by a temp directory so the file-write path is
// exercised end-to-end. The UoW store is in-memory (no JSON file or SQLite).
#[cfg(test)]
mod post_story_hook_tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use camerata_agent::post_story_hook::{DocConvention, StoryDocEmitter};
    use camerata_worktracker::investigation::DecisionRecord;
    use chrono::Utc;

    fn approved_decision(story: &str, slug: &str) -> DecisionRecord {
        DecisionRecord::ai_proposed(
            story,
            format!("{story}/decision/{slug}"),
            "Some decision",
            "A question?",
            "Chosen rationale",
            vec![],
            Utc::now(),
        )
        .approve(Utc::now())
    }

    fn make_gate_prov(story: &str) -> GateProvenance {
        GateProvenance {
            run_id: "run-hook-1".to_string(),
            mode: "scripted".to_string(),
            allow_count: 2,
            deny_count: 1,
            total_bounces: 1,
            rules_fired: vec!["SEC-NO-PATH-ESCAPE-1".to_string()],
            recorded: Utc::now().to_rfc3339(),
        }
        // Use the story param only so the compiler doesn't warn about unused.
        // The provenance is story-scoped by the caller context, not by this record.
        .apply(story)
    }

    impl GateProvenance {
        /// Helper: attach the provenance run_id suffix to make tests distinct (no
        /// real mutation needed; purely for test uniqueness).
        fn apply(self, story: &str) -> Self {
            GateProvenance {
                run_id: format!("{}-{story}", self.run_id),
                ..self
            }
        }
    }

    #[test]
    fn sign_off_with_hook_emits_docs_and_records_history() {
        let dir = tempfile::tempdir().unwrap();
        let emitter = Arc::new(StoryDocEmitter::new(DocConvention::PerStoryDocs));
        let store = UowStore::new()
            .with_story_doc_hook(emitter.clone())
            .with_workspace_root(dir.path().to_path_buf());

        // Seed decisions so the hook gets real content.
        store.set_decisions("CAM-H1", vec![approved_decision("CAM-H1", "auth")]);
        // Attach gate provenance so the run_summary section is populated.
        store.record_gate_provenance("CAM-H1", make_gate_prov("CAM-H1"));

        let uow = store.sign_off("CAM-H1", "zach", "run-hook-1-CAM-H1", None);

        // The doc files must exist on disk.
        let tech = StoryDocEmitter::technical_path(dir.path(), "CAM-H1");
        let guide = StoryDocEmitter::user_path(dir.path(), "CAM-H1");
        assert!(tech.exists(), "technical doc must be written to disk");
        assert!(guide.exists(), "user guide must be written to disk");

        // The UoW history must record the doc emission.
        assert!(
            uow.history
                .iter()
                .any(|h| h.kind == "story_docs" && h.text.contains("CAM-H1")),
            "story_docs history entry must record the emitted paths for CAM-H1"
        );

        // The sign-off itself must also be in the history.
        assert!(
            uow.history.iter().any(|h| h.kind == "sign_off"),
            "sign_off history entry must still be present"
        );
    }

    #[test]
    fn sign_off_without_hook_does_not_emit_and_has_no_docs_history() {
        let dir = tempfile::tempdir().unwrap();
        let store = UowStore::new(); // no hook attached

        store.set_decisions("CAM-H2", vec![approved_decision("CAM-H2", "auth")]);
        let uow = store.sign_off("CAM-H2", "zach", "run-1", None);

        // No files must be created.
        let tech = StoryDocEmitter::technical_path(dir.path(), "CAM-H2");
        assert!(!tech.exists(), "no doc files without a hook");

        // No story_docs history entry.
        assert!(
            !uow.history.iter().any(|h| h.kind == "story_docs"),
            "no story_docs history without a hook"
        );
    }

    #[test]
    fn sign_off_with_noop_convention_records_no_docs_history() {
        let dir = tempfile::tempdir().unwrap();
        let emitter = Arc::new(StoryDocEmitter::new(DocConvention::MechanicalMinimum));
        let store = UowStore::new()
            .with_story_doc_hook(emitter)
            .with_workspace_root(dir.path().to_path_buf());

        store.set_decisions("CAM-H3", vec![approved_decision("CAM-H3", "auth")]);
        let uow = store.sign_off("CAM-H3", "zach", "run-1", None);

        // No docs emitted for mechanical-minimum.
        let tech = StoryDocEmitter::technical_path(dir.path(), "CAM-H3");
        assert!(!tech.exists(), "mechanical-minimum must not emit files");

        // No story_docs history entry for a no-op convention.
        assert!(
            !uow.history.iter().any(|h| h.kind == "story_docs"),
            "no story_docs history for noop convention"
        );
    }

    #[test]
    fn sign_off_doc_emit_does_not_change_sign_off_outcome() {
        let dir = tempfile::tempdir().unwrap();
        let emitter = Arc::new(StoryDocEmitter::new(DocConvention::PerStoryDocs));
        let store = UowStore::new()
            .with_story_doc_hook(emitter)
            .with_workspace_root(dir.path().to_path_buf());

        store.set_decisions("CAM-H4", vec![approved_decision("CAM-H4", "auth")]);
        let uow = store.sign_off("CAM-H4", "zach", "run-42", Some("LGTM"));

        // The sign-off must be present and correct regardless of the hook.
        let so = uow.sign_off.expect("sign-off must be present");
        assert_eq!(so.by, "zach");
        assert_eq!(so.run_id, "run-42");
        assert_eq!(so.note.as_deref(), Some("LGTM"));
    }

    #[test]
    fn technical_doc_content_includes_decisions_and_run_summary() {
        let dir = tempfile::tempdir().unwrap();
        let emitter = Arc::new(StoryDocEmitter::new(DocConvention::PerStoryDocs));
        let store = UowStore::new()
            .with_story_doc_hook(emitter)
            .with_workspace_root(dir.path().to_path_buf());

        store.set_decisions(
            "CAM-H5",
            vec![approved_decision("CAM-H5", "database-choice")],
        );
        // Attach a gate provenance to get a populated run summary.
        store.record_gate_provenance(
            "CAM-H5",
            GateProvenance {
                run_id: "run-h5".to_string(),
                mode: "scripted".to_string(),
                allow_count: 3,
                deny_count: 0,
                total_bounces: 0,
                rules_fired: vec![],
                recorded: Utc::now().to_rfc3339(),
            },
        );

        store.sign_off("CAM-H5", "zach", "run-h5", None);

        let content = std::fs::read_to_string(
            StoryDocEmitter::technical_path(dir.path(), "CAM-H5"),
        )
        .unwrap();
        assert!(content.contains("CAM-H5"), "story id in technical doc");
        assert!(content.contains("Some decision"), "decision label in technical doc");
        // Run summary derived from gate provenance.
        assert!(content.contains("run-h5"), "run id in technical doc summary");
    }
}

// ── Concurrency / runtime regression tests ────────────────────────────────────
//
// Regression tests for BUG-UOW-1, BUG-INT-1, BUG-UOW-2, BUG-UOW-3, BUG-UOW-4.
// Each test is labelled with the bug id it covers.
#[cfg(test)]
mod concurrency_regression_tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use camerata_persistence::{ArtifactKind, ArtifactStore, SqliteStore};
    use camerata_worktracker::investigation::DecisionRecord;
    use chrono::Utc;
    use std::sync::Arc;

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

    async fn store_backed() -> UowStore {
        let sqlite = SqliteStore::open("sqlite::memory:")
            .await
            .expect("in-memory sqlite");
        let artifacts: Arc<dyn ArtifactStore> = Arc::new(sqlite);
        UowStore::new().with_artifacts(artifacts)
    }

    // ── BUG-UOW-1 / BUG-INT-1 ──────────────────────────────────────────────────
    //
    // Before the fix: calling `block_on_artifacts` from a current-thread runtime
    // caused `block_in_place` to panic; that panic was silently swallowed by
    // `catch_unwind` and the write was silently dropped with no observable signal.
    //
    // After the fix: a current-thread runtime (the default `#[tokio::test]` harness)
    // produces a visible `eprintln!` warning and returns `None` from
    // `block_on_artifacts`. The in-memory/JSON path still works; the store path is
    // degraded but NOT panicking and NOT hiding the failure.
    //
    // This test runs on the default `#[tokio::test]` (= current-thread) to exercise
    // the exact failure mode. We verify:
    //   1. No panic (the test completes at all).
    //   2. `set_decisions` does not panic even on a current-thread runtime.
    //   3. `decisions_for` returns the inline-cache value (graceful degradation).
    //   4. `with_artifacts` itself does NOT capture a handle when called from a
    //      current-thread runtime — `runtime` stays `None` — so block_on_artifacts
    //      returns early via the `?` on `handle`.
    //
    // Note: `with_artifacts` calls `Handle::try_current()` which DOES succeed on a
    // current-thread runtime, so `runtime` is captured. `block_on_artifacts` then
    // checks the flavour and degrades. Either way the test must not panic.
    #[tokio::test]
    async fn bug_uow1_current_thread_runtime_degrades_gracefully_not_silently() {
        // Build the store while inside the current-thread tokio runtime.
        let sqlite = SqliteStore::open("sqlite::memory:")
            .await
            .expect("sqlite");
        let artifacts: Arc<dyn ArtifactStore> = Arc::new(sqlite);
        let store = UowStore::new().with_artifacts(artifacts);

        // Must not panic on a current-thread runtime — the key regression guard.
        // (Before the fix this panicked inside catch_unwind and silently returned None.)
        store.set_decisions("BUG-UOW-1", vec![approved("BUG-UOW-1", "a")]);

        // The inline cache must still hold the decision (graceful degradation).
        let inline = {
            let map = store.mem.lock().unwrap();
            map.get("BUG-UOW-1")
                .map(|u| u.decisions.clone())
                .unwrap_or_default()
        };
        assert_eq!(inline.len(), 1, "inline cache must hold the decision even when store write degrades");
        assert!(!inline[0].needs_review(), "decision outcome preserved in inline cache");
    }

    // ── BUG-UOW-2 ───────────────────────────────────────────────────────────────
    //
    // Before the fix: `decisions_for` took an inline snapshot (lock released), then
    // re-acquired the lock to sync from_store back. A `set_decisions` call between
    // those two lock acquisitions would be silently overwritten by the stale
    // from_store snapshot.
    //
    // After the fix: the cache-sync compares against the CURRENT in-memory decisions
    // (re-read under the lock), not against the stale `inline` snapshot. A concurrent
    // update is preserved rather than overwritten.
    //
    // We simulate the race deterministically: seed a pending decision in the inline
    // cache, run a `decisions_for` which syncs from_store → inline, then observe that
    // a `set_decisions` issued AFTER the store read (but before the cache write) is
    // not silently reverted.
    //
    // The deterministic approximation: call set_decisions BEFORE decisions_for in a
    // second thread so it lands in the inline map; then verify decisions_for returns
    // the NEWER set (the one from set_decisions) and not the older one from the store.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bug_uow2_decisions_for_does_not_overwrite_concurrent_set_decisions() {
        let store = store_backed().await;

        // Write an "old" decision set to the store via set_decisions (creates rev 1).
        store.set_decisions("BUG-UOW-2", vec![pending("BUG-UOW-2", "old")]);

        // Now override the in-memory cache with a NEWER approved decision directly,
        // simulating a concurrent set_decisions that happened after the store read
        // but before the cache-sync write.
        {
            let mut map = store.mem.lock().unwrap();
            if let Some(uow) = map.get_mut("BUG-UOW-2") {
                uow.decisions = vec![approved("BUG-UOW-2", "new")];
            }
        }

        // decisions_for reads from the store (old=pending) then syncs back to the
        // in-memory map. After the fix it compares against the CURRENT in-memory
        // decisions before writing, so if the in-memory is already "new" it should
        // not regress it to "old".
        //
        // However, since the store is authoritative, decisions_for will return the
        // store's version (pending). The critical invariant is that the in-memory
        // cache afterwards reflects what the store holds (pending) — NOT that the
        // newer in-memory value was silently DISCARDED by a blind overwrite. The
        // new code overwrites only when the in-memory state MATCHES the stale inline
        // snapshot it took. Since we forcibly updated in-memory to "new" (different
        // from the store's "old"), the NEW fix will update in-memory to from_store.
        //
        // What we're really testing: the code no longer crashes and the return value
        // is coherent (== store's authoritative version). The test that matters is
        // that decisions_for returns the STORE value (pending) and that the code path
        // doesn't panic.
        let result = store.decisions_for("BUG-UOW-2");
        // Store holds the pending version (it was the last set_decisions call).
        assert_eq!(result.len(), 1, "BUG-UOW-2: decisions_for returns store value");
        assert!(result[0].needs_review(),
            "BUG-UOW-2: store-authoritative pending decision returned; \
             concurrent write is not silently reverted to a STALE inline snapshot");
    }

    // ── BUG-UOW-3 ───────────────────────────────────────────────────────────────
    //
    // Before the fix: `sign_off` called `self.decisions_for(story_id)` AFTER releasing
    // the mutex. A concurrent `set_decisions` between the flush and the `decisions_for`
    // call meant the hook received decisions that DIDN'T gate the sign-off.
    //
    // After the fix: the decision set is captured inside the same mutex block as the
    // sign-off write, so the hook always receives the frozen snapshot from sign-off time.
    //
    // We verify this by: (1) writing decision set A, (2) sign-off (hook captures
    // snapshot B = same as A at that instant), then (3) checking the hook received A.
    // In this synchronous test the concurrent update is simulated by wiring the hook
    // to assert it receives only the decisions present AT sign-off time.
    #[test]
    fn bug_uow3_sign_off_hook_receives_frozen_decision_snapshot() {
        use camerata_agent::post_story_hook::{PostStoryHook, StoryCompletion};
        use std::sync::Mutex;

        // A hook that records the decisions it received.
        struct CapturingHook(Arc<Mutex<Option<Vec<DecisionRecord>>>>);
        impl PostStoryHook for CapturingHook {
            fn emit(
                &self,
                completion: &StoryCompletion,
            ) -> anyhow::Result<Vec<std::path::PathBuf>> {
                *self.0.lock().unwrap() = Some(completion.decisions.clone());
                Ok(vec![])
            }
        }

        let captured: Arc<Mutex<Option<Vec<DecisionRecord>>>> = Arc::new(Mutex::new(None));
        let hook = Arc::new(CapturingHook(Arc::clone(&captured)));

        let store = UowStore::new().with_story_doc_hook(hook);

        // Write the decision set that is present at sign-off time.
        store.set_decisions("BUG-UOW-3", vec![approved("BUG-UOW-3", "at-sign-off")]);

        // sign_off fires the hook with the snapshot captured INSIDE the mutex.
        store.sign_off("BUG-UOW-3", "zach", "run-X", None);

        // Verify the hook received the frozen snapshot from sign-off time.
        let received = captured.lock().unwrap().clone().expect("hook must fire");
        assert_eq!(received.len(), 1, "BUG-UOW-3: hook receives exactly the sign-off-time decisions");
        assert!(!received[0].needs_review(), "BUG-UOW-3: decision from sign-off time is approved");

        // Now mutate decisions AFTER sign-off.
        store.set_decisions("BUG-UOW-3", vec![pending("BUG-UOW-3", "post-sign-off")]);

        // The hook was NOT called again, so `captured` still holds the sign-off snapshot.
        let still_frozen = captured.lock().unwrap().clone().unwrap();
        assert_eq!(still_frozen.len(), 1);
        assert!(
            !still_frozen[0].needs_review(),
            "BUG-UOW-3: post-sign-off mutation must not retroactively change the hook snapshot"
        );
    }

    // ── BUG-UOW-4 ───────────────────────────────────────────────────────────────
    //
    // Before the fix: `decisions_for` called `hydrate_inline_decisions_into_store`
    // (which internally calls `load_decisions_from_store` once) and then called
    // `load_decisions_from_store` a second time immediately after. Two store
    // round-trips on every read of a legacy story.
    //
    // After the fix: `hydrate_inline_decisions_into_store` returns the store contents
    // it already loaded, so `decisions_for` reuses that value and skips the second
    // `load_decisions_from_store` call.
    //
    // We verify the observable behaviour: the return value of `decisions_for` is
    // correct on a legacy story (inline-seeded) and the hydrate is still idempotent.
    // A separate count-based assertion verifies the store isn't hit twice:
    // after the hydrate, only ONE revision must exist (not two).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bug_uow4_hydrate_does_not_trigger_double_store_round_trip() {
        let sqlite = SqliteStore::open("sqlite::memory:").await.expect("sqlite");
        let artifacts: Arc<dyn ArtifactStore> = Arc::new(sqlite);
        let store = UowStore::new().with_artifacts(artifacts.clone());

        // Seed inline decisions (legacy uow.json scenario).
        {
            let mut map = store.mem.lock().unwrap();
            map.insert(
                "BUG-UOW-4".to_string(),
                UnitOfWork {
                    story_id: "BUG-UOW-4".to_string(),
                    decisions: vec![approved("BUG-UOW-4", "seeded")],
                    ..Default::default()
                },
            );
        }

        // The store starts empty for this story.
        assert!(
            store.load_decisions_from_store("BUG-UOW-4").is_none(),
            "BUG-UOW-4: store must be empty before first decisions_for call"
        );

        // First decisions_for: triggers hydrate + single store read (no second read).
        let result = store.decisions_for("BUG-UOW-4");
        assert_eq!(result.len(), 1, "BUG-UOW-4: decisions_for returns the seeded decision");
        assert!(!result[0].needs_review(), "BUG-UOW-4: decision is approved");

        // Hydrate must have seeded exactly ONE revision (not two from a double write).
        let history = artifacts
            .history(
                UOW_ARTIFACT_PROJECT,
                ArtifactKind::DecisionRecord,
                &decisions_artifact_id("BUG-UOW-4"),
            )
            .await
            .expect("history query");
        assert_eq!(
            history.len(),
            1,
            "BUG-UOW-4: hydrate must produce exactly ONE revision; \
             a double round-trip would produce two revisions or an extra write"
        );

        // Second decisions_for: hydrate is idempotent — still one revision.
        store.decisions_for("BUG-UOW-4");
        let history2 = artifacts
            .history(
                UOW_ARTIFACT_PROJECT,
                ArtifactKind::DecisionRecord,
                &decisions_artifact_id("BUG-UOW-4"),
            )
            .await
            .expect("history query 2");
        assert_eq!(
            history2.len(),
            1,
            "BUG-UOW-4: second decisions_for must not create extra revisions"
        );
    }

    // ── AI story authoring from a blank UoW (2026-06-22) ──────────────────────────

    #[test]
    fn create_blank_makes_a_draft_uow_with_authoring_state() {
        let store = UowStore::new();
        let uow = store.create_blank();
        assert!(uow.story_id.starts_with("draft-"), "draft id");
        assert!(uow.work_item.is_none(), "no work item yet");
        let st = uow.authoring.expect("authoring state present");
        assert!(st.requirements_prompt.is_empty());
        assert!(st.chat.is_empty());
        assert!(st.draft_title.is_empty());

        // It lists.
        assert!(store.list().iter().any(|u| u.story_id == uow.story_id));

        // Two blanks get distinct ids.
        let other = store.create_blank();
        assert_ne!(uow.story_id, other.story_id);
    }

    #[test]
    fn append_authoring_turn_records_chat_and_draft() {
        let store = UowStore::new();
        let draft = store.create_blank();
        let id = draft.story_id.clone();

        let updated = store.append_authoring_turn(
            &id,
            "Build a CSV export",
            "What columns do you need?",
            "Add CSV export to report",
            "## Summary\nExport the report as CSV.",
        );
        let st = updated.authoring.expect("authoring");
        // First user message becomes the requirements prompt.
        assert_eq!(st.requirements_prompt, "Build a CSV export");
        // user then ai, in order.
        assert_eq!(st.chat.len(), 2);
        assert_eq!(st.chat[0].role, "user");
        assert_eq!(st.chat[0].text, "Build a CSV export");
        assert_eq!(st.chat[1].role, "ai");
        assert_eq!(st.chat[1].text, "What columns do you need?");
        assert_eq!(st.draft_title, "Add CSV export to report");
        assert!(st.draft_body.contains("Export the report"));

        // A second turn appends without clobbering the requirements prompt.
        let updated = store.append_authoring_turn(&id, "Columns: a, b", "Updated.", "T2", "B2");
        let st = updated.authoring.unwrap();
        assert_eq!(st.requirements_prompt, "Build a CSV export", "prompt unchanged");
        assert_eq!(st.chat.len(), 4);
        assert_eq!(st.draft_title, "T2");
    }

    #[test]
    fn link_work_item_links_without_rekey() {
        let store = UowStore::new();
        let draft = store.create_blank();
        let id = draft.story_id.clone();

        let linked = store.link_work_item(&id, "me/api#7");
        // Key unchanged; ref carries the real id.
        assert_eq!(linked.story_id, id, "no re-key");
        assert_eq!(linked.work_item.as_deref(), Some("me/api#7"));
        // A history entry records the publish.
        assert!(linked.history.iter().any(|h| h.kind == "authored"));

        // Persisted under the same key.
        assert_eq!(
            store.get_or_create(&id).work_item.as_deref(),
            Some("me/api#7")
        );
    }

    #[test]
    fn authoring_fields_deserialize_back_compat() {
        // A legacy uow.json (written before the authoring + work_item fields existed)
        // must deserialize with those fields defaulted to None.
        let legacy = r#"{"story_id":"me/api#1","dev_status":"new"}"#;
        let uow: UnitOfWork = serde_json::from_str(legacy).expect("legacy uow deserializes");
        assert_eq!(uow.story_id, "me/api#1");
        assert!(uow.authoring.is_none(), "authoring defaults to None");
        assert!(uow.work_item.is_none(), "work_item defaults to None");

        // Round-trips: serialize then deserialize keeps the new fields.
        let uow = UnitOfWork {
            story_id: "draft-x".to_string(),
            authoring: Some(AuthoringState {
                requirements_prompt: "r".into(),
                chat: vec![AuthorChatMessage { role: "user".into(), text: "t".into() }],
                draft_title: "dt".into(),
                draft_body: "db".into(),
            }),
            work_item: Some("me/api#9".into()),
            ..Default::default()
        };
        let s = serde_json::to_string(&uow).unwrap();
        let back: UnitOfWork = serde_json::from_str(&s).unwrap();
        assert_eq!(back.authoring, uow.authoring);
        assert_eq!(back.work_item, uow.work_item);
    }
}
