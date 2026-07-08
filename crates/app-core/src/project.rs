//! The Project aggregate: the foundational data scope (ADR
//! project_container_and_rules_management). A project groups the repos in scope and
//! the full ruleset — per-repo base-rule selections, the cross-repo rules, the
//! process rules, and the architect's custom rules.
//!
//! This module owns the pure DOMAIN TYPES and the pure `&self`/`&mut self` transitions
//! (`RUST-HEADLESS-CORE-1` + `RUST-PURE-STATE-TRANSITIONS-1`). The STORE (`Arc<Mutex>` +
//! filesystem persistence) stays in the Axum adapter (`camerata-server`), which drives
//! these transitions to compute the next state and persists it.
//!
//! LLM seam (D2, const-relocation): `project` never CALLS the LLM — it only reads the
//! `DEFAULT_MODEL` string constant when seeding a project's per-step model slots. A trait
//! seam would be vacuous, so the honest minimal seam is to OWN the const here. The adapter's
//! `llm` module re-exports it (`pub use camerata_app_core::project::DEFAULT_MODEL`) so every
//! `crate::llm::DEFAULT_MODEL` call site keeps resolving.

use serde::{Deserialize, Serialize};

use camerata_checks::vcs_action::ProcessRuleConfig;
use camerata_fleet::tier::TierMap;

