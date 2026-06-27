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

use camerata_checks::vcs_action::ProcessRuleConfig;

use crate::llm::DEFAULT_MODEL;
use crate::model_tier::TierMap;

/// The NON-FLEET AI steps whose model is configured per-project on [`StepModels`].
///
/// Each variant maps 1:1 to a field on [`StepModels`] (see [`Project::model_for_step`])
/// and to exactly one (or one family of) `LlmRequest` call site(s). These are the steps
/// the governed development FLEET does NOT own — the fleet's per-stage models come from
/// the project's [`TierMap`] (`ORCH-MODEL-TIERING-1`), a separate axis. `StepKind` covers
/// everything else: the brownfield audit, the calibration pass, the research chat, story
/// authoring, decomposition, escalation translation, and clarification authoring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepKind {
    /// The brownfield AI audit pass (the LLM scan of architectural / structured / prose
    /// rules, plus the deep-lens tier). UI-picked: an explicit request model overrides.
    Audit,
    /// The calibration pass (severity recalibration + confidence tagging) that runs after
    /// the audit. UI-picked: an explicit request model overrides.
    Calibration,
    /// The research / side-by-side chat completion. UI-picked: an explicit request model
    /// overrides.
    ResearchChat,
    /// Story authoring (the draft-UoW clarification chat → story redraft).
    StoryAuthoring,
    /// AI decomposition of a parent story into grounded child stories.
    Decomposition,
    /// Escalation answer translation (restating a human decision as a resume directive).
    Escalation,
    /// AI-suggested clarifying questions for a story.
    Clarification,
}

/// serde default for each [`StepModels`] field — the shipped [`DEFAULT_MODEL`]. Used so a
/// project JSON written before a given step field existed deserializes to the default
/// rather than failing (mirrors [`default_max_iterations`]).
pub fn default_model() -> String {
    DEFAULT_MODEL.to_string()
}

pub fn default_watched_secs() -> u64 {
    std::env::var("CAMERATA_RUN_STALL_THRESHOLD_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(120)
}

pub fn default_routine_secs() -> u64 {
    std::env::var("CAMERATA_RUN_STALL_THRESHOLD_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(|s| s.max(120) * 5)
        .unwrap_or(600)
}

/// Per-project Layer-3 (agentic code-review) configuration (R7).
///
/// Default: off, using the project's `balanced` tier model when enabled.
/// Serialises with `serde(default)` so projects written before this field existed
/// deserialise to the disabled default without migration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct L3ReviewConfig {
    /// Whether L3 is enabled for this project.
    #[serde(default)]
    pub enabled: bool,
    /// The model id that runs the L3 reviewer. Empty = use the project's `balanced` tier model.
    #[serde(default)]
    pub model: String,
}

impl Default for L3ReviewConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            model: String::new(),
        }
    }
}

/// Per-project stall detection thresholds, split by context (watched = interactive,
/// routine = autonomous/walk-away). Mirrors the `StepModels` pattern.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StallThresholds {
    #[serde(default = "default_watched_secs")]
    pub watched_secs: u64,
    #[serde(default = "default_routine_secs")]
    pub routine_secs: u64,
}

impl Default for StallThresholds {
    fn default() -> Self {
        Self {
            watched_secs: default_watched_secs(),
            routine_secs: default_routine_secs(),
        }
    }
}

/// Per-project, per-step model configuration for every NON-FLEET AI step.
///
/// One model-id slot per [`StepKind`]. This mirrors [`TierMap`] exactly: `serde(default)`
/// on every field (legacy-JSON back-compat), a [`Default`] impl seeding every slot with
/// [`DEFAULT_MODEL`], and per-project storage on [`Project`] mutated only through
/// [`ProjectStore::set_step_model`]. Once a project exists, its step model is the SOLE
/// source for that step — there is no runtime env/const fallback (the only remaining
/// `DEFAULT_MODEL` floor is the project-less edge, e.g. the smoke-test chat with no active
/// project). UI-picked steps (audit / calibration / research_chat) still let an explicit
/// request model override this default; fallback steps read it directly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StepModels {
    #[serde(default = "default_model")]
    pub audit: String,
    #[serde(default = "default_model")]
    pub calibration: String,
    #[serde(default = "default_model")]
    pub research_chat: String,
    #[serde(default = "default_model")]
    pub story_authoring: String,
    #[serde(default = "default_model")]
    pub decomposition: String,
    #[serde(default = "default_model")]
    pub escalation: String,
    #[serde(default = "default_model")]
    pub clarification: String,
}

