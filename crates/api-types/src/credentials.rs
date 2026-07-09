//! Pure, serde-only credential wire shapes + the known-credential-name constants,
//! relocated here (Phase A of the DTO extraction) from `camerata_server::credentials`,
//! which re-exports every name below so `crate::credentials::X` call sites resolve
//! unchanged.
//!
//! The `CredentialStore` trait, `KeyringCredentialStore`, `MemoryCredentialStore`, and
//! the `resolve` fn all STAY in `camerata_server::credentials` (a later phase relocates
//! them) — they are behavior, not pure data.

use thiserror::Error;

// ── Error ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum CredentialError {
    #[error("keychain error: {0}")]
    Keychain(String),
    #[error("credential name '{0}' is not in the known-credential allowlist")]
    UnknownName(String),
}

// ── Known credential names ────────────────────────────────────────────────────

/// OpenRouter API key (model discovery + API driver).
pub const OPENROUTER_API_KEY: &str = "openrouter_api_key";
/// GitHub PAT (issues / PRs / push).  Replaces `CAMERATA_GITHUB_TOKEN` env var,
/// which remains supported as a back-compat fallback.
pub const GITHUB_TOKEN: &str = "github_token";
/// Anthropic API key for the `api` Claude backend.  Hydrated into the
/// `ANTHROPIC_API_KEY` env var at startup and on save; the env var remains supported
/// as a back-compat fallback.
pub const ANTHROPIC_API_KEY: &str = "anthropic_api_key";

/// Canonical set of all known credential names. Used by the list endpoint and as an
/// allowlist in the set endpoint (rejects arbitrary names).
pub const ALL_CREDENTIALS: &[&str] = &[OPENROUTER_API_KEY, GITHUB_TOKEN, ANTHROPIC_API_KEY];

// ── HTTP request/response shapes ──────────────────────────────────────────────

/// Response item for `GET /api/credentials`.
#[derive(serde::Serialize)]
pub struct CredentialListItem {
    pub name: String,
    pub is_set: bool,
    /// Masked form (first 4 chars + `••••`), or `None` when not set.
    pub masked: Option<String>,
}

/// Body for `POST /api/credentials/:name`.
#[derive(serde::Deserialize)]
pub struct SetCredentialReq {
    pub value: String,
}

/// Response for `POST /api/credentials/:name`.
#[derive(serde::Serialize)]
pub struct SetCredentialResp {
    pub ok: bool,
    pub masked: String,
}
