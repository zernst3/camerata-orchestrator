//! The routine dashboard: a third surface to manage scheduled governed routines
//! (ADR `routine_dashboard`). A table of routines with their schedule, prompt,
//! permission scope, enabled state, and last-run summary, plus enable/disable,
//! run-now, and a create form. Run-now executes a governed run (real gate verdicts)
//! and records the summary. The auto-fire scheduler is the remaining wiring.

use dioxus::prelude::*;

use crate::md::md_to_html;

/// Weekday labels, Sunday-first (matches the `weekdays` toggle vector order).
const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// Serialize the structured schedule picker into the human-readable schedule string
/// the BFF stores (e.g. `daily 09:00`, `weekly Mon,Wed 09:00`, `monthly day 15 09:00`,
/// `once 2026-06-20 14:00`). The empty-field fallbacks keep the string well-formed
/// even before every control is touched.
fn build_schedule(freq: &str, time: &str, date: &str, weekdays: &[bool], monthday: u32) -> String {
    let t = if time.is_empty() { "09:00" } else { time };
    match freq {
        "once" => {
            if date.is_empty() {
                format!("once {t}")
            } else {
                format!("once {date} {t}")
            }
        }
        "weekly" => {
            let days: Vec<&str> = weekdays
                .iter()
                .enumerate()
                .filter(|(_, on)| **on)
                .map(|(i, _)| WEEKDAYS[i])
                .collect();
            let days_str = if days.is_empty() {
                "Mon".to_string()
            } else {
                days.join(",")
            };
            format!("weekly {days_str} {t}")
        }
        "monthly" => format!("monthly day {monthday} {t}"),
        _ => format!("daily {t}"),
    }
}

/// Parse a stored schedule string back into the picker state, for Edit prefill.
/// Returns `(freq, time, date, weekdays, monthday)`. Anything that doesn't match a
/// known shape falls back to a daily-09:00 default (the schedule string is still
/// shown verbatim in the row, so nothing is lost — the picker just starts neutral).
fn parse_schedule(s: &str) -> (String, String, String, Vec<bool>, u32) {
    let default_days = vec![false, true, false, false, false, false, false];
    let parts: Vec<&str> = s.split_whitespace().collect();
    match parts.as_slice() {
        ["daily", time] => (
            "daily".into(),
            (*time).into(),
            String::new(),
            default_days,
            1,
        ),
        ["weekly", days, time] => {
            let mut wd = vec![false; 7];
            for d in days.split(',') {
                if let Some(i) = WEEKDAYS.iter().position(|w| w.eq_ignore_ascii_case(d)) {
                    wd[i] = true;
                }
            }
            ("weekly".into(), (*time).into(), String::new(), wd, 1)
        }
        ["monthly", "day", n, time] => (
            "monthly".into(),
            (*time).into(),
            String::new(),
            default_days,
            n.parse::<u32>().unwrap_or(1).clamp(1, 31),
        ),
        ["once", date, time] => (
            "once".into(),
            (*time).into(),
            (*date).into(),
            default_days,
            1,
        ),
        ["once", time] => (
            "once".into(),
            (*time).into(),
            String::new(),
            default_days,
            1,
        ),
        _ => (
            "daily".into(),
            "09:00".into(),
            String::new(),
            default_days,
            1,
        ),
    }
}

/// A routine as the BFF reports it (`/api/routines`).
#[derive(Clone, PartialEq, serde::Deserialize)]
struct RoutineView {
    id: String,
    name: String,
    schedule: String,
    /// The user's plain-language description (what they want).
    #[serde(default)]
    intent: String,
    /// The AI-authored operational prompt (shown on demand).
    prompt: String,
    scope: String,
    enabled: bool,
    last_run: Option<RoutineRunSummaryView>,
    /// Whether this routine is set up on this backend. Imported routines arrive
    /// un-provisioned and need a "Set up" before Start does anything. Defaults true so
    /// the field is optional against older BFFs.
    #[serde(default = "default_true")]
    provisioned: bool,
    /// When the scheduler last fired it (RFC3339). Carried for future display; not yet
    /// rendered.
    #[serde(default)]
    #[allow(dead_code)]
    last_fired: Option<String>,
    /// The project this routine belongs to, or `None` for a global routine. Drives the
    /// dashboard grouping; execution is global regardless.
    #[serde(default)]
    project_id: Option<String>,
    /// The model the routine's agent runs on (id from `/api/models`).
    #[serde(default)]
    model: String,
    /// The routine's lifecycle status (issue #43): `idle` | `running` |
    /// `blocked_needs_review` | `done` | `failed`. Defaults to `idle` so older BFFs that
    /// don't send it render a sensible badge.
    #[serde(default = "default_status")]
    status: String,
}

fn default_status() -> String {
    "idle".to_string()
}

/// Map a routine status wire string to a short badge label + a CSS modifier class.
/// Unknown values fall back to the idle styling so a new server status never renders blank.
fn status_badge(status: &str) -> (&'static str, &'static str) {
    match status {
        "running" => ("Running", "running"),
        "blocked_needs_review" => ("Blocked", "blocked"),
        "done" => ("Done", "done"),
        "failed" => ("Failed", "failed"),
        _ => ("Idle", "idle"),
    }
}

/// One model the routine form's picker offers (sourced from `GET /api/models/registry`).
#[derive(Clone, PartialEq, serde::Deserialize)]
struct ModelOption {
    /// Badge-enriched label (built from the registry display + free/context badges).
    label: String,
    id: String,
    /// Provider key: "claude" | "openrouter". Used for `<optgroup>` grouping.
    #[serde(default)]
    provider: String,
    #[serde(default)]
    free: bool,
}

#[derive(Clone, PartialEq, serde::Deserialize)]
struct ModelsResp {
    models: Vec<ModelOption>,
    #[serde(default)]
    default: String,
}

