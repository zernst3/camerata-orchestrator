//! Investigation-phase and decision-phase artifacts for the story lifecycle
//! (issues #17 and #18).
//!
//! The `Investigating` → `DecisionsApproved` → `Executing` phase sequence is:
//!
//! 1. **Investigating** (`FeatureStatus::Investigating`): the AI agent runs the
//!    investigation and writes an [`InvestigationArtifact`] note — what it found,
//!    what ambiguities exist, what it intends to build. One per story.
//! 2. **Decisions** (`FeatureStatus::Investigating` / `AwaitingClarification`):
//!    the investigation surfaces N structured [`DecisionRecord`]s — concrete
//!    tradeoffs that must be resolved before code is written. Each starts
//!    `Pending`; the architect reviews and sets it to `Approved` or `Rejected`.
//! 3. **Gate** (`decisions_approved_for_development`): no code is written until
//!    the investigation note is marked `reviewed` AND every `DecisionRecord` is
//!    in the `Approved` state. The server MUST call this predicate before
//!    transitioning the story to `FeatureStatus::Executing`.
//!
//! All types are fully serializable (serde JSON). They are designed to be stored
//! via `camerata_persistence::ArtifactStore` using two new `ArtifactKind` variants
//! (`InvestigationNote` and `DecisionRecord`; those additions are ROUTE-A, a
//! cross-crate public-API change routed to the human — see the decision doc
//! `docs/decisions/2026-06-19_investigation_and_decision_phases.md`).
//!
//! The `artifact_id` convention is:
//!   - Investigation note: `"{story_id}/investigation"`
//!   - Decision record:   `"{story_id}/decision/{slug}"` (slug = kebab-case label)
//!
//! Conventions honored:
//! - RUST-DOMAIN-4: newtype / strongly-typed IDs (using plain `String` here
//!   because `StoryId` in `crates/intake` would create a cross-crate dep cycle;
//!   the field names are explicit enough).
//! - RUST-DOMAIN-5: no async I/O here; this module is pure domain types and a
//!   predicate function. Persistence I/O stays in `camerata-persistence`.
//! - RUST-DOMAIN-6: thiserror (no error types needed at this layer; errors arise
//!   only when persisting, which is the persistence crate's concern).
//! - RUST-PURE-STATE-TRANSITIONS-1: constructors accept explicit timestamps so
//!   callers (including tests) inject deterministic values.
//! - ORCH-NEW-PATH-TESTS-1: comprehensive unit tests in this file.
//! - `robustness_over_terseness`: explicit field docs, verbose constructors.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// RevisionActor
// ---------------------------------------------------------------------------

/// Who authored or reviewed a particular revision of an investigation artifact
/// or decision record. Mirrors `camerata_persistence::EditActor` semantically
/// but lives here to keep this crate's types self-contained (no dep on
/// `camerata-persistence` from `camerata-worktracker`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevisionActor {
    /// The revision was authored by the AI agent during investigation.
    Ai,
    /// The revision was authored or approved by the human architect.
    User,
}

