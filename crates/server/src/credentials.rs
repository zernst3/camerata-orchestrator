//! App-wide, keychain-backed credentials manager.
//!
//! Credentials (API keys, tokens) are stored in the OS keychain (macOS Keychain /
//! Windows Credential Manager / libsecret on Linux) via the `keyring` crate — never
//! in env files, never in app config, never in the repo. The UI sets them once; the
//! backend reads them at call time only.
//!
//! The full credential value is NEVER sent over HTTP. The only thing that crosses the
//! wire is a **masked** form: first 4 chars + `••••` (U+2022) suffix.
//!
//! # Known credentials
//!
//! - `openrouter_api_key` — OpenRouter model discovery + API driver.
//! - `github_token`       — PAT for GitHub issues / PRs / push (replaces env-only
//!   `CAMERATA_GITHUB_TOKEN`; env var remains as back-compat fallback).
//!
//! # Extension
//!
//! Add a new `pub const` and push its name into [`ALL_CREDENTIALS`]. The store and
//! endpoints pick it up automatically.

use std::collections::HashMap;
use std::sync::Mutex;

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

/// Canonical set of all known credential names. Used by the list endpoint and as an
/// allowlist in the set endpoint (rejects arbitrary names).
pub const ALL_CREDENTIALS: &[&str] = &[OPENROUTER_API_KEY, GITHUB_TOKEN];

// ── Trait ─────────────────────────────────────────────────────────────────────

/// Abstraction over an app-wide credential vault. Implementations may be the real
/// OS keychain ([`KeyringCredentialStore`]) or an in-memory map ([`MemoryCredentialStore`])
/// used in tests.
pub trait CredentialStore: Send + Sync {
    /// Store a credential. Overwrites any existing value.
    fn set(&self, name: &str, value: &str) -> Result<(), CredentialError>;

    /// Retrieve a credential. Returns `None` when not set.
    fn get(&self, name: &str) -> Result<Option<String>, CredentialError>;

    /// Whether the credential is set (non-empty).
    fn is_set(&self, name: &str) -> Result<bool, CredentialError> {
        Ok(self.get(name)?.is_some())
    }

    /// Return a masked form of the credential: first 4 chars + `••••`, or `None` if
    /// the credential is not set.  This is the ONLY form that may cross the HTTP
    /// boundary — the full value must never be sent to the UI.
    fn masked(&self, name: &str) -> Result<Option<String>, CredentialError> {
        match self.get(name)? {
            None => Ok(None),
            Some(v) => Ok(Some(mask_value(&v))),
        }
    }
}

/// Produce a masked credential display: first 4 chars (or fewer if the value is
/// shorter) + the `••••` suffix.  The suffix uses U+2022 BULLET, not asterisks,
/// matching the design spec.
pub fn mask_value(v: &str) -> String {
    let prefix: String = v.chars().take(4).collect();
    format!("{prefix}••••")
}

// ── KeyringCredentialStore (production) ───────────────────────────────────────

/// Stores credentials in the OS keychain via the `keyring` crate.
/// Service name is fixed at `"camerata"` so all credentials live under the same
/// keychain entry group.
pub struct KeyringCredentialStore;

const KEYRING_SERVICE: &str = "camerata";

impl CredentialStore for KeyringCredentialStore {
    fn set(&self, name: &str, value: &str) -> Result<(), CredentialError> {
        keyring::Entry::new(KEYRING_SERVICE, name)
            .map_err(|e| CredentialError::Keychain(e.to_string()))?
            .set_password(value)
            .map_err(|e| CredentialError::Keychain(e.to_string()))
    }

    fn get(&self, name: &str) -> Result<Option<String>, CredentialError> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, name)
            .map_err(|e| CredentialError::Keychain(e.to_string()))?;
        match entry.get_password() {
            Ok(v) if !v.is_empty() => Ok(Some(v)),
            Ok(_) => Ok(None),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(CredentialError::Keychain(e.to_string())),
        }
    }
}

// ── MemoryCredentialStore (tests) ─────────────────────────────────────────────