impl ModelsResp {
    /// Return models grouped by provider for `<optgroup>` rendering.
    fn grouped(&self) -> Vec<(&'static str, Vec<&ModelOption>)> {
        let claude: Vec<&ModelOption> =
            self.models.iter().filter(|m| m.provider == "claude").collect();
        let openrouter: Vec<&ModelOption> =
            self.models.iter().filter(|m| m.provider == "openrouter").collect();
        let mut groups = Vec::new();
        if !claude.is_empty() {
            groups.push(("Claude (subscription)", claude));
        }
        if !openrouter.is_empty() {
            groups.push(("OpenRouter", openrouter));
        }
        groups
    }
}

/// One entry from the `/api/models/registry` wire response.
#[derive(serde::Deserialize)]
struct RegistryEntryWire {
    id: String,
    display: String,
    #[serde(default)]
    provider: String,
    #[serde(default)]
    free: bool,
    #[serde(default)]
    tool_use: bool,
    #[serde(default)]
    context: u64,
}

#[derive(serde::Deserialize)]
struct RegistryResp {
    models: Vec<RegistryEntryWire>,
}

/// Fetch the model catalog from the registry endpoint so the routine form can pick
/// the model its agent runs on. Falls back gracefully to Claude-only when no
/// OpenRouter key is set.
async fn fetch_models() -> Option<ModelsResp> {
    let resp: RegistryResp = reqwest::get(format!("{}/api/models/registry", crate::BFF_URL))
        .await
        .ok()?
        .json()
        .await
        .ok()?;

    let models: Vec<ModelOption> = resp
        .models
        .into_iter()
        .map(|e| {
            let mut badges = Vec::<String>::new();
            if e.free {
                badges.push("FREE".to_string());
            }
            if e.provider == "openrouter" && !e.tool_use {
                badges.push("no-tools".to_string());
            }
            if e.context > 0 {
                badges.push(format!("{}K ctx", e.context / 1000));
            }
            let label = if badges.is_empty() {
                e.display.clone()
            } else {
                format!("{} [{}]", e.display, badges.join("] ["))
            };
            ModelOption { label, id: e.id, provider: e.provider, free: e.free }
        })
        .collect();

    let default = models
        .iter()
        .find(|m| m.provider == "claude")
        .or_else(|| models.first())
        .map(|m| m.id.clone())
        .unwrap_or_default();

    Some(ModelsResp { models, default })
}

/// The slice of a project the routine dashboard needs: id + name, for the form's project
/// picker and the grouped table.
#[derive(Clone, PartialEq, serde::Deserialize)]
struct ProjectView {
    id: String,
    name: String,
}

/// Fetch the project list so routines can be assigned to (and grouped by) a project.
async fn fetch_projects() -> Option<Vec<ProjectView>> {
    reqwest::get(format!("{}/api/projects", crate::BFF_URL))
        .await
        .ok()?
        .json::<Vec<ProjectView>>()
        .await
        .ok()
}

/// A routine template: a preset configuration for common automation patterns.
/// (Mirrors the server-side RoutineTemplate shape.)
#[derive(Clone, PartialEq, serde::Deserialize, Debug)]
struct RoutineTemplate {
    id: String,
    name: String,
    description: String,
    #[serde(default)]
    schedule: String,
    #[serde(default)]
    scope: String,
    prompt: String,
    #[serde(default)]
    model: Option<String>,
}

/// Fetch available routine templates (preset configurations).
async fn fetch_routine_templates() -> Option<Vec<RoutineTemplate>> {
    reqwest::get(format!("{}/api/routines/templates", crate::BFF_URL))
        .await
        .ok()?
        .json::<Vec<RoutineTemplate>>()
        .await
        .ok()
}

/// Instantiate a routine from a template. Returns the routine prefilled with the
/// template's defaults, ready for the architect to review and customize before saving.
async fn instantiate_from_template(template_id: &str) -> Option<RoutineView> {
    reqwest::Client::new()
        .post(format!(
            "{}/api/routines/templates/{}/instantiate",
            crate::BFF_URL,
            template_id
        ))
        .send()
        .await
        .ok()?
        .json::<RoutineView>()
        .await
        .ok()
}

fn default_true() -> bool {
    true
}

#[derive(Clone, PartialEq, serde::Deserialize)]
struct RoutineRunSummaryView {
    outcome: String,
    #[allow(dead_code)]
    total_verdicts: usize,
    denies: usize,
    allows: usize,
}

/// An escalation as the BFF reports it (`/api/escalations`). The `?`-marked
/// fields are optional in the JSON and default to `None` so older BFF builds
/// remain compatible.
#[derive(Clone, PartialEq, serde::Deserialize)]
pub struct EscalationView {
    pub id: String,
    pub routine_id: String,
    pub routine_name: String,
    /// Why the routine stopped and raised this escalation.
    pub reason: String,
    /// The specific decision that is blocking the routine: what the architect
    /// needs to resolve before the routine can continue.
    pub stopped_for: String,
    /// AI-generated answer suggestions the architect can adopt verbatim or
    /// edit before submitting.
    #[serde(default)]
    pub suggestions: Vec<String>,
    #[serde(default)]
    pub raw_context: String,
    /// "open" | "resolved"
    pub status: String,
    #[serde(default)]
    pub human_answer: Option<String>,
    /// The directive the server translated the human answer into, returned on
    /// the POST /answer response. Displayed briefly after submit.
    #[serde(default)]
    pub translated_directive: Option<String>,
    pub created: String,
    #[serde(default)]
    pub resolved: Option<String>,
    /// The human <-> lead-engineer review conversation.
    #[serde(default)]
    pub conversation: Vec<EscalationMsgView>,
}

