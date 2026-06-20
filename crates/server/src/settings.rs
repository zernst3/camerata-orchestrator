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
    /// MACHINE-LOCAL per-repo path overrides: `owner/repo` → absolute local folder, for
    /// repos that live OUTSIDE the workspace-root convention. This is the resolution layer for
    /// the local-first model — it is keyed by repo identity, never travels in a project export,
    /// and is what makes an imported project's repos resolvable on THIS machine.
    #[serde(default)]
    pub repo_paths: std::collections::HashMap<String, String>,
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
