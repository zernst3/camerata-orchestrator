//! camerata-persistence: SQLite-backed session state, append-only provenance
//! log, version-tracked artifact store, and enforcement-catch ledger.
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
pub mod model;
pub mod store;

pub use artifacts::{
    encode, ArtifactKind, ArtifactRevision, ArtifactStore, EditActor, NewRevision, RevisionOp,
};
pub use enforcement_catch::{content_hash, EnforcementCatch, EnforcementCatchLedger};
pub use error::PersistenceError;
pub use model::{ProvenanceEntry, SessionRecord};
pub use store::{SqliteStore, Store};