impl RevisionActor {
    /// The stable snake_case string for this actor. Matches the `EditActor`
    /// wire format in `camerata-persistence` so the two are interchangeable
    /// in the JSON payload when the persistence layer stores these artifacts.
    pub fn as_str(&self) -> &'static str {
        match self {
            RevisionActor::Ai => "ai",
            RevisionActor::User => "user",
        }
    }

    /// Parse from the stored snake_case string. Returns `None` for unknown values.
    pub fn parse_str(s: &str) -> Option<Self> {
        match s {
            "ai" => Some(RevisionActor::Ai),
            "user" => Some(RevisionActor::User),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// RevisionProvenance
// ---------------------------------------------------------------------------

/// The who-and-when provenance carried on every revision of an investigation
/// artifact or decision record.
///
/// The persistence layer (`camerata-persistence`) separately records the
/// `EditActor` and `created_at` per row; this struct carries the same
/// information INSIDE the JSON payload so the artifact is self-describing
/// when decoded out of the revision log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevisionProvenance {
    /// Who authored (or last updated) this revision.
    pub actor: RevisionActor,
    /// UTC timestamp when this revision was written.
    pub at: DateTime<Utc>,
}

impl RevisionProvenance {
    /// Construct a provenance record. `at` is caller-supplied for determinism
    /// in tests (RUST-PURE-STATE-TRANSITIONS-1).
    pub fn new(actor: RevisionActor, at: DateTime<Utc>) -> Self {
        Self { actor, at }
    }

    /// Construct a provenance record with the current wall-clock time.
    /// Prefer `new` in tests; use this only in production callers.
    pub fn now(actor: RevisionActor) -> Self {
        Self {
            actor,
            at: Utc::now(),
        }
    }
}

// ---------------------------------------------------------------------------
// DecisionOutcome
// ---------------------------------------------------------------------------

/// The approval state of a [`DecisionRecord`].
///
/// A decision starts `Pending` when the AI agent surfaces it. The architect
/// reviews it and sets it to `Approved` (clearing the way for development) or
/// `Rejected` (the agent must re-investigate or the record must be revised
/// before it can be re-approved). A `Rejected` decision with an empty reason
/// is accepted by the type system but discouraged by convention.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DecisionOutcome {
    /// The architect has not yet reviewed this decision. Development is blocked
    /// as long as any record in the story's decision set is `Pending`.
    Pending,
    /// The architect reviewed and accepted this decision. Does NOT block
    /// development on its own; all decisions in the set must be `Approved`.
    Approved,
    /// The architect rejected this decision (e.g. the question was
    /// misframed, the wrong option was chosen, or more investigation is
    /// needed). Development is blocked until the record is revised and
    /// re-approved.
    Rejected {
        /// Plain-language reason for the rejection. Surfaced to the AI agent
        /// so it can re-investigate with this constraint in mind.
        reason: String,
    },
}

impl DecisionOutcome {
    /// Whether this outcome permits development to proceed (from the
    /// perspective of this single decision record). Returns `true` only when
    /// `Approved`.
    pub fn is_approved(&self) -> bool {
        matches!(self, DecisionOutcome::Approved)
    }

    /// Whether the architect still needs to act on this decision.
    /// Returns `true` for `Pending` and `Rejected`.
    pub fn needs_review(&self) -> bool {
        !self.is_approved()
    }
}

// ---------------------------------------------------------------------------
// InvestigationArtifact
// ---------------------------------------------------------------------------

/// The stored note produced by the AI agent during the `Investigating` phase.
///
/// One investigation note exists per story (artifact_id convention:
/// `"{story_id}/investigation"`). It is versioned in the artifact store, so
/// the architect can read the diff between the agent's first draft and any
/// revisions.
///
/// The note is NEVER auto-approved. The architect must explicitly set
/// `reviewed = true` before the story can advance to development
/// (issue #18's core commitment: the agent does not enter development
/// until the investigation is reviewed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvestigationArtifact {
    /// The artifact_id used in the revision log. Convention:
    /// `"{story_id}/investigation"`.
    pub artifact_id: String,
    /// The story this investigation belongs to.
    pub story_id: String,
    /// Free-form Markdown note authored by the AI agent.
    ///
    /// Contents (by convention, not enforced by the type):
    /// - What the story asks for, in the agent's own words.
    /// - Ambiguities identified.
    /// - External references consulted (docs, existing code paths).
    /// - Architectural decisions the agent expects to make (seeded into
    ///   `DecisionRecord`s by the agent after writing this note).
    /// - What the agent does NOT intend to build (explicit scope boundaries).
    ///
    /// This is NOT a task breakdown. A task breakdown belongs in the Plan
    /// phase.
    pub note: String,
    /// Whether the architect has reviewed this note. `false` until the
    /// architect explicitly approves it. The gate predicate
    /// (`decisions_approved_for_development`) does NOT check this field
    /// directly (it only checks decisions), but the server's gate check
    /// MUST also verify `reviewed == true` before permitting execution.
    /// See ROUTE-B in the decision doc.
    pub reviewed: bool,
    /// Provenance: who last wrote (or reviewed) this artifact and when.
    pub provenance: RevisionProvenance,
}