/// One turn in the escalation review conversation.
#[derive(Clone, PartialEq, serde::Deserialize)]
pub struct EscalationMsgView {
    /// "user" | "assistant"
    pub role: String,
    pub text: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub ts: String,
}

async fn fetch_routines() -> Option<Vec<RoutineView>> {
    reqwest::get(format!("{}/api/routines", crate::BFF_URL))
        .await
        .ok()?
        .json::<Vec<RoutineView>>()
        .await
        .ok()
}

/// Fetch all currently-open escalations so the dashboard can mark blocked rows.
async fn fetch_open_escalations() -> Option<Vec<EscalationView>> {
    reqwest::get(format!("{}/api/escalations?open=true", crate::BFF_URL))
        .await
        .ok()?
        .json::<Vec<EscalationView>>()
        .await
        .ok()
}

/// Send one message in the escalation review conversation; the lead-engineer agent
/// replies (it never unblocks — only `answer_escalation` does). Returns the updated
/// escalation with both turns appended.
async fn chat_escalation(id: &str, message: &str, model: &str) -> Option<EscalationView> {
    reqwest::Client::new()
        .post(format!("{}/api/escalations/{}/chat", crate::BFF_URL, id))
        .json(&serde_json::json!({ "message": message, "model": model }))
        .send()
        .await
        .ok()?
        .json::<EscalationView>()
        .await
        .ok()
}

/// Submit the architect's authorization to an escalation. Returns the resolved
/// escalation (including the server's `translated_directive`) on success.
async fn answer_escalation(id: &str, answer: &str) -> Option<EscalationView> {
    reqwest::Client::new()
        .post(format!("{}/api/escalations/{}/answer", crate::BFF_URL, id))
        .json(&serde_json::json!({ "answer": answer }))
        .send()
        .await
        .ok()?
        .json::<EscalationView>()
        .await
        .ok()
}

async fn set_enabled(id: &str, enabled: bool) -> Option<RoutineView> {
    reqwest::Client::new()
        .post(format!("{}/api/routines/{}/enable", crate::BFF_URL, id))
        .json(&serde_json::json!({ "enabled": enabled }))
        .send()
        .await
        .ok()?
        .json::<RoutineView>()
        .await
        .ok()
}

/// Provision an imported routine on this backend (the "Set up" action). Returns the
/// updated routine (now `provisioned`, still stopped).
async fn provision(id: &str) -> Option<RoutineView> {
    reqwest::Client::new()
        .post(format!("{}/api/routines/{}/provision", crate::BFF_URL, id))
        .send()
        .await
        .ok()?
        .json::<RoutineView>()
        .await
        .ok()
}

async fn run_now(id: &str) -> Option<RoutineView> {
    reqwest::Client::new()
        .post(format!("{}/api/routines/{}/run", crate::BFF_URL, id))
        .send()
        .await
        .ok()?
        .json::<RoutineView>()
        .await
        .ok()
}

async fn create_routine(
    name: &str,
    schedule: &str,
    intent: &str,
    prompt: &str,
    scope: &str,
    project_id: Option<&str>,
    model: &str,
) -> Option<RoutineView> {
    reqwest::Client::new()
        .post(format!("{}/api/routines", crate::BFF_URL))
        .json(&serde_json::json!({
            "name": name, "schedule": schedule, "intent": intent, "prompt": prompt,
            "scope": scope, "project_id": project_id, "model": model
        }))
        .send()
        .await
        .ok()?
        .json::<RoutineView>()
        .await
        .ok()
}

#[allow(clippy::too_many_arguments)] // a flat routine payload reads clearer than a struct here
async fn update_routine(
    id: &str,
    name: &str,
    schedule: &str,
    intent: &str,
    prompt: &str,
    scope: &str,
    project_id: Option<&str>,
    model: &str,
) -> Option<RoutineView> {
    reqwest::Client::new()
        .put(format!("{}/api/routines/{}", crate::BFF_URL, id))
        .json(&serde_json::json!({
            "name": name, "schedule": schedule, "intent": intent, "prompt": prompt,
            "scope": scope, "project_id": project_id, "model": model
        }))
        .send()
        .await
        .ok()?
        .json::<RoutineView>()
        .await
        .ok()
}

