//! camerata-persistence: SQLite-backed session state, append-only provenance
//! log, version-tracked artifact store, write-only enforcement-catch ledger, and
//! the readable governance-event audit trail.
//!
//! Public surface:
//! - [`SessionId`], [`ProvenanceEntry`] — domain value types (newtypes)
//! - [`Store`] — async trait (the seam for session/provenance)
//! - [`ArtifactStore`] — async trait (the seam for artifact revisions)
//! - [`EnforcementCatchLedger`] — async trait (the write-only enforcement-catch seam)
//! - [`SqliteStore`] — production impl backed by sqlx + SQLite
//! - [`ArtifactKind`], [`EditActor`], [`RevisionOp`] — artifact domain enums
//! - [`NewRevision`], [`ArtifactRevision`] — artifact input/output types
//! - [`encode`] — serialize a typed value to an artifact payload string
//! - [`EnforcementCatch`] — the INSERT-only enforcement catch record
//! - [`content_hash`] — FNV-1a hex hash for offending content (NEVER store raw)
//! - [`GovernanceEvent`], [`GovernanceLog`] — the readable governance-event audit
//!   trail (run lifecycle, gate verdicts, escalations, sign-off, etc.)
//! - [`FeedbackStore`] — the Product-Owner feedback loop's defect-report store (auto
//!   capture + click-to-report), storing `camerata_api_types::feedback::DefectReport`
//!   directly
//! - [`OrchestratorDecision`], [`DecisionOutcome`], [`DecisionOutcomeKind`],
//!   [`ClassCalibration`], [`OrchestratorDecisionLog`] — the confidence engine's
//!   decision + outcome + calibration store (the "measured override rate at max
//!   dial" moat metric)
//!
//! Conventions honored:
//! - RUST-DOMAIN-4: newtype IDs
//! - RUST-DOMAIN-5: async I/O throughout
//! - RUST-DOMAIN-6: thiserror error enum
//! - SQL-AUDIT-COLUMNS-1: `ts_ms` / `created_at` on every table
//! - SQL-DB-INDEX-1/2: FK and WHERE columns indexed
//! - RUST-PURE-STATE-TRANSITIONS-1: builder helpers are pure
//! - ORCH-NEW-PATH-TESTS-1: unit tests included

pub mod artifacts;
pub mod enforcement_catch;
pub mod error;
pub mod feedback;
pub mod governance_event;
pub mod model;
pub mod orchestrator_decision;
pub mod store;

pub use artifacts::{
    encode, ArtifactKind, ArtifactRevision, ArtifactStore, EditActor, NewRevision, RevisionOp,
};
pub use enforcement_catch::{content_hash, EnforcementCatch, EnforcementCatchLedger};
pub use error::PersistenceError;
pub use feedback::FeedbackStore;
pub use governance_event::{GovernanceEvent, GovernanceLog};
pub use model::{ProvenanceEntry, SessionRecord};
pub use orchestrator_decision::{
    ClassCalibration, DecisionOutcome, DecisionOutcomeKind, OrchestratorDecision,
    OrchestratorDecisionLog,
};
pub use store::{SqliteStore, Store};
