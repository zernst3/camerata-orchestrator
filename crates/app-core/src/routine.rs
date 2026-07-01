//! Routine domain types and pure functions (framework-agnostic, RUST-HEADLESS-CORE-1).
//!
//! These are the serde-only data shapes and deterministic helper functions that describe
//! a routine: its status lifecycle, run history, templates, and creation / drafting requests.
//! They carry no I/O dependency (no axum, no disk, no LLM call) and are re-exported by the
//! adapter crate `camerata-server` via its trimmed `routine` module.
//!
//! The `RoutineStore` (Arc<Mutex> + fs persistence + run engine) stays in `camerata-server`.

use serde::{Deserialize, Serialize};

/// The default model used when a routine does not specify one. This mirrors the
/// `DEFAULT_MODEL` constant in `camerata-server::llm`, but is declared here independently
/// so this crate carries no dependency on the server adapter.
pub const DEFAULT_ROUTINE_MODEL: &str = "claude-sonnet-4-6";

/// The lifecycle status of a routine, persisted alongside it (issue #43).
///
/// This is the AUTONOMY-PLANE status the dashboard surfaces as a badge: a routine sits
/// `Idle` until the scheduler (or a manual run) drives it `Running`; a completed run lands
/// `Done`; a run the gate blocked lands `BlockedNeedsReview` (and raises an escalation a
/// human resolves); an errored run lands `Failed`. Resolving the escalation returns the
/// routine to `Idle` so the next slot can run it.
///
/// Distinct from [`RoutineRunSummary::outcome`] (which describes the gate VERDICTS of the
/// last run): a run can complete with denies recorded yet still be `BlockedNeedsReview`
/// because those denies need a human decision before it can proceed.
#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "snake_case")]
pub enum RoutineStatus {
    /// Provisioned and waiting for its next scheduled slot (or a manual run). The default
    /// for routines persisted before this field existed, so they rehydrate sensibly.
    #[default]
    Idle,
    /// A run is in flight right now.
    Running,
    /// The last run was blocked by the gate and an escalation is awaiting human review.
    BlockedNeedsReview,
    /// The last run completed without anything needing human review.
    Done,
    /// The last run errored out (not a gate denial — an actual failure to run).
    Failed,
}

impl RoutineStatus {
    /// A short, stable wire/label string (also what `serde` serializes via `snake_case`).
    pub fn as_str(&self) -> &'static str {
        match self {
            RoutineStatus::Idle => "idle",
            RoutineStatus::Running => "running",
            RoutineStatus::BlockedNeedsReview => "blocked_needs_review",
            RoutineStatus::Done => "done",
            RoutineStatus::Failed => "failed",
        }
    }
}

/// The outcome summary of a routine's last run: real counts from the gate script.
#[derive(Clone, Serialize, Deserialize)]
pub struct RoutineRunSummary {
    /// "passed" when the governed run completed (denies are the gate working, not
    /// failures).
    pub outcome: String,
    pub total_verdicts: usize,
    pub denies: usize,
    pub allows: usize,
    /// The rule ids the gate denied this run, so a blocked routine can say WHICH rules
    /// stopped it (not just a count) in its escalation. Empty when nothing was denied.
    #[serde(default)]
    pub denied_rules: Vec<String>,
}

/// What triggered a routine run, recorded in its run history.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunTrigger {
    /// Fired by the auto-fire scheduler on its schedule.
    Scheduled,
    /// Run on demand from the dashboard ("Run now").
    Manual,
}

/// One entry in a routine's bounded run history: when it ran, what triggered it, the gate
/// outcome, and the escalation it raised if it blocked.
#[derive(Clone, Serialize, Deserialize)]
pub struct RoutineRun {
    /// RFC3339 timestamp of the run.
    pub ts: String,
    pub trigger: RunTrigger,
    pub summary: RoutineRunSummary,
    /// The escalation raised by this run when the gate blocked it (so a history row can link to the
    /// review). `None` for a clean run.
    #[serde(default)]
    pub escalation_id: Option<String>,
}

/// How many runs of history each routine retains (FIFO; the oldest is dropped past this).
pub const ROUTINE_RUN_HISTORY_CAP: usize = 20;

