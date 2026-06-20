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

use camerata_worktracker::investigation::DecisionRecord;

use crate::lifecycle::{TransitionError, UowStage};

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
    /// These are persisted here on the UoW as an additive home: the cross-crate
    /// `ArtifactStore`-backed persistence for investigation artifacts is ROUTE-A
    /// (a public-API change routed to the human; see the decision doc). Until that
    /// lands, the governed-dev loop needs SOMEWHERE durable to read the decision
    /// state from to enforce the gate, and the UoW is the natural per-story home.
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
    pub fn set_decisions(&self, story_id: &str, decisions: Vec<DecisionRecord>) -> UnitOfWork {
        let now = Self::now_rfc3339();
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
        // Snapshot the decisions under the lock-free clone path; the transition then
        // re-locks. Cloning is cheap relative to correctness, and keeps the gate check
        // reading the same persisted decisions the API exposes.
        let decisions = self.get_or_create(story_id).decisions;
        self.apply_transition(story_id, |s| s.approve_decisions(&decisions))
    }

    /// DecisionsApproved → Development, re-checking the decision gate. See
    /// [`UowStage::start_development`]. Returns the [`TransitionError`] (so the run
    /// start can block + surface why) when the gate is not satisfied.
    pub fn start_development(&self, story_id: &str) -> Result<UnitOfWork, TransitionError> {
        let decisions = self.get_or_create(story_id).decisions;
        self.apply_transition(story_id, |s| s.start_development(&decisions))
    }

    /// Development → AwaitingQa. See [`UowStage::finish_development`].
    pub fn finish_development(&self, story_id: &str) -> Result<UnitOfWork, TransitionError> {
        self.apply_transition(story_id, |s| s.finish_development())
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
}
