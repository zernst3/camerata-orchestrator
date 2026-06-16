//! Persisted app-level settings (not project-scoped).
//!
//! Today this holds the single thing the local-checkout subsystem needs: the
//! WORKSPACE ROOT the architect picks once — the visible folder under which every
//! project's repos are cloned (`<root>/<owner>/<repo>`). The fleet edits those local
//! clones, the developer runs/tests them, and an explicit step pushes + opens a PR.
//! Persisted to a JSON file in the per-user data dir, like the project store.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// The persisted settings document.
#[derive(Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Settings {
    /// Absolute path to the workspace root where project repos are cloned. `None`
    /// until the architect picks one (the UI prompts for it before any checkout).
    #[serde(default)]
    pub workspace_root: Option<String>,
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
        self.get()
            .workspace_root
            .filter(|p| !p.trim().is_empty())
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_get_workspace_root() {
        let store = SettingsStore::new();
        assert!(store.workspace_root().is_none());
        store.set_workspace_root(Some("/Users/me/Camerata".to_string()));
        assert_eq!(store.workspace_root().as_deref(), Some("/Users/me/Camerata"));
        // Empty / whitespace clears it.
        store.set_workspace_root(Some("   ".to_string()));
        assert!(store.workspace_root().is_none());
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
