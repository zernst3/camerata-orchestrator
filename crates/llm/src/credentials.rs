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
//! - `anthropic_api_key`  — Anthropic API key for the `api` Claude backend. Every model
//!   client reads it through [`anthropic_api_key`] at the moment the client is built (never
//!   cached, never copied into process env), so a key saved or replaced in the UI takes
//!   effect on the next call with no restart. The `ANTHROPIC_API_KEY` env var is only the
//!   fallback (CI and existing `.env` setups).
//!
//! # Extension
//!
//! Add a new `pub const` and push its name into [`ALL_CREDENTIALS`]. The store and
//! endpoints pick it up automatically.

use std::collections::HashMap;
use std::sync::Mutex;

// ── Error + known credential names + HTTP wire shapes ─────────────────────────
//
// Relocated to `camerata_api_types::credentials` (Phase A of the DTO extraction) — pure
// data with no dependency on the `CredentialStore` trait / keychain behavior below.
// Re-exported so every existing `crate::credentials::X` call site keeps resolving
// unchanged.
pub use camerata_api_types::credentials::{
    CredentialError, CredentialListItem, SetCredentialReq, SetCredentialResp, ALL_CREDENTIALS,
    ANTHROPIC_API_KEY, GITHUB_TOKEN, OPENROUTER_API_KEY,
};

// ── Trait ─────────────────────────────────────────────────────────────────────

/// Abstraction over an app-wide credential vault. Implementations may be the real
/// OS keychain ([`KeyringCredentialStore`]) or an in-memory map ([`MemoryCredentialStore`])
/// used in tests.
pub trait CredentialStore: Send + Sync {
    /// Store a credential. Overwrites any existing value.
    fn set(&self, name: &str, value: &str) -> Result<(), CredentialError>;

    /// Retrieve a credential. Returns `None` when not set.
    fn get(&self, name: &str) -> Result<Option<String>, CredentialError>;

    /// Remove a credential. Removing one that is not set is not an error. A credential saved in
    /// the app takes precedence over its env var ([`resolve_credential`]), so this is the only
    /// way to hand a credential back to the env fallback.
    fn delete(&self, name: &str) -> Result<(), CredentialError>;

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
///
/// The real platform backend is selected by `keyring` Cargo features, enabled per target in
/// `crates/llm/Cargo.toml` (`apple-native`, `windows-native`, Secret Service on Linux). With
/// no backend feature keyring 3 silently uses a non-persistent MOCK store, so the tests below
/// (`production_keyring_store_is_not_the_mock*`) fail loudly if that ever regresses.
///
/// # Process cache (one Keychain prompt per launch)
///
/// macOS asks "wants to use your confidential information" on the first Keychain read of every
/// freshly built, unsigned binary, and reads happen per call on handler threads (a request would
/// hang on the dialog each time). So reads go through a PROCESS-WIDE cache
/// ([`CachedCredentialStore`]): the Keychain is read once per credential on first use, `set` and
/// `delete` write through to the Keychain and update the cache, and every later read is served
/// from memory. A key saved or replaced in the app still takes effect on the very next call (the
/// write updates the cache), so no restart is needed. A change made OUTSIDE the app (Keychain
/// Access, `security`) is not seen until the next launch.
pub struct KeyringCredentialStore;

const KEYRING_SERVICE: &str = "camerata";

/// The seam that keeps a test away from the real keychain: when this env var is `1`, the
/// production store ([`KeyringCredentialStore`]) holds credentials in process memory and never
/// opens the OS keychain, so every credential resolves from its env var fallback alone.
///
/// Why it exists: a freshly built binary has a new code signature, so its first Keychain read
/// makes macOS ask for permission and the process blocks on the dialog. Every test that spawns
/// the real `camerata` binary sets this, and a scan test in `crates/cli/tests` fails when one
/// does not.
///
/// It removes a credential source and adds none: a run under the seam can only have a key the
/// environment gives it, so the client lock is exactly as strict with it as without it. It is
/// read once, when the store is first used; the process says so on stderr once.
pub const NO_KEYCHAIN_ENV: &str = "CAMERATA_NO_KEYCHAIN";

/// Where the production store keeps credentials: the OS keychain, or process memory under
/// [`NO_KEYCHAIN_ENV`].
enum ProductionBacking {
    Keychain(ServiceKeyringStore),
    Memory(MemoryCredentialStore),
}

impl CredentialStore for ProductionBacking {
    fn set(&self, name: &str, value: &str) -> Result<(), CredentialError> {
        match self {
            Self::Keychain(s) => s.set(name, value),
            Self::Memory(s) => s.set(name, value),
        }
    }