impl InvestigationArtifact {
    /// Construct an investigation note authored by the AI agent.
    ///
    /// `reviewed` is `false` by default (the architect must explicitly approve).
    /// `at` is caller-supplied for determinism.
    pub fn ai_authored(
        story_id: impl Into<String>,
        note: impl Into<String>,
        at: DateTime<Utc>,
    ) -> Self {
        let story_id = story_id.into();
        let artifact_id = format!("{story_id}/investigation");
        Self {
            artifact_id,
            story_id,
            note: note.into(),
            reviewed: false,
            provenance: RevisionProvenance::new(RevisionActor::Ai, at),
        }
    }

    /// Return a copy of this artifact with `reviewed = true` and the provenance
    /// updated to reflect the architect's review. The original is unchanged
    /// (RUST-PURE-STATE-TRANSITIONS-1 style). The updated copy is what the
    /// caller writes as the next revision.
    pub fn mark_reviewed(mut self, at: DateTime<Utc>) -> Self {
        self.reviewed = true;
        self.provenance = RevisionProvenance::new(RevisionActor::User, at);
        self
    }
}

// ---------------------------------------------------------------------------
// DecisionRecord
// ---------------------------------------------------------------------------

/// One structured decision captured during the investigation phase.
///
/// A story accumulates N decision records. Each starts `Pending` and must
/// reach `Approved` before the gate allows development to proceed.
///
/// The artifact_id convention is `"{story_id}/decision/{slug}"` where `slug`
/// is a URL-safe, kebab-case version of the `label` field. This is caller-
/// assigned, not derived inside this struct, to keep construction pure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionRecord {
    /// The artifact_id used in the revision log. Convention:
    /// `"{story_id}/decision/{slug}"`.
    pub artifact_id: String,
    /// The story this decision belongs to.
    pub story_id: String,
    /// Short plain-language label naming the decision space. Examples:
    /// `"Authentication strategy: JWT vs session cookies"`,
    /// `"Pagination approach: cursor vs offset"`.
    pub label: String,
    /// The question or ambiguity the agent surfaced that produced this
    /// decision. Plain-language, not technical. Examples:
    /// `"The story mentions 'remember me' — does the session need to survive
    ///  browser restarts, or only within the same browser session?"`.
    pub question: String,
    /// The chosen option and the reasoning. Authored by the AI agent initially;
    /// may be edited by the architect before approval.
    pub rationale: String,
    /// The alternatives that were explicitly considered and NOT chosen.
    /// Empty is acceptable when there were no real alternatives.
    #[serde(default)]
    pub alternatives_considered: Vec<String>,
    /// Current approval state. Starts `Pending`.
    pub outcome: DecisionOutcome,
    /// Provenance: who last wrote or approved this record and when.
    pub provenance: RevisionProvenance,
}

impl DecisionRecord {
    /// Construct a decision record authored by the AI agent, starting in
    /// `Pending` state. `at` is caller-supplied for determinism.
    pub fn ai_proposed(
        story_id: impl Into<String>,
        artifact_id: impl Into<String>,
        label: impl Into<String>,
        question: impl Into<String>,
        rationale: impl Into<String>,
        alternatives_considered: Vec<String>,
        at: DateTime<Utc>,
    ) -> Self {
        Self {
            artifact_id: artifact_id.into(),
            story_id: story_id.into(),
            label: label.into(),
            question: question.into(),
            rationale: rationale.into(),
            alternatives_considered,
            outcome: DecisionOutcome::Pending,
            provenance: RevisionProvenance::new(RevisionActor::Ai, at),
        }
    }

