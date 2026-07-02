//! The ProjectStore: the persistence adapter for the Project aggregate (ADR
//! project_container_and_rules_management). The pure DOMAIN TYPES and pure `&self`/`&mut self`
//! transitions now live in the framework-agnostic core (`camerata_app_core::project`,
//! `RUST-HEADLESS-CORE-1` + `RUST-PURE-STATE-TRANSITIONS-1`); they are re-exported below so every
//! `crate::project::X` call site is unchanged. This adapter owns the STORE — the `Arc<Mutex>` +
//! filesystem persistence — and drives the core transitions to compute the next state, then persists.
//!
//! The store is the persistence home for the NON-REPO rules (cross-repo / process): they span repos
//! or are account-level, so they cannot live in any single repo's `.camerata/` file — they live here,
//! and the engine's gates read them from here. Repo-local rules are additionally emitted into each
//! repo (see `arm`).

use std::sync::Mutex;

use serde::{Deserialize, Serialize};

// The Project aggregate + its pure domain types / transitions now live in the framework-agnostic
// core (RUST-HEADLESS-CORE-1); re-exported so `crate::project::{...}` call sites are unchanged. The
// `DEFAULT_MODEL` const was relocated to core alongside them (D2: the honest minimal seam — project
// only READS the const, never calls the LLM, so a trait would be vacuous); `crate::llm` re-exports
// it. The ProjectStore below (Arc<Mutex> + JSON persistence) stays in this adapter.
pub use camerata_app_core::project::{
    default_hierarchy_schema, default_max_iterations, default_model, default_operating_principles,
    default_routine_secs, default_watched_secs, export_ruleset, parse_ruleset, CustomRule,
    HierarchySchema, ImportOutcome, L3ReviewConfig, MemoryEntry, MemoryKind, MemoryStatus,
    ModelProfile, OperatingPrinciple, Project, ProjectImport, ProjectRuleset, RuleSelection,
    StallThresholds, StepKind, StepModels, TypeRelation, WorkType, DEFAULT_MODEL,
};

