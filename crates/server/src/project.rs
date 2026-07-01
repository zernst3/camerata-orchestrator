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

/// The project's model efficiency profile. Controls which models are assigned to each
/// entry point when "Apply profile" is invoked. `Balanced` is the serde default so
/// new and legacy projects both get sensible paid-subscription tiering out of the box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ModelProfile {
    /// Opus orchestrates; free OpenRouter models fill balanced/fast/steps; L3 free.
    /// Graceful fallback: if no free tool-use models exist in the registry, uses Balanced paid values.
    MaxEfficiency,
    /// Subscription-leaning: Opus/Sonnet/Haiku throughout; no free models; L3 off.
    #[default]
    Balanced,
    /// Opus/Sonnet throughout; L3 on with Sonnet; no free models.
    MaxQuality,
    /// No-op: user owns every entry; profile apply is a no-op.
    Custom,
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
    /// Legacy single-domain field (`*` = all). Superseded by `repos` below; kept for back-compat
    /// with projects saved before per-repo scoping and as the fallback in [`Self::applies_to_repo`].
    #[serde(default)]
    pub domain: String,
    /// The repos this custom rule applies to (the multiselect). Empty = all repos (the `*` case).
    #[serde(default)]
    pub repos: Vec<String>,
}

impl CustomRule {
    /// Whether this custom rule applies to `repo`. Prefers the explicit `repos` list; when it is
    /// empty, falls back to the legacy `domain` (`*`/empty = all, else an exact repo match).
    pub fn applies_to_repo(&self, repo: &str) -> bool {
        if !self.repos.is_empty() {
            return self.repos.iter().any(|r| r == repo);
        }
        let d = self.domain.trim();
        d.is_empty() || d == "*" || d == repo
    }
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

/// One work-item TYPE in a project's hierarchy schema (e.g. `"Epic"`, `"Story"`, or a custom
/// `"Spike"`). GitHub has no native "Epic"; a type is just a freetext name that the GitHub adapter
/// maps to a `type:<name>` label. See `docs/plans/2026-06-30_epic-design-page.md` §3.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkType {
    /// The type name (e.g. `"Feature"`). Freetext; custom types use any non-empty string.
    pub name: String,
    /// Whether this came from the shipped default palette (vs. a user-added custom type).
    #[serde(default)]
    pub builtin: bool,
    /// Whether a design may be ROOTED at this type (be the top node of a design tree).
    #[serde(default)]
    pub is_design_root: bool,
}

/// One allowed parent→child nesting in a project's hierarchy schema: `child` may nest under
/// `parent`. The full set forms a DAG — a parent may allow several child types, and a child type
/// may be allowed under several parents.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TypeRelation {
    /// The parent type name.
    pub parent: String,
    /// The child type name allowed under `parent`.
    pub child: String,
}

/// A project's WORK HIERARCHY SCHEMA: the work-item types this project uses and the allowed
/// parent→child nesting rules between them. Saved on the [`Project`] and portable (travels with
/// export). It is effectively Camerata's own, per-project, relationship-aware alternative to GitHub
/// Issue Types (per-project not org-locked, freetext + custom types, and it encodes hierarchy
/// RELATIONS that Issue Types do not). See `docs/plans/2026-06-30_epic-design-page.md` §3.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct HierarchySchema {
    /// The work-item types available in this project (built-in palette + custom).
    #[serde(default)]
    pub types: Vec<WorkType>,
    /// The allowed parent→child nestings (a DAG over `types`).
    #[serde(default)]
    pub relations: Vec<TypeRelation>,
}

