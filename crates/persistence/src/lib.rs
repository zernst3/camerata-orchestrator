//! camerata-persistence: SQLite-backed session state + append-only provenance log.
//!
//! Public surface:
//! - [`SessionId`], [`ProvenanceEntry`] — domain value types (newtypes)
//! - [`Store`] — async trait (the seam)
//! - [`SqliteStore`] — production impl backed by sqlx + SQLite
//!
//! Conventions honored:
//! - RUST-DOMAIN-4: newtype IDs
//! - RUST-DOMAIN-5: async I/O throughout
//! - RUST-DOMAIN-6: thiserror error enum
//! - SQL-AUDIT-COLUMNS-1: `created_at` on every table
//! - SQL-DB-INDEX-1/2: FK and WHERE columns indexed
//! - RUST-PURE-STATE-TRANSITIONS-1: builder helpers are pure
//! - ORCH-NEW-PATH-TESTS-1: unit tests included

pub mod error;
pub mod model;
pub mod store;

pub use error::PersistenceError;
pub use model::{ProvenanceEntry, SessionRecord};
pub use store::{SqliteStore, Store};
