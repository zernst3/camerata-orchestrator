//! Crate-level error type (RUST-DOMAIN-6: thiserror per crate).

use thiserror::Error;

#[derive(Debug, Error)]
pub enum PersistenceError {
    #[error("sqlx error: {0}")]
    Sqlx(#[from] sqlx::Error),

    #[error("migration error: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),

    #[error("json serialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("session not found: {0}")]
    SessionNotFound(String),
}