/// The seeded default hierarchy schema: the common Scrum/ADO ladder as a starting point the
/// architect can edit via the drag-and-drop builder. `Initiative` and `Epic` are design roots.
pub fn default_hierarchy_schema() -> HierarchySchema {
    fn ty(name: &str, is_design_root: bool) -> WorkType {
        WorkType { name: name.to_string(), builtin: true, is_design_root }
    }
    fn rel(parent: &str, child: &str) -> TypeRelation {
        TypeRelation { parent: parent.to_string(), child: child.to_string() }
    }
    HierarchySchema {
        types: vec![
            ty("Initiative", true),
            ty("Epic", true),
            ty("Feature", false),
            ty("Story", false),
            ty("Defect", false),
            ty("Task", false),
            ty("Bug", false),
        ],
        relations: vec![
            rel("Initiative", "Epic"),
            rel("Epic", "Feature"),
            rel("Feature", "Story"),
            rel("Feature", "Defect"),
            rel("Story", "Task"),
            rel("Story", "Bug"),
        ],
    }
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
    /// The model efficiency profile for this project. When a profile other than `Custom`
    /// is applied, it cascades concrete model assignments to the tier-map, step-models,
    /// and L3 config. Serde default = `Balanced`.
    #[serde(default)]
    pub model_profile: ModelProfile,
    /// Whether the Designer (vision/multimodal) band is enabled for this project.
    ///
    /// When `false` (the default), the orchestrator ignores `tier_map.vision` even if it
    /// is populated — the toggle gates *availability*, not *configuration*. Design-flavored
    /// stories are still built; the orchestrator delegates that work to the logic tiers.
    ///
    /// When `true`, vision-capable stages are available and the orchestrator may invoke the
    /// Designer band for stories that require multimodal reasoning (e.g. UI mockup work).
    ///
    /// Serde default fills `false` for projects persisted before this field existed — no
    /// migration required.
    #[serde(default)]
    pub vision_enabled: bool,
    /// A free-text PRODUCT BRIEF: what this product is, who it's for, the quality bar, the
    /// non-negotiables. The SOFT context (distinct from the rules) that lets an agent make a
    /// judgment call the per-story spec didn't anticipate. Woven into agent grounding under a
    /// `## Product context` heading. Travels with the project export. Serde default = empty for
    /// projects persisted before this field existed (no migration).
    #[serde(default)]
    pub product_brief: String,
    /// How a good engineer works on THIS project: the agent OPERATING PRINCIPLES (conduct, not the
    /// artifact). Seeded with [`default_operating_principles`]; the architect can disable a default
    /// or add custom ones. Woven into the agent's role/system context under `## How to work here`.
    /// Travels with the project export. Serde default seeds the defaults for projects persisted
    /// before this field existed (so everyone gets them until they customize); an explicitly-saved
    /// empty list stays empty.
    #[serde(default = "default_operating_principles")]
    pub operating_principles: Vec<OperatingPrinciple>,
    /// PROJECT MEMORY (#112, Layer 3): the accumulating, human-curated learnings (decisions,
    /// patterns, gotchas, constraints). Agents propose entries at run end; the architect curates;
    /// `Approved` entries (capped) feed grounding under `## What we have learned`. Travels with the
    /// project export. Serde default = empty for projects persisted before this field existed.
    #[serde(default)]
    pub memory: Vec<MemoryEntry>,
    /// The project's WORK HIERARCHY SCHEMA (the design-page work-type graph): the work-item types
    /// this project uses and the allowed parent→child nesting rules. Saved project-level and
    /// portable (travels with the project export). Serde default seeds [`default_hierarchy_schema`]
    /// (the common Scrum ladder) for projects persisted before this field existed — no migration
    /// required; an explicitly-saved empty schema stays empty. See
    /// `docs/plans/2026-06-30_epic-design-page.md`.
    #[serde(default = "default_hierarchy_schema")]
    pub hierarchy_schema: HierarchySchema,
}

/// One agent operating principle: a single imperative line the governed agent is held to (about
/// HOW it works, e.g. "report failures honestly"), with a stable id (for the shipped defaults) and
/// an `enabled` toggle so the architect can switch a default off without deleting it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OperatingPrinciple {
    /// Stable id (kebab-case for the shipped defaults; a custom one may use any non-empty string).
    pub id: String,
    /// The imperative the agent sees, e.g. "Prefer explicit, robust code over terse cleverness."
    pub text: String,
    /// Whether this principle is active. Absent → `true` (back-compat for any partial blob).
    #[serde(default = "default_true_principle")]
    pub enabled: bool,
}

fn default_true_principle() -> bool {
    true
}

/// What a [`MemoryEntry`] records — the accumulating PROJECT MEMORY (#112, Layer 3): the durable
/// learnings that carry across runs so agent N+1 doesn't rediscover what agent N learned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    /// A decision that was made (and should hold).
    #[default]
    Decision,
    /// A pattern established in this codebase.
    Pattern,
    /// A gotcha / sharp edge learned the hard way.
    Gotcha,
    /// A constraint to respect.
    Constraint,
}

/// The curation state of a [`MemoryEntry`]. Agents PROPOSE; the human curates. Only `Approved`
/// entries reach agent grounding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MemoryStatus {
    /// Proposed by an agent, awaiting human review (NOT yet grounded).
    #[default]
    Proposed,
    /// Approved by the human — durable, and woven into agent grounding.
    Approved,
    /// Retired: kept for the record but no longer grounded.
    Archived,
}