/// In-memory credential store for unit tests. No OS interaction.
#[derive(Default)]
pub struct MemoryCredentialStore {
    map: Mutex<HashMap<String, String>>,
}

impl MemoryCredentialStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl CredentialStore for MemoryCredentialStore {
    fn set(&self, name: &str, value: &str) -> Result<(), CredentialError> {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name.to_string(), value.to_string());
        Ok(())
    }

    fn get(&self, name: &str) -> Result<Option<String>, CredentialError> {
        let guard = self.map.lock().unwrap_or_else(|e| e.into_inner());
        Ok(guard.get(name).filter(|v| !v.is_empty()).cloned())
    }
}

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

// ── Handler helpers ───────────────────────────────────────────────────────────

/// Build the list of known credentials with their is_set / masked state.
pub fn list_credentials(store: &dyn CredentialStore) -> Vec<CredentialListItem> {
    ALL_CREDENTIALS
        .iter()
        .map(|name| {
            let masked = store.masked(name).unwrap_or(None);
            let is_set = masked.is_some();
            CredentialListItem {
                name: name.to_string(),
                is_set,
                masked,
            }
        })
        .collect()
}

/// Check whether `name` is in the allowlist. Returns `Err` if not.
pub fn validate_name(name: &str) -> Result<(), CredentialError> {
    if ALL_CREDENTIALS.contains(&name) {
        Ok(())
    } else {
        Err(CredentialError::UnknownName(name.to_string()))
    }
}