/// A scheduled governed routine.
#[derive(Clone, Serialize, Deserialize)]
pub struct Routine {
    pub id: String,
    pub name: String,
    /// Human-readable schedule (e.g. "daily 04:00"). The scheduler that fires on it is
    /// the remaining wiring.
    pub schedule: String,
    /// The user's plain-language description of WHAT they want the routine to do.
    /// This is what the user writes; the AI authors the operational `prompt` from it
    /// (ADR routine_authoring_intent_not_prompt).
    pub intent: String,
    /// The OPERATIONAL prompt the agent actually runs — authored from `intent` by the
    /// lead-engineer AI (model tiering, directives, governance framing) and
    /// human-reviewed. Never the user's raw description verbatim.
    pub prompt: String,
    /// The permission / rule scope the routine runs under (shown so an unattended
    /// agent's governance is legible).
    pub scope: String,
    pub enabled: bool,
    pub last_run: Option<RoutineRunSummary>,
    /// Whether this routine is provisioned on THIS backend (registered with the
    /// scheduler). Locally-created routines are provisioned on creation; routines that
    /// arrive via a project import start UN-provisioned and need an explicit "Set up"
    /// before the scheduler will fire them — so a "Start" can't silently no-op because
    /// the routine doesn't actually exist on the importer's machine. Defaults `true` so
    /// routines persisted before this field gain it rehydrate as already provisioned.
    #[serde(default = "default_true")]
    pub provisioned: bool,
    /// RFC3339 timestamp of the last time the auto-fire scheduler ran this routine, so a
    /// "daily 09:00" routine fires once per slot rather than once per tick.
    #[serde(default)]
    pub last_fired: Option<String>,
    /// Optional owning project (`project.id`). `None` = a global routine that does not
    /// travel with any single project's export.
    #[serde(default)]
    pub project_id: Option<String>,
    /// The model this routine's agent runs on (an id from the `/api/models` catalog).
    /// Defaults to the server default so routines persisted before this field rehydrate
    /// with a sensible model.
    #[serde(default = "default_model")]
    pub model: String,
    /// The routine's lifecycle status (issue #43): the autonomy-plane state the dashboard
    /// badges. Defaults to `Idle` so routines persisted before this field rehydrate as
    /// ready-to-run rather than absent.
    #[serde(default)]
    pub status: RoutineStatus,
    /// Bounded run history (oldest first), capped at [`ROUTINE_RUN_HISTORY_CAP`]. Each run records
    /// its trigger, gate outcome, and any escalation it raised. Serde-default so routines persisted
    /// before this field rehydrate with an empty history.
    #[serde(default)]
    pub runs: Vec<RoutineRun>,
}

fn default_true() -> bool {
    true
}

/// Returns the default model string for a routine. Public so the adapter crate's
/// `RoutineStore` (which constructs routines) can call it directly.
pub fn default_model() -> String {
    DEFAULT_ROUTINE_MODEL.to_string()
}

/// Resolve a requested model id to a concrete one: a blank/None request falls back to the
/// server default, so a routine always carries a real model id. Public so the adapter
/// crate's `RoutineStore` can call it when processing `CreateRoutineReq`.
pub fn resolve_model(req: &Option<String>) -> String {
    req.as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_string)
        .unwrap_or_else(default_model)
}

/// Request body to create a routine. The user supplies `intent`; `prompt` is the
/// reviewed operational prompt (from the draft step). If `prompt` is empty the
/// server scaffolds one from the intent so the raw description is never run as-is.
#[derive(Deserialize)]
pub struct CreateRoutineReq {
    pub name: String,
    pub schedule: String,
    pub intent: String,
    #[serde(default)]
    pub prompt: String,
    pub scope: String,
    /// The model id the routine's agent should run on. `None`/blank -> the server default.
    #[serde(default)]
    pub model: Option<String>,
    /// The project this routine belongs to (`project.id`), or `None` for a global
    /// routine. Routines execute globally regardless of the viewed project; this only
    /// controls organization (dashboard grouping) and which export a routine travels in.
    #[serde(default)]
    pub project_id: Option<String>,
}

/// Request body for the draft-prompt step: the user's intent + scope.
#[derive(Deserialize)]
pub struct DraftPromptReq {
    pub intent: String,
    #[serde(default)]
    pub scope: String,
    /// The model to author the operational prompt on (blank -> server default).
    #[serde(default)]
    pub model: String,
}

/// Response from the draft-prompt step.
#[derive(Serialize)]
pub struct DraftPromptResp {
    /// The drafted operational prompt for the user to review/edit.
    pub prompt: String,
    /// How it was authored: `scaffold` (deterministic fallback, no Claude) or
    /// `claude` (the lead-engineer AI authored it).
    pub authored_by: String,
}