    fn get(&self, name: &str) -> Result<Option<String>, CredentialError> {
        match self {
            Self::Keychain(s) => s.get(name),
            Self::Memory(s) => s.get(name),
        }
    }

    fn delete(&self, name: &str) -> Result<(), CredentialError> {
        match self {
            Self::Keychain(s) => s.delete(name),
            Self::Memory(s) => s.delete(name),
        }
    }
}

/// Pure: the keychain is bypassed exactly when the seam's value is `1` (surrounding whitespace
/// ignored). Unset, empty or anything else means the real keychain.
fn production_backing(no_keychain: Option<&str>) -> ProductionBacking {
    match no_keychain.map(str::trim) {
        Some("1") => ProductionBacking::Memory(MemoryCredentialStore::new()),
        _ => ProductionBacking::Keychain(ServiceKeyringStore::new(KEYRING_SERVICE)),
    }
}

impl KeyringCredentialStore {
    /// The one process-wide cached view of the `"camerata"` keychain service (or of the in-memory
    /// stand-in under [`NO_KEYCHAIN_ENV`]).
    fn shared() -> &'static CachedCredentialStore<ProductionBacking> {
        static SHARED: std::sync::OnceLock<CachedCredentialStore<ProductionBacking>> =
            std::sync::OnceLock::new();
        SHARED.get_or_init(|| {
            let backing = production_backing(std::env::var(NO_KEYCHAIN_ENV).ok().as_deref());
            if matches!(backing, ProductionBacking::Memory(_)) {
                eprintln!(
                    "[camerata-llm] {NO_KEYCHAIN_ENV}=1: the OS keychain is not read or written \
                     in this process; a credential comes from its env var alone"
                );
            }
            CachedCredentialStore::new(backing)
        })
    }
}

impl CredentialStore for KeyringCredentialStore {
    fn set(&self, name: &str, value: &str) -> Result<(), CredentialError> {
        Self::shared().set(name, value)
    }

    fn get(&self, name: &str) -> Result<Option<String>, CredentialError> {
        Self::shared().get(name)
    }

    fn delete(&self, name: &str) -> Result<(), CredentialError> {
        Self::shared().delete(name)
    }
}

/// A read-through, write-through cache over another store: the inner store is read at most once
/// per credential (until a read errors, which is never cached), `set` / `delete` write to the
/// inner store first and update the cache only when that succeeded.
pub struct CachedCredentialStore<S> {
    inner: S,
    cache: Mutex<HashMap<String, Option<String>>>,
}

impl<S: CredentialStore> CachedCredentialStore<S> {
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            cache: Mutex::new(HashMap::new()),
        }
    }
}

impl<S: CredentialStore> CredentialStore for CachedCredentialStore<S> {
    fn set(&self, name: &str, value: &str) -> Result<(), CredentialError> {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        self.inner.set(name, value)?;
        cache.insert(
            name.to_string(),
            Some(value.to_string()).filter(|v| !v.is_empty()),
        );
        Ok(())
    }

    fn get(&self, name: &str) -> Result<Option<String>, CredentialError> {
        // The lock is held across the first inner read on purpose: concurrent first readers
        // wait for ONE read (one OS prompt) instead of each raising their own.
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(hit) = cache.get(name) {
            return Ok(hit.clone());
        }
        let value = self.inner.get(name)?;
        cache.insert(name.to_string(), value.clone());
        Ok(value)
    }

    fn delete(&self, name: &str) -> Result<(), CredentialError> {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        self.inner.delete(name)?;
        cache.insert(name.to_string(), None);
        Ok(())
    }
}