impl Default for StepModels {
    fn default() -> Self {
        Self {
            audit: DEFAULT_MODEL.to_string(),
            calibration: DEFAULT_MODEL.to_string(),
            research_chat: DEFAULT_MODEL.to_string(),
            story_authoring: DEFAULT_MODEL.to_string(),
            decomposition: DEFAULT_MODEL.to_string(),
            escalation: DEFAULT_MODEL.to_string(),
            clarification: DEFAULT_MODEL.to_string(),
        }
    }
}

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
    /// Max developer→checker bounce-and-revise iterations a stage may take before the
    /// fleet stops the loop and raises the outstanding violations for human review
    /// (the loop guard, #29). Defaults to `1` (a dirty stage bounces exactly once),
    /// which keeps the historical behaviour. Adjustable from the Development Surface.
    #[serde(default = "default_max_iterations")]
    pub max_iterations: usize,
    /// Per-capability-band model mapping for the governed fleet (ORCH-MODEL-TIERING-1).
    ///
    /// Maps each [`CapabilityBand`] (`fast` / `balanced` / `strongest`) to a concrete
    /// model id. The fleet's task classifier maps each [`PlanTask`] to a band, and
    /// this map resolves the final model id per stage.
    ///
    /// Serde default fills in [`TierMap::default()`] for projects persisted before this
    /// field existed, so no migration is required. The default Claude tier map is:
    /// `fast=claude-haiku-4-5-20251001`, `balanced=claude-sonnet-4-6`,
    /// `strongest=claude-opus-4-8`.
    #[serde(default)]
    pub tier_map: TierMap,
    /// VCS-action gate configuration: per-rule enabled flags and tunables for the
    /// process rules that govern commit messages, PR titles, and branch names.
    ///
    /// Serde default fills in [`ProcessRuleConfig::default()`] for projects persisted
    /// before this field existed (back-compat, no migration required). The defaults
    /// match the previous hardcoded behaviour (conventional-commit + commit-doc
    /// enabled; ADO link + branch-naming opt-in).
    ///
    /// See `camerata_checks::vcs_action::{ProcessRuleConfig, build_rules}`.
    #[serde(default)]
    pub process_rule_config: ProcessRuleConfig,
    /// Per-step model configuration for every NON-FLEET AI step (audit, calibration,
    /// research chat, story authoring, decomposition, escalation, clarification).
    ///
    /// Serde default fills in [`StepModels::default()`] (every slot = [`DEFAULT_MODEL`])
    /// for projects persisted before this field existed — no migration required, and each
    /// individual field also `serde(default)`s so a partial blob still loads. Once a
    /// project exists this is the SOLE source for a step's model (no env/const fallback);
    /// the only remaining floor is the project-less edge. Mutated only via
    /// [`ProjectStore::set_step_model`].
    #[serde(default)]
    pub step_models: StepModels,
    /// Per-project stall detection thresholds split by run context (watched = interactive,
    /// routine = autonomous/walk-away). Defaults to 120s watched / 600s routine.
    #[serde(default)]
    pub stall_thresholds: StallThresholds,
    /// Layer-3 agentic code-review gate (R7). Opt-in per project. When off, the human
    /// is the reviewer. Serde default fills in [`L3ReviewConfig::default()`] (disabled)
    /// for projects persisted before this field existed — no migration required.
    #[serde(default)]
    pub l3_review: L3ReviewConfig,
}