/// Deterministic scaffold for the operational prompt when no Claude connection is
/// available to author it for real. Wraps the user's intent with the standard
/// governance/scope framing and marks model tiering as the lead engineer's call,
/// so the flow is usable offline and the user always reviews a structured prompt
/// rather than running their raw description. The real AI authoring replaces this
/// when Claude is connected.
pub fn scaffold_prompt(intent: &str, scope: &str) -> String {
    let scope = if scope.trim().is_empty() {
        "read-only"
    } else {
        scope.trim()
    };
    format!(
        "Objective (from the user's description):\n{intent}\n\n\
         Operating constraints:\n\
         - Every file write passes the governance gate (deny-before-execute); the agent \
         has no shell, no direct file tools, and cannot spawn subagents.\n\
         - Scope / rules: {scope}\n\
         - Model tiering: use the smallest capable model per task and escalate only for \
         genuinely hard reasoning (the lead engineer sets this per task once Claude is \
         connected).\n\
         - Be directive and concrete: prefer exact files and steps over open-ended \
         exploration.\n\
         - Report what was done, what the gate denied, and anything left for human \
         review.\n\n\
         [Draft scaffold — connect Claude so the lead engineer authors the full \
         operational prompt (including chosen model tiers) from your description.]"
    )
}

/// Request body to enable/disable a routine.
#[derive(Deserialize)]
pub struct SetEnabledReq {
    pub enabled: bool,
}

/// A preset routine template. Templates are data-driven (loaded at startup) and
/// define sensible defaults for common automated patterns. Each template is pure
/// data — instantiation never mutates the template.
///
/// The template is instantiable into a fully-editable Routine via
/// [`instantiate_from_template`]. An architect can use the resulting routine as-is
/// or edit any field before saving.
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct RoutineTemplate {
    /// A stable identifier for the template (e.g., "bug-triage", "security-scan").
    /// Used for lookups and UI references.
    pub id: String,
    /// Display name for the template (e.g., "Bug Triage Dashboard").
    pub name: String,
    /// Short description of what the template does (one sentence, shown in template picker).
    pub description: String,
    /// Default schedule for this template (e.g., "daily 04:00", "weekly Mon 09:00").
    /// Defaults to "daily 09:00" if not specified.
    #[serde(default = "default_template_schedule")]
    pub schedule: String,
    /// Default permission/rule scope (e.g., "read-only", "write (gated)").
    /// Defaults to "read-only" if not specified.
    #[serde(default = "default_template_scope")]
    pub scope: String,
    /// The operational prompt the routine will run (fully authored, governance-framed).
    /// Never the user's raw description; always a structured directive ready for execution.
    pub prompt: String,
    /// The default model tier for this template's agent (an id from the `/api/models` catalog).
    /// Defaults to the server default if not specified.
    #[serde(default)]
    pub model: Option<String>,
}

fn default_template_schedule() -> String {
    "daily 09:00".to_string()
}

fn default_template_scope() -> String {
    "read-only".to_string()
}

/// Instantiate a routine from a template. This creates a fresh Routine prefilled
/// with the template's defaults, ready for the architect to review and customize.
/// The template itself is never mutated.
///
/// The instantiated routine:
/// - Uses the template's name as its own name (the architect can edit it).
/// - Receives the template's schedule, scope, prompt, and model.
/// - Is NOT created in the store; the caller decides whether to persist it.
/// - Can be passed to `RoutineStore::create` to be finalized.
pub fn instantiate_from_template(template: &RoutineTemplate) -> Routine {
    let model = template
        .model
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_string)
        .unwrap_or_else(default_model);

    Routine {
        id: String::new(), // Will be assigned on actual creation
        name: template.name.clone(),
        schedule: template.schedule.clone(),
        intent: String::new(), // Architect fills in their own intent
        prompt: template.prompt.clone(),
        scope: template.scope.clone(),
        enabled: false, // Templates start disabled; architect enables after review
        last_run: None,
        provisioned: false, // Not yet created in the store
        last_fired: None,
        project_id: None, // Architect assigns to a project if desired
        model,
        status: RoutineStatus::Idle,
        runs: Vec::new(),
    }
}

