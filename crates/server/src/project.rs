//! The Project container: the foundational data scope (ADR
//! project_container_and_rules_management). A project groups the repos in scope and
//! the full ruleset — per-repo base-rule selections, the cross-repo rules, the
//! process rules, and the architect's custom rules. The user switches between
//! projects; everything reads the active one.
//!
//! This is also the persistence home for the NON-REPO rules (cross-repo / process):
//! they span repos or are account-level, so they cannot live in any single repo's
//! `.camerata/` file — they live here, and the engine's gates read them from here.
//! Repo-local rules are additionally emitted into each repo (see `arm`).

use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// An architect-authored rule (not from the corpus). Preserved across base-rule
/// upserts — camerata-ai's `CustomRule`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CustomRule {
    /// Short name (the emitted block is `### CUSTOM-{name}`).
    pub name: String,
    /// Free-text directive body.
    pub body: String,
    /// Domain it applies to (routes it to the matching repos; `*` = all).
    #[serde(default)]
    pub domain: String,
}

/// One selected BASE rule: which corpus/gate rule, the chosen alternative (if it
/// has options), and the repos it installs into.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuleSelection {
    /// The rule id.
    pub rule_id: String,
    /// The chosen alternative option id, when the rule has alternatives.
    #[serde(default)]
    pub chosen_option: Option<String>,
    /// The repos this rule installs into (its placement).
    #[serde(default)]
    pub repos: Vec<String>,
}

/// The project's full ruleset — the single source of truth that emit upserts.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectRuleset {
    /// Base (repo-local) rule selections with their bindings + chosen alternatives.
    #[serde(default)]
    pub selections: Vec<RuleSelection>,
    /// Cross-repo rule ids (API contracts) — project-level; the integration gate
    /// reads these.
    #[serde(default)]
    pub cross_repo: Vec<RuleSelection>,
    /// Process rule ids (commit/PR format) — project-level; the VCS-action gate
    /// reads these.
    #[serde(default)]
    pub process: Vec<RuleSelection>,
    /// Architect-authored custom rules — preserved across base upserts.
    #[serde(default)]
    pub custom: Vec<CustomRule>,
}

/// One project.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Project {
    /// Stable project id.
    pub id: String,
    /// Human name.
    pub name: String,
    /// The repos in scope (`owner/repo`).
    #[serde(default)]
    pub repos: Vec<String>,
    /// The project's ruleset (the source of truth).
    #[serde(default)]
    pub ruleset: ProjectRuleset,
    /// Repos that have been ONBOARDED (`owner/repo`) — the governance ruleset has been applied
    /// to them. Per-repo so a multi-repo project can be partially onboarded; travels with the
    /// project's export. A repo NOT in this set is "not yet onboarded".
    #[serde(default)]
    pub onboarded: Vec<String>,
}

impl Project {
    /// Replace the BASE rules (selections / cross-repo / process) from an edit or
    /// import, while PRESERVING the architect's custom rules. This is the upsert
    /// the ADR calls out: changing/deleting/adding base rules never clobbers custom.
    pub fn upsert_base_rules(
        &mut self,
        selections: Vec<RuleSelection>,
        cross_repo: Vec<RuleSelection>,
        process: Vec<RuleSelection>,
    ) {
        self.ruleset.selections = selections;
        self.ruleset.cross_repo = cross_repo;
        self.ruleset.process = process;
        // self.ruleset.custom is intentionally untouched.
    }

    /// Merge incoming custom rules by name: an incoming rule with an existing name
    /// REPLACES it (an explicit edit); a new name is ADDED. Existing custom rules
    /// NOT named in `incoming` are KEPT — an import/upsert never drops a custom rule
    /// the user didn't touch. (Deletion is an explicit `remove_custom`, never a
    /// side effect of an upsert.)
    pub fn merge_custom(&mut self, incoming: &[CustomRule]) {
        for c in incoming {
            if let Some(existing) = self.ruleset.custom.iter_mut().find(|x| x.name == c.name) {
                *existing = c.clone();
            } else {
                self.ruleset.custom.push(c.clone());
            }
        }
    }

    /// Mark `repos` as onboarded (union, deduped). Repos not already in the project's `repos`
    /// list are added there too, so onboarding a repo also brings it into scope.
    pub fn mark_onboarded(&mut self, repos: &[String]) {
        for r in repos {
            if !self.onboarded.iter().any(|x| x == r) {
                self.onboarded.push(r.clone());
            }
            if !self.repos.iter().any(|x| x == r) {
                self.repos.push(r.clone());
            }
        }
    }

    /// Explicitly remove a custom rule by name (the ONLY way a custom rule leaves
    /// the project). Returns true if one was removed.
    pub fn remove_custom(&mut self, name: &str) -> bool {
        let before = self.ruleset.custom.len();
        self.ruleset.custom.retain(|c| c.name != name);
        self.ruleset.custom.len() != before
    }
}