/// One PROJECT MEMORY entry (#112, Layer 3): a single curated learning. Agents propose them at run
/// end; the architect approves/edits/archives. Approved entries (capped) feed agent grounding under
/// `## What we have learned`. Travels with the project export, so the curated memory is transferable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryEntry {
    /// Stable id (`mem-N`).
    pub id: String,
    /// What kind of learning this is.
    #[serde(default)]
    pub kind: MemoryKind,
    /// The learning itself, one fact.
    pub text: String,
    /// Who proposed it: `"human"` or `"agent:<story-or-run>"`.
    #[serde(default)]
    pub source: String,
    /// Curation state. Absent → `Proposed`.
    #[serde(default)]
    pub status: MemoryStatus,
    /// RFC3339 creation timestamp.
    #[serde(default)]
    pub created: String,
}

/// The shipped DEFAULT operating principles — distilled from the standards this project's architect
/// and Claude have converged on. New projects get these; existing projects inherit them via the
/// serde default until they customize. All enabled by default; each can be toggled off in settings.
pub fn default_operating_principles() -> Vec<OperatingPrinciple> {
    let p = |id: &str, text: &str| OperatingPrinciple {
        id: id.to_string(),
        text: text.to_string(),
        enabled: true,
    };
    vec![
        p(
            "explicit-over-clever",
            "Prefer explicit, robust, readable code over terse cleverness; the cost of verbosity is \
             paid by AI, the benefit of context is paid back at debug time.",
        ),
        p(
            "confirm-irreversible",
            "Confirm or escalate before hard-to-reverse or structural changes; do not auto-apply \
             them.",
        ),
        p(
            "report-honestly",
            "Report outcomes faithfully. If tests fail, say so with the output. Never fake a \
             resolution or paper over a failure.",
        ),
        p(
            "match-surrounding-style",
            "Write code that reads like the code around it: match its naming, comment density, and \
             idioms.",
        ),
        p(
            "escalate-when-blocked",
            "Stop and escalate on a genuine blocking decision or a rule that calls for it. Do not \
             guess past it; do not escalate to dodge a judgment you can make.",
        ),
        p(
            "test-what-you-change",
            "Add tests for new behavior; keep existing tests passing; never weaken a test to make \
             it go green.",
        ),
        p(
            "performant-by-default",
            "Reach for the performant pattern by default: index the FK + WHERE columns, avoid N+1, \
             parallelize independent async.",
        ),
        p(
            "minimal-blast-radius",
            "Make the minimal correct change. Do not touch unrelated files or expand scope beyond \
             the story.",
        ),
    ]
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

    /// The next free `mem-N` id for a new project-memory entry (max existing suffix + 1).
    pub fn next_memory_id(&self) -> String {
        let max = self
            .memory
            .iter()
            .filter_map(|m| m.id.strip_prefix("mem-"))
            .filter_map(|n| n.parse::<usize>().ok())
            .max()
            .unwrap_or(0);
        format!("mem-{}", max + 1)
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
        self.mark_profile_custom();
    }

    /// Mark the project's model profile as `Custom`. Any single-entry model override
    /// (a tier band, a helper-step model, or the L3 model) means the active set no
    /// longer matches a named preset, so it is recorded as Custom. Only
    /// `apply_model_profile` re-establishes a non-Custom profile (it writes the
    /// cascade fields directly, bypassing these setters).
    pub(crate) fn mark_profile_custom(&mut self) {
        self.model_profile = ModelProfile::Custom;
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
            self.tier_map.balanced_primary()
        }
    }

    /// Replace the L3 review configuration for this project in place.
    pub fn set_l3_review(&mut self, config: L3ReviewConfig) {
        self.l3_review = config;
        self.mark_profile_custom();
    }
}