/// The keychain-backed store for an arbitrary keychain SERVICE name. [`KeyringCredentialStore`]
/// is this with the fixed `"camerata"` service (every production read and write goes through
/// this one implementation); it exists separately so a test can exercise the exact same code
/// against a throwaway service without ever touching the user's real `"camerata"` entries.
pub struct ServiceKeyringStore {
    service: &'static str,
}

impl ServiceKeyringStore {
    pub const fn new(service: &'static str) -> Self {
        Self { service }
    }
}

impl CredentialStore for ServiceKeyringStore {
    fn set(&self, name: &str, value: &str) -> Result<(), CredentialError> {
        keyring::Entry::new(self.service, name)
            .map_err(|e| CredentialError::Keychain(e.to_string()))?
            .set_password(value)
            .map_err(|e| CredentialError::Keychain(e.to_string()))
    }

    fn get(&self, name: &str) -> Result<Option<String>, CredentialError> {
        let entry = keyring::Entry::new(self.service, name)
            .map_err(|e| CredentialError::Keychain(e.to_string()))?;
        match entry.get_password() {
            Ok(v) if !v.is_empty() => Ok(Some(v)),
            Ok(_) => Ok(None),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(CredentialError::Keychain(e.to_string())),
        }
    }

    fn delete(&self, name: &str) -> Result<(), CredentialError> {
        match keyring::Entry::new(self.service, name)
            .map_err(|e| CredentialError::Keychain(e.to_string()))?
            .delete_credential()
        {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
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

    fn delete(&self, name: &str) -> Result<(), CredentialError> {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(name);
        Ok(())
    }
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
///
/// Private on purpose: only [`resolve_credential`] (and its wrappers) may name an env var, so
/// the compiler, not a convention, keeps every other module from reading one directly.
fn resolve(store: &dyn CredentialStore, cred_name: &str, env_var: &str) -> Option<String> {
    // Keychain wins.
    match store.get(cred_name) {
        Ok(Some(v)) if !v.trim().is_empty() => return Some(v),
        Ok(_) => {}
        // A read error (locked keychain, no Secret Service daemon, backend failure) must not
        // silently degrade to the env fallback with no trace, that is undiagnosable. Warn,
        // then fall back. Once per credential per process: a missing daemon fails every call.
        Err(e) => warn_keychain_unavailable_once(cred_name, env_var, &e),
    }
    // Fall back to env var (back-compat: existing CI/dotenv setups keep working).
    std::env::var(env_var).ok().filter(|v| !v.trim().is_empty())
}

/// Log "the keychain is unavailable, using the env fallback" the FIRST time per credential per
/// process, never on every call (a Linux box with no Secret Service daemon fails each read).
fn warn_keychain_unavailable_once(cred_name: &str, env_var: &str, e: &CredentialError) {
    static SEEN: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let mut seen = SEEN.lock().unwrap_or_else(|p| p.into_inner());
    if seen.iter().any(|n| n == cred_name) {
        return;
    }
    seen.push(cred_name.to_string());
    eprintln!(
        "[camerata-llm] keychain read of `{cred_name}` failed ({e}); falling back to {env_var} \
         (this is logged once per process)"
    );
}

// The env vars that back each credential when the store holds none. PRIVATE: nothing outside
// this file can name them, so nothing outside this file can read them.
const ANTHROPIC_API_KEY_ENV: &str = "ANTHROPIC_API_KEY";
const OPENROUTER_API_KEY_ENV: &str = "OPENROUTER_API_KEY";
const GITHUB_TOKEN_ENV: &str = "CAMERATA_GITHUB_TOKEN";

/// The env-var fallback for a known credential name, `None` for an unknown one.
fn env_var_for(cred_name: &str) -> Option<&'static str> {
    match cred_name {
        ANTHROPIC_API_KEY => Some(ANTHROPIC_API_KEY_ENV),
        OPENROUTER_API_KEY => Some(OPENROUTER_API_KEY_ENV),
        GITHUB_TOKEN => Some(GITHUB_TOKEN_ENV),
        _ => None,
    }
}

/// THE one rule for every credential (Anthropic key, OpenRouter key, GitHub token): the value
/// saved in the credential store (the OS keychain, set from the app's Settings) first, the
/// credential's env var ([`env_var_for`]) only as a fallback (CI, existing `.env` setups).
/// Blank / whitespace-only values count as absent at both layers. Read it at the moment a
/// client is built and never cache it, so a credential saved or replaced in the UI takes
/// effect on the next call with no restart. A name with no env var resolves from the store
/// alone. The result is the secret itself: never log or print it (use [`mask_value`]).
pub fn resolve_credential(store: &dyn CredentialStore, cred_name: &str) -> Option<String> {
    match env_var_for(cred_name) {
        Some(env_var) => resolve(store, cred_name, env_var),
        None => store
            .get(cred_name)
            .ok()
            .flatten()
            .filter(|v| !v.trim().is_empty()),
    }
}

/// The OpenRouter API key: [`resolve_credential`] for `openrouter_api_key`.
pub fn openrouter_api_key(store: &dyn CredentialStore) -> Option<String> {
    resolve_credential(store, OPENROUTER_API_KEY)
}

/// The GitHub token: [`resolve_credential`] for `github_token`.
pub fn github_token(store: &dyn CredentialStore) -> Option<String> {
    resolve_credential(store, GITHUB_TOKEN)
}

/// The user-facing fix for a missing Anthropic API key, shared by every error and help text
/// so the wording cannot drift (no em-dashes in user-facing text).
pub const API_KEY_FIX_HINT: &str = "add the key in the app (Settings, Chat backend, API) or set \
     ANTHROPIC_API_KEY (a key saved in the app takes precedence over ANTHROPIC_API_KEY)";

/// THE one rule for which Anthropic API key every model call uses, in the app and the CLI:
/// the key saved in the credential store (the macOS Keychain, set from the app's Settings)
/// first; the `ANTHROPIC_API_KEY` env var only as a fallback (CI, existing `.env` setups).
/// Blank / whitespace-only values count as absent at both layers.
///
/// Call this at the moment a client is built and never cache the result: a key saved or
/// replaced in the UI is then picked up by the next call with no restart. Every non-test read
/// of `ANTHROPIC_API_KEY` must go through here (a source-scan test enforces it).
///
/// The returned value is the secret itself: never log or print it (use [`mask_value`]).
pub fn anthropic_api_key(store: &dyn CredentialStore) -> Option<String> {
    resolve_credential(store, ANTHROPIC_API_KEY)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

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
        assert_eq!(
            s.get(OPENROUTER_API_KEY).unwrap(),
            Some("sk-test-value".to_string())
        );
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
        assert_eq!(
            s.get(GITHUB_TOKEN).unwrap(),
            Some("second-value".to_string())
        );
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
    #[serial_test::serial(process_env)]
    fn resolve_prefers_store_over_env() {
        let s = store();
        s.set(GITHUB_TOKEN, "keychain-token").unwrap();
        // Even with an env var set, the store value wins.
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_GITHUB_TOKEN", "env-token");
        let token = resolve(s.as_ref(), GITHUB_TOKEN, "CAMERATA_GITHUB_TOKEN");
        assert_eq!(token, Some("keychain-token".to_string()));
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn resolve_falls_back_to_env_when_store_empty() {
        let s = store();
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_GITHUB_TOKEN", "env-only-token");
        let token = resolve(s.as_ref(), GITHUB_TOKEN, "CAMERATA_GITHUB_TOKEN");
        assert_eq!(token, Some("env-only-token".to_string()));
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn resolve_returns_none_when_both_absent() {
        let s = store();
        let _env = camerata_test_env::EnvGuard::remove("CAMERATA_GITHUB_TOKEN");
        let token = resolve(s.as_ref(), GITHUB_TOKEN, "CAMERATA_GITHUB_TOKEN");
        assert_eq!(token, None);
    }

    // ── resolve_credential (all three credentials, one rule) ──────────────────

    /// (credential name, env var) for every credential the app knows.
    const ALL_THREE: [(&str, &str); 3] = [
        (ANTHROPIC_API_KEY, "ANTHROPIC_API_KEY"),
        (OPENROUTER_API_KEY, "OPENROUTER_API_KEY"),
        (GITHUB_TOKEN, "CAMERATA_GITHUB_TOKEN"),
    ];

    #[test]
    fn every_known_credential_has_an_env_fallback() {
        for name in ALL_CREDENTIALS {
            assert!(env_var_for(name).is_some(), "{name} has no env fallback");
        }
        for (name, env_var) in ALL_THREE {
            assert_eq!(env_var_for(name), Some(env_var));
        }
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn all_three_credentials_follow_store_then_env_with_blank_as_absent() {
        for (name, env_var) in ALL_THREE {
            let s = store();
            let _unset = camerata_test_env::EnvGuard::remove(env_var);
            assert_eq!(resolve_credential(s.as_ref(), name), None, "{name}: empty");
            // Saved after "startup": the next read sees it; replaced: the next read sees that.
            s.set(name, "store-1").unwrap();
            assert_eq!(
                resolve_credential(s.as_ref(), name).as_deref(),
                Some("store-1")
            );
            s.set(name, "store-2").unwrap();
            assert_eq!(
                resolve_credential(s.as_ref(), name).as_deref(),
                Some("store-2")
            );
            // Store wins over env.
            let _env = camerata_test_env::EnvGuard::set(env_var, "env-value");
            assert_eq!(
                resolve_credential(s.as_ref(), name).as_deref(),
                Some("store-2")
            );
            // Blank store value falls through to env; blank env is absent.
            s.set(name, "  ").unwrap();
            assert_eq!(
                resolve_credential(s.as_ref(), name).as_deref(),
                Some("env-value")
            );
            let _blank = camerata_test_env::EnvGuard::set(env_var, "  ");
            assert_eq!(resolve_credential(s.as_ref(), name), None, "{name}: blank");
        }
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn thin_wrappers_agree_with_resolve_credential() {
        let s = store();
        let _a = camerata_test_env::EnvGuard::remove("ANTHROPIC_API_KEY");
        let _o = camerata_test_env::EnvGuard::remove("OPENROUTER_API_KEY");
        let _g = camerata_test_env::EnvGuard::remove("CAMERATA_GITHUB_TOKEN");
        s.set(OPENROUTER_API_KEY, "or").unwrap();
        s.set(GITHUB_TOKEN, "gh").unwrap();
        assert_eq!(openrouter_api_key(s.as_ref()).as_deref(), Some("or"));
        assert_eq!(github_token(s.as_ref()).as_deref(), Some("gh"));
        assert_eq!(anthropic_api_key(s.as_ref()), None);
    }

    // ── anthropic_api_key (the one resolver) ──────────────────────────────────

    #[test]
    #[serial_test::serial(process_env)]
    fn anthropic_key_comes_from_the_store_with_no_env() {
        let s = store();
        let _env = camerata_test_env::EnvGuard::remove("ANTHROPIC_API_KEY");
        assert_eq!(anthropic_api_key(s.as_ref()), None);
        // Saved AFTER the (hypothetical) startup: the very next read sees it.
        s.set(ANTHROPIC_API_KEY, "sk-ant-saved").unwrap();
        assert_eq!(
            anthropic_api_key(s.as_ref()).as_deref(),
            Some("sk-ant-saved")
        );
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn anthropic_key_replacement_takes_effect_on_the_next_read() {
        let s = store();
        let _env = camerata_test_env::EnvGuard::remove("ANTHROPIC_API_KEY");
        s.set(ANTHROPIC_API_KEY, "sk-ant-old").unwrap();
        assert_eq!(anthropic_api_key(s.as_ref()).as_deref(), Some("sk-ant-old"));
        s.set(ANTHROPIC_API_KEY, "sk-ant-new").unwrap();
        assert_eq!(anthropic_api_key(s.as_ref()).as_deref(), Some("sk-ant-new"));
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn anthropic_key_falls_back_to_env_when_store_empty() {
        let s = store();
        let _env = camerata_test_env::EnvGuard::set("ANTHROPIC_API_KEY", "sk-ant-env");
        assert_eq!(anthropic_api_key(s.as_ref()).as_deref(), Some("sk-ant-env"));
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn anthropic_key_store_wins_over_env() {
        let s = store();
        s.set(ANTHROPIC_API_KEY, "sk-ant-store").unwrap();
        let _env = camerata_test_env::EnvGuard::set("ANTHROPIC_API_KEY", "sk-ant-env");
        assert_eq!(
            anthropic_api_key(s.as_ref()).as_deref(),
            Some("sk-ant-store")
        );
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn anthropic_key_blank_values_count_as_absent() {
        let s = store();
        s.set(ANTHROPIC_API_KEY, "   ").unwrap();
        let _env = camerata_test_env::EnvGuard::remove("ANTHROPIC_API_KEY");
        assert_eq!(anthropic_api_key(s.as_ref()), None);
        // A blank store value does not shadow a real env key.
        let _env2 = camerata_test_env::EnvGuard::set("ANTHROPIC_API_KEY", "sk-ant-env");
        assert_eq!(anthropic_api_key(s.as_ref()).as_deref(), Some("sk-ant-env"));
        // And a blank env value is absent too.
        let _env3 = camerata_test_env::EnvGuard::set("ANTHROPIC_API_KEY", "  ");
        assert_eq!(anthropic_api_key(s.as_ref()), None);
    }

    // ── the no-keychain seam ──────────────────────────────────────────────────

    #[test]
    fn the_seam_bypasses_the_keychain_only_when_set_to_one() {
        for value in [Some("1"), Some(" 1 ")] {
            assert!(
                matches!(production_backing(value), ProductionBacking::Memory(_)),
                "{value:?} must select the in-memory backing"
            );
        }
        for value in [None, Some(""), Some("0"), Some("true"), Some("yes")] {
            assert!(
                matches!(production_backing(value), ProductionBacking::Keychain(_)),
                "{value:?} must leave the real keychain in place"
            );
        }
    }

    /// Under the seam the production backing is a plain in-memory store: it starts empty (so a
    /// credential comes from its env var alone), round-trips a value without the keychain, and
    /// hands a deleted credential back to the env fallback like the real store does.
    #[test]
    fn under_the_seam_the_production_backing_is_an_empty_memory_store() {
        let backing = production_backing(Some("1"));
        assert_eq!(backing.get(ANTHROPIC_API_KEY).unwrap(), None);
        backing.set(ANTHROPIC_API_KEY, "sk-ant-memory").unwrap();
        assert_eq!(
            backing.get(ANTHROPIC_API_KEY).unwrap().as_deref(),
            Some("sk-ant-memory")
        );
        backing.delete(ANTHROPIC_API_KEY).unwrap();
        assert_eq!(backing.get(ANTHROPIC_API_KEY).unwrap(), None);
    }

    // ── the production store must be a REAL keychain, never keyring's mock ────

    /// Compile-time proof that the current platform's real backend is compiled in: these
    /// modules only exist when the matching keyring feature is enabled. With no backend
    /// feature keyring 3 falls back to a non-persistent mock and this fails to build.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_keychain_backend_is_compiled_in() {
        let _: Option<keyring::macos::MacCredential> = None;
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_credential_manager_backend_is_compiled_in() {
        let _: Option<keyring::windows::WinCredential> = None;
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_secret_service_backend_is_compiled_in() {
        let _: Option<keyring::secret_service::SsCredential> = None;
    }

    /// Always-on, cross-platform: the workspace lockfile resolves keyring WITH every platform's
    /// real backend crate. A mock-only keyring depends on `log` and `zeroize` alone.
    #[test]
    fn cargo_lock_resolves_keyring_with_real_platform_backends() {
        let lock =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.lock"))
                .expect("workspace Cargo.lock must be readable");
        let block = lock
            .split("[[package]]")
            .find(|b| b.contains("name = \"keyring\""))
            .expect("keyring must be in Cargo.lock");
        for backend in ["security-framework", "windows-sys", "dbus-secret-service"] {
            assert!(
                block.contains(&format!("\"{backend}")),
                "keyring resolves without `{backend}`: it is running on the non-persistent MOCK store \
                 on that platform. Enable the platform backend features in crates/llm/Cargo.toml.\n{block}"
            );
        }
    }

    /// A value written through the production keychain implementation must be readable back
    /// (keyring's mock hands every `Entry` its own empty credential, so a `get` after a `set`
    /// returns nothing). Uses a THROWAWAY service name, never the user's real `"camerata"`
    /// entries, and deletes what it wrote. Opt-in on every platform (`cargo test -- --ignored`):
    /// it is the one test that touches the real OS keychain, and on macOS a freshly built test
    /// binary can block on a Keychain permission dialog, which would stall an unattended gate.
    /// The two tests above still fail loudly, without any keychain, if the real backend is ever
    /// compiled out.
    #[test]
    #[ignore = "touches the real OS keychain and may wait on a permission dialog; run with \
                --ignored in an attended session"]
    fn production_keyring_store_is_not_the_mock_round_trip() {
        const SERVICE: &str = "camerata-keychain-selftest-DELETE-ME";
        let name = format!("probe-{}", std::process::id());
        let store = ServiceKeyringStore::new(SERVICE);
        // A previous crashed run may have left the item behind: start clean.
        store.delete(&name).unwrap();
        store.set(&name, "selftest-value").unwrap();
        let read_back = store.get(&name);
        // Clean up first so a failed assertion cannot leave the item behind.
        let _ = store.delete(&name);
        assert_eq!(
            read_back.unwrap().as_deref(),
            Some("selftest-value"),
            "a value saved to the keychain could not be read back: keyring is on its MOCK store"
        );
        assert_eq!(
            store.get(&name).unwrap(),
            None,
            "the throwaway entry must be deleted"
        );
    }

    // ── delete + the process cache ────────────────────────────────────────────

    #[test]
    #[serial_test::serial(process_env)]
    fn deleting_a_saved_credential_hands_it_back_to_the_env_fallback() {
        let s = store();
        let _env = camerata_test_env::EnvGuard::set("ANTHROPIC_API_KEY", "sk-ant-env");
        s.set(ANTHROPIC_API_KEY, "sk-ant-saved").unwrap();
        assert_eq!(
            anthropic_api_key(s.as_ref()).as_deref(),
            Some("sk-ant-saved")
        );
        s.delete(ANTHROPIC_API_KEY).unwrap();
        assert_eq!(anthropic_api_key(s.as_ref()).as_deref(), Some("sk-ant-env"));
        assert!(!s.is_set(ANTHROPIC_API_KEY).unwrap());
        // Deleting what is not set is not an error.
        s.delete(ANTHROPIC_API_KEY).unwrap();
    }

    /// An inner store that counts how often it is actually read.
    struct CountingStore {
        inner: MemoryCredentialStore,
        reads: std::sync::atomic::AtomicUsize,
    }

    impl CredentialStore for CountingStore {
        fn set(&self, name: &str, value: &str) -> Result<(), CredentialError> {
            self.inner.set(name, value)
        }
        fn get(&self, name: &str) -> Result<Option<String>, CredentialError> {
            self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.inner.get(name)
        }
        fn delete(&self, name: &str) -> Result<(), CredentialError> {
            self.inner.delete(name)
        }
    }

    fn counting() -> (
        CachedCredentialStore<Arc<CountingStore>>,
        Arc<CountingStore>,
    ) {
        let inner = Arc::new(CountingStore {
            inner: MemoryCredentialStore::new(),
            reads: std::sync::atomic::AtomicUsize::new(0),
        });
        (CachedCredentialStore::new(inner.clone()), inner)
    }

    impl<T: CredentialStore> CredentialStore for Arc<T> {
        fn set(&self, name: &str, value: &str) -> Result<(), CredentialError> {
            (**self).set(name, value)
        }
        fn get(&self, name: &str) -> Result<Option<String>, CredentialError> {
            (**self).get(name)
        }
        fn delete(&self, name: &str) -> Result<(), CredentialError> {
            (**self).delete(name)
        }
    }

    fn reads(inner: &CountingStore) -> usize {
        inner.reads.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[test]
    fn the_cache_reads_the_backing_store_once_per_credential() {
        let (cached, inner) = counting();
        inner.inner.set(ANTHROPIC_API_KEY, "sk-ant-1").unwrap();
        for _ in 0..5 {
            assert_eq!(
                cached.get(ANTHROPIC_API_KEY).unwrap().as_deref(),
                Some("sk-ant-1")
            );
        }
        assert_eq!(reads(&inner), 1, "later reads must be served from memory");
        // An absent credential is cached as absent too.
        for _ in 0..3 {
            assert_eq!(cached.get(GITHUB_TOKEN).unwrap(), None);
        }
        assert_eq!(reads(&inner), 2);
    }

    #[test]
    fn set_and_delete_write_through_and_update_the_cache_without_a_restart() {
        let (cached, inner) = counting();
        assert_eq!(cached.get(ANTHROPIC_API_KEY).unwrap(), None);
        cached.set(ANTHROPIC_API_KEY, "sk-ant-new").unwrap();
        assert_eq!(
            cached.get(ANTHROPIC_API_KEY).unwrap().as_deref(),
            Some("sk-ant-new")
        );
        assert_eq!(
            inner.inner.get(ANTHROPIC_API_KEY).unwrap().as_deref(),
            Some("sk-ant-new"),
            "the write must reach the backing store"
        );
        cached.set(ANTHROPIC_API_KEY, "sk-ant-newer").unwrap();
        assert_eq!(
            cached.get(ANTHROPIC_API_KEY).unwrap().as_deref(),
            Some("sk-ant-newer")
        );
        cached.delete(ANTHROPIC_API_KEY).unwrap();
        assert_eq!(cached.get(ANTHROPIC_API_KEY).unwrap(), None);
        assert_eq!(inner.inner.get(ANTHROPIC_API_KEY).unwrap(), None);
        assert_eq!(reads(&inner), 1, "set/delete never trigger a backing read");
    }

    #[test]
    fn a_failed_write_leaves_the_cache_untouched() {
        struct FailingWrites;
        impl CredentialStore for FailingWrites {
            fn set(&self, _: &str, _: &str) -> Result<(), CredentialError> {
                Err(CredentialError::Keychain("denied".into()))
            }
            fn get(&self, _: &str) -> Result<Option<String>, CredentialError> {
                Ok(Some("old".into()))
            }
            fn delete(&self, _: &str) -> Result<(), CredentialError> {
                Err(CredentialError::Keychain("denied".into()))
            }
        }
        let cached = CachedCredentialStore::new(FailingWrites);
        assert!(cached.set(GITHUB_TOKEN, "new").is_err());
        assert!(cached.delete(GITHUB_TOKEN).is_err());
        assert_eq!(cached.get(GITHUB_TOKEN).unwrap().as_deref(), Some("old"));
    }

    #[test]
    fn a_read_error_is_not_cached() {
        struct FlakyOnce(std::sync::atomic::AtomicBool);
        impl CredentialStore for FlakyOnce {
            fn set(&self, _: &str, _: &str) -> Result<(), CredentialError> {
                Ok(())
            }
            fn get(&self, _: &str) -> Result<Option<String>, CredentialError> {
                if self.0.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    Err(CredentialError::Keychain("locked".into()))
                } else {
                    Ok(Some("v".into()))
                }
            }
            fn delete(&self, _: &str) -> Result<(), CredentialError> {
                Ok(())
            }
        }
        let cached =
            CachedCredentialStore::new(FlakyOnce(std::sync::atomic::AtomicBool::new(true)));
        assert!(cached.get(GITHUB_TOKEN).is_err());
        assert_eq!(cached.get(GITHUB_TOKEN).unwrap().as_deref(), Some("v"));
    }

    #[test]
    fn the_fix_hint_names_both_sources_and_the_precedence() {
        assert!(API_KEY_FIX_HINT.contains("in the app"));
        assert!(API_KEY_FIX_HINT.contains("ANTHROPIC_API_KEY"));
        assert!(API_KEY_FIX_HINT.contains("takes precedence"));
        assert!(!API_KEY_FIX_HINT.contains('\u{2014}'));
    }
}
