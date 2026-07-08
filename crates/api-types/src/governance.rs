//! `GovernanceEventDto` — a pure-serde mirror of `camerata_persistence::GovernanceEvent`'s
//! wire shape (Phase H3, the read path over the Phase H1/H2 governance-event audit
//! trail; see `docs/plans/2026-07-01_backend-headless-core.md`).
//!
//! This crate must NOT depend on `camerata-persistence` (api-types is the
//! dependency-free serde leaf every adapter builds on — see the crate-level docs), so
//! this is a hand-mirrored struct rather than a re-export. Keep it field-for-field in
//! sync with `camerata_persistence::GovernanceEvent`: the server returns that type
//! directly over `GET /api/runs/:id/events` and `GET /api/governance/events` (both crates
//! serialize the SAME shape), and this DTO is the client-side decode target for that
//! wire shape.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize, Debug, Default)]
pub struct GovernanceEventDto {
    /// Row id assigned by the database.
    #[serde(default)]
    pub id: Option<i64>,
    /// The run this event belongs to.
    #[serde(default)]
    pub run_id: String,
    /// The story this run is executing, if known.
    #[serde(default)]
    pub story_id: Option<String>,
    /// RFC3339 UTC timestamp of the event.
    #[serde(default)]
    pub ts: String,
    /// The event kind, e.g. `"run_started"`, `"gate_deny"`, `"escalation_raised"`,
    /// `"run_finished"`. A plain string (forward-compatible with new kinds).
    #[serde(default)]
    pub kind: String,
    /// `"info"` | `"warn"` | `"error"`.
    #[serde(default)]
    pub severity: String,
    /// Who/what produced the event: `"agent"` | `"human"` | `"system"`.
    #[serde(default)]
    pub actor: String,
    /// The rule id involved, if this event is rule-driven.
    #[serde(default)]
    pub rule_id: Option<String>,
    /// A human-readable one-line reason/summary.
    #[serde(default)]
    pub reason: Option<String>,
    /// A JSON blob of structured extras, opaque to this DTO.
    #[serde(default)]
    pub detail: Option<String>,
}