async fn delete_routine(id: &str) -> bool {
    reqwest::Client::new()
        .delete(format!("{}/api/routines/{}", crate::BFF_URL, id))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Draft the operational prompt from the user's intent. Returns (prompt, authored_by).
async fn draft_prompt(intent: &str, scope: &str, model: &str) -> Option<(String, String)> {
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/api/routines/draft-prompt", crate::BFF_URL))
        .json(&serde_json::json!({ "intent": intent, "scope": scope, "model": model }))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let prompt = v.get("prompt")?.as_str()?.to_string();
    let authored_by = v
        .get("authored_by")
        .and_then(|a| a.as_str())
        .unwrap_or("scaffold")
        .to_string();
    Some((prompt, authored_by))
}

#[component]
pub fn RoutineDashboard() -> Element {
    let mut refresh = use_signal(|| 0u32);
    let routines_res = use_resource(move || {
        let _dep = refresh();
        async move { fetch_routines().await }
    });
    // Open escalations are fetched on the same `refresh` tick so blocked
    // badges appear / clear in lockstep with routine state changes.
    let escalations_res = use_resource(move || {
        let _dep = refresh();
        async move { fetch_open_escalations().await }
    });
    // Projects, for the form's project picker and the grouped table.
    let projects_res = use_resource(move || {
        let _dep = refresh();
        async move { fetch_projects().await }
    });
    // Model catalog, for the form's model picker (every AI-agent surface lets the user
    // pick the model). Fetched once; doesn't depend on refresh.
    let models_res = use_resource(fetch_models);
    // Routine templates (preset configurations). Fetched once; doesn't depend on refresh.
    let templates_res = use_resource(fetch_routine_templates);
    // Whether the template picker is shown (hidden by default).
    let mut showing_templates = use_signal(|| false);
    // Which escalation's review panel is currently expanded (by escalation id).
    let mut reviewing = use_signal(|| Option::<String>::None);
    // Per-escalation answer text. Keyed by escalation id; we store a flat
    // signal and use the reviewing id to associate it. For simplicity a single
    // signal covers the one expanded panel at a time.
    let mut answer_draft = use_signal(String::new);
    // After a successful submit the server returns a translated_directive. We
    // show it briefly before the panel closes on refresh.
    let mut last_directive = use_signal(|| Option::<String>::None);
    // The message being composed to the lead-engineer review agent, an in-flight flag,
    // and the model that agent answers on (seeded from the server default below).
    let mut chat_input = use_signal(String::new);
    let mut chatting = use_signal(|| false);
    let mut esc_model = use_signal(String::new);

    let mut name = use_signal(String::new);
    // Structured schedule builder. These drive a typical frequency picker (one-off /
    // daily / weekly / monthly) and serialize to the `schedule` string on save —
    // the BFF stores a human-readable schedule, so the UI owns the shape.
    let mut freq = use_signal(|| "daily".to_string());
    let mut sched_time = use_signal(|| "09:00".to_string());
    let mut sched_date = use_signal(String::new);
    // One toggle per weekday, Sun..Sat; Mon on by default.
    let mut weekdays = use_signal(|| vec![false, true, false, false, false, false, false]);
    let mut monthday = use_signal(|| 1u32);
    // The user writes INTENT; the AI drafts the operational PROMPT for review.
    let mut intent = use_signal(String::new);
    let mut prompt = use_signal(String::new);
    let mut authored_by = use_signal(String::new);
    let mut drafting = use_signal(|| false);
    let mut scope = use_signal(|| "read-only".to_string());
    // The project the form will assign the routine to (None = global).
    let mut routine_project = use_signal(|| Option::<String>::None);
    // The model the form will run the routine on; seeded from the server default once the
    // catalog loads (see below).
    let mut routine_model = use_signal(String::new);
    // When Some(id), the form is EDITING that routine (Save updates it) rather than
    // creating a new one. `pending_delete` holds the id awaiting a confirm click.
    let mut editing = use_signal(|| Option::<String>::None);
    let mut pending_delete = use_signal(|| Option::<String>::None);

    // Distinguish "still fetching" (outer None) from "resolved, but there are
    // genuinely none" — so an empty list shows its own state, not a stuck "Loading…".
    let loading = routines_res.read().is_none();
    let routines = routines_res.read().clone().flatten().unwrap_or_default();
    // Open escalations: keyed by routine_id for O(1) lookup when rendering rows.
    let escalations: Vec<EscalationView> =
        escalations_res.read().clone().flatten().unwrap_or_default();
    let projects: Vec<ProjectView> = projects_res.read().clone().flatten().unwrap_or_default();
    let models_resp = models_res.read().clone().flatten();
    let model_default = models_resp
        .as_ref()
        .map(|m| m.default.clone())
        .unwrap_or_default();
    // Seed the form's model from the server default once the catalog loads (only when the
    // form hasn't been given a model yet, e.g. fresh or just reset).
    if routine_model().is_empty() && !model_default.is_empty() {
        routine_model.set(model_default.clone());
    }
    // Seed the escalation review agent's model from the same default.
    if esc_model().is_empty() && !model_default.is_empty() {
        esc_model.set(model_default.clone());
    }

    // Load routine templates (preset configurations).
    let templates: Vec<RoutineTemplate> = templates_res
        .read()
        .clone()
        .flatten()
        .unwrap_or_default();

    // Group routines by project for display: each row carries an optional header that is
    // shown on the FIRST routine of each group. Routines run globally regardless of
    // project; this is purely organization. Order: by project name, with a "Global" group
    // (no/unknown project) last. Built here so the render loop stays a flat pass.
    let project_name = |id: &str| projects.iter().find(|p| p.id == id).map(|p| p.name.clone());
    // (group_key, group_label) for a routine; unknown/None project -> the Global group.
    let group_of = |r: &RoutineView| -> (String, String) {
        match r
            .project_id
            .as_deref()
            .and_then(|id| project_name(id).map(|n| (id.to_string(), n)))
        {
            Some((id, name)) => (id, name),
            None => ("\u{7f}global".to_string(), "Global".to_string()),
        }
    };
    let mut sorted: Vec<RoutineView> = routines.clone();
    // "\u{7f}global" sorts after real project names (DEL is a high code point), so the
    // Global group lands last; ties break by routine name for stable order.
    sorted.sort_by(|a, b| {
        let (ka, _) = group_of(a);
        let (kb, _) = group_of(b);
        ka.cmp(&kb).then_with(|| a.name.cmp(&b.name))
    });
    let mut rows: Vec<(Option<String>, RoutineView)> = Vec::with_capacity(sorted.len());
    let mut last_key: Option<String> = None;
    for r in sorted {
        let (key, label) = group_of(&r);
        let header = (last_key.as_deref() != Some(key.as_str())).then_some(label);
        last_key = Some(key);
        rows.push((header, r));
    }

    rsx! {
        div { class: "page page-wide routines-page",
            p { class: "eyebrow", "Automation" }
            h1 { class: "h1", "Routines" }
            p { class: "lede", "Scheduled governed runs. Each runs through the same gate as an interactive run; run one now to see its real verdicts summarized." }

            div { class: "routine-table",
                div { class: "routine-row routine-head",
                    span { "Routine" }
                    span { "Schedule" }
                    span { "Scope" }
                    span { "Last run" }
                    span { "" }
                }
                if loading {
                    p { class: "section-hint", "Loading…" }
                } else if routines.is_empty() {
                    p { class: "routine-empty", "No routines yet. Add one below to schedule a governed run." }
                }
                for (group_header, r) in rows.iter() {
                    {
                        let id_toggle = r.id.clone();
                        let id_provision = r.id.clone();
                        let id_run = r.id.clone();
                        let id_del = r.id.clone();
                        let r_edit = r.clone();
                        let enabled = r.enabled;
                        let provisioned = r.provisioned;
                        let last = r.last_run.clone();
                        let is_pending_delete = pending_delete().as_deref() == Some(r.id.as_str());
                        let is_editing_row = editing().as_deref() == Some(r.id.as_str());
                        // Find the open escalation for this specific routine, if any.
                        let open_esc: Option<EscalationView> = escalations
                            .iter()
                            .find(|e| e.routine_id == r.id && e.status == "open")
                            .cloned();
                        let is_blocked = open_esc.is_some();
                        let is_reviewing_row = open_esc
                            .as_ref()
                            .map(|e| reviewing().as_deref() == Some(e.id.as_str()))
                            .unwrap_or(false);
                        let row_cls = match (enabled, is_editing_row) {
                            (_, true) => "routine-row editing",
                            (true, _) => "routine-row",
                            (false, _) => "routine-row off",
                        };
                        rsx! {
                            // A project header on the first routine of each group. Routines
                            // run globally; this grouping is organization only.
                            if let Some(h) = group_header {
                                div { class: "routine-group-head",
                                    span { class: "routine-group-name", "{h}" }
                                }
                            }
                            // Row + optional review panel wrapped in a fragment so the
                            // panel can sit outside the grid as a full-width sibling.
                            div { class: "{row_cls}",
                                div { class: "routine-name",
                                    div { class: "routine-title-row",
                                        span { class: "routine-title", "{r.name}" }
                                        // Lifecycle status badge (issue #43). When the
                                        // routine is blocked the dedicated review pill below
                                        // is the actionable affordance, so suppress the
                                        // duplicate "Blocked" badge here.
                                        if !is_blocked {
                                            {
                                                let (label, modifier) = status_badge(&r.status);
                                                rsx! {
                                                    span { class: "routine-status-badge {modifier}", "{label}" }
                                                }
                                            }
                                        }
                                    }
                                    span { class: "routine-prompt", "{r.intent}" }
                                    // Blocked pill: clicking toggles the inline review panel.
                                    if is_blocked {
                                        {
                                            let esc_id = open_esc.as_ref().map(|e| e.id.clone()).unwrap_or_default();
                                            let esc_id_click = esc_id.clone();
                                            rsx! {
                                                button {
                                                    class: "routine-blocked",
                                                    onclick: move |_| {
                                                        // Toggle the panel for this escalation.
                                                        if reviewing().as_deref() == Some(esc_id_click.as_str()) {
                                                            reviewing.set(None);
                                                        } else {
                                                            reviewing.set(Some(esc_id_click.clone()));
                                                            answer_draft.set(String::new());
                                                            chat_input.set(String::new());
                                                            last_directive.set(None);
                                                        }
                                                    },
                                                    "blocked - needs review"
                                                }
                                            }
                                        }
                                    }
                                }
                                span { class: "routine-sched",
                                    "{r.schedule}"
                                    if !provisioned {
                                        span { class: "routine-needs-setup", "needs setup" }
                                    }
                                }
                                span { class: "routine-scope", "{r.scope}" }
                                span { class: "routine-last",
                                    {
                                        match last {
                                            Some(s) => rsx! {
                                                span { class: "routine-passed", "{s.outcome} · {s.denies} denied, {s.allows} allowed" }
                                            },
                                            None => rsx! { span { class: "routine-never", "not run yet" } },
                                        }
                                    }
                                }
                                div { class: "routine-actions",
                                    if provisioned {
                                        // Start / Stop arms or disarms the scheduler for this routine.
                                        button {
                                            class: "btn-restart",
                                            onclick: move |_| {
                                                let id = id_toggle.clone();
                                                spawn(async move {
                                                    if set_enabled(&id, !enabled).await.is_some() {
                                                        refresh += 1;
                                                    }
                                                });
                                            },
                                            if enabled { "Stop" } else { "Start" }
                                        }
                                    } else {
                                        // Imported routine: must be set up on this backend before it can run.
                                        button {
                                            class: "btn-restart btn-setup",
                                            onclick: move |_| {
                                                let id = id_provision.clone();
                                                spawn(async move {
                                                    if provision(&id).await.is_some() {
                                                        refresh += 1;
                                                    }
                                                });
                                            },
                                            "Set up"
                                        }
                                    }
                                    button {
                                        class: "btn-run-sm",
                                        onclick: move |_| {
                                            let id = id_run.clone();
                                            spawn(async move {
                                                if run_now(&id).await.is_some() {
                                                    refresh += 1;
                                                }
                                            });
                                        },
                                        "Run now"
                                    }
                                    button {
                                        class: "btn-edit-sm",
                                        onclick: move |_| {
                                            // Prefill the form with this routine and switch it to edit mode.
                                            let rt = r_edit.clone();
                                            let (f, t, d, wd, md) = parse_schedule(&rt.schedule);
                                            name.set(rt.name.clone());
                                            freq.set(f);
                                            sched_time.set(t);
                                            sched_date.set(d);
                                            weekdays.set(wd);
                                            monthday.set(md);
                                            intent.set(rt.intent.clone());
                                            prompt.set(rt.prompt.clone());
                                            scope.set(rt.scope.clone());
                                            routine_project.set(rt.project_id.clone());
                                            // Prefill the model; an older routine with none
                                            // recorded leaves it blank and the seeding
                                            // effect refills the default on next render.
                                            routine_model.set(rt.model.clone());
                                            authored_by.set(String::new());
                                            editing.set(Some(rt.id.clone()));
                                            pending_delete.set(None);
                                        },
                                        "Edit"
                                    }
                                    button {
                                        class: if is_pending_delete { "btn-delete-sm confirm" } else { "btn-delete-sm" },
                                        onclick: move |_| {
                                            let id = id_del.clone();
                                            if pending_delete().as_deref() == Some(id.as_str()) {
                                                // Second click — actually delete.
                                                pending_delete.set(None);
                                                spawn(async move {
                                                    if delete_routine(&id).await {
                                                        refresh += 1;
                                                    }
                                                });
                                            } else {
                                                // First click — arm the confirm.
                                                pending_delete.set(Some(id));
                                            }
                                        },
                                        if is_pending_delete { "Confirm?" } else { "Delete" }
                                    }
                                }
                            }
                            // Inline review panel: expands below the row when the
                            // architect clicks "blocked - needs review". Sits outside the
                            // grid row so it can span the full table width.
                            if let Some(esc) = open_esc.clone().filter(|_| is_reviewing_row) {
                                {
                                    let esc_id_submit = esc.id.clone();
                                    let esc_id_close = esc.id.clone();
                                    let esc_id_chat = esc.id.clone();
                                    rsx! {
                                        div { class: "escalation-panel",
                                            div { class: "escalation-panel-head",
                                                span { class: "escalation-panel-name", "{esc.routine_name}" }
                                                span { class: "escalation-panel-id", "{esc.id}" }
                                            }
                                            // Why the routine stopped.
                                            p { class: "escalation-reason", "{esc.reason}" }
                                            // The specific decision needed: most prominent field.
                                            div { class: "escalation-stopped-for", "{esc.stopped_for}" }

                                            // ── Conversation with the lead engineer ──────────────
                                            // A real back-and-forth: ask why, get clarification.
                                            // Chatting NEVER unblocks; only Authorize (below) does.
                                            if !esc.conversation.is_empty() {
                                                div { class: "escalation-chat-thread",
                                                    for m in esc.conversation.iter() {
                                                        {
                                                            let is_ai = m.role == "assistant";
                                                            let cls = if is_ai { "escalation-turn ai" } else { "escalation-turn you" };
                                                            rsx! {
                                                                div { class: "{cls}",
                                                                    span { class: "escalation-turn-role",
                                                                        if is_ai { "Lead engineer" } else { "You" }
                                                                    }
                                                                    if is_ai {
                                                                        div { class: "escalation-turn-text md", dangerous_inner_html: md_to_html(&m.text) }
                                                                    } else {
                                                                        div { class: "escalation-turn-text", "{m.text}" }
                                                                    }
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }

                                            // Ask the lead engineer (with a model picker for this agent).
                                            div { class: "escalation-chat-row",
                                                div { class: "escalation-chat-controls",
                                                    span { class: "escalation-chat-label", "Ask the lead engineer" }
                                                    select {
                                                        class: "addressee-input escalation-model",
                                                        value: "{esc_model}",
                                                        onchange: move |e| esc_model.set(e.value()),
                                                        for (group_label , opts) in models_resp.as_ref().map(|m| m.grouped()).unwrap_or_default().into_iter() {
                                                            optgroup { label: "{group_label}",
                                                                for mo in opts.into_iter() {
                                                                    option { key: "{mo.id}", value: "{mo.id}", "{mo.label}" }
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                                textarea {
                                                    class: "escalation-answer-input",
                                                    rows: "2",
                                                    placeholder: "Ask why it stopped, or for clarification (this does NOT unblock)...",
                                                    value: "{chat_input}",
                                                    oninput: move |e| chat_input.set(e.value()),
                                                }
                                                button {
                                                    class: "btn-restart",
                                                    disabled: chat_input().trim().is_empty() || chatting(),
                                                    onclick: move |_| {
                                                        let id = esc_id_chat.clone();
                                                        let msg = chat_input();
                                                        let md = esc_model();
                                                        if msg.trim().is_empty() { return; }
                                                        chatting.set(true);
                                                        spawn(async move {
                                                            if chat_escalation(&id, &msg, &md).await.is_some() {
                                                                chat_input.set(String::new());
                                                                refresh += 1;
                                                            }
                                                            chatting.set(false);
                                                        });
                                                    },
                                                    if chatting() { "Asking…" } else { "Ask" }
                                                }
                                            }

                                            // ── Authorize & unblock — the ONLY thing that resolves it ──
                                            div { class: "escalation-authorize",
                                                p { class: "escalation-suggestions-label", "Authorize a decision to unblock" }
                                                if !esc.suggestions.is_empty() {
                                                    div { class: "escalation-suggestions",
                                                        for suggestion in esc.suggestions.iter() {
                                                            {
                                                                let text = suggestion.clone();
                                                                rsx! {
                                                                    button {
                                                                        class: "escalation-suggestion",
                                                                        onclick: move |_| { answer_draft.set(text.clone()); },
                                                                        "{suggestion}"
                                                                    }
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                                textarea {
                                                    class: "escalation-answer-input",
                                                    rows: "2",
                                                    placeholder: "Your decision (e.g. \"go ahead with option B\") — this unblocks the routine...",
                                                    value: "{answer_draft}",
                                                    oninput: move |e| answer_draft.set(e.value()),
                                                }
                                                div { class: "escalation-submit-row",
                                                    button {
                                                        class: "btn-run",
                                                        disabled: answer_draft().trim().is_empty(),
                                                        onclick: move |_| {
                                                            let id = esc_id_submit.clone();
                                                            let text = answer_draft();
                                                            if text.trim().is_empty() { return; }
                                                            spawn(async move {
                                                                if let Some(resolved) = answer_escalation(&id, &text).await {
                                                                    if let Some(directive) = resolved.translated_directive {
                                                                        last_directive.set(Some(directive));
                                                                    }
                                                                    reviewing.set(None);
                                                                    answer_draft.set(String::new());
                                                                    chat_input.set(String::new());
                                                                    refresh += 1;
                                                                }
                                                            });
                                                        },
                                                        "Authorize & unblock"
                                                    }
                                                    button {
                                                        class: "btn-restart",
                                                        onclick: move |_| {
                                                            // Dismiss the panel without authorizing.
                                                            if reviewing().as_deref() == Some(esc_id_close.as_str()) {
                                                                reviewing.set(None);
                                                                answer_draft.set(String::new());
                                                                chat_input.set(String::new());
                                                                last_directive.set(None);
                                                            }
                                                        },
                                                        "Dismiss"
                                                    }
                                                }
                                                if let Some(directive) = last_directive() {
                                                    p { class: "escalation-directive", "Directive: {directive}" }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            div { class: "routine-create",
                p { class: "section-label",
                    if editing().is_some() { "Edit routine" } else { "Add a routine" }
                }
                p { class: "section-hint", "Describe what you want the routine to do. Camerata's lead engineer drafts the operational prompt (model tiering, directives, scope) from it — you review and edit before it runs." }
                // Template picker (feature #59): expand when user clicks to browse presets.
                if !templates.is_empty() && editing().is_none() {
                    div { class: "routine-template-picker",
                        button {
                            class: "btn-restart",
                            onclick: move |_| showing_templates.set(!showing_templates()),
                            if showing_templates() { "Hide template gallery" } else { "Start from a template" }
                        }
                        if showing_templates() {
                            div { class: "routine-templates-list",
                                for tmpl in templates.iter() {
                                    {
                                        let tmpl_id = tmpl.id.clone();
                                        let tmpl_name = tmpl.name.clone();
                                        let tmpl_desc = tmpl.description.clone();
                                        rsx! {
                                            div { class: "routine-template-card",
                                                div { class: "template-title", "{tmpl_name}" }
                                                p { class: "template-description", "{tmpl_desc}" }
                                                button {
                                                    class: "btn-edit-sm",
                                                    onclick: move |_| {
                                                        let id = tmpl_id.clone();
                                                        spawn(async move {
                                                            if let Some(rt) = instantiate_from_template(&id).await {
                                                                name.set(rt.name.clone());
                                                                intent.set(rt.intent.clone());
                                                                prompt.set(rt.prompt.clone());
                                                                scope.set(rt.scope.clone());
                                                                routine_model.set(rt.model.clone());
                                                                routine_project.set(rt.project_id.clone());
                                                                authored_by.set(String::new());
                                                                // Parse schedule into structured form
                                                                let (f, t, d, wd, md) = parse_schedule(&rt.schedule);
                                                                freq.set(f);
                                                                sched_time.set(t);
                                                                sched_date.set(d);
                                                                weekdays.set(wd);
                                                                monthday.set(md);
                                                                showing_templates.set(false);
                                                            }
                                                        });
                                                    },
                                                    "Use this template"
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                div { class: "routine-create-row",
                    input { class: "addressee-input", placeholder: "name", value: "{name}", oninput: move |e| name.set(e.value()) }
                    label { class: "sched-field sched-scope-field",
                        span { "Permissions" }
                        select {
                            class: "addressee-input",
                            value: "{scope}",
                            onchange: move |e| scope.set(e.value()),
                            option { value: "read-only", "Read-only — inspect & report, no file changes" }
                            option { value: "write (gated)", "Write — gated edits on a branch, no push" }
                            option { value: "write + open PR", "Write + open PR — gated edits, pushed for review" }
                        }
                    }
                    label { class: "sched-field sched-scope-field",
                        span { "Project" }
                        select {
                            class: "addressee-input",
                            value: "{routine_project().unwrap_or_default()}",
                            onchange: move |e| {
                                let v = e.value();
                                routine_project.set(if v.is_empty() { None } else { Some(v) });
                            },
                            option { value: "", "Global (no project)" }
                            for p in projects.iter() {
                                option { key: "{p.id}", value: "{p.id}", "{p.name}" }
                            }
                        }
                    }
                    label { class: "sched-field sched-scope-field",
                        span { "Model" }
                        select {
                            class: "addressee-input",
                            value: "{routine_model}",
                            onchange: move |e| routine_model.set(e.value()),
                            for (group_label , opts) in models_resp.as_ref().map(|m| m.grouped()).unwrap_or_default().into_iter() {
                                optgroup { label: "{group_label}",
                                    for m in opts.into_iter() {
                                        option { key: "{m.id}", value: "{m.id}", "{m.label}" }
                                    }
                                }
                            }
                        }
                    }
                }
                p { class: "section-hint sched-scope-hint",
                    "Permissions cap what the unattended run may do. "
                    b { "Read-only" }
                    " can analyze the repo but writes nothing. "
                    b { "Write" }
                    " lets it edit files on a working branch (every write still passes the governance gate) without pushing. "
                    b { "Write + open PR" }
                    " also pushes that branch and opens a pull request for your review. Nothing auto-merges."
                }
                // Structured schedule picker — frequency, then the controls that
                // frequency needs (weekday toggles / day-of-month / one-off date),
                // plus a time. Serialized to the schedule string on save.
                div { class: "sched-picker",
                    div { class: "sched-freq",
                        {
                            let opts = [("once", "One-off"), ("daily", "Daily"), ("weekly", "Weekly"), ("monthly", "Monthly")];
                            rsx! {
                                for (val, label) in opts.iter() {
                                    {
                                        let v = val.to_string();
                                        let on = freq() == *val;
                                        let cls = if on { "sched-freq-btn on" } else { "sched-freq-btn" };
                                        rsx! {
                                            button {
                                                key: "{val}",
                                                class: "{cls}",
                                                onclick: move |_| freq.set(v.clone()),
                                                "{label}"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    div { class: "sched-detail",
                        // Weekly: per-day toggles.
                        if freq() == "weekly" {
                            div { class: "sched-dow",
                                for i in 0..7usize {
                                    {
                                        let on = weekdays().get(i).copied().unwrap_or(false);
                                        let cls = if on { "sched-dow-btn on" } else { "sched-dow-btn" };
                                        rsx! {
                                            button {
                                                key: "{i}",
                                                class: "{cls}",
                                                onclick: move |_| {
                                                    let mut w = weekdays();
                                                    if i < w.len() { w[i] = !w[i]; }
                                                    weekdays.set(w);
                                                },
                                                "{WEEKDAYS[i]}"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        // Monthly: day-of-month.
                        if freq() == "monthly" {
                            label { class: "sched-field",
                                span { "Day of month" }
                                input {
                                    class: "addressee-input sched-num",
                                    r#type: "number", min: "1", max: "31",
                                    value: "{monthday}",
                                    oninput: move |e| {
                                        if let Ok(n) = e.value().parse::<u32>() {
                                            monthday.set(n.clamp(1, 31));
                                        }
                                    },
                                }
                            }
                        }
                        // One-off: a calendar date.
                        if freq() == "once" {
                            label { class: "sched-field",
                                span { "Date" }
                                input {
                                    class: "addressee-input",
                                    r#type: "date",
                                    value: "{sched_date}",
                                    oninput: move |e| sched_date.set(e.value()),
                                }
                            }
                        }
                        // Time applies to every frequency.
                        label { class: "sched-field",
                            span { "Time" }
                            input {
                                class: "addressee-input",
                                r#type: "time",
                                value: "{sched_time}",
                                oninput: move |e| sched_time.set(e.value()),
                            }
                        }
                    }
                    p { class: "sched-preview",
                        "Schedule: "
                        span { class: "sched-preview-val", "{build_schedule(&freq(), &sched_time(), &sched_date(), &weekdays(), monthday())}" }
                    }
                }
                // INTENT: what the user wants (their words).
                textarea {
                    class: "routine-intent-input",
                    rows: "2",
                    placeholder: "Describe what you want this routine to do (e.g. \"nightly, scan deps for advisories and open governed PRs for safe upgrades\")",
                    value: "{intent}",
                    oninput: move |e| intent.set(e.value()),
                }
                // DRAFT the operational prompt from the intent.
                div { class: "routine-draft-row",
                    button {
                        class: "btn-restart",
                        disabled: intent().trim().is_empty() || drafting(),
                        onclick: move |_| {
                            let (i, sc, md) = (intent(), scope(), routine_model());
                            if i.trim().is_empty() { return; }
                            drafting.set(true);
                            spawn(async move {
                                if let Some((p, by)) = draft_prompt(&i, &sc, &md).await {
                                    prompt.set(p);
                                    authored_by.set(by);
                                }
                                drafting.set(false);
                            });
                        },
                        if drafting() { "Drafting…" } else { "Draft operational prompt" }
                    }
                    if !authored_by().is_empty() {
                        span { class: "routine-authored",
                            {
                                if authored_by() == "claude" {
                                    "authored by the lead engineer — review & edit below"
                                } else {
                                    "draft scaffold (connect Claude for a fully-authored prompt) — review & edit below"
                                }
                            }
                        }
                    }
                }
                // REVIEW the operational prompt (editable).
                textarea {
                    class: "routine-prompt-input",
                    rows: "7",
                    placeholder: "The operational prompt the agent will run (draft it above, then review/edit). Leave empty to scaffold from your description on save.",
                    value: "{prompt}",
                    oninput: move |e| prompt.set(e.value()),
                }
                div { class: "routine-save-row",
                    button {
                        class: "btn-run",
                        onclick: move |_| {
                            let s = build_schedule(&freq(), &sched_time(), &sched_date(), &weekdays(), monthday());
                            let (n, i, p, sc) = (name(), intent(), prompt(), scope());
                            if n.is_empty() || i.trim().is_empty() {
                                return;
                            }
                            let edit_id = editing();
                            let pid = routine_project();
                            let md = routine_model();
                            spawn(async move {
                                let pid = pid.as_deref();
                                let ok = match &edit_id {
                                    Some(id) => update_routine(id, &n, &s, &i, &p, &sc, pid, &md).await.is_some(),
                                    None => create_routine(&n, &s, &i, &p, &sc, pid, &md).await.is_some(),
                                };
                                if ok {
                                    refresh += 1;
                                }
                            });
                            // Reset the form back to a fresh "create" state.
                            name.set(String::new());
                            intent.set(String::new());
                            prompt.set(String::new());
                            authored_by.set(String::new());
                            freq.set("daily".to_string());
                            sched_time.set("09:00".to_string());
                            sched_date.set(String::new());
                            weekdays.set(vec![false, true, false, false, false, false, false]);
                            monthday.set(1);
                            scope.set("read-only".to_string());
                            routine_project.set(None);
                            // Clear the model; the seeding effect refills it with the
                            // server default on the next render.
                            routine_model.set(String::new());
                            editing.set(None);
                        },
                        if editing().is_some() { "Save changes" } else { "Add routine" }
                    }
                    if editing().is_some() {
                        button {
                            class: "btn-restart",
                            onclick: move |_| {
                                // Cancel edit: clear the form and drop edit mode.
                                name.set(String::new());
                                intent.set(String::new());
                                prompt.set(String::new());
                                authored_by.set(String::new());
                                freq.set("daily".to_string());
                                sched_time.set("09:00".to_string());
                                sched_date.set(String::new());
                                weekdays.set(vec![false, true, false, false, false, false, false]);
                                monthday.set(1);
                                scope.set("read-only".to_string());
                                editing.set(None);
                            },
                            "Cancel"
                        }
                    }
                }
            }
        }
    }
}