/// The full set of TRANSFERABLE project CONFIG fields that travel through an export →
/// import round-trip (issue #111). Per the config-vs-data ADR
/// (`2026-06-21_project_config_vs_data_separation`), every field on `Project` is config
/// and must round-trip; only DATA (UoW, settings/workspace_root/repo_paths, drafts) — which
/// lives in separate stores, not on `Project` — stays local.
///
/// `name`/`overwrite` are handled separately by [`ProjectStore::import_or_overwrite`]; this
/// struct carries the remaining transferable fields so they can be applied uniformly in BOTH
/// the overwrite-in-place and new-project branches.
#[derive(Debug, Clone, Default)]
pub struct ProjectImport {
    /// The repos in scope (`owner/repo`).
    pub repos: Vec<String>,
    /// The project's ruleset (the source of truth, including custom rules).
    pub ruleset: ProjectRuleset,
    /// Repos already onboarded in the source project.
    pub onboarded: Vec<String>,
    /// Loop-guard iteration cap.
    pub max_iterations: usize,
    /// Per-capability-band model mapping (ORCH-MODEL-TIERING-1).
    pub tier_map: TierMap,
    /// VCS-action gate configuration.
    pub process_rule_config: ProcessRuleConfig,
    /// Per-step model configuration for non-fleet AI steps.
    pub step_models: StepModels,
    /// Stall-detection thresholds.
    pub stall_thresholds: StallThresholds,
    /// Layer-3 agentic code-review gate config.
    pub l3_review: L3ReviewConfig,
    /// The model efficiency profile.
    pub model_profile: ModelProfile,
    /// Whether the Designer (vision/multimodal) band is enabled.
    pub vision_enabled: bool,
    /// The free-text product brief (soft context).
    pub product_brief: String,
    /// The agent operating principles (conduct).
    pub operating_principles: Vec<OperatingPrinciple>,
    /// The curated project memory (Layer 3).
    pub memory: Vec<MemoryEntry>,
    /// The work hierarchy schema (design-page work-type graph). Constructed in code (from the
    /// import request), so no serde attribute; `HierarchySchema: Default` covers `..Default::default()`.
    pub hierarchy_schema: HierarchySchema,
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
                existing.hierarchy_schema = import.hierarchy_schema;
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
                    hierarchy_schema: import.hierarchy_schema,
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
            model_profile: ModelProfile::default(),
            vision_enabled: false,
            product_brief: String::new(),
            operating_principles: Vec::new(),
            memory: Vec::new(),
            hierarchy_schema: crate::project::HierarchySchema::default(),
            ruleset: ProjectRuleset {
                selections: vec![sel("OLD-1")],
                cross_repo: vec![],
                process: vec![],
                custom: vec![CustomRule {
                    name: "house-style".into(),
                    body: "Prefer X.".into(),
                    domain: "*".into(),
                    repos: Vec::new(),
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
            repos: Vec::new(),
        }
    }

    fn proj_on_balanced() -> Project {
        Project {
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
            model_profile: ModelProfile::Balanced,
            vision_enabled: false,
            product_brief: String::new(),
            operating_principles: Vec::new(),
            memory: Vec::new(),
            hierarchy_schema: crate::project::HierarchySchema::default(),
            ruleset: ProjectRuleset {
                selections: vec![],
                cross_repo: vec![],
                process: vec![],
                custom: vec![],
            },
        }
    }

    #[test]
    fn manual_step_model_override_flips_profile_to_custom() {
        let mut p = proj_on_balanced();
        assert_eq!(p.model_profile, ModelProfile::Balanced);
        p.set_model_for_step(StepKind::Audit, "some-other-model".into());
        assert_eq!(
            p.model_profile,
            ModelProfile::Custom,
            "a per-step override must deviate the project off its preset"
        );
    }

    #[test]
    fn manual_l3_override_flips_profile_to_custom() {
        let mut p = proj_on_balanced();
        p.set_l3_review(L3ReviewConfig { enabled: true, model: "pinned-l3".into() });
        assert_eq!(p.model_profile, ModelProfile::Custom);
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
            model_profile: ModelProfile::default(),
            vision_enabled: false,
            product_brief: String::new(),
            operating_principles: Vec::new(),
            memory: Vec::new(),
            hierarchy_schema: crate::project::HierarchySchema::default(),
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
            model_profile: ModelProfile::default(),
            vision_enabled: false,
            product_brief: String::new(),
            operating_principles: Vec::new(),
            memory: Vec::new(),
            hierarchy_schema: crate::project::HierarchySchema::default(),
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
            model_profile: ModelProfile::default(),
            vision_enabled: false,
            product_brief: String::new(),
            operating_principles: Vec::new(),
            memory: Vec::new(),
            hierarchy_schema: crate::project::HierarchySchema::default(),
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
            model_profile: ModelProfile::default(),
            vision_enabled: false,
            product_brief: String::new(),
            operating_principles: Vec::new(),
            memory: Vec::new(),
            hierarchy_schema: crate::project::HierarchySchema::default(),
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
            model_profile: ModelProfile::default(),
            vision_enabled: false,
            product_brief: String::new(),
            operating_principles: Vec::new(),
            memory: Vec::new(),
            hierarchy_schema: crate::project::HierarchySchema::default(),
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