/// The shipped default for [`Project::max_iterations`]: one bounce-and-revise pass,
/// matching the original single-bounce behaviour. Also the value `serde` fills in for
/// projects persisted before this field existed.
pub fn default_max_iterations() -> usize {
    1
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

    /// Set the loop-guard ceiling (#29), clamped to at least `1` — a project can
    /// never disable the bounce, only cap how many revise passes a stage may take.
    pub fn set_max_iterations(&mut self, n: usize) {
        self.max_iterations = n.max(1);
    }

    /// Replace the VCS-gate process-rule configuration for this project.
    ///
    /// The new config takes effect on the next VCS action performed under this
    /// project (i.e., the live [`ProcessRule`] set is rebuilt via
    /// `camerata_checks::vcs_action::build_rules` on each gate call).
    pub fn set_process_rule_config(&mut self, config: ProcessRuleConfig) {
        self.process_rule_config = config;
    }

    /// The configured model id for a given NON-FLEET AI [`StepKind`] on THIS project.
    ///
    /// This is the project's per-step source of truth: once a project exists there is no
    /// env/const fallback for these steps — the value here is authoritative (it is seeded
    /// to [`DEFAULT_MODEL`] at creation and changed only via
    /// [`ProjectStore::set_step_model`]). UI-picked steps still let an explicit request
    /// model override this default at the call site; fallback steps use it directly.
    pub fn model_for_step(&self, step: StepKind) -> &str {
        match step {
            StepKind::Audit => &self.step_models.audit,
            StepKind::Calibration => &self.step_models.calibration,
            StepKind::ResearchChat => &self.step_models.research_chat,
            StepKind::StoryAuthoring => &self.step_models.story_authoring,
            StepKind::Decomposition => &self.step_models.decomposition,
            StepKind::Escalation => &self.step_models.escalation,
            StepKind::Clarification => &self.step_models.clarification,
        }
    }

    /// Set the model id for a single [`StepKind`] on this project IN PLACE, leaving every
    /// other step untouched. The per-project mutation primitive behind
    /// [`ProjectStore::set_step_model`]; isolation (a change to one project never leaks to
    /// another) is guaranteed because this only ever borrows `&mut self`.
    pub fn set_model_for_step(&mut self, step: StepKind, model: String) {
        match step {
            StepKind::Audit => self.step_models.audit = model,
            StepKind::Calibration => self.step_models.calibration = model,
            StepKind::ResearchChat => self.step_models.research_chat = model,
            StepKind::StoryAuthoring => self.step_models.story_authoring = model,
            StepKind::Decomposition => self.step_models.decomposition = model,
            StepKind::Escalation => self.step_models.escalation = model,
            StepKind::Clarification => self.step_models.clarification = model,
        }
    }

    /// Explicitly remove a custom rule by name (the ONLY way a custom rule leaves
    /// the project). Returns true if one was removed.
    pub fn remove_custom(&mut self, name: &str) -> bool {
        let before = self.ruleset.custom.len();
        self.ruleset.custom.retain(|c| c.name != name);
        self.ruleset.custom.len() != before
    }

    /// Return the stall threshold in milliseconds for this project, keyed by whether
    /// the run is autonomous (walk-away/routine) or watched (interactive).
    pub fn stall_threshold_ms(&self, autonomous: bool) -> u128 {
        let secs = if autonomous {
            self.stall_thresholds.routine_secs
        } else {
            self.stall_thresholds.watched_secs
        };
        secs as u128 * 1_000
    }

    /// Replace this project's stall thresholds in place.
    pub fn set_stall_thresholds(&mut self, thresholds: StallThresholds) {
        self.stall_thresholds = thresholds;
    }

    /// The effective model id for the L3 reviewer on this project.
    ///
    /// Returns `self.l3_review.model` when it is non-empty; falls back to
    /// `self.tier_map.balanced` so a project that opts in but doesn't pin a specific
    /// model uses its configured balanced tier.
    pub fn l3_model(&self) -> &str {
        if !self.l3_review.model.trim().is_empty() {
            &self.l3_review.model
        } else {
            &self.tier_map.balanced
        }
    }

    /// Replace the L3 review configuration for this project in place.
    pub fn set_l3_review(&mut self, config: L3ReviewConfig) {
        self.l3_review = config;
    }
}

/// Outcome of a [`ProjectStore::import_or_overwrite`] call.
#[derive(Debug)]
pub enum ImportOutcome {
    /// No project with the same name existed; a new one was created.
    Created(Project),
    /// A project with the same name existed and was overwritten in place (same id).
    Overwritten(Project),
    /// A project with the same name existed and `overwrite=false`; nothing was changed.
    Conflict,
}

impl ImportOutcome {
    /// Unwrap the project from `Created` or `Overwritten`; panics on `Conflict`.
    pub fn into_project(self) -> Project {
        match self {
            ImportOutcome::Created(p) | ImportOutcome::Overwritten(p) => p,
            ImportOutcome::Conflict => panic!("ImportOutcome::Conflict has no project"),
        }
    }