/// Project store + the active selection, persisted to a JSON file so projects
/// (their configs + pointers, NOT repo contents) survive across launches.
/// Clone-shareable.
#[derive(Clone, Default)]
pub struct ProjectStore {
    inner: std::sync::Arc<Mutex<State>>,
    /// Where the store persists. `None` = in-memory only (tests).
    path: Option<std::sync::Arc<std::path::PathBuf>>,
}

#[derive(Default, Serialize, Deserialize)]
struct State {
    projects: Vec<Project>,
    active: Option<String>,
    counter: usize,
}

impl ProjectStore {
    /// An empty, NON-persisted store (tests / clean in-memory use).
    pub fn new() -> Self {
        Self::default()
    }

    /// Load the store from `path` (or start empty if it doesn't exist yet), and
    /// persist every change back to it. This is what the running app uses, so
    /// projects survive restarts.
    pub fn load_or_new(path: std::path::PathBuf) -> Self {
        let state = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<State>(&s).ok())
            .unwrap_or_default();
        Self {
            inner: std::sync::Arc::new(Mutex::new(state)),
            path: Some(std::sync::Arc::new(path)),
        }
    }

    /// Write the current state to disk (best-effort; a write failure does not break
    /// the running store).
    fn save(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let Ok(state) = self.inner.lock() else {
            return;
        };
        if let Ok(json) = serde_json::to_string_pretty(&*state) {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(path.as_path(), json);
        }
    }

    /// All projects.
    pub fn list(&self) -> Vec<Project> {
        self.inner.lock().map(|s| s.projects.clone()).unwrap_or_default()
    }

    /// Look up a project by id.
    pub fn get(&self, id: &str) -> Option<Project> {
        let s = self.inner.lock().ok()?;
        s.projects.iter().find(|p| p.id == id).cloned()
    }

    /// The active project (the first one if none is explicitly active).
    pub fn active(&self) -> Option<Project> {
        let s = self.inner.lock().ok()?;
        match &s.active {
            Some(id) => s.projects.iter().find(|p| &p.id == id).cloned(),
            None => s.projects.first().cloned(),
        }
    }

    /// Create a project and make it active.
    pub fn create(&self, name: &str, repos: Vec<String>) -> Option<Project> {
        let project = {
            let mut s = self.inner.lock().ok()?;
            s.counter += 1;
            let id = format!("proj-{}", s.counter);
            let project = Project {
                id: id.clone(),
                name: name.to_string(),
                repos,
                ruleset: ProjectRuleset::default(),
                onboarded: Vec::new(),
            };
            s.projects.push(project.clone());
            s.active = Some(id);
            project
        };
        self.save();
        Some(project)
    }

    /// Import a project from an external JSON export: give it a FRESH id (so it never
    /// collides with an existing one), make it active. Name / repos / ruleset come from
    /// the imported document.
    pub fn import(&self, name: &str, repos: Vec<String>, ruleset: ProjectRuleset) -> Option<Project> {
        let project = {
            let mut s = self.inner.lock().ok()?;
            s.counter += 1;
            let id = format!("proj-{}", s.counter);
            let project = Project {
                id: id.clone(),
                name: name.to_string(),
                repos,
                ruleset,
                onboarded: Vec::new(),
            };
            s.projects.push(project.clone());
            s.active = Some(id);
            project
        };
        self.save();
        Some(project)
    }

    /// Delete a project by id. If it was the active one, the active pointer falls back
    /// to the first remaining project (or none). Returns true if one was removed.
    pub fn delete(&self, id: &str) -> bool {
        let ok = {
            let mut s = match self.inner.lock() {
                Ok(s) => s,
                Err(_) => return false,
            };
            let before = s.projects.len();
            s.projects.retain(|p| p.id != id);
            let removed = s.projects.len() != before;
            if removed && s.active.as_deref() == Some(id) {
                s.active = s.projects.first().map(|p| p.id.clone());
            }
            removed
        };
        if ok {
            self.save();
        }
        ok
    }

    /// Set the active project.
    pub fn set_active(&self, id: &str) -> bool {
        let ok = {
            let mut s = match self.inner.lock() {
                Ok(s) => s,
                Err(_) => return false,
            };
            if s.projects.iter().any(|p| p.id == id) {
                s.active = Some(id.to_string());
                true
            } else {
                false
            }
        };
        if ok {
            self.save();
        }
        ok
    }

    /// Mutate a project in place by id, returning the updated copy.
    pub fn update<F: FnOnce(&mut Project)>(&self, id: &str, f: F) -> Option<Project> {
        let updated = {
            let mut s = self.inner.lock().ok()?;
            let p = s.projects.iter_mut().find(|p| p.id == id)?;
            f(p);
            p.clone()
        };
        self.save();
        Some(updated)
    }
}

