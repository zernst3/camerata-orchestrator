//! Routine dashboard backend (ADR `routine_dashboard`).
//!
//! A routine is a scheduled governed run: a name, a schedule, a prompt, a permission
//! scope, an enabled flag, and the last-run summary. "Run now" executes a governed run
//! immediately, reusing the REAL gate script from the run engine (so the recorded
//! verdicts are genuine, token-free). The auto-fire scheduler (an engine-owned timer)
//! is the remaining wiring; this turn ships the model, the store, and run-now so the
//! dashboard can list, toggle, and run routines.
//!
//! Routine templates (feature #59) provide a data-driven way to instantiate preset
//! routines: a template defines a name, description, default cadence, model tier,
//! operational prompt, and governance scope. The pure `instantiate_from_template`
//! function creates a fully-editable routine from a template without mutating it.
//! The template is a data shape (loaded at startup), not UI; templates are extensible
//! and portable across projects.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

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
}

fn default_true() -> bool {
    true
}

fn default_model() -> String {
    crate::llm::DEFAULT_MODEL.to_string()
}

/// Resolve a requested model id to a concrete one: a blank/None request falls back to the
/// server default, so a routine always carries a real model id.
fn resolve_model(req: &Option<String>) -> String {
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

/// Routine store. In-memory by default ([`new`]/[`seeded`]); [`at`] additionally
/// persists to `<data_dir>/camerata/routines.json` so an architect's routines survive
/// a restart (routines were previously lost on every launch). `Clone` is a shallow
/// handle (shared `Arc`s) so it can live in [`crate::AppState`].
#[derive(Clone, Default)]
pub struct RoutineStore {
    items: Arc<Mutex<Vec<Routine>>>,
    counter: Arc<AtomicUsize>,
    /// Disk path when persistence is on; `None` for the in-memory store.
    path: Option<Arc<PathBuf>>,
}

impl RoutineStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Persist to (and rehydrate from) `path`. On load, the id counter is advanced
    /// past the highest existing `rt-N` so new ids never collide with rehydrated ones.
    pub fn at(path: PathBuf) -> Self {
        let items: Vec<Routine> = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let max = items
            .iter()
            .filter_map(|r| r.id.strip_prefix("rt-"))
            .filter_map(|n| n.parse::<usize>().ok())
            .max()
            .unwrap_or(0);
        Self {
            items: Arc::new(Mutex::new(items)),
            counter: Arc::new(AtomicUsize::new(max)),
            path: Some(Arc::new(path)),
        }
    }

    /// Best-effort flush of the in-memory list to disk. The in-memory state is always
    /// authoritative; a failed write never blocks the mutation that triggered it.
    fn flush(&self) {
        let Some(p) = &self.path else { return };
        let Ok(items) = self.items.lock() else { return };
        if let Ok(s) = serde_json::to_string(&*items) {
            let _ = std::fs::write(p.as_ref(), s);
        }
    }

    /// A store seeded with representative routines so the dashboard has content.
    pub fn seeded() -> Self {
        let store = Self::new();
        let mk =
            |id: &str, name: &str, schedule: &str, intent: &str, scope: &str, enabled: bool| {
                Routine {
                    id: id.to_string(),
                    name: name.to_string(),
                    schedule: schedule.to_string(),
                    intent: intent.to_string(),
                    // Demo data: the operational prompt is the scaffold of the intent
                    // (the live create path does the same, or AI-authors it).
                    prompt: scaffold_prompt(intent, scope),
                    scope: scope.to_string(),
                    enabled,
                    last_run: None,
                    provisioned: true,
                    last_fired: None,
                    project_id: None,
                    model: default_model(),
                    status: RoutineStatus::Idle,
                }
            };
        let seed = vec![
            mk(
                "rt-1",
                "Nightly dependency + security sweep",
                "daily 04:00",
                "Scan dependencies for advisories; open governed PRs for safe upgrades.",
                "SEC-* + maintenance, write behind the gate",
                true,
            ),
            mk(
                "rt-2",
                "Stale-PR auditor",
                "weekly Mon 09:00",
                "Flag PRs with no activity in 14 days and summarize what they are blocked on.",
                "read-only",
                true,
            ),
            mk(
                "rt-3",
                "Convention drift check",
                "daily 06:00",
                "Check that CONVENTIONS rule ids referenced in code still exist.",
                "ARCH-*, read-only",
                false,
            ),
        ];
        if let Ok(mut guard) = store.items.lock() {
            *guard = seed;
        }
        store.counter.store(3, Ordering::SeqCst);
        store
    }

    pub fn list(&self) -> Vec<Routine> {
        self.items.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn create(&self, req: &CreateRoutineReq) -> Routine {
        let n = self.counter.fetch_add(1, Ordering::SeqCst) + 1;
        // The user's raw intent is never run as-is: if the reviewed operational
        // prompt is empty, scaffold one from the intent.
        let prompt = if req.prompt.trim().is_empty() {
            scaffold_prompt(&req.intent, &req.scope)
        } else {
            req.prompt.clone()
        };
        let routine = Routine {
            id: format!("rt-{n}"),
            name: req.name.clone(),
            schedule: req.schedule.clone(),
            intent: req.intent.clone(),
            prompt,
            scope: req.scope.clone(),
            enabled: true,
            last_run: None,
            // Created here, so it's provisioned on this backend immediately.
            provisioned: true,
            last_fired: None,
            project_id: req.project_id.clone(),
            model: resolve_model(&req.model),
            status: RoutineStatus::Idle,
        };
        if let Ok(mut guard) = self.items.lock() {
            guard.push(routine.clone());
        }
        self.flush();
        routine
    }

    /// Create a routine that arrived via a project import: associated with `project_id`,
    /// and deliberately UN-provisioned + stopped, so the importer explicitly sets it up
    /// and starts it (never silently auto-running someone else's unattended agent on
    /// import). Shares the id counter with [`create`] so ids never collide.
    pub fn create_imported(&self, req: &CreateRoutineReq, project_id: &str) -> Routine {
        let n = self.counter.fetch_add(1, Ordering::SeqCst) + 1;
        let prompt = if req.prompt.trim().is_empty() {
            scaffold_prompt(&req.intent, &req.scope)
        } else {
            req.prompt.clone()
        };
        let routine = Routine {
            id: format!("rt-{n}"),
            name: req.name.clone(),
            schedule: req.schedule.clone(),
            intent: req.intent.clone(),
            prompt,
            scope: req.scope.clone(),
            enabled: false,
            last_run: None,
            provisioned: false,
            last_fired: None,
            project_id: Some(project_id.to_string()),
            model: resolve_model(&req.model),
            status: RoutineStatus::Idle,
        };
        if let Ok(mut guard) = self.items.lock() {
            guard.push(routine.clone());
        }
        self.flush();
        routine
    }

    /// Routines belonging to a project (`project_id` match), for export + grouping.
    pub fn list_for_project(&self, project_id: &str) -> Vec<Routine> {
        self.items
            .lock()
            .map(|g| {
                g.iter()
                    .filter(|r| r.project_id.as_deref() == Some(project_id))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Replace a project's routines wholesale (used on project overwrite-import so a
    /// re-import doesn't duplicate). Drops the project's existing routines, then creates
    /// each incoming one as imported (un-provisioned + stopped). Returns the count added.
    pub fn replace_for_project(&self, project_id: &str, reqs: &[CreateRoutineReq]) -> usize {
        if let Ok(mut guard) = self.items.lock() {
            guard.retain(|r| r.project_id.as_deref() != Some(project_id));
        }
        self.flush();
        for req in reqs {
            self.create_imported(req, project_id);
        }
        reqs.len()
    }

    /// Edit a routine's user-facing fields in place (name / schedule / intent /
    /// prompt / scope). Mirrors `create`'s rule: an empty reviewed prompt is
    /// re-scaffolded from the intent so a routine never runs the raw intent as-is.
    /// `enabled` and `last_run` are preserved.
    pub fn update(&self, id: &str, req: &CreateRoutineReq) -> Option<Routine> {
        let mut guard = self.items.lock().ok()?;
        let r = guard.iter_mut().find(|r| r.id == id)?;
        r.name = req.name.clone();
        r.schedule = req.schedule.clone();
        r.intent = req.intent.clone();
        r.scope = req.scope.clone();
        r.prompt = if req.prompt.trim().is_empty() {
            scaffold_prompt(&req.intent, &req.scope)
        } else {
            req.prompt.clone()
        };
        r.project_id = req.project_id.clone();
        r.model = resolve_model(&req.model);
        let updated = r.clone();
        drop(guard);
        self.flush();
        Some(updated)
    }

    /// Delete a routine by id. Returns true if one was removed.
    pub fn delete(&self, id: &str) -> bool {
        let Ok(mut guard) = self.items.lock() else {
            return false;
        };
        let before = guard.len();
        guard.retain(|r| r.id != id);
        let removed = guard.len() != before;
        drop(guard);
        if removed {
            self.flush();
        }
        removed
    }

    pub fn set_enabled(&self, id: &str, enabled: bool) -> Option<Routine> {
        let mut guard = self.items.lock().ok()?;
        let r = guard.iter_mut().find(|r| r.id == id)?;
        r.enabled = enabled;
        let updated = r.clone();
        drop(guard);
        self.flush();
        Some(updated)
    }

    /// Mark a routine provisioned on this backend (the "Set up" action for an imported
    /// routine). Idempotent; provisioning never auto-enables — the architect still
    /// presses Start.
    pub fn set_provisioned(&self, id: &str) -> Option<Routine> {
        let mut guard = self.items.lock().ok()?;
        let r = guard.iter_mut().find(|r| r.id == id)?;
        r.provisioned = true;
        let updated = r.clone();
        drop(guard);
        self.flush();
        Some(updated)
    }

    /// Set a routine's lifecycle status explicitly (issue #43). Used to drive a routine
    /// `Running` at the start of a run and to return it to `Idle` once a human resolves the
    /// escalation that blocked it. Returns the updated routine, or `None` for an unknown id.
    pub fn set_status(&self, id: &str, status: RoutineStatus) -> Option<Routine> {
        let mut guard = self.items.lock().ok()?;
        let r = guard.iter_mut().find(|r| r.id == id)?;
        r.status = status;
        let updated = r.clone();
        drop(guard);
        self.flush();
        Some(updated)
    }

    /// Record that the scheduler fired this routine at `ts` (RFC3339), so the same slot
    /// isn't fired again on the next tick. Separate from `run_now`'s summary so the
    /// scheduler can stamp the fire even when it drives the run itself.
    pub fn mark_fired(&self, id: &str, ts: &str) -> Option<Routine> {
        let mut guard = self.items.lock().ok()?;
        let r = guard.iter_mut().find(|r| r.id == id)?;
        r.last_fired = Some(ts.to_string());
        let updated = r.clone();
        drop(guard);
        self.flush();
        Some(updated)
    }

    /// Run a routine now: execute a governed run via the REAL gate script and record
    /// the summary. Token-free and instant (the pure script, not the timed executor).
    pub fn run_now(&self, id: &str) -> Option<Routine> {
        let events = crate::run::run_event_script();
        let denies = events.iter().filter(|e| e.verdict == "deny").count();
        let allows = events.iter().filter(|e| e.verdict == "allow").count();
        // Capture WHICH rules were denied (deduped, in order) so a blocked routine can name
        // them in its escalation rather than just reporting a count.
        let mut denied_rules: Vec<String> = Vec::new();
        for e in events.iter().filter(|e| e.verdict == "deny") {
            if let Some(rule) = &e.rule {
                if !denied_rules.contains(rule) {
                    denied_rules.push(rule.clone());
                }
            }
        }
        let summary = RoutineRunSummary {
            outcome: "passed".to_string(),
            total_verdicts: events.len(),
            denies,
            allows,
            denied_rules,
        };
        let mut guard = self.items.lock().ok()?;
        let r = guard.iter_mut().find(|r| r.id == id)?;
        r.last_run = Some(summary);
        // Drive the lifecycle status from the run (issue #43): a run the gate blocked needs
        // a human, so it lands `BlockedNeedsReview` (the escalation hook then raises the
        // review); an unblocked run lands `Done`.
        r.status = if denies > 0 {
            RoutineStatus::BlockedNeedsReview
        } else {
            RoutineStatus::Done
        };
        let updated = r.clone();
        drop(guard);
        self.flush();
        Some(updated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeded_lists_three_routines() {
        let store = RoutineStore::seeded();
        let list = store.list();
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].id, "rt-1");
        assert!(list[0].enabled);
        assert!(!list[2].enabled);
    }

    #[test]
    fn toggle_and_create_and_run() {
        let store = RoutineStore::seeded();
        assert!(store.set_enabled("rt-3", true).unwrap().enabled);

        let created = store.create(&CreateRoutineReq {
            name: "Ad-hoc".to_string(),
            schedule: "manual".to_string(),
            intent: "do a thing".to_string(),
            prompt: String::new(),
            scope: "read-only".to_string(),
            project_id: None,
            model: None,
        });
        assert_eq!(created.id, "rt-4");
        assert_eq!(store.list().len(), 4);
        // Empty prompt -> scaffolded from intent (never run the raw intent as-is).
        assert!(created.prompt.contains("do a thing"));
        assert!(created.prompt.contains("governance gate"));
        assert_eq!(created.intent, "do a thing");

        // Run-now records a real-gate summary (2 denies + 1 allow from the script).
        let ran = store.run_now("rt-1").unwrap();
        let summary = ran.last_run.expect("recorded");
        assert_eq!(summary.outcome, "passed");
        assert_eq!(summary.denies, 2);
        assert_eq!(summary.allows, 1);

        assert!(store.run_now("nope").is_none());
    }

    #[test]
    fn update_edits_fields_and_preserves_enabled_and_last_run() {
        let store = RoutineStore::seeded();
        // Record a run on rt-1 so we can prove last_run survives an edit.
        store.run_now("rt-1").unwrap();

        let edited = store
            .update(
                "rt-1",
                &CreateRoutineReq {
                    name: "Renamed".to_string(),
                    schedule: "weekly Mon,Wed 09:00".to_string(),
                    intent: "new intent".to_string(),
                    prompt: String::new(), // empty -> re-scaffolded from intent
                    scope: "write (gated)".to_string(),
                    project_id: None,
                    model: None,
                },
            )
            .unwrap();
        assert_eq!(edited.name, "Renamed");
        assert_eq!(edited.schedule, "weekly Mon,Wed 09:00");
        assert_eq!(edited.scope, "write (gated)");
        assert!(
            edited.prompt.contains("new intent"),
            "empty prompt re-scaffolded"
        );
        assert!(edited.enabled, "enabled flag preserved across edit");
        assert!(edited.last_run.is_some(), "last_run preserved across edit");

        assert!(store
            .update(
                "nope",
                &CreateRoutineReq {
                    name: "x".into(),
                    schedule: "daily 09:00".into(),
                    intent: "x".into(),
                    prompt: String::new(),
                    scope: "read-only".into(),
                    project_id: None,
                    model: None,
                }
            )
            .is_none());
    }

    #[test]
    fn persists_across_reload_and_advances_counter() {
        // A temp path unique to this test (no Date/rand available; use the test name).
        let path = std::env::temp_dir().join("camerata-routine-persist-across-reload-test.json");
        let _ = std::fs::remove_file(&path);

        // First store: create one routine, which flushes to disk.
        {
            let store = RoutineStore::at(path.clone());
            assert_eq!(store.list().len(), 0, "starts empty when file is absent");
            let created = store.create(&CreateRoutineReq {
                name: "Nightly".to_string(),
                schedule: "daily 04:00".to_string(),
                intent: "scan deps".to_string(),
                prompt: String::new(),
                scope: "read-only".to_string(),
                project_id: None,
                model: None,
            });
            assert_eq!(created.id, "rt-1");
            store.set_enabled("rt-1", false);
        }

        // Second store at the same path: rehydrates the routine AND its disabled flag,
        // and the counter is advanced so the next id is rt-2 (no collision).
        {
            let store = RoutineStore::at(path.clone());
            let list = store.list();
            assert_eq!(list.len(), 1, "rehydrated the persisted routine");
            assert_eq!(list[0].id, "rt-1");
            assert!(!list[0].enabled, "disabled flag survived the reload");

            let next = store.create(&CreateRoutineReq {
                name: "Second".to_string(),
                schedule: "weekly Mon 09:00".to_string(),
                intent: "audit PRs".to_string(),
                prompt: String::new(),
                scope: "read-only".to_string(),
                project_id: None,
                model: None,
            });
            assert_eq!(
                next.id, "rt-2",
                "counter advanced past the rehydrated max id"
            );
        }

        // Delete also persists: a third store sees only the survivor.
        {
            let store = RoutineStore::at(path.clone());
            assert!(store.delete("rt-1"));
        }
        {
            let store = RoutineStore::at(path.clone());
            let list = store.list();
            assert_eq!(list.len(), 1);
            assert_eq!(list[0].id, "rt-2", "delete persisted across reload");
        }

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn imported_routines_are_project_scoped_unprovisioned_and_replaceable() {
        let store = RoutineStore::new();
        // A global routine (no project) created the normal way: provisioned + enabled.
        store.create(&CreateRoutineReq {
            name: "Global".into(),
            schedule: "daily 09:00".into(),
            intent: "x".into(),
            prompt: String::new(),
            scope: "read-only".into(),
            project_id: None,
            model: None,
        });

        let reqs = vec![
            CreateRoutineReq {
                name: "A".into(),
                schedule: "daily 09:00".into(),
                intent: "a".into(),
                prompt: String::new(),
                scope: "read-only".into(),
                project_id: None, // create_imported sets the project id from its arg
                model: None,
            },
            CreateRoutineReq {
                name: "B".into(),
                schedule: "weekly Mon 09:00".into(),
                intent: "b".into(),
                prompt: String::new(),
                scope: "read-only".into(),
                project_id: None,
                model: None,
            },
        ];
        assert_eq!(store.replace_for_project("p1", &reqs), 2);

        let p1 = store.list_for_project("p1");
        assert_eq!(p1.len(), 2);
        assert!(
            p1.iter().all(|r| !r.provisioned && !r.enabled),
            "imported routines arrive un-provisioned + stopped"
        );
        assert!(p1.iter().all(|r| r.project_id.as_deref() == Some("p1")));
        // The global routine is untouched.
        assert_eq!(store.list().len(), 3);

        // Re-import REPLACES (no duplicate pile-up).
        assert_eq!(store.replace_for_project("p1", &reqs[..1]), 1);
        assert_eq!(store.list_for_project("p1").len(), 1);
        assert_eq!(store.list().len(), 2, "global + one project routine");
    }

    #[test]
    fn run_now_sets_lifecycle_status_and_set_status_resets() {
        let store = RoutineStore::seeded();
        // A fresh routine starts Idle.
        assert_eq!(store.list()[0].status, RoutineStatus::Idle);

        // The scripted gate denies (2 denies) -> the run lands BlockedNeedsReview.
        let ran = store.run_now("rt-1").unwrap();
        assert_eq!(ran.status, RoutineStatus::BlockedNeedsReview);

        // Resolving the block returns the routine to Idle so the next slot can run.
        let reset = store.set_status("rt-1", RoutineStatus::Idle).unwrap();
        assert_eq!(reset.status, RoutineStatus::Idle);
        // set_status on an unknown id is a no-op None.
        assert!(store.set_status("nope", RoutineStatus::Done).is_none());
    }

    #[test]
    fn status_persists_and_back_compat_defaults_to_idle() {
        let path = std::env::temp_dir().join("camerata-routine-status-persist-test.json");
        let _ = std::fs::remove_file(&path);

        // Persist a routine in the BlockedNeedsReview state.
        {
            let store = RoutineStore::at(path.clone());
            store.create(&CreateRoutineReq {
                name: "Nightly".into(),
                schedule: "daily 04:00".into(),
                intent: "scan".into(),
                prompt: String::new(),
                scope: "read-only".into(),
                project_id: None,
                model: None,
            });
            store.run_now("rt-1").unwrap(); // -> BlockedNeedsReview, flushed
        }
        // Rehydrate: the status survives the reload.
        {
            let store = RoutineStore::at(path.clone());
            assert_eq!(store.list()[0].status, RoutineStatus::BlockedNeedsReview);
        }

        // Back-compat: a routine JSON WITHOUT a `status` field rehydrates as Idle.
        {
            let legacy = r#"[{"id":"rt-9","name":"Legacy","schedule":"daily 09:00",
                "intent":"x","prompt":"p","scope":"read-only","enabled":true,
                "last_run":null}]"#;
            std::fs::write(&path, legacy).unwrap();
            let store = RoutineStore::at(path.clone());
            let r = &store.list()[0];
            assert_eq!(
                r.status,
                RoutineStatus::Idle,
                "missing status -> Idle default"
            );
            // Other absent optional fields also defaulted (provisioned true, model set).
            assert!(r.provisioned);
            assert!(!r.model.is_empty());
        }

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn delete_removes_only_the_named_routine() {
        let store = RoutineStore::seeded();
        assert_eq!(store.list().len(), 3);
        assert!(store.delete("rt-2"));
        assert_eq!(store.list().len(), 2);
        assert!(store.list().iter().all(|r| r.id != "rt-2"));
        // Deleting a missing id is a no-op false.
        assert!(!store.delete("rt-2"));
        assert!(!store.delete("nope"));
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
        };

        // Serialize both and verify they match (id is already empty, so this is direct).
        let tmpl_json =
            serde_json::to_string(&from_template).expect("from_template serializes");
        let hand_json = serde_json::to_string(&hand_built).expect("hand_built serializes");
        assert_eq!(tmpl_json, hand_json, "instantiated and hand-built routines serialize identically");
    }
}
