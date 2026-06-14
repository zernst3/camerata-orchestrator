//! Domain value types for the persistence layer.
//!
//! Newtypes follow RUST-DOMAIN-4. These are separate from camerata-core's
//! `SessionId` so the persistence layer can own its DB row shape without
//! leaking ORM details into the core domain.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Newtypes (RUST-DOMAIN-4)
// ---------------------------------------------------------------------------

/// Opaque surrogate key for a provenance entry row.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProvenanceId(pub i64);

// ---------------------------------------------------------------------------
// Row models
// ---------------------------------------------------------------------------

/// A recorded agent session (one row in `agent_sessions`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    /// The session identifier as reported by the agent runtime.
    pub session_id: String,
    /// The scoped role name (e.g. "Backend", "Frontend").
    pub role: String,
    /// UTC timestamp when the session was started.
    pub started_at: DateTime<Utc>,
    /// Audit column: when this row was inserted (SQL-AUDIT-COLUMNS-1).
    pub created_at: DateTime<Utc>,
}

/// One append-only provenance entry (one row in `provenance_log`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvenanceEntry {
    /// Surrogate PK.
    pub id: Option<ProvenanceId>,
    /// FK to `agent_sessions.session_id`.
    pub session_id: String,
    /// Human-readable description of the change that was made.
    pub change_description: String,
    /// Camerata rule IDs cited for this decision (stored as a JSON array).
    pub rule_ids: Vec<String>,
    /// Free-form outcome string (e.g. "allowed", "denied", "revised").
    pub outcome: String,
    /// Audit column (SQL-AUDIT-COLUMNS-1).
    pub created_at: DateTime<Utc>,
}

impl ProvenanceEntry {
    /// Pure constructor — builds a new (un-persisted) entry (RUST-PURE-STATE-TRANSITIONS-1).
    pub fn new(
        session_id: impl Into<String>,
        change_description: impl Into<String>,
        rule_ids: Vec<String>,
        outcome: impl Into<String>,
    ) -> Self {
        Self {
            id: None,
            session_id: session_id.into(),
            change_description: change_description.into(),
            rule_ids,
            outcome: outcome.into(),
            created_at: Utc::now(),
        }
    }
}
