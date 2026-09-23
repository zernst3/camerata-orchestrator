//! Persisted app-level settings (not project-scoped).
//!
//! Today this holds the single thing the local-checkout subsystem needs: the
//! WORKSPACE ROOT the architect picks once — the visible folder under which every
//! project's repos are cloned (`<root>/<owner>/<repo>`). The fleet edits those local
//! clones, the developer runs/tests them, and an explicit step pushes + opens a PR.
//! Persisted to a JSON file in the per-user data dir, like the project store.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};

pub use camerata_llm::ProjectBackend;

/// Tolerant deserialization for `chat_backend`: accepts the field missing entirely
/// (`#[serde(default)]` handles that), an explicit `null` (the shape the OLD `llm_backend:
/// Option<String>` field could persist), or a `"cli"`/`"api"` string (the OLD field's other
/// values, and the CURRENT wire representation) — collapsing anything that isn't exactly
/// `"api"` (case-insensitive, trimmed) to `ProjectBackend::Cli`, the default. This is what
/// lets an old `settings.json` with `"llm_backend": null` or `"llm_backend": "api"` still
/// load cleanly under the new field (via `#[serde(alias = "llm_backend")]`) instead of
/// hard-failing the whole document's parse.
fn deserialize_chat_backend<'de, D>(deserializer: D) -> Result<ProjectBackend, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(deserializer)?;
    Ok(match opt.as_deref().map(|s| s.trim().to_ascii_lowercase()) {
        Some(ref s) if s == "api" => ProjectBackend::Api,
        _ => ProjectBackend::Cli,
    })
}

/// The persisted settings document.
#[derive(Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Settings {
    /// Absolute path to the workspace root where project repos are cloned. `None`
    /// until the architect picks one (the UI prompts for it before any checkout).
    #[serde(default)]
    pub workspace_root: Option<String>,
    /// MACHINE-LOCAL per-repo path overrides: `owner/repo` → absolute local folder, for
    /// repos that live OUTSIDE the workspace-root convention. This is the resolution layer for
    /// the local-first model — it is keyed by repo identity, never travels in a project export,
    /// and is what makes an imported project's repos resolvable on THIS machine.
    #[serde(default)]
    pub repo_paths: std::collections::HashMap<String, String>,
    /// APP-LEVEL (cross-project) model id for the GLOBAL chat assistant. The chat is a
    /// single global assistant, so its model is an app setting, NOT a per-project step.
    /// `None`/blank means "use the [`crate::llm::DEFAULT_MODEL`] floor". An explicit
    /// per-request `model` on the chat POST still overrides this (highest precedence).
    #[serde(default)]
    pub chat_model: Option<String>,
    /// The GLOBAL CHAT backend (`docs/design/2026-09-22_per-project-backend.md`): used
    /// ONLY by the project-less chatbox assistant — nothing project-related reads this.
    /// `Cli` (the default) spawns the logged-in Claude Code CLI (no API key); `Api` uses the
    /// Anthropic Messages API (`ANTHROPIC_API_KEY`). No env fallback and no cross-setting
    /// override: this is the assistant's ENTIRE backend input.
    ///
    /// Renamed from the old `llm_backend: Option<String>` field, which used to ALSO govern
    /// project-scoped work (the pre-per-project-backend model) and had a three-way
    /// precedence (stored setting > `CAMERATA_LLM_BACKEND` env > `cli` default) — both are
    /// gone. `#[serde(alias = "llm_backend")]` + the tolerant [`deserialize_chat_backend`]
    /// means an old settings.json (key `llm_backend`, value `null`/`"cli"`/`"api"`/anything
    /// else) still loads cleanly, collapsing straight to this field.
    #[serde(default, alias = "llm_backend", deserialize_with = "deserialize_chat_backend")]
    pub chat_backend: ProjectBackend,
    /// OpenRouter provider-safety policy (safe-by-default no-train/no-retain provider
    /// enforcement) — see `docs/design/2026-07-28_openrouter-provider-safety.md`.
    /// `#[serde(default)]` so a settings file predating this field (or a hand-trimmed
    /// one) still deserializes to `ProviderPolicy::default()` (`safe_mode: true`), never
    /// panics and never silently resolves to an unsafe posture.
    #[serde(default)]
    pub provider_policy: camerata_llm::provider_policy::ProviderPolicy,
}