    /// Whether this outcome produced a project (i.e. not `Conflict`).
    pub fn project(&self) -> Option<&Project> {
        match self {
            ImportOutcome::Created(p) | ImportOutcome::Overwritten(p) => Some(p),
            ImportOutcome::Conflict => None,
        }
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
            self.import_or_overwrite(name, repos, ruleset, Vec::new(), false)?
                .into_project(),
        )
    }

    /// Import (or overwrite) a project from an exported JSON document.
    ///
    /// - No existing project with the same `name` → create a fresh id, make active.
    /// - Same `name` exists and `overwrite=false` → return `Conflict` (no mutation).
    /// - Same `name` exists and `overwrite=true` → replace repos/ruleset/onboarded
    ///   IN PLACE (same id), make active.
    pub fn import_or_overwrite(
        &self,
        name: &str,
        repos: Vec<String>,
        ruleset: ProjectRuleset,
        onboarded: Vec<String>,
        overwrite: bool,
    ) -> Option<ImportOutcome> {
        let outcome = {
            let mut s = self.inner.lock().ok()?;
            if let Some(existing) = s.projects.iter_mut().find(|p| p.name == name) {
                if !overwrite {
                    return Some(ImportOutcome::Conflict);
                }
                // Overwrite in place — keep the same id.
                existing.repos = repos;
                existing.ruleset = ruleset;
                existing.onboarded = onboarded;
                let updated = existing.clone();
                s.active = Some(updated.id.clone());
                ImportOutcome::Overwritten(updated)
            } else {
                s.counter += 1;
                let id = format!("proj-{}", s.counter);
                let project = Project {
                    id: id.clone(),
                    name: name.to_string(),
                    repos,
                    ruleset,
                    onboarded,
                    max_iterations: default_max_iterations(),
                    tier_map: TierMap::default(),
                    process_rule_config: ProcessRuleConfig::default(),
                    step_models: StepModels::default(),
                    stall_thresholds: StallThresholds::default(),
                    l3_review: L3ReviewConfig::default(),
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
            max_iterations: default_max_iterations(),
            tier_map: crate::model_tier::TierMap::default(),
            process_rule_config: ProcessRuleConfig::default(),
            step_models: StepModels::default(),
            stall_thresholds: StallThresholds::default(),
            l3_review: L3ReviewConfig::default(),
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
            max_iterations: default_max_iterations(),
            tier_map: crate::model_tier::TierMap::default(),
            process_rule_config: ProcessRuleConfig::default(),
            step_models: StepModels::default(),
            stall_thresholds: StallThresholds::default(),
            l3_review: L3ReviewConfig::default(),
            ruleset: ProjectRuleset {
                custom: vec![custom("a", "A1"), custom("b", "B1")],
                ..Default::default()
            },
        };
        // An import that only mentions "a" (edited) and a new "c" — "b" is untouched.
        p.merge_custom(&[custom("a", "A2"), custom("c", "C1")]);
        let by_name = |n: &str| p.ruleset.custom.iter().find(|c| c.name == n).cloned();
        assert_eq!(
            by_name("a").unwrap().body,
            "A2",
            "named custom rule was edited"
        );
        assert_eq!(
            by_name("b").unwrap().body,
            "B1",
            "untouched custom rule REMAINS"
        );
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
            max_iterations: default_max_iterations(),
            tier_map: crate::model_tier::TierMap::default(),
            process_rule_config: ProcessRuleConfig::default(),
            step_models: StepModels::default(),
            stall_thresholds: StallThresholds::default(),
            l3_review: L3ReviewConfig::default(),
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
    fn new_projects_default_to_one_iteration() {
        let store = ProjectStore::new();
        let p = store.create("A", vec![]).unwrap();
        assert_eq!(
            p.max_iterations, 1,
            "the shipped default is a single bounce"
        );
    }

    #[test]
    fn set_max_iterations_clamps_to_at_least_one() {
        let mut p = Project {
            id: "p".into(),
            name: "P".into(),
            repos: vec![],
            onboarded: vec![],
            max_iterations: default_max_iterations(),
            tier_map: crate::model_tier::TierMap::default(),
            process_rule_config: ProcessRuleConfig::default(),
            step_models: StepModels::default(),
            stall_thresholds: StallThresholds::default(),
            l3_review: L3ReviewConfig::default(),
            ruleset: ProjectRuleset::default(),
        };
        p.set_max_iterations(5);
        assert_eq!(p.max_iterations, 5);
        // 0 must not disable the bounce — clamped up to 1.
        p.set_max_iterations(0);
        assert_eq!(p.max_iterations, 1);
    }

    #[test]
    fn max_iterations_defaults_when_absent_from_persisted_json() {
        // A project JSON written before this field existed must deserialize with the
        // default (serde fills it in), not fail.
        let json = r#"{
            "id": "proj-1",
            "name": "Legacy",
            "repos": [],
            "ruleset": {},
            "onboarded": []
        }"#;
        let p: Project = serde_json::from_str(json).unwrap();
        assert_eq!(p.max_iterations, 1);
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
            max_iterations: default_max_iterations(),
            tier_map: crate::model_tier::TierMap::default(),
            process_rule_config: ProcessRuleConfig::default(),
            step_models: StepModels::default(),
            stall_thresholds: StallThresholds::default(),
            l3_review: L3ReviewConfig::default(),
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

    // ── import_or_overwrite ────────────────────────────────────────────────────

    fn ruleset_with(rule: &str) -> ProjectRuleset {
        ProjectRuleset {
            selections: vec![sel(rule)],
            ..Default::default()
        }
    }

    #[test]
    fn import_or_overwrite_creates_new_when_no_collision() {
        let store = ProjectStore::new();
        let outcome = store
            .import_or_overwrite(
                "Alpha",
                vec!["me/api".into()],
                ruleset_with("R-1"),
                vec![],
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
                vec!["me/web".into()],
                ruleset_with("R-NEW"),
                vec![],
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
                vec!["me/new1".into(), "me/new2".into()],
                ruleset_with("R-REPLACED"),
                vec!["me/new1".into()],
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
                vec!["me/api".into(), "me/web".into()],
                Default::default(),
                onboarded.clone(),
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
    fn step_models_default_when_absent_from_legacy_project_json() {
        // (b) A project JSON written before step_models existed must deserialize with the
        // default StepModels (serde fills it in), not fail. Mirrors the tier_map/max_iter tests.
        let json = r#"{
            "id": "proj-1",
            "name": "Legacy",
            "repos": [],
            "ruleset": {},
            "onboarded": []
        }"#;
        let p: Project = serde_json::from_str(json).unwrap();
        assert_eq!(
            p.step_models,
            StepModels::default(),
            "legacy project must deserialise with default step_models"
        );
        // And a PARTIAL step_models blob (only one field present) fills the rest from default —
        // the per-field #[serde(default)] is what guarantees this.
        let partial = r#"{
            "id": "proj-2",
            "name": "Partial",
            "repos": [],
            "ruleset": {},
            "onboarded": [],
            "step_models": { "audit": "claude-opus-4-8" }
        }"#;
        let p2: Project = serde_json::from_str(partial).unwrap();
        assert_eq!(p2.step_models.audit, "claude-opus-4-8");
        assert_eq!(
            p2.step_models.calibration, DEFAULT_MODEL,
            "absent step fields fall back to DEFAULT_MODEL even in a partial blob"
        );
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
    fn step_models_custom_values_survive_project_json_roundtrip() {
        let mut original = Project {
            id: "p".into(),
            name: "P".into(),
            repos: vec![],
            onboarded: vec![],
            max_iterations: default_max_iterations(),
            tier_map: crate::model_tier::TierMap::default(),
            process_rule_config: ProcessRuleConfig::default(),
            step_models: StepModels::default(),
            stall_thresholds: StallThresholds::default(),
            l3_review: L3ReviewConfig::default(),
            ruleset: ProjectRuleset::default(),
        };
        original.set_model_for_step(StepKind::Decomposition, "claude-opus-4-8".into());
        original.set_model_for_step(StepKind::ResearchChat, "claude-haiku-4-5-20251001".into());
        let json = serde_json::to_string(&original).unwrap();
        let back: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(back.step_models, original.step_models);
        assert_eq!(back.model_for_step(StepKind::Decomposition), "claude-opus-4-8");
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
        use crate::project::StallThresholds;
        store.set_stall_thresholds(&a.id, StallThresholds { watched_secs: 300, routine_secs: 1800 }).unwrap();
        let updated_a = store.get(&a.id).unwrap();
        let unchanged_b = store.get(&b.id).unwrap();
        assert_eq!(updated_a.stall_thresholds.watched_secs, 300);
        assert_eq!(unchanged_b.stall_thresholds.watched_secs, 120, "B must be untouched");
    }

    #[test]
    fn stall_thresholds_default_when_absent_from_legacy_json() {
        let json = r#"{"id":"p","name":"P","repos":[],"ruleset":{},"onboarded":[]}"#;
        let p: crate::project::Project = serde_json::from_str(json).unwrap();
        assert_eq!(p.stall_thresholds.watched_secs, 120);
        assert_eq!(p.stall_thresholds.routine_secs, 600);
    }
}
