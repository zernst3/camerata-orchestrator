//! camerata-persistence: SQLite-backed session state, append-only provenance
//! log, and version-tracked artifact store.
//!
//! Public surface:
//! - [`SessionId`], [`ProvenanceEntry`] — domain value types (newtypes)
//! - [`Store`] — async trait (the seam for session/provenance)
//! - [`ArtifactStore`] — async trait (the seam for artifact revisions)
//! - [`SqliteStore`] — production impl backed by sqlx + SQLite
//! - [`ArtifactKind`], [`EditActor`], [`RevisionOp`] — artifact domain enums
//! - [`NewRevision`], [`ArtifactRevision`] — artifact input/output types
//! - [`encode`] — serialize a typed value to an artifact payload string
//!
//! Conventions honored:
//! - RUST-DOMAIN-4: newtype IDs
//! - RUST-DOMAIN-5: async I/O throughout
//! - RUST-DOMAIN-6: thiserror error enum
//! - SQL-AUDIT-COLUMNS-1: `created_at` on every table
//! - SQL-DB-INDEX-1/2: FK and WHERE columns indexed
//! - RUST-PURE-STATE-TRANSITIONS-1: builder helpers are pure
//! - ORCH-NEW-PATH-TESTS-1: unit tests included

pub mod artifacts;
pub mod error;
pub mod model;
pub mod store;

pub use artifacts::{
    encode, ArtifactKind, ArtifactRevision, ArtifactStore, EditActor, NewRevision, RevisionOp,
};
pub use error::PersistenceError;
pub use model::{ProvenanceEntry, SessionRecord};
pub use store::{SqliteStore, Store};