    /// Return a copy with the outcome set to `Approved` and provenance
    /// updated to the architect's approval time. Pure (does not mutate self).
    pub fn approve(mut self, at: DateTime<Utc>) -> Self {
        self.outcome = DecisionOutcome::Approved;
        self.provenance = RevisionProvenance::new(RevisionActor::User, at);
        self
    }

    /// Return a copy with the outcome set to `Rejected` and provenance
    /// updated to the architect's rejection time. Pure (does not mutate self).
    pub fn reject(mut self, reason: impl Into<String>, at: DateTime<Utc>) -> Self {
        self.outcome = DecisionOutcome::Rejected {
            reason: reason.into(),
        };
        self.provenance = RevisionProvenance::new(RevisionActor::User, at);
        self
    }

    /// Whether the architect still needs to act on this record.
    pub fn needs_review(&self) -> bool {
        self.outcome.needs_review()
    }
}

// ---------------------------------------------------------------------------
// Versioned<T>
// ---------------------------------------------------------------------------

/// A thin wrapper that carries a `version` number alongside any typed artifact,
/// enabling time-travel reads and optimistic-concurrency writes without changing
/// the artifact's own shape.
///
/// This is the decoded form of an `ArtifactRevision` payload. The persistence
/// layer stores `version` as a row column; `Versioned<T>` re-attaches it after
/// decoding the JSON payload so callers can reason about revision ordering.
///
/// # Example
///
/// ```
/// # use camerata_worktracker::investigation::*;
/// # use chrono::Utc;
/// let note = InvestigationArtifact::ai_authored("CAM-1", "Found X and Y", Utc::now());
/// let versioned = Versioned { version: 1, artifact: note };
/// assert_eq!(versioned.version, 1);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Versioned<T> {
    /// The monotonic version number as stored in `artifact_revisions.version`,
    /// starting at 1 and incrementing per revision of the same artifact.
    pub version: i64,
    /// The fully-decoded artifact at this version.
    pub artifact: T,
}

impl<T> Versioned<T> {
    /// Wrap an artifact with a version number.
    pub fn new(version: i64, artifact: T) -> Self {
        Self { version, artifact }
    }
}

// ---------------------------------------------------------------------------
// Gate predicate
// ---------------------------------------------------------------------------

/// Returns `true` when development is permitted for this story based on the
/// state of its decision records.
///
/// Gate semantics (issue #18):
///
/// - **At least one decision must exist.** An empty decision list means the
///   investigation has not surfaced any tradeoffs, which is suspicious for any
///   non-trivial story. The gate blocks to be safe; the agent must produce at
///   least one record (even `"No tradeoffs identified"` with `Approved` outcome
///   is a valid explicit record).
/// - **Every decision must be `Approved`.** A single `Pending` or `Rejected`
///   record blocks development.
///
/// The caller (the server's `run.rs` endpoint or the fleet's start path) MUST
/// call this predicate before transitioning the story to `FeatureStatus::Executing`.
/// It SHOULD also separately verify that the `InvestigationArtifact.reviewed`
/// field is `true` (that check is the server's concern — ROUTE-B).
///
/// # Examples
///
/// ```
/// # use camerata_worktracker::investigation::*;
/// # use chrono::Utc;
/// let t = Utc::now();
///
/// // No decisions yet: blocked.
/// assert!(!decisions_approved_for_development(&[]));
///
/// // One pending decision: blocked.
/// let pending = DecisionRecord::ai_proposed(
///     "CAM-1", "CAM-1/decision/auth", "Auth strategy",
///     "JWT or session?", "Chose JWT", vec![], t,
/// );
/// assert!(!decisions_approved_for_development(&[pending.clone()]));
///
/// // One approved decision: permitted.
/// let approved = pending.approve(t);
/// assert!(decisions_approved_for_development(&[approved]));
/// ```
pub fn decisions_approved_for_development(decisions: &[DecisionRecord]) -> bool {
    !decisions.is_empty()
        && decisions
            .iter()
            .all(|d| matches!(d.outcome, DecisionOutcome::Approved))
}