/// Export a project's ruleset as pretty JSON (the portable source of truth).
pub fn export_ruleset(project: &Project) -> String {
    serde_json::to_string_pretty(&project.ruleset).unwrap_or_else(|_| "{}".to_string())
}

/// Parse an imported ruleset JSON into its base + custom parts. The caller applies
/// the base parts via `upsert_base_rules` (preserving existing custom) and may
/// merge custom separately.
pub fn parse_ruleset(json: &str) -> anyhow::Result<ProjectRuleset> {
    Ok(serde_json::from_str(json)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(id: &str) -> RuleSelection {
        RuleSelection {
            rule_id: id.to_string(),
            chosen_option: None,
            repos: vec!["me/api".to_string()],
        }
    }

    #[test]
    fn upsert_base_preserves_custom_rules() {
        let mut p = Project {
            id: "p1".into(),
            name: "Proj".into(),
            repos: vec!["me/api".into()],
            onboarded: vec![],
            ruleset: ProjectRuleset {
                selections: vec![sel("OLD-1")],
                cross_repo: vec![],
                process: vec![],
                custom: vec![CustomRule {
                    name: "house-style".into(),
                    body: "Prefer X.".into(),
                    domain: "*".into(),
                }],
            },
        };
        // Replace base rules entirely.
        p.upsert_base_rules(vec![sel("NEW-1"), sel("NEW-2")], vec![], vec![]);
        assert_eq!(p.ruleset.selections.len(), 2);
        assert_eq!(p.ruleset.selections[0].rule_id, "NEW-1");
        // Custom rule survived the base upsert.
        assert_eq!(p.ruleset.custom.len(), 1);
        assert_eq!(p.ruleset.custom[0].name, "house-style");
    }

    fn custom(name: &str, body: &str) -> CustomRule {
        CustomRule {
            name: name.to_string(),
            body: body.to_string(),
            domain: "*".to_string(),
        }
    }

    #[test]
    fn merge_custom_keeps_untouched_edits_named_and_never_drops() {
        let mut p = Project {
            id: "p".into(),
            name: "P".into(),
            repos: vec![],
            onboarded: vec![],
            ruleset: ProjectRuleset {
                custom: vec![custom("a", "A1"), custom("b", "B1")],
                ..Default::default()
            },
        };
        // An import that only mentions "a" (edited) and a new "c" — "b" is untouched.
        p.merge_custom(&[custom("a", "A2"), custom("c", "C1")]);
        let by_name = |n: &str| p.ruleset.custom.iter().find(|c| c.name == n).cloned();
        assert_eq!(by_name("a").unwrap().body, "A2", "named custom rule was edited");
        assert_eq!(by_name("b").unwrap().body, "B1", "untouched custom rule REMAINS");
        assert!(by_name("c").is_some(), "new custom rule added");
        assert_eq!(p.ruleset.custom.len(), 3, "nothing dropped");
    }

    #[test]
    fn custom_rules_only_leave_on_explicit_remove() {
        let mut p = Project {
            id: "p".into(),
            name: "P".into(),
            repos: vec![],
            onboarded: vec![],
            ruleset: ProjectRuleset {
                custom: vec![custom("keep", "K"), custom("gone", "G")],
                ..Default::default()
            },
        };
        // A base-rule upsert does NOT remove custom rules.
        p.upsert_base_rules(vec![sel("X")], vec![], vec![]);
        assert_eq!(p.ruleset.custom.len(), 2, "upsert never drops custom");
        // Only an explicit remove does.
        assert!(p.remove_custom("gone"));
        assert!(!p.remove_custom("nope"));
        assert_eq!(p.ruleset.custom.len(), 1);
        assert_eq!(p.ruleset.custom[0].name, "keep");
    }

    #[test]
    fn store_create_active_and_switch() {
        let store = ProjectStore::new();
        let a = store.create("A", vec!["me/api".into()]).unwrap();
        let b = store.create("B", vec!["me/web".into()]).unwrap();
        assert_eq!(store.list().len(), 2);
        // Newest created is active.
        assert_eq!(store.active().unwrap().id, b.id);
        assert!(store.set_active(&a.id));
        assert_eq!(store.active().unwrap().id, a.id);
        assert!(!store.set_active("nope"));
    }

    #[test]
    fn export_import_round_trip() {
        let project = Project {
            id: "p".into(),
            name: "P".into(),
            repos: vec!["me/api".into()],
            onboarded: vec![],
            ruleset: ProjectRuleset {
                selections: vec![sel("R-1")],
                cross_repo: vec![sel("INTEGRATION-API-CONTRACT-1")],
                process: vec![sel("PROCESS-CONVENTIONAL-COMMIT-1")],
                custom: vec![],
            },
        };
        let json = export_ruleset(&project);
        let back = parse_ruleset(&json).unwrap();
        assert_eq!(back, project.ruleset);
    }
}