/// Load the built-in routine templates. This is a starter set embedded in the binary.
/// Future enhancement: could load from a config file or database.
pub fn builtin_templates() -> Vec<RoutineTemplate> {
    vec![
        RoutineTemplate {
            id: "bug-triage".to_string(),
            name: "Bug Triage Dashboard".to_string(),
            description: "Summarize open bugs and flag stale/duplicate issues for review."
                .to_string(),
            schedule: "daily 09:00".to_string(),
            scope: "read-only".to_string(),
            prompt: r#"Objective:
Audit the project's bug tracker. Summarize open bugs by status / age, flag any that
have been sitting for 30+ days without activity, and surface likely duplicates for
deduplication review.

Operating constraints:
- Scope / rules: read-only (inspect + report, no changes)
- Be directive and concrete: link to specific issues, quantify findings.
- Report what you discovered, prioritize by staleness, and suggest next steps.
- Model tiering: use a compact model for the systematic pass (pull issues, age them),
  escalate to reasoning if detecting subtle duplicate patterns.

The architect will review your report and file any blocking issues."#
                .to_string(),
            model: None,
        },
        RoutineTemplate {
            id: "security-scan".to_string(),
            name: "Security Scan & Patch".to_string(),
            description: "Scan dependencies for known vulnerabilities and propose patches."
                .to_string(),
            schedule: "daily 04:00".to_string(),
            scope: "write (gated)".to_string(),
            prompt: r#"Objective:
Perform a nightly security audit. Scan all direct and transitive dependencies for
known CVEs and security advisories, then author governed PRs to patch safe upgrades.

Operating constraints:
- Scope / rules: write (gated) — open branches with edits, no push until approved.
- Every file write passes the governance gate (deny-before-execute).
- Only propose upgrades with high confidence (no security downgrade, no API breakage).
- Link each PR to the advisory it addresses (e.g., https://nvd.nist.gov/...).
- Be directive: exact versions, exact commit history, exact test commands.
- Model tiering: compact model for systematic scanning; escalate reasoning for
  complex dependency graphs or version conflicts.

The architect will review each proposed PR and merge or close as needed."#
                .to_string(),
            model: None,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_field_rehydrates_empty_for_pre_history_json() {
        // A routine persisted before the `runs` field rehydrates with an empty history.
        let json = r#"{"id":"rt-1","name":"Old","schedule":"manual","intent":"i",
            "prompt":"p","scope":"read-only","enabled":true,"last_run":null}"#;
        let r: Routine = serde_json::from_str(json).unwrap();
        assert!(r.runs.is_empty());
    }

    #[test]
    fn builtin_templates_exist_and_are_valid() {
        let templates = builtin_templates();
        // At least the two starter templates exist.
        assert!(templates.len() >= 2);
        // Each has a unique id.
        let ids: Vec<_> = templates.iter().map(|t| &t.id).collect();
        assert_eq!(ids.len(), ids.iter().collect::<std::collections::HashSet<_>>().len());
        // Each has required fields non-empty.
        for t in templates {
            assert!(!t.id.is_empty());
            assert!(!t.name.is_empty());
            assert!(!t.description.is_empty());
            assert!(!t.prompt.is_empty());
            assert!(!t.schedule.is_empty());
            assert!(!t.scope.is_empty());
        }
    }

    #[test]
    fn instantiate_from_template_yields_valid_editable_routine() {
        let templates = builtin_templates();
        let template = &templates[0]; // Bug triage template

        let routine = instantiate_from_template(template);

        // Basic structure is valid.
        assert!(routine.id.is_empty(), "instantiation doesn't assign an id yet");
        assert_eq!(routine.name, template.name, "name matches template");
        assert_eq!(routine.schedule, template.schedule);
        assert_eq!(routine.scope, template.scope);
        assert_eq!(routine.prompt, template.prompt);
        // Sensible defaults for a new routine.
        assert!(!routine.enabled, "templates start disabled");
        assert!(!routine.provisioned, "templates start unprovisioned");
        assert!(routine.intent.is_empty(), "intent left for architect to fill");
        assert!(routine.project_id.is_none(), "no project assigned");
        assert!(routine.last_run.is_none(), "never been run");
        assert_eq!(routine.status, RoutineStatus::Idle);
    }

    #[test]
    fn instantiate_from_template_resolves_model_like_create() {
        let templates = builtin_templates();
        let template = &templates[0];

        let routine = instantiate_from_template(template);
        // Model defaults to server default when not specified in template.
        assert!(!routine.model.is_empty(), "model is resolved to default");
        assert_eq!(routine.model, default_model());
    }

    #[test]
    fn instantiate_from_template_with_explicit_model() {
        let mut template = builtin_templates()[0].clone();
        template.model = Some("claude-opus".to_string());

        let routine = instantiate_from_template(&template);
        assert_eq!(routine.model, "claude-opus");
    }

    #[test]
    fn instantiate_from_template_is_indistinguishable_from_hand_built() {
        // A routine built from a template should be indistinguishable from one
        // that was hand-authored to the same shape. This test verifies they
        // serialize identically (modulo id, which is assigned on store creation).
        let template = builtin_templates()[0].clone();
        let from_template = instantiate_from_template(&template);

        let hand_built = Routine {
            id: String::new(),
            name: template.name.clone(),
            schedule: template.schedule.clone(),
            intent: String::new(),
            prompt: template.prompt.clone(),
            scope: template.scope.clone(),
            enabled: false,
            last_run: None,
            provisioned: false,
            last_fired: None,
            project_id: None,
            model: default_model(),
            status: RoutineStatus::Idle,
            runs: Vec::new(),
        };

        // Serialize both and verify they match (id is already empty, so this is direct).
        let tmpl_json =
            serde_json::to_string(&from_template).expect("from_template serializes");
        let hand_json = serde_json::to_string(&hand_built).expect("hand_built serializes");
        assert_eq!(tmpl_json, hand_json, "instantiated and hand-built routines serialize identically");
    }
}