/// [`L3ReviewConfig`], [`StallThresholds`], and [`StepModels`] (plus the constants/serde-
/// default functions their `Default`/serde impls require: [`DEFAULT_MODEL`],
/// [`DEFAULT_ROUTINE_STALL_SECS`], [`default_model`], [`default_watched_secs`],
/// [`default_routine_secs`]) were relocated to `camerata_api_types::project` (Phase A of
/// the DTO extraction) — they are fully self-contained and carry no dependency on
/// `TierMap` / `ProcessRuleConfig`, unlike `Project` itself which stays here. Re-exported
/// so every existing `crate::project::X` / `camerata_app_core::project::X` call site
/// (including `llm`'s `pub use camerata_app_core::project::DEFAULT_MODEL`) keeps
/// resolving unchanged.
pub use camerata_api_types::project::{
    default_model, default_routine_secs, default_watched_secs, L3ReviewConfig, StallThresholds,
    StepModels, DEFAULT_MODEL, DEFAULT_ROUTINE_STALL_SECS,
};

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
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuleSelection {
    /// The rule id.
    pub rule_id: String,
    /// The chosen alternative option id, when the rule has alternatives.
    #[serde(default)]
    pub chosen_option: Option<String>,
    /// The repos this rule installs into (its placement).
    #[serde(default)]
    pub repos: Vec<String>,
    /// The corpus content-hash baselined for this selection — the fingerprint of the
    /// corpus rule as it stood when the architect last ACCEPTED it (via onboarding or
    /// the rule-drift "Update this rule" action). Drift detection compares this against
    /// the current corpus hash: when it is `Some(h)` and `h` differs from the current
    /// hash, the rule has drifted upstream. `None` means "never baselined" and is treated
    /// as in-sync (a freshly selected rule does not report drift until it is baselined).
    /// Serde default keeps projects persisted before this field existed loading cleanly.
    #[serde(default)]
    pub applied_hash: Option<String>,
    /// The resolved directive text as it stood when this selection was last baselined.
    /// Carried alongside `applied_hash` so the rule-drift notice can render a real
    /// before/after diff (applied vs. current corpus directive) rather than just a hash.
    /// `None` when never baselined. Serde default for back-compat.
    #[serde(default)]
    pub applied_directive: Option<String>,
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

impl HierarchySchema {
    /// Whether this schema is usable for design-mode child proposal / validation: it needs at
    /// least one type AND one parent→child relation. An empty schema (no types or no relations)
    /// makes the design-author prompt emit `ALLOWED_CHILD_TYPES: []` and makes materialize
    /// validation reject every child — a bug source, not a valid configuration.
    pub fn is_usable(&self) -> bool {
        !self.types.is_empty() && !self.relations.is_empty()
    }

    /// Resolve the EFFECTIVE schema: this one if it is usable, otherwise the seeded default
    /// ladder ([`default_hierarchy_schema`]). Centralizes the "empty schema → default ladder"
    /// fallback so the design-author handler, the materialize handler, and any hierarchy read
    /// can't drift. Does NOT mutate/persist; it only chooses which schema to use in-flight.
    pub fn resolve_effective(self) -> HierarchySchema {
        if self.is_usable() {
            self
        } else {
            default_hierarchy_schema()
        }
    }
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
    /// Maps each `CapabilityBand` (`fast` / `balanced` / `strongest`) to a concrete
    /// model id. The fleet's task classifier maps each `PlanTask` to a band, and
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
    /// `ProjectStore::set_step_model`.
    #[serde(default)]
    pub step_models: StepModels,
    /// Per-project stall detection thresholds split by run context (watched = interactive,
    /// routine = autonomous/walk-away). Defaults to 120s watched / 1800s routine (the generous
    /// autonomous auto-cancel default, LIFECYCLE-6).
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
    /// project (i.e., the live `ProcessRule` set is rebuilt via
    /// `camerata_checks::vcs_action::build_rules` on each gate call).
    pub fn set_process_rule_config(&mut self, config: ProcessRuleConfig) {
        self.process_rule_config = config;
    }

    /// The configured model id for a given NON-FLEET AI [`StepKind`] on THIS project.
    ///
    /// This is the project's per-step source of truth: once a project exists there is no
    /// env/const fallback for these steps — the value here is authoritative (it is seeded
    /// to [`DEFAULT_MODEL`] at creation and changed only via
    /// `ProjectStore::set_step_model`). UI-picked steps still let an explicit request
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
    /// `ProjectStore::set_step_model`; isolation (a change to one project never leaks to
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
    ///
    /// `pub` (was `pub(crate)` when this type lived in the server crate): the Axum
    /// adapter's `set_tier_map` handler edits the tier bands directly on a `&mut Project`
    /// and then calls this to flip the profile, so it must be reachable across the
    /// crate boundary (#117 backend headless-core split).
    pub fn mark_profile_custom(&mut self) {
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
/// `name`/`overwrite` are handled separately by `ProjectStore::import_or_overwrite`; this
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

/// Outcome of a `ProjectStore::import_or_overwrite` call.
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
            ..Default::default()
        }
    }

    #[test]
    fn resolve_effective_seeds_default_ladder_only_when_empty() {
        // The empty-schema fallback that fixes the design-canvas "planner proposes zero
        // children" bug: an empty (or partially-empty) schema resolves to the default ladder;
        // a usable custom schema is used verbatim.

        // Fully empty → default ladder (Epic → Feature exists).
        let resolved = HierarchySchema::default().resolve_effective();
        assert_eq!(resolved, default_hierarchy_schema());
        assert!(resolved
            .relations
            .iter()
            .any(|r| r.parent == "Epic" && r.child == "Feature"));

        // Types but no relations → still unusable → default ladder.
        let types_only = HierarchySchema {
            types: vec![WorkType {
                name: "Objective".into(),
                builtin: false,
                is_design_root: true,
            }],
            relations: vec![],
        };
        assert!(!types_only.is_usable());
        assert_eq!(types_only.resolve_effective(), default_hierarchy_schema());

        // A usable custom schema is returned unchanged (not clobbered).
        let custom = HierarchySchema {
            types: vec![WorkType {
                name: "Objective".into(),
                builtin: false,
                is_design_root: true,
            }],
            relations: vec![TypeRelation {
                parent: "Objective".into(),
                child: "KeyResult".into(),
            }],
        };
        assert!(custom.is_usable());
        assert_eq!(custom.clone().resolve_effective(), custom);
    }

    #[test]
    fn upsert_base_preserves_custom_rules() {
        let mut p = Project {
            id: "p1".into(),
            name: "Proj".into(),
            repos: vec!["me/api".into()],
            onboarded: vec![],
            max_iterations: default_max_iterations(),
            tier_map: TierMap::default(),
            process_rule_config: ProcessRuleConfig::default(),
            step_models: StepModels::default(),
            stall_thresholds: StallThresholds::default(),
            l3_review: L3ReviewConfig::default(),
            model_profile: ModelProfile::default(),
            vision_enabled: false,
            product_brief: String::new(),
            operating_principles: Vec::new(),
            memory: Vec::new(),
            hierarchy_schema: HierarchySchema::default(),
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
            tier_map: TierMap::default(),
            process_rule_config: ProcessRuleConfig::default(),
            step_models: StepModels::default(),
            stall_thresholds: StallThresholds::default(),
            l3_review: L3ReviewConfig::default(),
            model_profile: ModelProfile::Balanced,
            vision_enabled: false,
            product_brief: String::new(),
            operating_principles: Vec::new(),
            memory: Vec::new(),
            hierarchy_schema: HierarchySchema::default(),
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
            tier_map: TierMap::default(),
            process_rule_config: ProcessRuleConfig::default(),
            step_models: StepModels::default(),
            stall_thresholds: StallThresholds::default(),
            l3_review: L3ReviewConfig::default(),
            model_profile: ModelProfile::default(),
            vision_enabled: false,
            product_brief: String::new(),
            operating_principles: Vec::new(),
            memory: Vec::new(),
            hierarchy_schema: HierarchySchema::default(),
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
            tier_map: TierMap::default(),
            process_rule_config: ProcessRuleConfig::default(),
            step_models: StepModels::default(),
            stall_thresholds: StallThresholds::default(),
            l3_review: L3ReviewConfig::default(),
            model_profile: ModelProfile::default(),
            vision_enabled: false,
            product_brief: String::new(),
            operating_principles: Vec::new(),
            memory: Vec::new(),
            hierarchy_schema: HierarchySchema::default(),
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
    fn set_max_iterations_clamps_to_at_least_one() {
        let mut p = Project {
            id: "p".into(),
            name: "P".into(),
            repos: vec![],
            onboarded: vec![],
            max_iterations: default_max_iterations(),
            tier_map: TierMap::default(),
            process_rule_config: ProcessRuleConfig::default(),
            step_models: StepModels::default(),
            stall_thresholds: StallThresholds::default(),
            l3_review: L3ReviewConfig::default(),
            model_profile: ModelProfile::default(),
            vision_enabled: false,
            product_brief: String::new(),
            operating_principles: Vec::new(),
            memory: Vec::new(),
            hierarchy_schema: HierarchySchema::default(),
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
    fn export_import_round_trip() {
        let project = Project {
            id: "p".into(),
            name: "P".into(),
            repos: vec!["me/api".into()],
            onboarded: vec![],
            max_iterations: default_max_iterations(),
            tier_map: TierMap::default(),
            process_rule_config: ProcessRuleConfig::default(),
            step_models: StepModels::default(),
            stall_thresholds: StallThresholds::default(),
            l3_review: L3ReviewConfig::default(),
            model_profile: ModelProfile::default(),
            vision_enabled: false,
            product_brief: String::new(),
            operating_principles: Vec::new(),
            memory: Vec::new(),
            hierarchy_schema: HierarchySchema::default(),
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

    #[test]
    fn step_models_default_when_absent_from_legacy_project_json() {
        // A project JSON written before step_models existed must deserialize with the
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
    fn step_models_custom_values_survive_project_json_roundtrip() {
        let mut original = Project {
            id: "p".into(),
            name: "P".into(),
            repos: vec![],
            onboarded: vec![],
            max_iterations: default_max_iterations(),
            tier_map: TierMap::default(),
            process_rule_config: ProcessRuleConfig::default(),
            step_models: StepModels::default(),
            stall_thresholds: StallThresholds::default(),
            l3_review: L3ReviewConfig::default(),
            model_profile: ModelProfile::default(),
            vision_enabled: false,
            product_brief: String::new(),
            operating_principles: Vec::new(),
            memory: Vec::new(),
            hierarchy_schema: HierarchySchema::default(),
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
    fn stall_thresholds_default_when_absent_from_legacy_json() {
        // Guard against a stray env override from the test host: assert against the
        // functions that also produce the serde defaults, so this stays true whether or not
        // CAMERATA_RUN_STALL_THRESHOLD_SECS is set in the environment.
        let json = r#"{"id":"p","name":"P","repos":[],"ruleset":{},"onboarded":[]}"#;
        let p: Project = serde_json::from_str(json).unwrap();
        assert_eq!(p.stall_thresholds.watched_secs, default_watched_secs());
        assert_eq!(p.stall_thresholds.routine_secs, default_routine_secs());
    }

    /// LIFECYCLE-6: with no env override, the ROUTINE (autonomous) default is the generous
    /// 30-minute floor, so a walk-away run that auto-cancels on stall gets a long grace period.
    #[test]
    fn routine_stall_default_is_generous_when_env_absent() {
        if std::env::var("CAMERATA_RUN_STALL_THRESHOLD_SECS").is_ok() {
            // An override is set on this host; the env-scaled branch is exercised elsewhere.
            return;
        }
        assert_eq!(default_routine_secs(), DEFAULT_ROUTINE_STALL_SECS);
        assert_eq!(DEFAULT_ROUTINE_STALL_SECS, 1_800);
        // And an autonomous run reads the generous threshold in ms.
        let json = r#"{"id":"p","name":"P","repos":[],"ruleset":{},"onboarded":[]}"#;
        let p: Project = serde_json::from_str(json).unwrap();
        assert_eq!(
            p.stall_threshold_ms(true),
            u128::from(DEFAULT_ROUTINE_STALL_SECS) * 1_000
        );
    }

    // ── model_tier co-split: pure serde tests that operate on a Project literal ──

    #[test]
    fn tier_map_defaults_when_absent_from_legacy_project_json() {
        // A project JSON written before tier_map existed must deserialise correctly
        // with serde filling in the default TierMap. Mirrors the max_iterations test.
        let json = r#"{
            "id": "proj-1",
            "name": "Legacy",
            "repos": [],
            "ruleset": {},
            "onboarded": []
        }"#;
        let p: Project = serde_json::from_str(json).unwrap();
        assert_eq!(
            p.tier_map,
            TierMap::default(),
            "legacy project must deserialise with default tier_map"
        );
    }

    #[test]
    fn tier_map_custom_values_survive_project_json_roundtrip() {
        let original = Project {
            id: "p".into(),
            name: "P".into(),
            repos: vec![],
            onboarded: vec![],
            max_iterations: default_max_iterations(),
            tier_map: TierMap {
                fast: vec!["haiku-custom".into()],
                balanced: vec!["sonnet-custom".into()],
                strongest: "opus-custom".into(),
                vision: vec![],
            },
            process_rule_config: ProcessRuleConfig::default(),
            step_models: StepModels::default(),
            stall_thresholds: StallThresholds::default(),
            l3_review: L3ReviewConfig::default(),
            model_profile: ModelProfile::default(),
            vision_enabled: false,
            product_brief: String::new(),
            operating_principles: Vec::new(),
            memory: Vec::new(),
            hierarchy_schema: HierarchySchema::default(),
            ruleset: ProjectRuleset::default(),
        };
        let json = serde_json::to_string(&original).unwrap();
        let back: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(back.tier_map, original.tier_map);
    }
}