// ---------------------------------------------------------------------------
// Tests (ORCH-NEW-PATH-TESTS-1)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    // -----------------------------------------------------------------------
    // RevisionActor
    // -----------------------------------------------------------------------

    #[test]
    fn revision_actor_as_str_and_parse_str_round_trip() {
        for actor in [RevisionActor::Ai, RevisionActor::User] {
            let s = actor.as_str();
            let parsed = RevisionActor::parse_str(s).expect("round-trip");
            assert_eq!(parsed, actor, "RevisionActor::{s}");
        }
    }

    #[test]
    fn revision_actor_parse_str_rejects_unknown() {
        assert!(RevisionActor::parse_str("system").is_none());
        assert!(RevisionActor::parse_str("").is_none());
    }

    #[test]
    fn revision_actor_serde_round_trip() {
        let json_ai = serde_json::to_string(&RevisionActor::Ai).unwrap();
        assert_eq!(json_ai, "\"ai\"");
        let back_ai: RevisionActor = serde_json::from_str(&json_ai).unwrap();
        assert_eq!(back_ai, RevisionActor::Ai);

        let json_user = serde_json::to_string(&RevisionActor::User).unwrap();
        assert_eq!(json_user, "\"user\"");
        let back_user: RevisionActor = serde_json::from_str(&json_user).unwrap();
        assert_eq!(back_user, RevisionActor::User);
    }

    // -----------------------------------------------------------------------
    // RevisionProvenance
    // -----------------------------------------------------------------------

    #[test]
    fn revision_provenance_serde_round_trip() {
        let t = Utc::now();
        let p = RevisionProvenance::new(RevisionActor::Ai, t);
        let json = serde_json::to_string(&p).unwrap();
        let back: RevisionProvenance = serde_json::from_str(&json).unwrap();
        assert_eq!(back.actor, RevisionActor::Ai);
        // Timestamps may not be byte-identical after a JSON round-trip due to
        // subsecond precision serialization, but should be equal by value.
        assert_eq!(back.at.timestamp(), t.timestamp());
    }

    // -----------------------------------------------------------------------
    // DecisionOutcome
    // -----------------------------------------------------------------------

    #[test]
    fn decision_outcome_pending_is_not_approved() {
        assert!(!DecisionOutcome::Pending.is_approved());
        assert!(DecisionOutcome::Pending.needs_review());
    }

    #[test]
    fn decision_outcome_approved_is_approved() {
        assert!(DecisionOutcome::Approved.is_approved());
        assert!(!DecisionOutcome::Approved.needs_review());
    }

    #[test]
    fn decision_outcome_rejected_is_not_approved() {
        let rejected = DecisionOutcome::Rejected {
            reason: "Wrong choice".to_string(),
        };
        assert!(!rejected.is_approved());
        assert!(rejected.needs_review());
    }

    #[test]
    fn decision_outcome_serde_round_trip_all_variants() {
        // Pending
        let pending_json = serde_json::to_string(&DecisionOutcome::Pending).unwrap();
        assert!(
            pending_json.contains("\"pending\""),
            "pending JSON: {pending_json}"
        );
        let back: DecisionOutcome = serde_json::from_str(&pending_json).unwrap();
        assert_eq!(back, DecisionOutcome::Pending);

        // Approved
        let approved_json = serde_json::to_string(&DecisionOutcome::Approved).unwrap();
        assert!(
            approved_json.contains("\"approved\""),
            "approved JSON: {approved_json}"
        );
        let back: DecisionOutcome = serde_json::from_str(&approved_json).unwrap();
        assert_eq!(back, DecisionOutcome::Approved);

        // Rejected
        let rejected = DecisionOutcome::Rejected {
            reason: "Needs rework".to_string(),
        };
        let rejected_json = serde_json::to_string(&rejected).unwrap();
        assert!(
            rejected_json.contains("\"rejected\""),
            "rejected JSON: {rejected_json}"
        );
        assert!(rejected_json.contains("Needs rework"));
        let back: DecisionOutcome = serde_json::from_str(&rejected_json).unwrap();
        assert_eq!(back, rejected);
    }

    // -----------------------------------------------------------------------
    // InvestigationArtifact construction
    // -----------------------------------------------------------------------

    #[test]
    fn investigation_artifact_ai_authored_sets_fields_correctly() {
        let t = Utc::now();
        let note =
            InvestigationArtifact::ai_authored("CAM-1", "Found ambiguity in the auth flow.", t);

        assert_eq!(note.story_id, "CAM-1");
        assert_eq!(note.artifact_id, "CAM-1/investigation");
        assert_eq!(note.note, "Found ambiguity in the auth flow.");
        assert!(!note.reviewed, "new note must not be auto-reviewed");
        assert_eq!(note.provenance.actor, RevisionActor::Ai);
        assert_eq!(note.provenance.at.timestamp(), t.timestamp());
    }

    #[test]
    fn investigation_artifact_mark_reviewed_returns_new_copy() {
        let t = Utc::now();
        let note = InvestigationArtifact::ai_authored("CAM-2", "Some finding.", t);
        assert!(!note.reviewed);

        let reviewed = note.clone().mark_reviewed(t);
        assert!(reviewed.reviewed);
        assert_eq!(reviewed.provenance.actor, RevisionActor::User);
        // Original is unchanged.
        assert!(!note.reviewed);
    }

    #[test]
    fn investigation_artifact_serde_round_trip() {
        let t = Utc::now();
        let note = InvestigationArtifact::ai_authored("CAM-3", "Investigated the story.", t);
        let json = serde_json::to_string(&note).unwrap();
        let back: InvestigationArtifact = serde_json::from_str(&json).unwrap();
        assert_eq!(back.story_id, note.story_id);
        assert_eq!(back.artifact_id, note.artifact_id);
        assert_eq!(back.note, note.note);
        assert_eq!(back.reviewed, note.reviewed);
        assert_eq!(back.provenance.actor, note.provenance.actor);
    }

    #[test]
    fn investigation_artifact_reviewed_round_trips_json() {
        let t = Utc::now();
        let reviewed = InvestigationArtifact::ai_authored("CAM-4", "Note.", t).mark_reviewed(t);
        let json = serde_json::to_string(&reviewed).unwrap();
        let back: InvestigationArtifact = serde_json::from_str(&json).unwrap();
        assert!(back.reviewed);
        assert_eq!(back.provenance.actor, RevisionActor::User);
    }

    // -----------------------------------------------------------------------
    // DecisionRecord construction and transitions
    // -----------------------------------------------------------------------

    fn sample_decision(at: DateTime<Utc>) -> DecisionRecord {
        DecisionRecord::ai_proposed(
            "CAM-1",
            "CAM-1/decision/auth-strategy",
            "Authentication strategy: JWT vs session cookies",
            "The story mentions 'remember me'. Does the session survive browser restarts?",
            "Chose JWT: stateless, works across services, avoids server-side session store.",
            vec!["Session cookies: simpler revocation but requires session store.".to_string()],
            at,
        )
    }

    #[test]
    fn decision_record_ai_proposed_starts_pending() {
        let t = Utc::now();
        let d = sample_decision(t);

        assert_eq!(d.story_id, "CAM-1");
        assert_eq!(d.artifact_id, "CAM-1/decision/auth-strategy");
        assert_eq!(d.outcome, DecisionOutcome::Pending);
        assert_eq!(d.provenance.actor, RevisionActor::Ai);
        assert!(d.alternatives_considered.len() == 1);
    }

    #[test]
    fn decision_record_approve_returns_new_copy_with_approved_outcome() {
        let t = Utc::now();
        let pending = sample_decision(t);
        let approved = pending.clone().approve(t);

        assert_eq!(approved.outcome, DecisionOutcome::Approved);
        assert_eq!(approved.provenance.actor, RevisionActor::User);
        // Original unchanged.
        assert_eq!(pending.outcome, DecisionOutcome::Pending);
    }

    #[test]
    fn decision_record_reject_returns_new_copy_with_rejected_outcome() {
        let t = Utc::now();
        let pending = sample_decision(t);
        let rejected = pending
            .clone()
            .reject("Misframed — should be session storage first.", t);

        assert!(matches!(
            &rejected.outcome,
            DecisionOutcome::Rejected { reason } if reason.contains("Misframed")
        ));
        assert_eq!(rejected.provenance.actor, RevisionActor::User);
        // Original unchanged.
        assert_eq!(pending.outcome, DecisionOutcome::Pending);
    }

    #[test]
    fn decision_record_needs_review_follows_outcome() {
        let t = Utc::now();
        assert!(sample_decision(t).needs_review(), "Pending needs review");
        assert!(
            !sample_decision(t).approve(t).needs_review(),
            "Approved does not need review"
        );
        assert!(
            sample_decision(t).reject("reason", t).needs_review(),
            "Rejected needs review"
        );
    }

    #[test]
    fn decision_record_serde_round_trip_pending() {
        let t = Utc::now();
        let d = sample_decision(t);
        let json = serde_json::to_string(&d).unwrap();
        let back: DecisionRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back.story_id, d.story_id);
        assert_eq!(back.artifact_id, d.artifact_id);
        assert_eq!(back.outcome, DecisionOutcome::Pending);
        assert_eq!(back.alternatives_considered, d.alternatives_considered);
    }

    #[test]
    fn decision_record_serde_round_trip_approved() {
        let t = Utc::now();
        let approved = sample_decision(t).approve(t);
        let json = serde_json::to_string(&approved).unwrap();
        let back: DecisionRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back.outcome, DecisionOutcome::Approved);
        assert_eq!(back.provenance.actor, RevisionActor::User);
    }

    #[test]
    fn decision_record_serde_round_trip_rejected() {
        let t = Utc::now();
        let rejected = sample_decision(t).reject("Needs rework", t);
        let json = serde_json::to_string(&rejected).unwrap();
        let back: DecisionRecord = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            &back.outcome,
            DecisionOutcome::Rejected { reason } if reason == "Needs rework"
        ));
    }

    #[test]
    fn decision_record_default_alternatives_is_empty() {
        let t = Utc::now();
        let d = DecisionRecord::ai_proposed(
            "S-1",
            "S-1/decision/pagination",
            "Pagination approach",
            "Cursor or offset?",
            "Chose cursor: stable under inserts.",
            vec![], // explicit empty
            t,
        );
        assert!(d.alternatives_considered.is_empty());

        // Round-trip with serde default
        let json = serde_json::to_string(&d).unwrap();
        let back: DecisionRecord = serde_json::from_str(&json).unwrap();
        assert!(back.alternatives_considered.is_empty());
    }

    // -----------------------------------------------------------------------
    // Versioned<T>
    // -----------------------------------------------------------------------

    #[test]
    fn versioned_wraps_artifact_and_exposes_version() {
        let t = Utc::now();
        let note = InvestigationArtifact::ai_authored("CAM-5", "Note at v3.", t);
        let v = Versioned::new(3, note.clone());
        assert_eq!(v.version, 3);
        assert_eq!(v.artifact.story_id, "CAM-5");
    }

    #[test]
    fn versioned_serde_round_trip_with_investigation_artifact() {
        let t = Utc::now();
        let note = InvestigationArtifact::ai_authored("CAM-6", "v7 note.", t);
        let v: Versioned<InvestigationArtifact> = Versioned::new(7, note);
        let json = serde_json::to_string(&v).unwrap();
        let back: Versioned<InvestigationArtifact> = serde_json::from_str(&json).unwrap();
        assert_eq!(back.version, 7);
        assert_eq!(back.artifact.story_id, "CAM-6");
    }

    #[test]
    fn versioned_serde_round_trip_with_decision_record() {
        let t = Utc::now();
        let d = sample_decision(t).approve(t);
        let v: Versioned<DecisionRecord> = Versioned::new(2, d);
        let json = serde_json::to_string(&v).unwrap();
        let back: Versioned<DecisionRecord> = serde_json::from_str(&json).unwrap();
        assert_eq!(back.version, 2);
        assert_eq!(back.artifact.outcome, DecisionOutcome::Approved);
    }

    // -----------------------------------------------------------------------
    // decisions_approved_for_development gate predicate
    // -----------------------------------------------------------------------

    #[test]
    fn gate_rejects_empty_decision_list() {
        assert!(
            !decisions_approved_for_development(&[]),
            "empty decisions list must block development"
        );
    }

    #[test]
    fn gate_rejects_single_pending_decision() {
        let t = Utc::now();
        let d = sample_decision(t);
        assert!(!decisions_approved_for_development(&[d]));
    }

    #[test]
    fn gate_rejects_single_rejected_decision() {
        let t = Utc::now();
        let d = sample_decision(t).reject("Wrong choice.", t);
        assert!(!decisions_approved_for_development(&[d]));
    }

    #[test]
    fn gate_permits_single_approved_decision() {
        let t = Utc::now();
        let d = sample_decision(t).approve(t);
        assert!(decisions_approved_for_development(&[d]));
    }

    #[test]
    fn gate_permits_multiple_all_approved_decisions() {
        let t = Utc::now();

        let d1 = sample_decision(t).approve(t);
        let d2 = DecisionRecord::ai_proposed(
            "CAM-1",
            "CAM-1/decision/pagination",
            "Pagination approach",
            "Cursor or offset?",
            "Cursor: stable under concurrent inserts.",
            vec![],
            t,
        )
        .approve(t);

        assert!(decisions_approved_for_development(&[d1, d2]));
    }

    #[test]
    fn gate_rejects_when_one_of_many_decisions_is_pending() {
        let t = Utc::now();

        let approved = sample_decision(t).approve(t);
        let pending = DecisionRecord::ai_proposed(
            "CAM-1",
            "CAM-1/decision/pagination",
            "Pagination approach",
            "Cursor or offset?",
            "Cursor.",
            vec![],
            t,
        );

        assert!(
            !decisions_approved_for_development(&[approved, pending]),
            "one pending among approved must block"
        );
    }

    #[test]
    fn gate_rejects_when_one_of_many_decisions_is_rejected() {
        let t = Utc::now();

        let approved = sample_decision(t).approve(t);
        let rejected = DecisionRecord::ai_proposed(
            "CAM-1",
            "CAM-1/decision/pagination",
            "Pagination approach",
            "Cursor or offset?",
            "Cursor.",
            vec![],
            t,
        )
        .reject("Needs more thought.", t);

        assert!(
            !decisions_approved_for_development(&[approved, rejected]),
            "one rejected among approved must block"
        );
    }

    #[test]
    fn gate_rejects_all_pending() {
        let t = Utc::now();
        let d1 = sample_decision(t);
        let d2 = DecisionRecord::ai_proposed(
            "CAM-1",
            "CAM-1/decision/pagination",
            "Pagination",
            "Q?",
            "A.",
            vec![],
            t,
        );
        assert!(!decisions_approved_for_development(&[d1, d2]));
    }

    #[test]
    fn gate_handles_single_approved_among_zero_others() {
        // Edge case: a story with exactly one decision that is approved.
        // This is the minimal valid state.
        let t = Utc::now();
        let no_tradeoffs = DecisionRecord::ai_proposed(
            "CAM-99",
            "CAM-99/decision/no-tradeoffs",
            "No tradeoffs identified",
            "Are there any meaningful tradeoffs in this trivial story?",
            "No tradeoffs identified; the implementation path is unambiguous.",
            vec![],
            t,
        )
        .approve(t);
        assert!(decisions_approved_for_development(&[no_tradeoffs]));
    }
}
