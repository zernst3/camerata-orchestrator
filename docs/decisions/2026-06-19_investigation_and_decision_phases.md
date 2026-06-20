# Investigation and User-Decision Phases (Issues #17 and #18)

**Date:** 2026-06-19
**Issues:** [#18 — Investigation and user-decision phases (pre-code)](https://github.com/zernst3/camerata-orchestrator/issues/18), [#17 — Story lifecycle and persistent structured data](https://github.com/zernst3/camerata-orchestrator/issues/17)
**Status:** Phase-state-machine and data types decided and implemented (additive, within-crate). Cross-crate wiring routed to Zach (ROUTE-1).

---

## Context

The current `CanonicalStory` in `crates/worktracker/src/lib.rs` carries `FeatureStatus` — a lifecycle position — but carries **no structured artifact** for the investigation that precedes development. The status transitions (`Intake -> Investigating -> AwaitingClarification -> Planned -> Executing`) exist, but the artifacts those phases produce are not yet modeled as stored, queryable structured data. Today:

- The investigation produces no persisted note. Nothing the architect can review, approve, or time-travel.
- User decisions (tradeoffs, ambiguities resolved during investigation) are not captured as structured records. They live (if at all) in prose in the tracker's description field.
- The gate that blocks development until decisions are approved is implicit (the architect just does not run the fleet yet). It is not enforced by the domain model.

Issues #17 and #18 ask for structured, stored, replayable artifacts for these phases, with full revision history (actor/operation provenance), and an explicit gate that blocks development until decisions are approved.

---

## Phase State Machine

```
Intake → Investigating → DecisionsApproved → Development
             ↑                ↑
      (may loop while   (approval gate;
       gathering info)   blocks transition)
```

Concretely, the existing `FeatureStatus` already models the `Intake` and `Investigating` (and `AwaitingClarification`) states. This decision adds:

1. **`InvestigationArtifact`** — the stored note the investigation phase produces. One per story, versioned (revision history), reviewed before development.
2. **`DecisionRecord`** — one structured decision captured during investigation. A story may accumulate N decisions (each is a separate versioned artifact row). All must be in `approved` state before development can begin.
3. **The approved-gate predicate** — `decisions_approved_for_development(decisions: &[DecisionRecord]) -> bool`. Returns `true` when the story has at least one decision and every decision is in `DecisionOutcome::Approved` state. The fleet must call this before entering `Executing`.

The existing `Phase::Executing` in `crates/intake/src/project.rs` and `FeatureStatus::Executing` in `crates/worktracker/src/lib.rs` are the development gate. The new predicate is the enforcement mechanism.

---

## Artifact Shapes

### `InvestigationArtifact`

Location: `crates/worktracker/src/investigation.rs` (new module, additive).

```rust
pub struct InvestigationArtifact {
    /// Stable id, unique within a story. Typically the story id with a suffix
    /// (e.g. "CAM-1/investigation"). Serves as the artifact_id in the revision log.
    pub artifact_id: String,
    /// The story this investigation belongs to.
    pub story_id: String,
    /// Free-form Markdown note authored by the AI agent during investigation.
    /// Includes: scope clarification, ambiguities surfaced, references consulted,
    /// and the basis for the decisions that follow. NOT a task breakdown —
    /// that is the Plan phase.
    pub note: String,
    /// Whether the artifact has been reviewed and approved by the architect.
    /// Investigation is NEVER auto-approved: the human must review it before
    /// any code is written (the core commitment of issue #18).
    pub reviewed: bool,
    /// Who/when the artifact was most recently written (AI) or reviewed (User).
    pub provenance: RevisionProvenance,
}
```

The field `reviewed: bool` encodes the gate: `Investigating -> DecisionsApproved` is only legal when `reviewed == true` AND all decisions are approved (see below).

### `DecisionRecord`

One structured decision captured during investigation. A story accumulates N of these.

```rust
pub struct DecisionRecord {
    /// Stable id within the story, e.g. "CAM-1/decision/auth-strategy".
    pub artifact_id: String,
    /// The story this decision belongs to.
    pub story_id: String,
    /// Short plain-language label: what was decided, e.g.
    /// "Authentication strategy: JWT vs session cookies".
    pub label: String,
    /// What the investigation surfaced as the question or ambiguity.
    pub question: String,
    /// The chosen option and the reasoning behind it.
    pub rationale: String,
    /// The alternatives that were NOT chosen and why.
    pub alternatives_considered: Vec<String>,
    /// Outcome: Pending (not yet approved), Approved, or Rejected (needs rework).
    pub outcome: DecisionOutcome,
    /// Who/when the decision was last authored or approved.
    pub provenance: RevisionProvenance,
}

pub enum DecisionOutcome {
    Pending,
    Approved,
    Rejected { reason: String },
}
```

### `RevisionProvenance`

Provenance carried on every versioned artifact, matching the semantic of `EditActor` in `crates/persistence/src/artifacts.rs`.

```rust
pub struct RevisionProvenance {
    /// Who authored this revision: the AI agent or the human architect.
    pub actor: RevisionActor,
    /// UTC timestamp of this revision.
    pub at: DateTime<Utc>,
}

pub enum RevisionActor {
    Ai,
    User,
}
```

### `Versioned<T>` Wrapper

A thin wrapper used in-memory (and as the decoded form of an `ArtifactRevision.payload`) to carry a version number alongside any artifact, enabling time-travel reads and optimistic-concurrency writes:

```rust
pub struct Versioned<T> {
    /// The version as recorded in `artifact_revisions.version`.
    pub version: i64,
    /// The fully-decoded artifact at this version.
    pub artifact: T,
}
```

---

## Persistence Strategy: Extend the Existing `ArtifactStore` (JSON over SQLite)

The existing persistence story (`crates/persistence/src/artifacts.rs`) is:
- An append-only `artifact_revisions` table in SQLite (already in production shape).
- `ArtifactKind` discriminates what is stored (currently: `OnboardingDocument`, `UserStory`, `Clarification`, `Suggestion`, `RefinementSession`).
- `encode<T: Serialize>` / `ArtifactRevision::decode<T: DeserializeOwned>` serialize the typed artifact as a JSON payload.
- The `artifact_id` column is a caller-assigned stable string, opaque to the store.

**Decision: Extend `ArtifactKind` with two new variants** — `InvestigationNote` and `DecisionRecord` — and use the existing table, DDL, and indexing unchanged. No new table, no new migration beyond adding the two enum variants.

**Rationale:**
- The revision-history requirement (actor/operation/timestamp provenance) is already 100% satisfied by the existing table and `EditActor`/`RevisionOp`/`created_at` columns.
- The time-travel requirement (`revision_at(project_id, kind, artifact_id, version)`) is already implemented.
- Adding a new `ArtifactKind` variant is additive (the `parse_str` arm returns `None` for unknown strings; existing rows are unaffected).
- An alternative of a separate `decisions` table would duplicate the versioning infrastructure for no query advantage: the access patterns (current state + full history per story) are identical to the existing `current_artifact` + `history` methods.
- SQLite is already the persistence backend per ADR `2026-06-14_persistence_sqlite_event_sourced_versioning.md`. A separate decisions store (e.g. in-memory JSON files) would split the persistence story and lose the revision-history guarantees at rest.

The `artifact_id` for an investigation note is `"{story_id}/investigation"`. For a decision record it is `"{story_id}/decision/{slug}"` where `slug` is a URL-safe label (kebab-case of the `label` field).

---

## The Approved-Gate Predicate

```rust
/// Returns true when development is permitted for this story's decisions.
///
/// Gate semantics (issue #18):
///  - At least one decision must exist (an empty decisions list means investigation
///    has not surfaced any tradeoffs yet, which is suspicious; block to be safe).
///  - Every decision must be in the `Approved` state. A single `Pending` or
///    `Rejected` decision blocks development.
///
/// The caller (the fleet or the server's "start governed run" endpoint) must
/// check this predicate BEFORE transitioning the story to `FeatureStatus::Executing`.
pub fn decisions_approved_for_development(decisions: &[DecisionRecord]) -> bool {
    !decisions.is_empty()
        && decisions
            .iter()
            .all(|d| matches!(d.outcome, DecisionOutcome::Approved))
}
```

**Why "at least one":** An investigation that produces zero decision records signals the investigation was either not run or was empty. Requiring at least one forces the agent to surface at least one tradeoff (even "no tradeoffs identified" is a decision that must be explicitly approved by the architect).

---

## What Is NOT Done Here (Routed to Human — ROUTE-1)

The following require structural changes (new cross-crate API surface, public trait changes, or new persistence method signatures) and are therefore **routed** rather than auto-applied:

### ROUTE-A: Add `InvestigationNote` and `DecisionRecord` variants to `ArtifactKind` in `crates/persistence`

`crates/persistence/src/artifacts.rs` is in a DIFFERENT crate than the new types in `crates/worktracker/src/investigation.rs`. Adding the new `ArtifactKind` variants to the existing enum changes the public API surface of `camerata-persistence`. This is a one-line addition per variant but crosses crate boundaries (ROUTE-1).

**Proposed change:**
```rust
// In crates/persistence/src/artifacts.rs
pub enum ArtifactKind {
    // ... existing variants ...
    /// An investigation note produced by the agent during the Investigating phase.
    InvestigationNote,
    /// A structured decision record captured during investigation.
    DecisionRecord,
}
```
And the corresponding `as_str` / `parse_str` arms: `"investigation_note"` and `"decision_record"`.

### ROUTE-B: Wire the gate predicate into the server's `run.rs` / `fleet` start endpoint

The `decisions_approved_for_development` predicate lives in `crates/worktracker`. The fleet start path lives in `crates/server/src/run.rs` and `crates/fleet`. Wiring the gate requires the server to load the story's decision records from the persistence layer and call the predicate before spawning agents. This touches the server's `AppState` (adding access to the artifact store) and possibly the fleet's `run` API — both are cross-crate public trait changes (ROUTE-1).

### ROUTE-C: Surfacing investigation artifacts and decisions in the cockpit

`crates/ui/src/cockpit.rs` currently shows five stage tabs: INTAKE / INVESTIGATION / PLAN / STATUS / QA. The INVESTIGATION tab's `StagePanel` (around line 6296) currently has a placeholder body. It should render the `InvestigationArtifact` (the note, reviewed status) and the list of `DecisionRecord`s (each with its outcome and approve/reject affordances). This is a UI change touching the cockpit's signal-driven Dioxus components and requires the server to expose a new endpoint (cross-crate).

---

## Summary of What IS Done Here (Additive, Within `crates/worktracker`)

1. New module `crates/worktracker/src/investigation.rs` with:
   - `RevisionActor` (enum: Ai / User)
   - `RevisionProvenance` (actor + timestamp)
   - `DecisionOutcome` (Pending / Approved / Rejected)
   - `InvestigationArtifact` (structured investigation note)
   - `DecisionRecord` (structured decision with outcome)
   - `Versioned<T>` (thin version-number wrapper)
   - `decisions_approved_for_development` predicate
   - Full serde (Serialize / Deserialize) on all types
   - Unit tests for serde round-trips, the approved-gate predicate (all cases), and the `Versioned<T>` wrapper

2. Module wired into `crates/worktracker/src/lib.rs` with appropriate re-exports.

3. This decision document.

---

## Cross-Reference

| Existing type | How it relates |
|---|---|
| `CanonicalStory` (`crates/worktracker`) | Carries the story's `FeatureStatus`; the gate predicate is called before advancing to `Executing` |
| `FeatureStatus::Investigating` / `AwaitingClarification` | The phase during which `InvestigationArtifact` and `DecisionRecord`s are written |
| `ArtifactStore` / `ArtifactRevision` (`crates/persistence`) | The persistence layer that stores versioned artifacts; extended with two new `ArtifactKind` variants (ROUTE-A) |
| `EditActor` / `RevisionOp` (`crates/persistence`) | Parallel to the new `RevisionActor`; the persistence layer's own provenance vocabulary |
| `Phase::Executing` (`crates/intake/src/project.rs`) | The development phase blocked until the gate predicate passes |
| STAGE_TABS (`crates/ui/src/cockpit.rs` line 1910) | The existing INVESTIGATION tab that will render the new artifacts (ROUTE-C) |