/// Clone-shareable settings store, persisted to a JSON file so the workspace choice
/// survives restarts.
#[derive(Clone, Default)]
pub struct SettingsStore {
    inner: std::sync::Arc<Mutex<Settings>>,
    /// Where the store persists. `None` = in-memory only (tests).
    path: Option<std::sync::Arc<std::path::PathBuf>>,
}

impl SettingsStore {
    /// An empty, NON-persisted store (tests / clean in-memory use).
    pub fn new() -> Self {
        Self::default()
    }

    /// Load settings from `path` (or start empty), persisting every change back.
    pub fn load_or_new(path: std::path::PathBuf) -> Self {
        let settings = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<Settings>(&s).ok())
            .unwrap_or_default();
        Self {
            inner: std::sync::Arc::new(Mutex::new(settings)),
            path: Some(std::sync::Arc::new(path)),
        }
    }

    /// Write the current settings to disk (best-effort).
    fn save(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let Ok(settings) = self.inner.lock() else {
            return;
        };
        if let Ok(json) = serde_json::to_string_pretty(&*settings) {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(path.as_path(), json);
        }
    }

    /// The current settings.
    pub fn get(&self) -> Settings {
        self.inner.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// The configured workspace root, if one is set and non-empty.
    pub fn workspace_root(&self) -> Option<String> {
        self.get().workspace_root.filter(|p| !p.trim().is_empty())
    }

    /// Set (or clear) the workspace root, persisting the change.
    pub fn set_workspace_root(&self, path: Option<String>) -> Settings {
        let updated = {
            let mut s = match self.inner.lock() {
                Ok(s) => s,
                Err(_) => return Settings::default(),
            };
            s.workspace_root = path.filter(|p| !p.trim().is_empty());
            s.clone()
        };
        self.save();
        updated
    }

    /// The app-level chat-assistant model, if one is set and non-empty.
    pub fn chat_model(&self) -> Option<String> {
        self.get().chat_model.filter(|m| !m.trim().is_empty())
    }

    /// Set (or clear) the app-level chat-assistant model, persisting the change.
    pub fn set_chat_model(&self, model: Option<String>) -> Settings {
        let updated = {
            let mut s = match self.inner.lock() {
                Ok(s) => s,
                Err(_) => return Settings::default(),
            };
            s.chat_model = model.filter(|m| !m.trim().is_empty());
            s.clone()
        };
        self.save();
        updated
    }

    /// The GLOBAL CHAT backend (`Cli` | `Api`) — used ONLY by the project-less chatbox
    /// assistant. Never `None`: `Cli` is both the serde default and the [`Default`] impl, so
    /// this always resolves to a concrete value with no env fallback of any kind.
    pub fn chat_backend(&self) -> ProjectBackend {
        self.get().chat_backend
    }

    /// Set the global chat backend, persisting the change. Returns the updated settings.
    pub fn set_chat_backend(&self, backend: ProjectBackend) -> Settings {
        let updated = {
            let mut s = match self.inner.lock() {
                Ok(s) => s,
                Err(_) => return Settings::default(),
            };
            s.chat_backend = backend;
            s.clone()
        };
        self.save();
        updated
    }

    /// The current OpenRouter provider-safety policy. Never `None` — defaults to
    /// `ProviderPolicy::default()` (`safe_mode: true`) when nothing has been stored yet,
    /// so every request-building call site gets a safe answer even before any settings
    /// file exists.
    pub fn provider_policy(&self) -> camerata_llm::provider_policy::ProviderPolicy {
        self.get().provider_policy
    }

    /// Set the OpenRouter provider-safety policy, persisting the change. Returns the
    /// updated settings. Note: per the design doc, turning `safe_mode` off is meant to
    /// be a SESSION-scoped act (reset to safe on app restart) — that reset behavior is a
    /// Pass-2 app-lifecycle concern (e.g. calling this with the default at startup), not
    /// enforced by this setter itself, which just persists whatever it's given.
    pub fn set_provider_policy(
        &self,
        policy: camerata_llm::provider_policy::ProviderPolicy,
    ) -> Settings {
        let updated = {
            let mut s = match self.inner.lock() {
                Ok(s) => s,
                Err(_) => return Settings::default(),
            };
            s.provider_policy = policy;
            s.clone()
        };
        self.save();
        updated
    }

    /// **Pass 2's session-scoped safety reset** (design doc §3): force `safe_mode` back to
    /// `true`, unconditionally, on every app start — regardless of what was persisted.
    /// `pinned_provider` is preserved (only `safe_mode` is session-scoped; the pin may
    /// persist across restarts).
    ///
    /// MECHANISM: called once from `AppState::from_env`, right after the settings file
    /// loads, so it runs at the top of every BFF process boot. The desktop shell
    /// (`crates/ui/src/main.rs` → `server_process::ensure_server_running`) spawns a FRESH
    /// BFF subprocess on every app launch (unless reusing an already-healthy standalone
    /// server on `:8787`, a dev-only edge case — see the design doc's "Pass 2 landed"
    /// section for that caveat), so "server process boot" and "app session start" coincide
    /// in the shipped desktop flow. This was chosen over a purely client-side session flag
    /// because `safe_mode` is read server-side by the enforcement seam
    /// (`provider_policy::provider_constraint_for_request`) on every request — a
    /// server-side reset is the only place that can guarantee testing mode never survives
    /// a restart even if the UI never loads (e.g. a routine/cron run against the same BFF).
    ///
    /// A no-op write when `safe_mode` is already `true` (the common case) — does not touch
    /// disk unless a reset is actually needed, so a normal safe-mode boot doesn't rewrite
    /// `settings.json` on every launch.
    pub fn reset_provider_policy_to_safe_on_startup(
        &self,
    ) -> camerata_llm::provider_policy::ProviderPolicy {
        let current = self.provider_policy();
        if current.safe_mode {
            return current;
        }
        self.set_provider_policy(camerata_llm::provider_policy::ProviderPolicy {
            safe_mode: true,
            pinned_provider: current.pinned_provider,
        })
        .provider_policy
    }

    /// The machine-local override path for `repo` (`owner/repo`), if one was set.
    pub fn repo_path(&self, repo: &str) -> Option<String> {
        self.get()
            .repo_paths
            .get(repo)
            .cloned()
            .filter(|p| !p.trim().is_empty())
    }

    /// Record (or clear, when `path` is empty) the machine-local override for `repo`.
    pub fn set_repo_path(&self, repo: &str, path: Option<String>) {
        {
            let Ok(mut s) = self.inner.lock() else { return };
            match path.filter(|p| !p.trim().is_empty()) {
                Some(p) => {
                    s.repo_paths.insert(repo.to_string(), p);
                }
                None => {
                    s.repo_paths.remove(repo);
                }
            }
        }
        self.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_get_workspace_root() {
        let store = SettingsStore::new();
        assert!(store.workspace_root().is_none());
        store.set_workspace_root(Some("/Users/me/Camerata".to_string()));
        assert_eq!(
            store.workspace_root().as_deref(),
            Some("/Users/me/Camerata")
        );
        // Empty / whitespace clears it.
        store.set_workspace_root(Some("   ".to_string()));
        assert!(store.workspace_root().is_none());
    }

    #[test]
    fn set_and_get_chat_model() {
        let store = SettingsStore::new();
        assert!(store.chat_model().is_none());
        store.set_chat_model(Some("claude-opus-5".to_string()));
        assert_eq!(store.chat_model().as_deref(), Some("claude-opus-5"));
        // Empty / whitespace clears it.
        store.set_chat_model(Some("   ".to_string()));
        assert!(store.chat_model().is_none());
    }

    #[test]
    fn provider_policy_defaults_to_safe_mode_on_with_no_pin_before_anything_is_set() {
        let store = SettingsStore::new();
        let policy = store.provider_policy();
        assert!(policy.safe_mode, "a fresh store must read as safe_mode=true");
        assert_eq!(policy.pinned_provider, None);
    }

    #[test]
    fn set_and_get_provider_policy() {
        let store = SettingsStore::new();
        let policy = camerata_llm::provider_policy::ProviderPolicy {
            safe_mode: false,
            pinned_provider: Some("deepinfra".to_string()),
        };
        store.set_provider_policy(policy.clone());
        assert_eq!(store.provider_policy(), policy);
    }

    #[test]
    fn provider_policy_persists_across_reload() {
        let dir =
            std::env::temp_dir().join(format!("camerata-settings-policy-{}", std::process::id()));
        let path = dir.join("settings.json");
        let _ = std::fs::remove_dir_all(&dir);
        {
            let store = SettingsStore::load_or_new(path.clone());
            store.set_provider_policy(camerata_llm::provider_policy::ProviderPolicy {
                safe_mode: false,
                pinned_provider: Some("deepinfra".to_string()),
            });
        }
        let reloaded = SettingsStore::load_or_new(path);
        let policy = reloaded.provider_policy();
        assert!(!policy.safe_mode);
        assert_eq!(policy.pinned_provider.as_deref(), Some("deepinfra"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A settings.json predating this feature (no `provider_policy` key at all) must
    /// still load as safe_mode=true — the whole point of `#[serde(default)]` on the
    /// field. This is the fail-safe-on-upgrade guarantee.
    #[test]
    fn settings_file_without_provider_policy_key_loads_as_safe_default() {
        let dir = std::env::temp_dir()
            .join(format!("camerata-settings-legacy-{}", std::process::id()));
        let path = dir.join("settings.json");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A settings file with only the pre-existing fields, no `provider_policy` key.
        std::fs::write(&path, r#"{"workspace_root": "/tmp/ws", "chat_model": null, "llm_backend": null}"#).unwrap();
        let store = SettingsStore::load_or_new(path);
        let policy = store.provider_policy();
        assert!(policy.safe_mode, "missing provider_policy key must default to safe_mode=true");
        assert_eq!(policy.pinned_provider, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── Pass 2: session-scoped safety reset ─────────────────────────────────────

    #[test]
    fn startup_reset_forces_off_to_on_and_preserves_the_pin() {
        let store = SettingsStore::new();
        store.set_provider_policy(camerata_llm::provider_policy::ProviderPolicy {
            safe_mode: false,
            pinned_provider: Some("deepinfra".to_string()),
        });
        assert!(!store.provider_policy().safe_mode, "precondition: testing mode was on");

        let reset = store.reset_provider_policy_to_safe_on_startup();

        assert!(reset.safe_mode, "the reset must force safe_mode back to true");
        assert_eq!(
            reset.pinned_provider.as_deref(),
            Some("deepinfra"),
            "the pin is NOT session-scoped — it must survive the reset"
        );
        // The reset must have actually persisted, not just returned a value.
        assert!(store.provider_policy().safe_mode);
    }

    #[test]
    fn startup_reset_is_a_noop_when_already_safe() {
        let store = SettingsStore::new();
        store.set_provider_policy(camerata_llm::provider_policy::ProviderPolicy {
            safe_mode: true,
            pinned_provider: Some("novita".to_string()),
        });
        let reset = store.reset_provider_policy_to_safe_on_startup();
        assert!(reset.safe_mode);
        assert_eq!(reset.pinned_provider.as_deref(), Some("novita"));
    }

    #[test]
    fn startup_reset_on_a_fresh_store_with_no_policy_ever_set_stays_safe() {
        // A brand-new store (no settings.json yet) already reads as safe_mode=true via the
        // struct default — the reset must not disturb that, and must not panic on a store
        // with nothing persisted.
        let store = SettingsStore::new();
        let reset = store.reset_provider_policy_to_safe_on_startup();
        assert!(reset.safe_mode);
        assert_eq!(reset.pinned_provider, None);
    }

    /// End-to-end across a simulated restart: persist testing mode to disk, "restart" by
    /// loading a fresh `SettingsStore` from the same path (mirrors what `AppState::from_env`
    /// does on every real boot), run the startup reset, and confirm safe_mode reads back as
    /// true from disk — the exact guarantee the design doc's §3 asks for ("can never be
    /// silently left on across sessions").
    #[test]
    fn startup_reset_survives_a_simulated_process_restart() {
        let dir = std::env::temp_dir()
            .join(format!("camerata-settings-reset-{}", std::process::id()));
        let path = dir.join("settings.json");
        let _ = std::fs::remove_dir_all(&dir);
        {
            // "Session 1": operator turns testing mode on and it persists to disk.
            let store = SettingsStore::load_or_new(path.clone());
            store.set_provider_policy(camerata_llm::provider_policy::ProviderPolicy {
                safe_mode: false,
                pinned_provider: Some("deepinfra".to_string()),
            });
        }
        {
            // "Session 2": a fresh process boots, loads the same file, and runs the reset —
            // exactly the `AppState::from_env` sequence.
            let store = SettingsStore::load_or_new(path.clone());
            assert!(!store.provider_policy().safe_mode, "loaded the OFF value from disk");
            store.reset_provider_policy_to_safe_on_startup();
        }
        // "Session 3": reload again to prove the reset itself persisted (not just an
        // in-memory return value that a real process boot would also throw away).
        let reloaded = SettingsStore::load_or_new(path);
        assert!(reloaded.provider_policy().safe_mode, "safe_mode must never survive a restart as OFF");
        assert_eq!(reloaded.provider_policy().pinned_provider.as_deref(), Some("deepinfra"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn set_and_get_chat_backend() {
        let store = SettingsStore::new();
        assert_eq!(store.chat_backend(), ProjectBackend::Cli, "a fresh store defaults to Cli");
        store.set_chat_backend(ProjectBackend::Api);
        assert_eq!(store.chat_backend(), ProjectBackend::Api);
        store.set_chat_backend(ProjectBackend::Cli);
        assert_eq!(store.chat_backend(), ProjectBackend::Cli);
    }

    #[test]
    fn chat_backend_persists_across_reload() {
        let dir =
            std::env::temp_dir().join(format!("camerata-settings-chatbackend-{}", std::process::id()));
        let path = dir.join("settings.json");
        let _ = std::fs::remove_dir_all(&dir);
        {
            let store = SettingsStore::load_or_new(path.clone());
            store.set_chat_backend(ProjectBackend::Api);
        }
        // A fresh load sees the persisted backend.
        let reloaded = SettingsStore::load_or_new(path);
        assert_eq!(reloaded.chat_backend(), ProjectBackend::Api);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// MIGRATION: a settings.json persisted by the OLD `llm_backend: Option<String>` model
    /// (key `llm_backend`, not `chat_backend`) must still load — via `#[serde(alias =
    /// "llm_backend")]` — collapsing straight onto the new `chat_backend` field.
    #[test]
    fn legacy_llm_backend_key_migrates_to_chat_backend() {
        let dir = std::env::temp_dir()
            .join(format!("camerata-settings-legacy-backend-{}", std::process::id()));
        let path = dir.join("settings.json");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        std::fs::write(&path, r#"{"llm_backend": "api"}"#).unwrap();
        let store = SettingsStore::load_or_new(path.clone());
        assert_eq!(
            store.chat_backend(),
            ProjectBackend::Api,
            "a legacy llm_backend=\"api\" settings.json must migrate to chat_backend=Api"
        );

        std::fs::write(&path, r#"{"llm_backend": null}"#).unwrap();
        let store_null = SettingsStore::load_or_new(path.clone());
        assert_eq!(
            store_null.chat_backend(),
            ProjectBackend::Cli,
            "a legacy llm_backend=null settings.json must migrate to chat_backend=Cli"
        );

        std::fs::write(&path, "{}").unwrap();
        let store_absent = SettingsStore::load_or_new(path);
        assert_eq!(
            store_absent.chat_backend(),
            ProjectBackend::Cli,
            "a settings.json with no llm_backend/chat_backend key at all defaults to Cli"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn persists_across_reload() {
        let dir = std::env::temp_dir().join(format!("camerata-settings-{}", std::process::id()));
        let path = dir.join("settings.json");
        let _ = std::fs::remove_dir_all(&dir);
        {
            let store = SettingsStore::load_or_new(path.clone());
            store.set_workspace_root(Some("/tmp/ws".to_string()));
        }
        // A fresh load sees the persisted value.
        let reloaded = SettingsStore::load_or_new(path);
        assert_eq!(reloaded.workspace_root().as_deref(), Some("/tmp/ws"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