use camerata_checks::vcs_action::ProcessRuleConfig;
use crate::model_tier::TierMap;

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
        let mut state = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<State>(&s).ok())
            .unwrap_or_default();
        // ISOLATION (A7): validate the persisted active pointer. A dangling `active` id
        // (project deleted out-of-band, hand-edited file, partial write) would otherwise
        // make `active()` fall through to `projects.first()`, silently grounding the chat
        // on the wrong project. Reset to the first project (or None when empty).
        let active_is_valid = state
            .active
            .as_ref()
            .is_some_and(|id| state.projects.iter().any(|p| &p.id == id));
        if !active_is_valid {
            state.active = state.projects.first().map(|p| p.id.clone());
        }
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
        self.inner
            .lock()
            .map(|s| s.projects.clone())
            .unwrap_or_default()
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
                max_iterations: default_max_iterations(),
                tier_map: TierMap::default(),
                process_rule_config: ProcessRuleConfig::default(),
                step_models: StepModels::default(),
                stall_thresholds: StallThresholds::default(),
                l3_review: L3ReviewConfig::default(),
                model_profile: ModelProfile::default(),
                vision_enabled: false,
                product_brief: String::new(),
                operating_principles: default_operating_principles(),
                memory: Vec::new(),
                hierarchy_schema: default_hierarchy_schema(),
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
    pub fn import(
        &self,
        name: &str,
        repos: Vec<String>,
        ruleset: ProjectRuleset,
    ) -> Option<Project> {
        Some(
            self.import_or_overwrite(
                name,
                ProjectImport {
                    repos,
                    ruleset,
                    max_iterations: default_max_iterations(),
                    ..Default::default()
                },
                false,
            )?
            .into_project(),
        )
    }

    /// Import (or overwrite) a project from an exported JSON document.
    ///
    /// The full TRANSFERABLE config travels in `import` (issue #111) — repos, ruleset,
    /// onboarded, plus tier_map / process_rule_config / step_models / stall_thresholds /
    /// l3_review / model_profile / vision_enabled / max_iterations. Per the config-vs-data
    /// ADR every `Project` field is config and must round-trip; only DATA (UoW, settings,
    /// drafts — separate stores) stays local.
    ///
    /// - No existing project with the same `name` → create a fresh id from `import`, make active.
    /// - Same `name` exists and `overwrite=false` → return `Conflict` (no mutation).
    /// - Same `name` exists and `overwrite=true` → replace the full config IN PLACE
    ///   (same id), make active.
    pub fn import_or_overwrite(
        &self,
        name: &str,
        import: ProjectImport,
        overwrite: bool,
    ) -> Option<ImportOutcome> {
        let outcome = {
            let mut s = self.inner.lock().ok()?;
            if let Some(existing) = s.projects.iter_mut().find(|p| p.name == name) {
                if !overwrite {
                    return Some(ImportOutcome::Conflict);
                }
                // Overwrite in place — keep the same id, replace the full transferable config.
                existing.repos = import.repos;
                existing.ruleset = import.ruleset;
                existing.onboarded = import.onboarded;
                existing.max_iterations = import.max_iterations;
                existing.tier_map = import.tier_map;
                existing.process_rule_config = import.process_rule_config;
                existing.step_models = import.step_models;
                existing.stall_thresholds = import.stall_thresholds;
                existing.l3_review = import.l3_review;
                existing.model_profile = import.model_profile;
                existing.vision_enabled = import.vision_enabled;
                existing.product_brief = import.product_brief;
                existing.operating_principles = import.operating_principles;
                existing.memory = import.memory;
                // Seed the default ladder when the import omitted a schema (empty), but keep an
                // intentionally-provided non-empty imported schema untouched — resolve_effective
                // only substitutes when the schema is unusable.
                existing.hierarchy_schema = import.hierarchy_schema.resolve_effective();
                let updated = existing.clone();
                s.active = Some(updated.id.clone());
                ImportOutcome::Overwritten(updated)
            } else {
                s.counter += 1;
                let id = format!("proj-{}", s.counter);
                let project = Project {
                    id: id.clone(),
                    name: name.to_string(),
                    repos: import.repos,
                    ruleset: import.ruleset,
                    onboarded: import.onboarded,
                    max_iterations: import.max_iterations,
                    tier_map: import.tier_map,
                    process_rule_config: import.process_rule_config,
                    step_models: import.step_models,
                    stall_thresholds: import.stall_thresholds,
                    l3_review: import.l3_review,
                    model_profile: import.model_profile,
                    vision_enabled: import.vision_enabled,
                    product_brief: import.product_brief,
                    operating_principles: import.operating_principles,
                    memory: import.memory,
                    // Seed the default ladder when the import omitted a schema (empty), but keep
                    // an intentionally-provided non-empty imported schema untouched.
                    hierarchy_schema: import.hierarchy_schema.resolve_effective(),
                };
                s.projects.push(project.clone());
                s.active = Some(id);
                ImportOutcome::Created(project)
            }
        };
        self.save();
        Some(outcome)
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

    /// Set the model for a single [`StepKind`] on ONE project (by id), persisting the
    /// change. Mirrors the tier-map write path: it mutates ONLY the named project and
    /// saves. Per-project isolation is structural — the closure borrows just that one
    /// project's `&mut`, so a change to project A can never touch project B. Returns the
    /// updated project, or `None` when no project has that id.
    pub fn set_step_model(
        &self,
        id: &str,
        step: StepKind,
        model: String,
    ) -> Option<Project> {
        self.update(id, |p| p.set_model_for_step(step, model))
    }

    /// Set the stall thresholds for a single project by id. Returns the updated project,
    /// or `None` when no project has that id.
    pub fn set_stall_thresholds(
        &self,
        id: &str,
        thresholds: StallThresholds,
    ) -> Option<Project> {
        self.update(id, |p| p.set_stall_thresholds(thresholds))
    }

    /// Set the L3 review configuration for a single project by id. Returns the updated
    /// project, or `None` when no project has that id.
    pub fn set_l3_review(&self, id: &str, config: L3ReviewConfig) -> Option<Project> {
        self.update(id, |p| p.set_l3_review(config))
    }

    /// Replace a project's work hierarchy schema (the design-page work-type graph). Returns the
    /// updated project, or `None` if no project has that id.
    pub fn set_hierarchy_schema(&self, id: &str, schema: HierarchySchema) -> Option<Project> {
        self.update(id, |p| p.hierarchy_schema = schema)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(id: &str) -> RuleSelection {
        RuleSelection {
            rule_id: id.to_string(),
            chosen_option: None,
            repos: vec!["me/api".to_string()],
            ..Default::default()
        }
    }

    #[test]
    fn new_projects_default_to_one_iteration() {
        let store = ProjectStore::new();
        let p = store.create("A", vec![]).unwrap();
        assert_eq!(
            p.max_iterations, 1,
            "the shipped default is a single bounce"
        );
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

    // ── import_or_overwrite ────────────────────────────────────────────────────

    fn ruleset_with(rule: &str) -> ProjectRuleset {
        ProjectRuleset {
            selections: vec![sel(rule)],
            ..Default::default()
        }
    }

    #[test]
    fn create_seeds_default_operating_principles_and_empty_brief() {
        let store = ProjectStore::new();
        let p = store.create("Acme", vec![]).unwrap();
        assert!(p.product_brief.is_empty(), "brief starts empty");
        assert!(
            !p.operating_principles.is_empty(),
            "a new project is seeded with the default operating principles"
        );
        assert!(
            p.operating_principles.iter().all(|x| x.enabled),
            "the shipped defaults are all enabled"
        );
        assert!(
            p.operating_principles
                .iter()
                .any(|x| x.id == "escalate-when-blocked"),
            "the escalate-when-blocked default is present"
        );
    }

    #[test]
    fn create_seeds_default_hierarchy_schema() {
        let store = ProjectStore::new();
        let p = store.create("Acme", vec![]).unwrap();
        let h = &p.hierarchy_schema;
        assert!(
            h.types.iter().any(|t| t.name == "Epic" && t.is_design_root),
            "a new project is seeded with the default ladder incl. Epic as a design root"
        );
        assert!(
            h.types.iter().any(|t| t.name == "Story"),
            "the default palette includes Story"
        );
        assert!(
            h.relations
                .iter()
                .any(|r| r.parent == "Feature" && r.child == "Defect"),
            "the default relations allow a Defect under a Feature (multiple child types per parent)"
        );
    }

    #[test]
    fn set_hierarchy_schema_replaces_and_persists() {
        let store = ProjectStore::new();
        let p = store.create("Acme", vec![]).unwrap();
        let custom = HierarchySchema {
            types: vec![WorkType {
                name: "Spike".to_string(),
                builtin: false,
                is_design_root: true,
            }],
            relations: vec![],
        };
        let updated = store.set_hierarchy_schema(&p.id, custom.clone()).unwrap();
        assert_eq!(
            updated.hierarchy_schema, custom,
            "the setter replaces the whole schema"
        );
        assert_eq!(
            store.get(&p.id).unwrap().hierarchy_schema,
            custom,
            "and the replacement is readable back off the store"
        );
    }

    #[test]
    fn import_round_trips_hierarchy_schema() {
        let store = ProjectStore::new();
        let custom = HierarchySchema {
            types: vec![WorkType {
                name: "Objective".to_string(),
                builtin: false,
                is_design_root: true,
            }],
            relations: vec![TypeRelation {
                parent: "Objective".to_string(),
                child: "KeyResult".to_string(),
            }],
        };
        let import = ProjectImport {
            hierarchy_schema: custom.clone(),
            ..Default::default()
        };
        let project = store
            .import_or_overwrite("Imported", import, false)
            .unwrap()
            .into_project();
        assert_eq!(
            project.hierarchy_schema, custom,
            "the schema travels through import (portable project-level field)"
        );
    }

    #[test]
    fn import_without_schema_seeds_the_default_ladder() {
        // The design-canvas "planner proposes zero children" bug: a project imported without a
        // schema used to get the EMPTY `HierarchySchema::default()`, which makes design-author
        // emit `ALLOWED_CHILD_TYPES: []`. It must now seed the default ladder instead.
        let store = ProjectStore::new();

        // (a) The `import()` convenience path (`..Default::default()` → empty schema).
        let project = store
            .import("Imported", vec![], ProjectRuleset::default())
            .expect("imported");
        assert!(
            project.hierarchy_schema.is_usable(),
            "imported project seeds a usable (non-empty) hierarchy schema",
        );
        assert_eq!(
            project.hierarchy_schema, default_hierarchy_schema(),
            "the seeded schema is the default ladder",
        );

        // (b) An explicit EMPTY schema on `import_or_overwrite` is also seeded (not clobbered
        //     when non-empty, but seeded when empty).
        let empty = store
            .import_or_overwrite(
                "Imported2",
                ProjectImport {
                    hierarchy_schema: HierarchySchema::default(),
                    ..Default::default()
                },
                false,
            )
            .unwrap()
            .into_project();
        assert_eq!(
            empty.hierarchy_schema, default_hierarchy_schema(),
            "an empty imported schema is seeded to the default ladder",
        );
    }

    #[test]
    fn import_round_trips_brief_and_principles() {
        let store = ProjectStore::new();
        let import = ProjectImport {
            product_brief: "An app for X; users care about Y; never compromise Z.".to_string(),
            operating_principles: vec![OperatingPrinciple {
                id: "custom-1".to_string(),
                text: "Ship the smallest correct thing.".to_string(),
                enabled: true,
            }],
            ..Default::default()
        };
        let p = match store.import_or_overwrite("Imported", import, false) {
            Some(ImportOutcome::Created(p)) => p,
            _ => panic!("expected a Created outcome"),
        };
        assert_eq!(
            p.product_brief,
            "An app for X; users care about Y; never compromise Z."
        );
        assert_eq!(
            p.operating_principles.len(),
            1,
            "imported custom principles replace the defaults, not append"
        );
        assert_eq!(p.operating_principles[0].id, "custom-1");
    }

    #[test]
    fn import_or_overwrite_creates_new_when_no_collision() {
        let store = ProjectStore::new();
        let outcome = store
            .import_or_overwrite(
                "Alpha",
                ProjectImport {
                    repos: vec!["me/api".into()],
                    ruleset: ruleset_with("R-1"),
                    max_iterations: default_max_iterations(),
                    ..Default::default()
                },
                false,
            )
            .unwrap();
        let project = match outcome {
            ImportOutcome::Created(p) => p,
            other => panic!("expected Created, got {other:?}"),
        };
        // Fresh id minted.
        assert!(project.id.starts_with("proj-"), "id was minted");
        // Made active.
        assert_eq!(store.active().unwrap().id, project.id);
        // Fields round-trip.
        assert_eq!(project.name, "Alpha");
        assert_eq!(project.repos, vec!["me/api".to_string()]);
        assert_eq!(project.ruleset.selections[0].rule_id, "R-1");
    }

    #[test]
    fn import_or_overwrite_returns_conflict_without_mutation() {
        let store = ProjectStore::new();
        // Seed a project named "Beta".
        let original = store.create("Beta", vec!["me/api".into()]).unwrap();
        // Attempt import with same name, overwrite=false.
        let outcome = store
            .import_or_overwrite(
                "Beta",
                ProjectImport {
                    repos: vec!["me/web".into()],
                    ruleset: ruleset_with("R-NEW"),
                    max_iterations: default_max_iterations(),
                    ..Default::default()
                },
                false,
            )
            .unwrap();
        assert!(
            matches!(outcome, ImportOutcome::Conflict),
            "expected Conflict"
        );
        // Store is unchanged — original project untouched.
        assert_eq!(store.list().len(), 1);
        let still_original = store.get(&original.id).unwrap();
        assert_eq!(still_original.repos, vec!["me/api".to_string()]);
        assert!(still_original.ruleset.selections.is_empty());
    }

    #[test]
    fn import_or_overwrite_overwrites_in_place_keeping_same_id() {
        let store = ProjectStore::new();
        let original = store.create("Gamma", vec!["me/old".into()]).unwrap();
        let original_id = original.id.clone();
        // Overwrite with new data.
        let outcome = store
            .import_or_overwrite(
                "Gamma",
                ProjectImport {
                    repos: vec!["me/new1".into(), "me/new2".into()],
                    ruleset: ruleset_with("R-REPLACED"),
                    onboarded: vec!["me/new1".into()],
                    max_iterations: default_max_iterations(),
                    ..Default::default()
                },
                true,
            )
            .unwrap();
        let overwritten = match outcome {
            ImportOutcome::Overwritten(p) => p,
            other => panic!("expected Overwritten, got {other:?}"),
        };
        // Same id preserved.
        assert_eq!(
            overwritten.id, original_id,
            "id must not change on overwrite"
        );
        // Repos/ruleset/onboarded replaced.
        assert_eq!(
            overwritten.repos,
            vec!["me/new1".to_string(), "me/new2".to_string()]
        );
        assert_eq!(overwritten.ruleset.selections[0].rule_id, "R-REPLACED");
        assert_eq!(overwritten.onboarded, vec!["me/new1".to_string()]);
        // Still active.
        assert_eq!(store.active().unwrap().id, original_id);
        // Store still has exactly one project.
        assert_eq!(store.list().len(), 1);
    }

    #[test]
    fn import_or_overwrite_onboarded_round_trips() {
        let store = ProjectStore::new();
        let onboarded = vec!["me/api".to_string(), "me/web".to_string()];
        let outcome = store
            .import_or_overwrite(
                "Delta",
                ProjectImport {
                    repos: vec!["me/api".into(), "me/web".into()],
                    onboarded: onboarded.clone(),
                    max_iterations: default_max_iterations(),
                    ..Default::default()
                },
                false,
            )
            .unwrap();
        let p = outcome.project().unwrap().clone();
        assert_eq!(
            p.onboarded, onboarded,
            "onboarded field round-trips through import"
        );
        // Verify it persists in the store too.
        assert_eq!(store.get(&p.id).unwrap().onboarded, onboarded);
    }

    // ── StepModels (per-project, per-step model config) ────────────────────────

    #[test]
    fn new_project_seeds_every_step_to_default_model() {
        // (a) A freshly created project carries DEFAULT_MODEL in every step slot.
        let store = ProjectStore::new();
        let p = store.create("SM", vec![]).unwrap();
        assert_eq!(p.step_models.audit, DEFAULT_MODEL);
        assert_eq!(p.step_models.calibration, DEFAULT_MODEL);
        assert_eq!(p.step_models.research_chat, DEFAULT_MODEL);
        assert_eq!(p.step_models.story_authoring, DEFAULT_MODEL);
        assert_eq!(p.step_models.decomposition, DEFAULT_MODEL);
        assert_eq!(p.step_models.escalation, DEFAULT_MODEL);
        assert_eq!(p.step_models.clarification, DEFAULT_MODEL);
        // model_for_step agrees with the field for every kind.
        for step in [
            StepKind::Audit,
            StepKind::Calibration,
            StepKind::ResearchChat,
            StepKind::StoryAuthoring,
            StepKind::Decomposition,
            StepKind::Escalation,
            StepKind::Clarification,
        ] {
            assert_eq!(
                p.model_for_step(step),
                DEFAULT_MODEL,
                "new project defaults step {step:?} to DEFAULT_MODEL"
            );
        }
    }

    #[test]
    fn set_step_model_is_per_project_isolated() {
        // (c) THE CRITICAL ISOLATION TEST: setting a step model on project A must NOT leak
        // to project B. Each project owns its own StepModels.
        let store = ProjectStore::new();
        let a = store.create("A", vec![]).unwrap();
        let b = store.create("B", vec![]).unwrap();

        // Set A's audit model to a non-default id.
        let updated_a = store
            .set_step_model(&a.id, StepKind::Audit, "claude-opus-4-8".to_string())
            .unwrap();
        assert_eq!(updated_a.model_for_step(StepKind::Audit), "claude-opus-4-8");

        // A's audit model changed...
        assert_eq!(
            store.get(&a.id).unwrap().model_for_step(StepKind::Audit),
            "claude-opus-4-8",
            "project A's audit model was set"
        );
        // ...but B's audit model is STILL the default — the change did not leak.
        assert_eq!(
            store.get(&b.id).unwrap().model_for_step(StepKind::Audit),
            DEFAULT_MODEL,
            "project B's audit model must be untouched by A's change"
        );
        // And A's OTHER steps are untouched too (patch semantics, one step per call).
        assert_eq!(
            store.get(&a.id).unwrap().model_for_step(StepKind::Calibration),
            DEFAULT_MODEL,
            "setting A's audit model must not touch A's calibration model"
        );
    }

    #[test]
    fn set_step_model_persists_to_disk_and_survives_reload() {
        // (d) PERSISTENCE: set a step model, then reload the store from disk — the value
        // survives the serde round-trip through the on-disk JSON file.
        let dir = std::env::temp_dir().join(format!(
            "camerata-stepmodels-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join("projects.json");

        let id = {
            let store = ProjectStore::load_or_new(path.clone());
            let p = store.create("Persisted", vec![]).unwrap();
            store
                .set_step_model(&p.id, StepKind::Escalation, "claude-haiku-4-5-20251001".to_string())
                .unwrap();
            p.id
        };

        // Fresh store from the SAME file: the change must have been written through.
        let reloaded = ProjectStore::load_or_new(path.clone());
        let p = reloaded.get(&id).expect("project survived reload");
        assert_eq!(
            p.model_for_step(StepKind::Escalation),
            "claude-haiku-4-5-20251001",
            "the step model survived persistence + reload"
        );
        // Untouched steps reloaded at the default.
        assert_eq!(p.model_for_step(StepKind::Audit), DEFAULT_MODEL);

        // Cleanup (best-effort).
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn new_project_seeds_stall_thresholds_to_defaults() {
        let store = ProjectStore::new();
        let p = store.create("T", vec![]).unwrap();
        assert_eq!(p.stall_thresholds.watched_secs, 120);
        assert_eq!(p.stall_thresholds.routine_secs, 600);
    }

    #[test]
    fn stall_threshold_ms_returns_correct_value_by_kind() {
        let store = ProjectStore::new();
        let p = store.create("T2", vec![]).unwrap();
        assert_eq!(p.stall_threshold_ms(false), 120_000);
        assert_eq!(p.stall_threshold_ms(true), 600_000);
    }

    #[test]
    fn set_stall_thresholds_is_per_project_isolated() {
        let store = ProjectStore::new();
        let a = store.create("A", vec![]).unwrap();
        let b = store.create("B", vec![]).unwrap();
        store.set_stall_thresholds(&a.id, StallThresholds { watched_secs: 300, routine_secs: 1800 }).unwrap();
        let updated_a = store.get(&a.id).unwrap();
        let unchanged_b = store.get(&b.id).unwrap();
        assert_eq!(updated_a.stall_thresholds.watched_secs, 300);
        assert_eq!(unchanged_b.stall_thresholds.watched_secs, 120, "B must be untouched");
    }
}