/// Resolve a credential: keychain first, then env-var fallback.
///
/// Used by `AppState::github_token()` (and any future similar helpers) to give
/// the keychain priority while keeping the env var working for back-compat.
pub fn resolve(
    store: &dyn CredentialStore,
    cred_name: &str,
    env_var: &str,
) -> Option<String> {
    // Keychain wins.
    if let Ok(Some(v)) = store.get(cred_name) {
        return Some(v);
    }
    // Fall back to env var (back-compat: existing CI/dotenv setups keep working).
    std::env::var(env_var).ok().filter(|v| !v.is_empty())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use super::*;

    fn store() -> Arc<MemoryCredentialStore> {
        Arc::new(MemoryCredentialStore::new())
    }

    // ── mask_value ────────────────────────────────────────────────────────────

    #[test]
    fn mask_shows_first_four_chars_and_bullets() {
        assert_eq!(mask_value("sk-openrouter-abc123"), "sk-o••••");
    }

    #[test]
    fn mask_handles_short_values() {
        // Value shorter than 4 chars: show what's there + the bullet suffix.
        assert_eq!(mask_value("ab"), "ab••••");
        assert_eq!(mask_value("a"), "a••••");
    }

    #[test]
    fn mask_never_returns_full_value() {
        let full = "sk-openrouter-supersecretkey-xyz";
        let masked = mask_value(full);
        // The masked form must NOT contain the full value.
        assert!(!masked.contains(full));
        // And must NOT be longer than 4 visible chars + 4 bullets (8 chars from bullets).
        let bullet_count = masked.chars().filter(|&c| c == '\u{2022}').count();
        assert_eq!(bullet_count, 4);
    }

    // ── MemoryCredentialStore ─────────────────────────────────────────────────

    #[test]
    fn set_and_get_round_trip() {
        let s = store();
        s.set(OPENROUTER_API_KEY, "sk-test-value").unwrap();
        assert_eq!(s.get(OPENROUTER_API_KEY).unwrap(), Some("sk-test-value".to_string()));
    }

    #[test]
    fn get_returns_none_when_not_set() {
        let s = store();
        assert_eq!(s.get(OPENROUTER_API_KEY).unwrap(), None);
    }

    #[test]
    fn is_set_reflects_presence() {
        let s = store();
        assert!(!s.is_set(OPENROUTER_API_KEY).unwrap());
        s.set(OPENROUTER_API_KEY, "sk-x").unwrap();
        assert!(s.is_set(OPENROUTER_API_KEY).unwrap());
    }

    #[test]
    fn masked_returns_masked_form_when_set() {
        let s = store();
        s.set(OPENROUTER_API_KEY, "sk-openrouter-abc123").unwrap();
        let m = s.masked(OPENROUTER_API_KEY).unwrap();
        assert_eq!(m, Some("sk-o••••".to_string()));
    }

    #[test]
    fn masked_returns_none_when_not_set() {
        let s = store();
        assert_eq!(s.masked(OPENROUTER_API_KEY).unwrap(), None);
    }

    #[test]
    fn masked_never_returns_full_value() {
        let s = store();
        let full = "sk-openrouter-supersecret";
        s.set(GITHUB_TOKEN, full).unwrap();
        let masked = s.masked(GITHUB_TOKEN).unwrap().unwrap();
        assert!(!masked.contains(full));
        assert!(masked.len() < full.len());
    }

    #[test]
    fn overwrite_works() {
        let s = store();
        s.set(GITHUB_TOKEN, "first-value").unwrap();
        s.set(GITHUB_TOKEN, "second-value").unwrap();
        assert_eq!(s.get(GITHUB_TOKEN).unwrap(), Some("second-value".to_string()));
    }

    // ── list_credentials ─────────────────────────────────────────────────────

    #[test]
    fn list_credentials_shows_all_names_with_is_set_false() {
        let s = store();
        let list = list_credentials(s.as_ref());
        // Every known credential appears.
        assert_eq!(list.len(), ALL_CREDENTIALS.len());
        for item in &list {
            assert!(!item.is_set);
            assert!(item.masked.is_none());
        }
    }

    #[test]
    fn list_credentials_shows_is_set_true_when_stored() {
        let s = store();
        s.set(OPENROUTER_API_KEY, "sk-test").unwrap();
        let list = list_credentials(s.as_ref());
        let or_item = list.iter().find(|i| i.name == OPENROUTER_API_KEY).unwrap();
        assert!(or_item.is_set);
        assert!(or_item.masked.is_some());
        // The GITHUB_TOKEN entry must still read is_set=false.
        let gh_item = list.iter().find(|i| i.name == GITHUB_TOKEN).unwrap();
        assert!(!gh_item.is_set);
    }

    // ── validate_name ─────────────────────────────────────────────────────────

    #[test]
    fn validate_name_allows_known_names() {
        assert!(validate_name(OPENROUTER_API_KEY).is_ok());
        assert!(validate_name(GITHUB_TOKEN).is_ok());
    }

    #[test]
    fn validate_name_rejects_unknown_names() {
        assert!(validate_name("ANTHROPIC_API_KEY").is_err());
        assert!(validate_name("").is_err());
        assert!(validate_name("arbitrary_key").is_err());
    }

    // ── resolve (store-then-env fallback) ─────────────────────────────────────

    #[test]
    fn resolve_prefers_store_over_env() {
        let s = store();
        s.set(GITHUB_TOKEN, "keychain-token").unwrap();
        // Even with an env var set, the store value wins.
        std::env::set_var("CAMERATA_GITHUB_TOKEN", "env-token");
        let token = resolve(s.as_ref(), GITHUB_TOKEN, "CAMERATA_GITHUB_TOKEN");
        std::env::remove_var("CAMERATA_GITHUB_TOKEN");
        assert_eq!(token, Some("keychain-token".to_string()));
    }

    #[test]
    fn resolve_falls_back_to_env_when_store_empty() {
        let s = store();
        std::env::set_var("CAMERATA_GITHUB_TOKEN", "env-only-token");
        let token = resolve(s.as_ref(), GITHUB_TOKEN, "CAMERATA_GITHUB_TOKEN");
        std::env::remove_var("CAMERATA_GITHUB_TOKEN");
        assert_eq!(token, Some("env-only-token".to_string()));
    }

    #[test]
    fn resolve_returns_none_when_both_absent() {
        let s = store();
        std::env::remove_var("CAMERATA_GITHUB_TOKEN");
        let token = resolve(s.as_ref(), GITHUB_TOKEN, "CAMERATA_GITHUB_TOKEN");
        assert_eq!(token, None);
    }
}
