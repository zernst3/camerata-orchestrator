//! The enterprise / architect surface: the single-pane cockpit.
//!
//! Where the consumer app-builder surface is a calm, guided, one-decision-per-screen
//! wizard (the human is led), the cockpit is a dense control surface (the human
//! steers a fleet). It is the UI realization of `docs/UI_DESIGN.md` section 2: three
//! panes on one screen, nothing opens a separate window.
//!
//! - LEFT: the story spine (every story + its lifecycle status) and a NEEDS YOU queue.
//! - CENTER: a stage that swaps by the selected story's status, with a live status
//!   strip showing the governed fleet and the gate activity.
//! - RIGHT: an inspector that binds to the selection (the gate's enforced rules).
//!
//! Wiring: the spine and the inspector rules are fetched from the BFF
//! (`camerata-server`) over HTTP (`/api/stories`, `/api/rules`), not read in-process,
//! the same client/server split that makes the server cloud-hostable. The fleet and
//! gate-activity panels are still representative; live execution + a status stream are
//! the next phase (the same path `worktracker-demo` / `po-demo` exercise).

use dioxus::prelude::*;

use camerata_worktracker::{CanonicalStory, FeatureStatus};

// Chorale (crates.io, headless table) backs the brownfield audit-findings and
// proposed-rules tables — the surfaces where the data genuinely scales.
use chorale_core::{
    Alignment, BadgeVariant, BadgeVariantMap, CellValue, ColumnDef, ColumnId, FilterKind,
    RenderKind, RowId, TableState,
};
use chorale_dioxus::{use_table, CellRenderer, CellRenderers, Table};

/// One enforced gate rule, as returned by the BFF `/api/rules` endpoint (GOV-1 is
/// filtered out server-side). The cockpit just renders what the BFF returns.
#[derive(Clone, PartialEq, serde::Deserialize)]
struct CockpitRule {
    id: String,
    statement: String,
}

/// Fetch the canonical story spine from the BFF.
async fn fetch_stories() -> Option<Vec<CanonicalStory>> {
    reqwest::get(format!("{}/api/stories", crate::BFF_URL))
        .await
        .ok()?
        .json::<Vec<CanonicalStory>>()
        .await
        .ok()
}

/// Fetch the gate's enforced rules from the BFF.
async fn fetch_rules() -> Option<Vec<CockpitRule>> {
    reqwest::get(format!("{}/api/rules", crate::BFF_URL))
        .await
        .ok()?
        .json::<Vec<CockpitRule>>()
        .await
        .ok()
}

// ── Projects ───────────────────────────────────────────────────────────────────

#[derive(Clone, PartialEq, serde::Deserialize)]
struct RuleSelectionView {
    rule_id: String,
    #[serde(default)]
    chosen_option: Option<String>,
    #[serde(default)]
    repos: Vec<String>,
}

#[derive(Clone, PartialEq, serde::Deserialize)]
struct CustomRuleView {
    name: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    domain: String,
}

#[derive(Clone, PartialEq, serde::Deserialize, Default)]
struct RulesetView {
    #[serde(default)]
    selections: Vec<RuleSelectionView>,
    #[serde(default)]
    cross_repo: Vec<RuleSelectionView>,
    #[serde(default)]
    process: Vec<RuleSelectionView>,
    #[serde(default)]
    custom: Vec<CustomRuleView>,
}

#[derive(Clone, PartialEq, serde::Deserialize)]
struct ProjectView {
    id: String,
    name: String,
    #[serde(default)]
    repos: Vec<String>,
    #[serde(default)]
    ruleset: RulesetView,
}

async fn fetch_projects() -> Option<Vec<ProjectView>> {
    reqwest::get(format!("{}/api/projects", crate::BFF_URL))
        .await
        .ok()?
        .json::<Vec<ProjectView>>()
        .await
        .ok()
}

async fn fetch_active_project() -> Option<ProjectView> {
    reqwest::get(format!("{}/api/projects/active", crate::BFF_URL))
        .await
        .ok()?
        .json::<Option<ProjectView>>()
        .await
        .ok()
        .flatten()
}

async fn create_project(name: &str, repos: Vec<String>) -> Option<ProjectView> {
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/api/projects", crate::BFF_URL))
        .json(&serde_json::json!({ "name": name, "repos": repos }))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    serde_json::from_value(v.get("project")?.clone()).ok()
}

async fn set_active_project(id: &str) -> bool {
    reqwest::Client::new()
        .post(format!("{}/api/projects/active", crate::BFF_URL))
        .json(&serde_json::json!({ "id": id }))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

#[derive(Clone, PartialEq, serde::Deserialize)]
struct AppliedOptionView {
    id: String,
    label: String,
    #[serde(default)]
    directive: String,
}

#[derive(Clone, PartialEq, serde::Deserialize)]
struct AppliedRuleView {
    id: String,
    repo: String,
    title: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    chosen_option: Option<String>,
    #[serde(default)]
    chosen_label: Option<String>,
    #[serde(default)]
    options: Vec<AppliedOptionView>,
    #[serde(default)]
    is_custom: bool,
    #[serde(default)]
    in_corpus: bool,
}

/// Reconcile the project's repos with the rule-bank: what's actually applied,
/// rehydrated with the full source rule (alternatives + context).
async fn fetch_reconcile(project_id: &str) -> Option<Vec<AppliedRuleView>> {
    let v: serde_json::Value = reqwest::get(format!(
        "{}/api/projects/{}/reconcile",
        crate::BFF_URL,
        project_id
    ))
    .await
    .ok()?
    .json()
    .await
    .ok()?;
    if !v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) {
        return None;
    }
    serde_json::from_value(v.get("applied").cloned()?).ok()
}

/// Re-emit the project's ruleset (source of truth) into its repos — one PR per repo.
async fn emit_project_rules(project_id: &str) -> Option<Vec<ArmResultView>> {
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/api/projects/{}/emit", crate::BFF_URL, project_id))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    if !v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) {
        return None;
    }
    serde_json::from_value(v.get("results").cloned()?).ok()
}

/// Add or edit (by name) a custom rule on a project.
async fn add_custom_rule(project_id: &str, name: &str, body: &str, domain: &str) -> bool {
    reqwest::Client::new()
        .post(format!("{}/api/projects/{}/custom", crate::BFF_URL, project_id))
        .json(&serde_json::json!({ "name": name, "body": body, "domain": domain }))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Delete a custom rule by name (the only way a custom rule leaves a project).
async fn delete_custom_rule(project_id: &str, name: &str) -> bool {
    reqwest::Client::new()
        .post(format!("{}/api/projects/{}/custom/delete", crate::BFF_URL, project_id))
        .json(&serde_json::json!({ "name": name }))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

fn custom_columns() -> Vec<ColumnDef<CustomRuleView>> {
    vec![
        ColumnDef::new(ColumnId("name"), "Name", |c: &CustomRuleView| {
            CellValue::Text(c.name.clone())
        })
        .sortable()
        .filter(FilterKind::Text)
        .initial_width(200.0),
        ColumnDef::new(ColumnId("domain"), "Domain", |c: &CustomRuleView| {
            CellValue::Text(if c.domain.is_empty() { "*".to_string() } else { c.domain.clone() })
        })
        .sortable()
        .initial_width(150.0),
        ColumnDef::new(ColumnId("body"), "Directive", |c: &CustomRuleView| {
            CellValue::Text(c.body.clone())
        })
        .initial_width(460.0),
    ]
}

/// The custom-rules editor: a chorale table of the project's custom rules grouped
/// by domain, with selection -> delete. The add/edit form lives in the parent.
#[component]
fn CustomRulesTable(custom: Vec<CustomRuleView>, project_id: String, refresh: Signal<u32>) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let rows: Vec<(RowId, CustomRuleView)> =
        custom.iter().map(|c| (RowId::new(), c.clone())).collect();
    let id_map: std::collections::HashMap<RowId, CustomRuleView> =
        rows.iter().map(|(r, c)| (*r, c.clone())).collect();
    let handle = use_table(move || TableState::new(rows.clone(), custom_columns()));
    // Group by domain so custom rules cluster by where they apply.
    use_hook(move || handle.set_grouping(vec![ColumnId("domain")]));

    rsx! {
        Table { handle, sort_enabled: true, selection_enabled: true }
        button {
            class: "btn-restart",
            onclick: move |_| {
                let sel = handle.selected_ids();
                let names: Vec<String> = sel.iter().filter_map(|id| id_map.get(id).map(|c| c.name.clone())).collect();
                if names.is_empty() { return; }
                let pid = project_id.clone();
                let mut refresh = refresh;
                spawn(async move {
                    let mut n = 0;
                    for name in &names {
                        if delete_custom_rule(&pid, name).await { n += 1; }
                    }
                    if n > 0 {
                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("Deleted {n} custom rule(s)."));
                        refresh += 1;
                    }
                });
            },
            "Delete selected custom rules"
        }
    }
}

/// Import a ruleset JSON (upsert base rules; the server preserves custom).
async fn import_ruleset(project_id: &str, json: String) -> bool {
    reqwest::Client::new()
        .post(format!("{}/api/projects/{}/ruleset", crate::BFF_URL, project_id))
        .header("content-type", "application/json")
        .body(json)
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// The project Rules-management screen: the active project's ruleset (repo-local,
/// cross-repo, process, custom) + export/import. The full per-rule editor + re-emit
/// is phased (see the ADR); this is the project-scoped management surface and the
/// home for the non-repo rules.
#[component]
fn RulesView() -> Element {
    let mut refresh = use_signal(|| 0u32);
    let active = use_resource(move || {
        let _ = refresh();
        async move { fetch_active_project().await }
    });
    let projects = use_resource(move || {
        let _ = refresh();
        async move { fetch_projects().await }
    });
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let mut new_name = use_signal(String::new);
    let mut import_text = use_signal(String::new);
    let mut applied = use_signal(|| Option::<Vec<AppliedRuleView>>::None);
    let mut reconciling = use_signal(|| false);
    let mut cr_name = use_signal(String::new);
    let mut cr_domain = use_signal(String::new);
    let mut cr_body = use_signal(String::new);
    let mut emitting = use_signal(|| false);

    let proj = active.read().clone().flatten();
    let proj_list = projects.read().clone().flatten().unwrap_or_default();

    rsx! {
        div { class: "page page-wide",
            p { class: "eyebrow", "Project" }
            h1 { class: "h1", "Rules" }
            p { class: "lede", "Manage the active project's ruleset: repo-local rules, the cross-repo (API contract) rules, the process (commit/PR) rules, and your custom rules. The cross-repo and process rules live at the project level (no repo owns them); the engine's gates read them here. Editing produces one emit that upserts the repo files and the project config — custom rules are preserved." }

            div { class: "proj-bar",
                span { class: "proj-label", "Project:" }
                if proj_list.is_empty() {
                    span { class: "proj-none", "none yet" }
                }
                for p in proj_list.iter() {
                    {
                        let id = p.id.clone();
                        let is_active = proj.as_ref().map(|a| a.id == p.id).unwrap_or(false);
                        let cls = if is_active { "proj-chip on" } else { "proj-chip" };
                        rsx! {
                            button {
                                class: "{cls}",
                                onclick: move |_| {
                                    let id = id.clone();
                                    spawn(async move { if set_active_project(&id).await { refresh += 1; } });
                                },
                                "{p.name}"
                            }
                        }
                    }
                }
                input {
                    class: "addressee-input",
                    placeholder: "new project name",
                    value: "{new_name}",
                    oninput: move |e| new_name.set(e.value()),
                }
                button {
                    class: "btn-restart",
                    onclick: move |_| {
                        let n = new_name();
                        if n.trim().is_empty() { return; }
                        spawn(async move { if create_project(&n, Vec::new()).await.is_some() { refresh += 1; } });
                        new_name.set(String::new());
                    },
                    "New project"
                }
            }

            match &proj {
                None => rsx! {
                    p { class: "section-hint", "Create a project, then onboard repos into it (the Onboard tab) to populate its ruleset." }
                },
                Some(p) => {
                    let export = serde_json::to_string_pretty(&serde_json::json!({
                        "selections": p.ruleset.selections.iter().map(|s| serde_json::json!({"rule_id": s.rule_id, "chosen_option": s.chosen_option, "repos": s.repos})).collect::<Vec<_>>(),
                        "cross_repo": p.ruleset.cross_repo.iter().map(|s| serde_json::json!({"rule_id": s.rule_id, "repos": s.repos})).collect::<Vec<_>>(),
                        "process": p.ruleset.process.iter().map(|s| serde_json::json!({"rule_id": s.rule_id, "repos": s.repos})).collect::<Vec<_>>(),
                        "custom": p.ruleset.custom.iter().map(|c| serde_json::json!({"name": c.name, "body": c.body, "domain": c.domain})).collect::<Vec<_>>(),
                    })).unwrap_or_default();
                    let pid = p.id.clone();
                    let pid_rec = p.id.clone();
                    let pid_emit = p.id.clone();
                    rsx! {
                        div { class: "rules-sections",
                            RuleCount { label: "Repo-local rules", n: p.ruleset.selections.len() }
                            RuleCount { label: "Cross-repo rules (API contracts)", n: p.ruleset.cross_repo.len() }
                            RuleCount { label: "Process rules (commit/PR)", n: p.ruleset.process.len() }
                            RuleCount { label: "Custom rules", n: p.ruleset.custom.len() }
                        }

                        // Re-emit: rebuild the source-of-truth emit from this project's
                        // ruleset (base selections + custom) and open a PR per repo.
                        div { class: "rules-emit",
                            button {
                                class: "btn-run",
                                disabled: emitting(),
                                onclick: move |_| {
                                    let id = pid_emit.clone();
                                    emitting.set(true);
                                    spawn(async move {
                                        match emit_project_rules(&id).await {
                                            Some(results) => {
                                                if results.is_empty() {
                                                    crate::toast::push_toast(toasts, crate::toast::ToastKind::Warning, "Nothing emitted (no repo-local or custom rules, or repos unreachable).");
                                                }
                                                for r in results {
                                                    if r.ok {
                                                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("{}: emitted \u{2192} {}", r.repo, r.url.unwrap_or_default()));
                                                    } else {
                                                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, format!("{}: {}", r.repo, r.message.unwrap_or_default()));
                                                    }
                                                }
                                            }
                                            None => crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, "Emit failed — needs GitHub Contents + PR write on the connected token."),
                                        }
                                        emitting.set(false);
                                    });
                                },
                                if emitting() { "Emitting…" } else { "Emit ruleset to repos (re-emit)" }
                            }
                            span { class: "rules-emit-hint", "Rebuilds each repo's AGENTS.md / CONVENTIONS.md / gate config from this project's ruleset. Custom rules are always carried through." }
                        }

                        // Reconcile: read what's ACTUALLY in the repos and rehydrate
                        // each rule's source (alternatives + context) from the rule-bank.
                        p { class: "section-label", "Applied in the repos (reconciled with the rule-bank)" }
                        p { class: "section-hint", "Reads each repo's emitted gate config (the ground truth of what's applied) and matches every rule id back to its source rule — so you see the alternatives and context, not just the adopted directive. Needs GitHub connected." }
                        button {
                            class: "btn-restart",
                            disabled: reconciling(),
                            onclick: move |_| {
                                let id = pid_rec.clone();
                                reconciling.set(true);
                                spawn(async move {
                                    applied.set(fetch_reconcile(&id).await);
                                    reconciling.set(false);
                                });
                            },
                            if reconciling() { "Reconciling…" } else { "Reconcile with repos" }
                        }
                        if let Some(rules) = applied() {
                            div { class: "applied-list",
                                if rules.is_empty() {
                                    p { class: "section-hint", "No rules found in the repos yet (none armed, or GitHub not connected)." }
                                }
                                for r in rules.iter() {
                                    div { class: "applied-rule",
                                        div { class: "applied-rule-head",
                                            span { class: "applied-rule-id", "{r.id}" }
                                            span { class: "applied-rule-repo", "{r.repo}" }
                                            if r.is_custom { span { class: "applied-tag custom", "custom" } }
                                            if !r.in_corpus && !r.is_custom { span { class: "applied-tag drift", "not in rule-bank" } }
                                        }
                                        if !r.title.is_empty() && r.title != r.id {
                                            p { class: "applied-rule-title", "{r.title}" }
                                        }
                                        if !r.summary.is_empty() {
                                            p { class: "applied-rule-summary", "{r.summary}" }
                                        }
                                        if !r.options.is_empty() {
                                            div { class: "applied-options",
                                                for o in r.options.iter() {
                                                    {
                                                        let is_chosen = r.chosen_option.as_deref() == Some(o.id.as_str());
                                                        let cls = if is_chosen { "applied-option chosen" } else { "applied-option" };
                                                        rsx! {
                                                            div { class: "{cls}", title: "{o.directive}",
                                                                span { class: "applied-option-mark", if is_chosen { "● chosen" } else { "○" } }
                                                                span { class: "applied-option-label", "{o.label}" }
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
                        // Custom rules editor: add/edit (by name) + a grouped table.
                        p { class: "section-label", "Custom rules" }
                        p { class: "section-hint", "Your own rules — no corpus source. They're carried into every emit and removed only when you delete them here. Adding a name that already exists edits it." }
                        {
                            let pid_add = p.id.clone();
                            rsx! {
                                div { class: "routine-create-row",
                                    input { class: "addressee-input", placeholder: "name", value: "{cr_name}", oninput: move |e| cr_name.set(e.value()) }
                                    input { class: "addressee-input", placeholder: "domain (e.g. api-layer, or * for all)", value: "{cr_domain}", oninput: move |e| cr_domain.set(e.value()) }
                                }
                                textarea { class: "routine-intent-input", rows: "3", placeholder: "the directive the agent should follow…", value: "{cr_body}", oninput: move |e| cr_body.set(e.value()) }
                                button {
                                    class: "btn-run",
                                    onclick: move |_| {
                                        let (name, domain, body) = (cr_name(), cr_domain(), cr_body());
                                        if name.trim().is_empty() || body.trim().is_empty() { return; }
                                        let pid = pid_add.clone();
                                        spawn(async move {
                                            if add_custom_rule(&pid, &name, &body, &domain).await { refresh += 1; }
                                        });
                                        cr_name.set(String::new());
                                        cr_domain.set(String::new());
                                        cr_body.set(String::new());
                                    },
                                    "Save custom rule"
                                }
                            }
                        }
                        if !p.ruleset.custom.is_empty() {
                            CustomRulesTable { key: "cr-{p.ruleset.custom.len()}", custom: p.ruleset.custom.clone(), project_id: p.id.clone(), refresh }
                        }

                        p { class: "section-label", "Export ruleset (source of truth)" }
                        textarea { class: "routine-prompt-input", rows: "8", readonly: true, value: "{export}" }
                        p { class: "section-label", "Import ruleset (upsert base; preserves custom)" }
                        textarea {
                            class: "routine-prompt-input",
                            rows: "6",
                            placeholder: "paste a ruleset JSON…",
                            value: "{import_text}",
                            oninput: move |e| import_text.set(e.value()),
                        }
                        button {
                            class: "btn-run",
                            onclick: move |_| {
                                let (id, json) = (pid.clone(), import_text());
                                if json.trim().is_empty() { return; }
                                spawn(async move {
                                    if import_ruleset(&id, json).await {
                                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, "Ruleset imported (custom rules preserved).");
                                        refresh += 1;
                                    } else {
                                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, "Import failed — check the JSON shape.");
                                    }
                                });
                                import_text.set(String::new());
                            },
                            "Import ruleset"
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn RuleCount(label: String, n: usize) -> Element {
    rsx! {
        div { class: "rule-count",
            span { class: "rule-count-n", "{n}" }
            span { class: "rule-count-l", "{label}" }
        }
    }
}

/// The active work-tracker connection as the BFF reports it (`GET /api/provider`).
#[derive(serde::Deserialize, Clone, PartialEq)]
struct ProviderView {
    /// Human label, e.g. "native (in-process)" or "github (token; …)".
    provider: String,
    /// True when a real external tracker (GitHub) is wired.
    live: bool,
}

/// Fetch the active provider/connection from the BFF.
async fn fetch_provider() -> Option<ProviderView> {
    reqwest::get(format!("{}/api/provider", crate::BFF_URL))
        .await
        .ok()?
        .json::<ProviderView>()
        .await
        .ok()
}

/// A run as the BFF reports it (`GET /api/runs/:id`): status plus the REAL gate
/// verdicts produced so far.
#[derive(Clone, PartialEq, serde::Deserialize)]
struct RunView {
    #[serde(default)]
    id: String,
    story_id: String,
    status: String,
    events: Vec<RunGateEvent>,
    done: bool,
    #[serde(default)]
    mode: String,
}

/// One real gate verdict in a run.
#[derive(Clone, PartialEq, serde::Deserialize)]
struct RunGateEvent {
    verdict: String,
    rule: Option<String>,
    detail: String,
}

/// Start a governed run for a story; returns the run id.
async fn start_run(story_id: &str) -> Option<String> {
    let resp = reqwest::Client::new()
        .post(format!("{}/api/stories/{}/run", crate::BFF_URL, story_id))
        .send()
        .await
        .ok()?;
    let v: serde_json::Value = resp.json().await.ok()?;
    v.get("run_id")?.as_str().map(|s| s.to_string())
}

/// Fetch the current state of a run.
async fn fetch_run(run_id: &str) -> Option<RunView> {
    reqwest::get(format!("{}/api/runs/{}", crate::BFF_URL, run_id))
        .await
        .ok()?
        .json::<RunView>()
        .await
        .ok()
}

/// Map a run status string to a label + badge CSS modifier.
fn run_status_badge(status: &str) -> (&'static str, &'static str) {
    match status {
        "planned" => ("PLANNED", "neutral"),
        "executing" => ("EXECUTING", "active"),
        "gating" => ("GATING", "active"),
        "awaiting_qa" => ("AWAITING QA", "warn"),
        _ => ("RUNNING", "active"),
    }
}

/// A clarification as the BFF reports it (`/api/stories/:id/clarifications`).
#[derive(Clone, PartialEq, serde::Deserialize)]
struct ClarificationView {
    id: String,
    story_id: String,
    question: String,
    addressee: String,
    answer: Option<String>,
    answered_by: Option<String>,
}

/// Fetch all OPEN clarifications across stories (the NEEDS YOU queue).
async fn fetch_open_clarifications() -> Option<Vec<ClarificationView>> {
    reqwest::get(format!("{}/api/clarifications", crate::BFF_URL))
        .await
        .ok()?
        .json::<Vec<ClarificationView>>()
        .await
        .ok()
}

/// Fetch the clarifications on a story.
async fn fetch_clarifications(story_id: &str) -> Option<Vec<ClarificationView>> {
    reqwest::get(format!(
        "{}/api/stories/{}/clarifications",
        crate::BFF_URL,
        story_id
    ))
    .await
    .ok()?
    .json::<Vec<ClarificationView>>()
    .await
    .ok()
}

/// Post a clarifying question on a story, addressed to `addressee`.
async fn post_clarification(
    story_id: &str,
    question: &str,
    addressee: &str,
) -> Option<ClarificationView> {
    reqwest::Client::new()
        .post(format!(
            "{}/api/stories/{}/clarifications",
            crate::BFF_URL,
            story_id
        ))
        .json(&serde_json::json!({ "question": question, "addressee": addressee }))
        .send()
        .await
        .ok()?
        .json::<ClarificationView>()
        .await
        .ok()
}

/// Record the answer to a clarification.
async fn answer_clarification(cid: &str, answer: &str, answered_by: &str) -> Option<ClarificationView> {
    reqwest::Client::new()
        .post(format!("{}/api/clarifications/{}/answer", crate::BFF_URL, cid))
        .json(&serde_json::json!({ "answer": answer, "answered_by": answered_by }))
        .send()
        .await
        .ok()?
        .json::<ClarificationView>()
        .await
        .ok()
}

/// A proposed child story from decomposition (editable before commit). Serializes
/// back to the BFF on commit.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct ProposedChildView {
    kind: String,
    title: String,
    description: String,
}

/// Propose the component children for a parent (not yet created).
async fn fetch_proposal(story_id: &str) -> Option<Vec<ProposedChildView>> {
    reqwest::Client::new()
        .post(format!("{}/api/stories/{}/decompose", crate::BFF_URL, story_id))
        .send()
        .await
        .ok()?
        .json::<Vec<ProposedChildView>>()
        .await
        .ok()
}

/// Commit the edited children; returns the created child stories.
async fn commit_children(story_id: &str, children: &[ProposedChildView]) -> Option<Vec<CanonicalStory>> {
    reqwest::Client::new()
        .post(format!(
            "{}/api/stories/{}/decompose/commit",
            crate::BFF_URL,
            story_id
        ))
        .json(&serde_json::json!({ "children": children }))
        .send()
        .await
        .ok()?
        .json::<Vec<CanonicalStory>>()
        .await
        .ok()
}

/// The committed children of a parent.
async fn fetch_children(story_id: &str) -> Option<Vec<CanonicalStory>> {
    reqwest::get(format!("{}/api/stories/{}/children", crate::BFF_URL, story_id))
        .await
        .ok()?
        .json::<Vec<CanonicalStory>>()
        .await
        .ok()
}

/// Map a canonical status to a short label + a badge CSS modifier.
fn status_badge(status: FeatureStatus) -> (&'static str, &'static str) {
    match status {
        FeatureStatus::Intake => ("INTAKE", "neutral"),
        FeatureStatus::Investigating => ("INVESTIGATING", "active"),
        FeatureStatus::AwaitingClarification => ("NEEDS ANSWER", "warn"),
        FeatureStatus::Planned => ("PLANNED", "neutral"),
        FeatureStatus::Executing => ("EXECUTING", "active"),
        FeatureStatus::Gating => ("GATING", "active"),
        FeatureStatus::AwaitingQa => ("AWAITING QA", "warn"),
        FeatureStatus::SignedOff => ("SIGNED OFF", "done"),
        FeatureStatus::Done => ("DONE", "done"),
        FeatureStatus::Blocked => ("BLOCKED", "block"),
        FeatureStatus::Rejected => ("REJECTED", "block"),
    }
}

/// Which of the five read-only stage tabs is the active one for a given status.
/// The tabs are indicators driven by the engine, not free navigation.
fn active_stage_index(status: FeatureStatus) -> usize {
    match status {
        FeatureStatus::Intake => 0,
        FeatureStatus::Investigating | FeatureStatus::AwaitingClarification => 1,
        FeatureStatus::Planned => 2,
        FeatureStatus::Executing | FeatureStatus::Gating | FeatureStatus::Blocked => 3,
        FeatureStatus::AwaitingQa | FeatureStatus::SignedOff | FeatureStatus::Done => 4,
        FeatureStatus::Rejected => 0,
    }
}

const STAGE_TABS: &[&str] = &["INTAKE", "INVESTIGATION", "PLAN", "STATUS", "QA"];

/// Which view the enterprise cockpit is showing. Routines live INSIDE the cockpit
/// (it's an architect tool), reached via the cockpit's own nav, not a top-level app.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CockpitView {
    /// The control surface: the story spine + center stage + inspector.
    Stories,
    /// Onboard a repo into governance (brownfield: install rules into an
    /// existing repo; greenfield: scaffold a new one). The ENTRY POINT for a
    /// repo new to Camerata — distinct from a story's Investigation phase.
    Onboard,
    /// Manage the active project's ruleset (repo-local / cross-repo / process /
    /// custom) AFTER onboarding — the ongoing control surface over the same
    /// project ruleset the brownfield flow first populates.
    Rules,
    /// The scheduled-routine dashboard.
    Routines,
    /// The local workspace: clone the active project's repos into the chosen folder,
    /// see their checkout status, and ship a branch (push + PR).
    Workspace,
}

/// The cockpit's internal nav: switch between the control surface (stories) and the
/// routine dashboard. Both are architect tools, so both live in the Enterprise app.
#[component]
fn CockpitNav(view: Signal<CockpitView>) -> Element {
    let mut view = view;
    let cls = |v: CockpitView| {
        if view() == v {
            "cockpit-nav-tab on"
        } else {
            "cockpit-nav-tab"
        }
    };
    rsx! {
        div { class: "cockpit-nav",
            button {
                class: cls(CockpitView::Stories),
                onclick: move |_| view.set(CockpitView::Stories),
                "Control surface"
            }
            button {
                class: cls(CockpitView::Onboard),
                onclick: move |_| view.set(CockpitView::Onboard),
                "Onboard repos"
            }
            button {
                class: cls(CockpitView::Rules),
                onclick: move |_| view.set(CockpitView::Rules),
                "Rules"
            }
            button {
                class: cls(CockpitView::Workspace),
                onclick: move |_| view.set(CockpitView::Workspace),
                "Workspace"
            }
            button {
                class: cls(CockpitView::Routines),
                onclick: move |_| view.set(CockpitView::Routines),
                "Routines"
            }
        }
    }
}

#[component]
pub fn CockpitApp() -> Element {
    // Which cockpit view (control surface vs routines). Declared first so all hooks
    // below run unconditionally in a stable order regardless of the view.
    let view = use_signal(|| CockpitView::Stories);

    // Both data sets come from the BFF over HTTP. `use_resource` runs the fetch when
    // the cockpit mounts; the embedded server (see main.rs) is up by then.
    let stories_res = use_resource(fetch_stories);
    let rules_res = use_resource(fetch_rules);
    // The active connection (native vs GitHub), shown honestly in the topbar.
    let provider_res = use_resource(fetch_provider);

    let mut selected = use_signal(|| 0usize);
    let mut selected_rule = use_signal(|| 0usize);
    // Which stage tab the user is previewing. `None` follows the selected story's
    // actual lifecycle stage; clicking a tab overrides it so the tabs navigate.
    let mut viewed_stage = use_signal(|| Option::<usize>::None);
    // The live run for the selected story, if one has been started. Polled to
    // completion; its gate events are REAL verdicts from the BFF run engine.
    let mut active_run = use_signal(|| Option::<RunView>::None);

    // A shared refresh tick: bumped whenever a clarification is posted or answered,
    // so both the NEEDS YOU queue here and the per-story thread refetch together.
    let clarify_refresh = use_signal(|| 0u32);
    use_context_provider(|| clarify_refresh);
    let open_clars_res = use_resource(move || {
        let _dep = clarify_refresh();
        async move { fetch_open_clarifications().await }
    });

    let stories_loaded = stories_res.read().clone();
    let rules_loaded = rules_res.read().clone();
    // A resolved-but-None fetch means the BFF was unreachable / returned junk.
    let errored = matches!(&stories_loaded, Some(None)) || matches!(&rules_loaded, Some(None));

    // Routines + Onboard live inside the cockpit (architect tools). All hooks above
    // have run, so branching here is safe.
    if view() == CockpitView::Onboard {
        let conn = provider_res.read().clone().flatten();
        return rsx! {
            div { class: "cockpit",
                CockpitNav { view }
                div { class: "cockpit-scroll",
                    OnboardView { connection: conn }
                }
            }
        };
    }
    if view() == CockpitView::Rules {
        return rsx! {
            div { class: "cockpit",
                CockpitNav { view }
                div { class: "cockpit-scroll",
                    RulesView {}
                }
            }
        };
    }
    if view() == CockpitView::Routines {
        return rsx! {
            div { class: "cockpit",
                CockpitNav { view }
                div { class: "cockpit-scroll",
                    crate::routines::RoutineDashboard {}
                }
            }
        };
    }
    if view() == CockpitView::Workspace {
        return rsx! {
            div { class: "cockpit",
                CockpitNav { view }
                div { class: "cockpit-scroll",
                    crate::workspace::WorkspaceView {}
                }
            }
        };
    }

    match (stories_loaded, rules_loaded) {
        (Some(Some(story_list)), Some(Some(rules))) => {
            if story_list.is_empty() {
                return rsx! {
                    div { class: "cockpit",
                        CockpitNav { view }
                        CockpitNotice { kind: "empty".to_string() }
                    }
                };
            }
            let current = story_list[selected().min(story_list.len() - 1)].clone();
            let active_stage = active_stage_index(current.status);
            // The tabs navigate: an explicit click overrides; otherwise we follow
            // the story's real lifecycle stage.
            let effective_stage = viewed_stage().unwrap_or(active_stage);
            let conn = provider_res.read().clone().flatten();
            // Gate tallies derived from the live run's REAL verdicts (not fixtures).
            // No active run for this story -> no tallies (None), shown as "idle".
            let gate_tally: Option<(usize, usize)> = match active_run() {
                Some(ref r) if r.story_id == current.id => Some((
                    r.events.iter().filter(|e| e.verdict == "deny").count(),
                    r.events.iter().filter(|e| e.verdict == "allow").count(),
                )),
                _ => None,
            };

            rsx! {
                div { class: "cockpit",
                    CockpitNav { view }
                    CockpitTopBar { story: current.clone(), connection: conn.clone() }

                    div { class: "cockpit-body",
                        // ── LEFT: story spine (from /api/stories) + NEEDS YOU queue ──
                        aside { class: "cockpit-rail",
                            p { class: "cockpit-rail-label", "STORY SPINE" }
                            div { class: "spine-list",
                                for (i , s) in story_list.iter().enumerate() {
                                    {
                                        let (badge, badge_cls) = status_badge(s.status);
                                        let sel = i == selected();
                                        let cls = if sel { "spine-item sel" } else { "spine-item" };
                                        rsx! {
                                            button {
                                                class: "{cls}",
                                                onclick: move |_| selected.set(i),
                                                span { class: "spine-title", "{s.title}" }
                                                span { class: "spine-badge {badge_cls}", "{badge}" }
                                            }
                                        }
                                    }
                                }
                                button { class: "spine-new", "+ New story" }
                            }

                            {
                                let open_clars = open_clars_res.read().clone().flatten().unwrap_or_default();
                                let n = open_clars.len();
                                rsx! {
                                    p { class: "cockpit-rail-label needs", "NEEDS YOU ({n})" }
                                    div { class: "needs-list",
                                        if open_clars.is_empty() {
                                            p { class: "needs-empty", "Nothing needs you right now." }
                                        }
                                        for c in open_clars.iter() {
                                            {
                                                let target = story_list.iter().position(|s| s.id == c.story_id);
                                                let q = c.question.clone();
                                                let who = c.addressee.clone();
                                                rsx! {
                                                    button {
                                                        class: "needs-item",
                                                        onclick: move |_| {
                                                            if let Some(i) = target {
                                                                selected.set(i);
                                                            }
                                                        },
                                                        span { class: "needs-dot warn" }
                                                        span {
                                                            span { class: "needs-q", "{q}" }
                                                            span { class: "needs-who", "asked {who}" }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        // ── CENTER: stage tabs + active stage panel + status strip ──
                        section { class: "cockpit-stage",
                            div { class: "stage-tabs",
                                for (i , tab) in STAGE_TABS.iter().enumerate() {
                                    {
                                        // "on" = the story's real stage; "view" = the tab the
                                        // user is previewing. Clicking a tab previews that stage.
                                        let mut cls = String::from("stage-tab");
                                        if i == active_stage { cls.push_str(" on"); }
                                        if i == effective_stage && effective_stage != active_stage { cls.push_str(" view"); }
                                        rsx! {
                                            button {
                                                class: "{cls}",
                                                onclick: move |_| viewed_stage.set(Some(i)),
                                                "{tab}"
                                            }
                                        }
                                    }
                                }
                            }

                            div { class: "stage-panel",
                                // Run control: start a governed run for this story and
                                // poll it to completion, streaming the real gate verdicts.
                                {
                                    let sid = current.id.clone();
                                    rsx! {
                                        button {
                                            class: "btn-run",
                                            onclick: move |_| {
                                                let sid = sid.clone();
                                                spawn(async move {
                                                    if let Some(rid) = start_run(&sid).await {
                                                        loop {
                                                            if let Some(rv) = fetch_run(&rid).await {
                                                                let done = rv.done;
                                                                active_run.set(Some(rv));
                                                                if done {
                                                                    break;
                                                                }
                                                            }
                                                            tokio::time::sleep(std::time::Duration::from_millis(600)).await;
                                                        }
                                                    }
                                                });
                                            },
                                            "▶ Run this story (governed)"
                                        }
                                    }
                                }

                                // Agent activity: peek at each agent's GENERATED prompt +
                                // output for the active run (the otherwise-hidden prompting).
                                {
                                    let rid = match active_run() {
                                        Some(ref r) if r.story_id == current.id => r.id.clone(),
                                        _ => String::new(),
                                    };
                                    rsx! { crate::agent_activity::AgentActivity { run_id: rid } }
                                }

                                // A live run for THIS story (when the user is on its actual
                                // stage) shows the real gate stream; otherwise the panel for
                                // whichever stage tab is being previewed.
                                {
                                    match active_run() {
                                        Some(r) if r.story_id == current.id && viewed_stage().is_none() => {
                                            rsx! { LiveRunPanel { run: r } }
                                        }
                                        _ => rsx! { StagePanel { story: current.clone(), stage: effective_stage } },
                                    }
                                }

                                // The clarify-bridge: ask the team a question, pick who
                                // to ask, and see the thread. In-process now.
                                ClarifySection { story_id: current.id.clone() }

                                // Decomposition: split this story into component
                                // children per the practice, review/edit, create.
                                DecomposeSection { story_id: current.id.clone() }
                            }

                            div { class: "status-strip",
                                div { class: "strip-fleet",
                                    match active_run() {
                                        Some(ref r) if r.story_id == current.id => {
                                            let (label, badge_cls) = run_status_badge(&r.status);
                                            rsx! {
                                                span { class: "fleet-pill {badge_cls}",
                                                    span { class: "fleet-role", "run" }
                                                    span { class: "fleet-state", "{label}" }
                                                }
                                            }
                                        }
                                        _ => rsx! {
                                            span { class: "fleet-idle", "No active run — press Run this story to start the governed fleet." }
                                        },
                                    }
                                }
                                div { class: "strip-gates",
                                    match gate_tally {
                                        Some((deny, allow)) => rsx! {
                                            span { class: "gate-tally",
                                                span { class: "gate-num", "{deny}" }
                                                " gate denials"
                                            }
                                            span { class: "gate-tally",
                                                span { class: "gate-num", "{allow}" }
                                                " allowed writes"
                                            }
                                        },
                                        None => rsx! {
                                            span { class: "gate-tally idle", "gate: idle" }
                                        },
                                    }
                                }
                            }
                        }

                        // ── RIGHT: inspector. Enforced rules from /api/rules. ──
                        aside { class: "cockpit-inspector",
                            p { class: "cockpit-rail-label", "INSPECTOR" }
                            p { class: "inspector-hint", "The rules this fleet is governed by. These are the gate's actual enforced rules." }
                            div { class: "rule-list",
                                for (i , r) in rules.iter().enumerate() {
                                    {
                                        let sel = i == selected_rule();
                                        let cls = if sel { "rule-chip sel" } else { "rule-chip" };
                                        rsx! {
                                            button {
                                                class: "{cls}",
                                                onclick: move |_| selected_rule.set(i),
                                                "{r.id}"
                                            }
                                        }
                                    }
                                }
                            }
                            {
                                let idx = selected_rule().min(rules.len().saturating_sub(1));
                                let r = &rules[idx];
                                rsx! {
                                    div { class: "rule-detail",
                                        p { class: "rule-id", "{r.id}" }
                                        p { class: "rule-enforce",
                                            span { class: "enforce-dot" }
                                            "deterministic, active"
                                        }
                                        p { class: "rule-label", "Statement" }
                                        p { class: "rule-statement", "{r.statement}" }
                                        p { class: "rule-label", "Enforcement" }
                                        p { class: "rule-statement", "Checked at the MCP tool boundary before the write executes (deny-before-execute), and re-checked out-of-process after the task. Binary pass/fail." }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        _ if errored => rsx! {
            div { class: "cockpit",
                CockpitNav { view }
                CockpitNotice { kind: "error".to_string() }
            }
        },
        _ => rsx! {
            div { class: "cockpit",
                CockpitNav { view }
                CockpitNotice { kind: "loading".to_string() }
            }
        },
    }
}

/// Loading / error / empty placeholder for the cockpit, shown while the BFF fetch
/// is pending or if it fails.
#[component]
fn CockpitNotice(kind: String) -> Element {
    let (title, body) = match kind.as_str() {
        "loading" => (
            "Connecting to the engine…",
            "Reaching the local Camerata server.",
        ),
        "error" => (
            "Can't reach the engine",
            "The Camerata server isn't responding on localhost:8787. It starts with the app; if this persists, restart the app.",
        ),
        _ => (
            "No stories yet — clean slate",
            "Nothing is seeded. Connect GitHub, then onboard one or more repos (the \u{201c}Onboard repos\u{201d} tab) or adopt a tracker issue to bring real stories into the spine.",
        ),
    };
    rsx! {
        div { class: "cockpit-notice",
            p { class: "cockpit-notice-title", "{title}" }
            p { class: "cockpit-notice-body", "{body}" }
        }
    }
}

// ── Brownfield scan: data + chorale-backed tables ──────────────────────────────

#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
struct FindingView {
    #[serde(default)]
    repo: String,
    path: String,
    line: usize,
    rule_id: String,
    severity: String,
    snippet: String,
    detail: String,
}

#[derive(Clone, PartialEq, serde::Deserialize)]
struct RuleOptionView {
    id: String,
    label: String,
    #[serde(default)]
    directive: String,
}

#[derive(Clone, PartialEq, serde::Deserialize)]
struct ProposedRuleView {
    id: String,
    title: String,
    kind: String,
    #[serde(default)]
    enforcement: String,
    #[serde(default)]
    options: Vec<RuleOptionView>,
    #[serde(default)]
    default_option: Option<String>,
    #[serde(default)]
    scope: String,
    #[serde(default)]
    repos: Vec<String>,
    #[serde(default)]
    placement: String,
    finding_count: usize,
    #[allow(dead_code)]
    recommended: bool,
}

#[derive(Clone, PartialEq, serde::Deserialize)]
struct StackView {
    repo: String,
    #[serde(default)]
    languages: Vec<String>,
    #[serde(default)]
    frameworks: Vec<String>,
}

#[derive(Clone, PartialEq, serde::Deserialize)]
struct ScanReportView {
    #[serde(default)]
    repos: Vec<String>,
    #[serde(default)]
    stacks: Vec<StackView>,
    files_scanned: usize,
    findings: Vec<FindingView>,
    proposed_rules: Vec<ProposedRuleView>,
    gated: bool,
    #[serde(default)]
    message: Option<String>,
}

async fn scan_repos(repos: &[String]) -> Option<ScanReportView> {
    reqwest::Client::new()
        .post(format!("{}/api/onboard/scan", crate::BFF_URL))
        .json(&serde_json::json!({ "repos": repos }))
        .send()
        .await
        .ok()?
        .json::<ScanReportView>()
        .await
        .ok()
}

/// A fully-resolved rule sent to arm (the chosen directive + where it installs).
#[derive(Clone, serde::Serialize)]
struct ArmRuleReq {
    id: String,
    title: String,
    directive: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    option: Option<String>,
    enforcement: String,
    scope: String,
    repos: Vec<String>,
}

#[derive(Clone, serde::Deserialize)]
struct ArmResultView {
    repo: String,
    ok: bool,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

/// Arm: install the selected rules into their repos via governance PRs.
async fn arm_rules(rules: &[ArmRuleReq]) -> Option<Vec<ArmResultView>> {
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/api/onboard/arm", crate::BFF_URL))
        .json(&serde_json::json!({ "rules": rules }))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    if !v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) {
        return None;
    }
    serde_json::from_value(v.get("results").cloned()?).ok()
}

/// Accept selected findings as tech debt: open a GitHub issue. Returns the URL.
async fn create_ticket(repo: &str, findings: &[FindingView]) -> Option<String> {
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/api/onboard/ticket", crate::BFF_URL))
        .json(&serde_json::json!({ "repo": repo, "findings": findings }))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    if v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) {
        v.get("url").and_then(|u| u.as_str()).map(|s| s.to_string())
    } else {
        None
    }
}

fn finding_columns() -> Vec<ColumnDef<FindingView>> {
    let sev = BadgeVariantMap::new()
        .with("high", BadgeVariant::new("High", "red"))
        .with("medium", BadgeVariant::new("Medium", "yellow"));
    vec![
        ColumnDef::new(ColumnId("repo"), "Repo", |f: &FindingView| {
            CellValue::Text(f.repo.clone())
        })
        .sortable()
        .filter(FilterKind::Text)
        .initial_width(180.0),
        ColumnDef::new(ColumnId("severity"), "Severity", |f: &FindingView| {
            CellValue::Text(f.severity.clone())
        })
        .sortable()
        .render_kind(RenderKind::Badge(sev))
        .initial_width(110.0),
        ColumnDef::new(ColumnId("type"), "Finding type", |f: &FindingView| {
            CellValue::Text(f.rule_id.clone())
        })
        .sortable()
        .filter(FilterKind::Text)
        .initial_width(250.0),
        ColumnDef::new(ColumnId("loc"), "Location", |f: &FindingView| {
            CellValue::Text(format!("{}:{}", f.path, f.line))
        })
        .sortable()
        .filter(FilterKind::Text)
        .initial_width(280.0),
        ColumnDef::new(ColumnId("snippet"), "Snippet", |f: &FindingView| {
            CellValue::Text(f.snippet.clone())
        })
        .initial_width(380.0),
    ]
}

fn rule_columns() -> Vec<ColumnDef<ProposedRuleView>> {
    let kind = BadgeVariantMap::new()
        .with("mechanical", BadgeVariant::new("Mechanical", "green"))
        .with("review", BadgeVariant::new("Review", "yellow"));
    let scope = BadgeVariantMap::new()
        .with("repo-local", BadgeVariant::new("Repo-local", "blue"))
        .with("cross-repo", BadgeVariant::new("Cross-repo", "purple"))
        .with("process", BadgeVariant::new("Process", "gray"));
    vec![
        ColumnDef::new(ColumnId("id"), "Rule", |r: &ProposedRuleView| {
            CellValue::Text(r.id.clone())
        })
        .sortable()
        .filter(FilterKind::Text)
        .initial_width(230.0),
        ColumnDef::new(ColumnId("scope"), "Scope", |r: &ProposedRuleView| {
            CellValue::Text(r.scope.clone())
        })
        .sortable()
        .render_kind(RenderKind::Badge(scope))
        .initial_width(130.0),
        ColumnDef::new(ColumnId("repos"), "Applies to", |r: &ProposedRuleView| {
            CellValue::Text(if r.repos.is_empty() {
                "—".to_string()
            } else if r.repos.len() <= 2 {
                r.repos.join(", ")
            } else {
                format!("{} repos", r.repos.len())
            })
        })
        .filter(FilterKind::Text)
        .initial_width(180.0),
        ColumnDef::new(ColumnId("placement"), "Gate placement", |r: &ProposedRuleView| {
            CellValue::Text(r.placement.clone())
        })
        .initial_width(300.0),
        ColumnDef::new(ColumnId("kind"), "Kind", |r: &ProposedRuleView| {
            CellValue::Text(r.kind.clone())
        })
        .sortable()
        .render_kind(RenderKind::Badge(kind))
        .initial_width(120.0),
        ColumnDef::new(
            ColumnId("count"),
            "Existing violations",
            |r: &ProposedRuleView| CellValue::Integer(r.finding_count as i64),
        )
        .sortable()
        .render_kind(RenderKind::Number)
        .alignment(Alignment::Right)
        .initial_width(150.0),
    ]
}

/// The findings table with TRIAGE: sort by repo/severity/type, filter, select rows
/// and Ignore / Resolve / Accept-as-tech-debt (open a ticket) them. Virtualized by
/// chorale, so a large audit doesn't choke the UI.
#[component]
fn FindingsTable(
    findings: Vec<FindingView>,
    repos: Vec<String>,
    descriptions: std::collections::HashMap<String, String>,
) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let target_repo = repos.first().cloned().unwrap_or_default();
    let rows: Vec<(RowId, FindingView)> =
        findings.iter().map(|f| (RowId::new(), f.clone())).collect();
    let id_map: std::collections::HashMap<RowId, FindingView> =
        rows.iter().map(|(r, f)| (*r, f.clone())).collect();
    let handle = use_table(move || TableState::new(rows.clone(), finding_columns()));
    let mut busy = use_signal(|| false);

    // Hover the rule id in the "type" column to read what it enforces (and, once a
    // rule's alternative is chosen, the chosen alternative) — no memorizing.
    let renderers = {
        let desc = descriptions.clone();
        let mut m: std::collections::HashMap<ColumnId, CellRenderer> =
            std::collections::HashMap::new();
        m.insert(
            ColumnId("type"),
            std::sync::Arc::new(move |val: &CellValue| {
                let rid = match val {
                    CellValue::Text(s) => s.clone(),
                    _ => String::new(),
                };
                let tip = desc.get(&rid).cloned().unwrap_or_else(|| rid.clone());
                rsx! { span { title: "{tip}", "{rid}" } }
            }) as CellRenderer,
        );
        CellRenderers::new(m)
    };

    rsx! {
        div { class: "findings-toolbar",
            button {
                class: "btn-restart",
                onclick: move |_| {
                    let sel = handle.selected_ids();
                    if sel.is_empty() { return; }
                    let n = sel.len();
                    handle.remove_rows(&sel);
                    crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("Ignored {n} finding(s)."));
                },
                "Ignore selected"
            }
            button {
                class: "btn-restart",
                onclick: move |_| {
                    let sel = handle.selected_ids();
                    if sel.is_empty() { return; }
                    let n = sel.len();
                    handle.remove_rows(&sel);
                    crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("Queued {n} for a governed fix (runs when Claude is connected)."));
                },
                "Resolve selected"
            }
            button {
                class: "btn-run",
                disabled: busy(),
                onclick: move |_| {
                    let sel = handle.selected_ids();
                    let picked: Vec<FindingView> = sel.iter().filter_map(|id| id_map.get(id).cloned()).collect();
                    if picked.is_empty() { return; }
                    let repo = target_repo.clone();
                    busy.set(true);
                    spawn(async move {
                        match create_ticket(&repo, &picked).await {
                            Some(url) => {
                                crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("Tech-debt ticket opened: {url}"));
                                handle.remove_rows(&sel);
                            }
                            None => crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, "Couldn't open the ticket — needs Issues write on the connected token."),
                        }
                        busy.set(false);
                    });
                },
                if busy() { "Filing…" } else { "Accept as tech debt \u{2192} ticket" }
            }
        }
        Table { handle, sort_enabled: true, filter_enabled: true, selection_enabled: true, resize_enabled: true, cell_renderers: renderers }
    }
}

/// The alternative-selection step: rules that carry alternatives get a per-rule
/// picker. Rules with an adopted default pre-select it ("use defaults" sets them
/// all at once); rules WITHOUT a default must be chosen before they can be armed.
/// The chosen map (rule id -> option id) is shared via context so the findings
/// table's rule-id hover can show the chosen alternative.
#[component]
fn AlternativesPicker(rules: Vec<ProposedRuleView>, all_repos: Vec<String>) -> Element {
    let mut chosen = use_context::<Signal<std::collections::HashMap<String, String>>>();
    let placement = use_context::<Signal<std::collections::HashMap<String, Vec<String>>>>();
    let opt_rules: Vec<ProposedRuleView> =
        rules.into_iter().filter(|r| !r.options.is_empty()).collect();
    if opt_rules.is_empty() {
        return rsx! {};
    }
    let defaults: Vec<(String, String)> = opt_rules
        .iter()
        .filter_map(|r| r.default_option.clone().map(|d| (r.id.clone(), d)))
        .collect();

    rsx! {
        div { class: "alts",
            div { class: "alts-head",
                div {
                    p { class: "scan-section-h", "Choose an alternative per rule" }
                    p { class: "scan-section-sub", "Some rules ship an adopted default; rules without one require a choice before they can be armed." }
                }
                button {
                    class: "btn-restart",
                    onclick: move |_| {
                        for (id, d) in &defaults {
                            chosen.write().insert(id.clone(), d.clone());
                        }
                    },
                    "Use defaults where available"
                }
            }
            for r in opt_rules.iter() {
                {
                    let rid = r.id.clone();
                    let current = chosen.read().get(&r.id).cloned().or_else(|| r.default_option.clone());
                    let must_choose = r.default_option.is_none() && current.is_none();
                    let cls = if must_choose { "alt-row must" } else { "alt-row" };
                    rsx! {
                        div { class: "{cls}",
                            div { class: "alt-rule",
                                span { class: "alt-rule-id", "{r.id}" }
                                span { class: "alt-rule-title", "{r.title}" }
                                if must_choose {
                                    span { class: "alt-must", "choice required" }
                                }
                            }
                            select {
                                class: "alt-select",
                                value: current.clone().unwrap_or_default(),
                                onchange: move |e| { chosen.write().insert(rid.clone(), e.value()); },
                                if r.default_option.is_none() {
                                    option { value: "", "— choose an alternative —" }
                                }
                                for o in r.options.iter() {
                                    option {
                                        value: "{o.id}",
                                        selected: current.as_deref() == Some(o.id.as_str()),
                                        title: "{o.directive}",
                                        if r.default_option.as_deref() == Some(o.id.as_str()) {
                                            "{o.label} (default)"
                                        } else {
                                            "{o.label}"
                                        }
                                    }
                                }
                            }
                        }
                        // Per-rule repo placement: the domain-matched suggestion is
                        // pre-selected; toggle any repo to override (force a domain
                        // rule into a repo it wouldn't be suggested for, or remove one).
                        div { class: "alt-repos",
                            span { class: "alt-repos-label", "installs into:" }
                            for repo in all_repos.iter() {
                                {
                                    let mut placement = placement;
                                    let rid2 = r.id.clone();
                                    let repo2 = repo.clone();
                                    let on = placement.read().get(&r.id).map(|v| v.contains(repo)).unwrap_or(false);
                                    let chip_cls = if on { "repo-chip on" } else { "repo-chip" };
                                    rsx! {
                                        button {
                                            class: "{chip_cls}",
                                            onclick: move |_| {
                                                let mut p = placement.write();
                                                let entry = p.entry(rid2.clone()).or_default();
                                                if let Some(pos) = entry.iter().position(|x| x == &repo2) {
                                                    entry.remove(pos);
                                                } else {
                                                    entry.push(repo2.clone());
                                                }
                                            },
                                            "{repo}"
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
}

/// The proposed-rules table with SELECTION (chorale checkboxes) — accept/reject
/// each rule into the approved starter set.
#[component]
fn ProposedRulesTable(rules: Vec<ProposedRuleView>) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let chosen = use_context::<Signal<std::collections::HashMap<String, String>>>();
    let placement = use_context::<Signal<std::collections::HashMap<String, Vec<String>>>>();
    let rows: Vec<(RowId, ProposedRuleView)> =
        rules.iter().map(|r| (RowId::new(), r.clone())).collect();
    let id_map: std::collections::HashMap<RowId, ProposedRuleView> =
        rows.iter().map(|(r, p)| (*r, p.clone())).collect();
    let handle = use_table(move || TableState::new(rows.clone(), rule_columns()));
    let mut arming = use_signal(|| false);

    rsx! {
        Table { handle, sort_enabled: true, selection_enabled: true }
        div { class: "findings-toolbar",
            button {
                class: "btn-run",
                disabled: arming(),
                onclick: move |_| {
                    let sel = handle.selected_ids();
                    let picked: Vec<ProposedRuleView> = sel.iter().filter_map(|id| id_map.get(id).cloned()).collect();
                    if picked.is_empty() { return; }
                    // Resolve each selected rule to its adopted directive; a rule
                    // with alternatives and no choice yet blocks arming.
                    let mut arm_reqs = Vec::new();
                    let mut unresolved = Vec::new();
                    for r in &picked {
                        let (directive, option) = if r.options.is_empty() {
                            (r.title.clone(), None)
                        } else {
                            let oid = chosen.read().get(&r.id).cloned().or_else(|| r.default_option.clone());
                            match oid.clone().and_then(|o| r.options.iter().find(|x| x.id == o).map(|x| x.directive.clone())) {
                                Some(d) if !d.is_empty() => (d, oid),
                                _ => { unresolved.push(r.id.clone()); continue; }
                            }
                        };
                        // Use the architect's placement override if set, else the
                        // domain-matched suggestion. A rule routed to zero repos is skipped.
                        let repos = placement.read().get(&r.id).cloned().unwrap_or_else(|| r.repos.clone());
                        if repos.is_empty() { continue; }
                        arm_reqs.push(ArmRuleReq {
                            id: r.id.clone(),
                            title: r.title.clone(),
                            directive,
                            option,
                            enforcement: r.enforcement.clone(),
                            scope: r.scope.clone(),
                            repos,
                        });
                    }
                    if !unresolved.is_empty() {
                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Warning, format!("Choose an alternative first for: {}", unresolved.join(", ")));
                        return;
                    }
                    arming.set(true);
                    spawn(async move {
                        match arm_rules(&arm_reqs).await {
                            Some(results) => {
                                for r in results {
                                    if r.ok {
                                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("{}: governance PR \u{2192} {}", r.repo, r.url.unwrap_or_default()));
                                    } else {
                                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, format!("{}: {}", r.repo, r.message.unwrap_or_default()));
                                    }
                                }
                            }
                            None => crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, "Arm failed — needs Contents + PR write on the connected token."),
                        }
                        arming.set(false);
                    });
                },
                if arming() { "Arming…" } else { "Arm selected rules \u{2192} governance PR" }
            }
        }
    }
}

/// Which onboarding path the user is setting up.
#[derive(Clone, Copy, PartialEq, Eq)]
enum OnboardPath {
    /// Install governance into an EXISTING repo (scan → propose → audit → arm).
    Brownfield,
    /// Scaffold a NEW repo with the rules baked in from commit zero.
    Greenfield,
}

/// The repo-onboarding ENTRY POINT: bring a repo new to Camerata under
/// governance. Brownfield (existing repo) and greenfield (new repo) are the two
/// paths. This is distinct from a story's Investigation phase — onboarding sets
/// up the REPO's rules + CI gate; Investigation is per-STORY refinement.
///
/// Connection-gated and honest: the scan/audit/arm engine runs against GitHub, so
/// the actionable steps light up once a GitHub token is connected. Until then it
/// explains exactly what each step will do.
#[component]
fn OnboardView(connection: Option<ProviderView>) -> Element {
    let mut path = use_signal(|| OnboardPath::Brownfield);
    let mut repo = use_signal(String::new);
    let mut scan = use_signal(|| Option::<ScanReportView>::None);
    let mut scanning = use_signal(|| false);
    let connected = connection.as_ref().map(|c| c.live).unwrap_or(false);

    let brownfield_cls = if path() == OnboardPath::Brownfield { "onboard-path on" } else { "onboard-path" };
    let greenfield_cls = if path() == OnboardPath::Greenfield { "onboard-path on" } else { "onboard-path" };

    // The flow steps differ slightly by path; both are gated on a connection.
    let steps: &[(&str, &str)] = match path() {
        OnboardPath::Brownfield => &[
            ("Point at the repo", "Name an existing owner/repo your token can reach."),
            ("Scan + propose a starter ruleset", "Camerata maps the stack and conventions and proposes a starting RuleSet — you review, you don't author from scratch."),
            ("Approve / edit", "Adjust and approve the rules. You own the final set."),
            ("Audit", "Scan the existing code against the approved rules and list what's already wrong (the 5-minute payoff). Secret/SQL content rules audit today; AST architecture rules follow."),
            ("Arm", "Generate ONE governance PR: CONVENTIONS.md/AGENTS.md, an enforced CI workflow, and the gate's rule-subset config. Merge it and new violations are stopped at the gate."),
        ],
        OnboardPath::Greenfield => &[
            ("Name the new repo", "Camerata scaffolds it with the rules baked in from commit zero."),
            ("Pick the starter ruleset", "Start from the corpus defaults for your stack; edit and approve."),
            ("Scaffold + arm", "Create the repo with CONVENTIONS.md/AGENTS.md, the CI gate, and the gate config already in place — governed from the first commit."),
        ],
    };

    rsx! {
        div { class: "onboard",
            div { class: "onboard-head",
                p { class: "onboard-title", "Onboard repos into governance" }
                p { class: "onboard-sub", "Bring a repo new to Camerata under the gate. This sets up the REPO's rules and CI enforcement — separate from a story's Investigation phase, which refines one piece of work." }
            }

            // Path chooser.
            div { class: "onboard-paths",
                button {
                    class: "{brownfield_cls}",
                    onclick: move |_| path.set(OnboardPath::Brownfield),
                    span { class: "onboard-path-h", "Brownfield" }
                    span { class: "onboard-path-d", "Install governance into an existing repo." }
                }
                button {
                    class: "{greenfield_cls}",
                    onclick: move |_| path.set(OnboardPath::Greenfield),
                    span { class: "onboard-path-h", "Greenfield" }
                    span { class: "onboard-path-d", "Scaffold a new repo, governed from commit zero." }
                }
            }

            // Connection gate.
            if !connected {
                div { class: "onboard-gate",
                    span { class: "onboard-gate-dot" }
                    div {
                        p { class: "onboard-gate-h", "Connect GitHub to begin" }
                        p { class: "onboard-gate-b",
                            "Set "
                            span { class: "mono", "CAMERATA_GITHUB_TOKEN" }
                            " (and restart the app) so Camerata can read the repo. The steps below activate once a token is connected. See "
                            span { class: "mono", "docs/USER_GUIDE.md" }
                            "."
                        }
                    }
                }
            }

            // Repo input — a SET of repos (a brownfield onboarding spans
            // inter-related repos), one owner/repo per line.
            div { class: "onboard-repo-block",
                label { class: "onboard-repo-label", "Repositories — one owner/repo per line (a feature often spans several)" }
                textarea {
                    class: "onboard-repos-input",
                    rows: "4",
                    placeholder: "acme/api\nacme/worker\nacme/web",
                    value: "{repo}",
                    oninput: move |e| repo.set(e.value()),
                }
                button {
                    class: "onboard-cta",
                    disabled: !connected || repo().trim().is_empty() || scanning(),
                    // Brownfield scans the whole repo SET (audit + propose rules) via
                    // the gated /api/onboard/scan; greenfield scaffolding is next.
                    onclick: move |_| {
                        if path() != OnboardPath::Brownfield { return; }
                        let repos: Vec<String> = repo()
                            .lines()
                            .flat_map(|l| l.split(','))
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect();
                        if repos.is_empty() { return; }
                        scanning.set(true);
                        spawn(async move {
                            scan.set(scan_repos(&repos).await);
                            scanning.set(false);
                        });
                    },
                    {
                        match (path(), scanning()) {
                            (OnboardPath::Greenfield, _) => "Scaffold repo",
                            (_, true) => "Scanning…",
                            (_, false) => "Scan repos",
                        }
                    }
                }
            }

            // Scan results: the audit findings + proposed-rules tables (chorale).
            if let Some(report) = scan() {
                if report.gated {
                    div { class: "onboard-gate",
                        span { class: "onboard-gate-dot" }
                        div {
                            p { class: "onboard-gate-h", "Scan not run" }
                            p { class: "onboard-gate-b", "{report.message.clone().unwrap_or_default()}" }
                        }
                    }
                } else {
                    ScanResults { report }
                }
            }

            // The flow (shown until a scan has run).
            if scan().is_none() {
                div { class: "onboard-steps",
                    for (i , (h , b)) in steps.iter().enumerate() {
                        div { class: "onboard-step",
                            span { class: "onboard-step-n", "{i + 1}" }
                            div {
                                p { class: "onboard-step-h", "{h}" }
                                p { class: "onboard-step-b", "{b}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Renders one brownfield scan's results: the audit summary, the findings table,
/// and the proposed-rules table. Keyed by the parent so a new scan remounts the
/// chorale tables with fresh rows.
#[component]
fn ScanResults(report: ScanReportView) -> Element {
    let high = report.findings.iter().filter(|f| f.severity == "high").count();
    let table_key = format!("{}-{}", report.repos.join(","), report.findings.len());

    // The architect's per-rule alternative choices (rule id -> option id), seeded
    // with each rule's default. Shared so the findings hover reads the choice.
    let chosen = use_signal(|| {
        let mut m = std::collections::HashMap::<String, String>::new();
        for r in &report.proposed_rules {
            if let Some(d) = &r.default_option {
                m.insert(r.id.clone(), d.clone());
            }
        }
        m
    });
    use_context_provider(|| chosen);

    // Per-rule repo placement (rule id -> repos it installs into), seeded with the
    // domain-matched suggestion. The architect can override it; arm uses this map.
    let placement = use_signal(|| {
        let mut m = std::collections::HashMap::<String, Vec<String>>::new();
        for r in &report.proposed_rules {
            m.insert(r.id.clone(), r.repos.clone());
        }
        m
    });
    use_context_provider(|| placement);

    // rule id -> what it enforces (the chosen/default alternative's directive, else
    // the rule title), for the findings-table rule-id hover.
    let descriptions: std::collections::HashMap<String, String> = report
        .proposed_rules
        .iter()
        .map(|r| {
            let picked = chosen.read().get(&r.id).cloned().or_else(|| r.default_option.clone());
            let desc = picked
                .and_then(|oid| r.options.iter().find(|o| o.id == oid).map(|o| o.directive.clone()))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| r.title.clone());
            (r.id.clone(), desc)
        })
        .collect();

    rsx! {
        div { class: "scan-results",
            if let Some(msg) = report.message.clone() {
                p { class: "scan-note", "{msg}" }
            }
            div { class: "scan-summary",
                span { class: "scan-stat",
                    span { class: "scan-stat-n", "{report.repos.len()}" }
                    " repos"
                }
                span { class: "scan-stat",
                    span { class: "scan-stat-n", "{report.findings.len()}" }
                    " findings"
                }
                span { class: "scan-stat",
                    span { class: "scan-stat-n high", "{high}" }
                    " high severity"
                }
                span { class: "scan-stat",
                    span { class: "scan-stat-n", "{report.files_scanned}" }
                    " files scanned"
                }
            }

            if !report.stacks.is_empty() {
                div { class: "scan-stacks",
                    for s in report.stacks.iter() {
                        div { class: "scan-stack",
                            span { class: "scan-stack-repo", "{s.repo}" }
                            span { class: "scan-stack-tech",
                                {
                                    let mut tech = s.languages.clone();
                                    tech.extend(s.frameworks.clone());
                                    if tech.is_empty() { "stack not detected".to_string() } else { tech.join(" · ") }
                                }
                            }
                        }
                    }
                }
            }

            p { class: "scan-section-h", "Findings already in these repos" }
            p { class: "scan-section-sub", "What the gate would deny on a new write — already present. Select rows to Ignore, Resolve, or Accept as tech debt (opens a ticket). Sort by repo/severity/type; filter by type or location." }
            FindingsTable { key: "f-{table_key}", findings: report.findings.clone(), repos: report.repos.clone(), descriptions: descriptions.clone() }

            p { class: "scan-section-h", "Proposed starter ruleset" }
            p { class: "scan-section-sub", "Select the rules to arm (each shows its scope, placement, and how many existing violations it catches). You own the final set; arming generates the governance PR." }
            ProposedRulesTable { key: "r-{table_key}", rules: report.proposed_rules.clone() }

            AlternativesPicker { rules: report.proposed_rules.clone(), all_repos: report.repos.clone() }
        }
    }
}

#[component]
fn CockpitTopBar(story: CanonicalStory, connection: Option<ProviderView>) -> Element {
    let (badge, badge_cls) = status_badge(story.status);

    // Real connection status from /api/provider — the thing that matters when
    // wiring GitHub. No fabricated cost meter / agent counts.
    let (conn_cls, conn_label) = match &connection {
        Some(p) if p.live => ("conn-ok", format!("● {}", p.provider)),
        Some(p) => ("conn-warn", format!("● {} (no GitHub token)", p.provider)),
        None => ("conn-warn", "● connecting…".to_string()),
    };

    // SOURCE (where it's tracked) vs BUILD TARGETS (where its code lands) — the
    // two independent axes from the credential-delegated-scope decision.
    let source = match story.external_ref.as_ref() {
        Some(r) => format!("{:?} {}", r.provider, r.external_id),
        None => "native".to_string(),
    };
    let targets = if story.targets.is_empty() {
        "no targets yet".to_string()
    } else {
        story
            .targets
            .iter()
            .map(|t| match &t.role {
                Some(role) => format!("{} ({role})", t.repo),
                None => t.repo.clone(),
            })
            .collect::<Vec<_>>()
            .join(", ")
    };

    rsx! {
        div { class: "cockpit-topbar",
            div { class: "topbar-line1",
                span { class: "topbar-brand", "Camerata · Conductor" }
                span { class: "topbar-story", "{story.title}" }
                span { class: "topbar-status {badge_cls}", "{badge}" }
            }
            div { class: "topbar-line3",
                span { class: "topbar-axis-label", "source:" }
                span { class: "topbar-axis-val", "{source}" }
                span { class: "topbar-sep", "·" }
                span { class: "topbar-axis-label", "targets:" }
                span { class: "topbar-axis-val", "{targets}" }
            }
            div { class: "topbar-line2",
                span { class: "topbar-axis-label", "tracker:" }
                span { class: "{conn_cls}", "{conn_label}" }
            }
        }
    }
}

/// The center-stage body for one lifecycle stage of the selected story. Driven by
/// the (clickable) stage tab index, NOT by fabricated fixtures: it describes what
/// happens at each stage for THIS story, marks stages the story hasn't reached
/// yet, and never invents gate events, diffs, or provenance. Real run activity
/// shows in `LiveRunPanel` once a run is started.
#[component]
fn StagePanel(story: CanonicalStory, stage: usize) -> Element {
    let actual = active_stage_index(story.status);
    let reached = stage <= actual;
    let (name, body) = match stage {
        0 => (
            "Intake",
            "The story is in the spine. From here the architect investigates it, asks the requirements owner any clarifying questions, decomposes it, and runs the governed fleet.",
        ),
        1 => (
            "Investigation",
            "The lead engineer reads the story against repo context and raises clarifying questions via the bridge below. Answers come back from the requirements owner before any code is written.",
        ),
        2 => (
            "Plan",
            "The story is decomposed into component child stories per the team's practice (use the panel below). Each child is independently governable and targets its own repo.",
        ),
        3 => (
            "Execution & gating",
            "The governed fleet runs the work in isolated worktrees; every write passes the gate (deny-before-execute), and each task is re-checked after. Press \u{201c}Run this story\u{201d} above to start it and watch the real verdicts stream in.",
        ),
        4 => (
            "QA & sign-off",
            "Review the produced diff and the gate results, then sign off to ship. Provenance (PR links, gate verdicts, sign-off) is written back to the tracker item.",
        ),
        _ => ("Stage", ""),
    };
    rsx! {
        div { class: "panel-generic",
            p { class: "panel-h", "{story.title}" }
            p { class: "stage-name", "{name}" }
            if !story.description.is_empty() {
                p { class: "panel-sub", "{story.description}" }
            }
            p { class: "panel-sub", "{body}" }
            if !reached {
                p { class: "stage-not-reached",
                    "This stage hasn't been reached yet — the story is currently at "
                    span { class: "stage-not-reached-now", "{STAGE_TABS[actual]}" }
                    "."
                }
            }
        }
    }
}

/// The live governed run: the real gate verdicts from the BFF run engine, streamed
/// in as the run walks to completion.
#[component]
fn LiveRunPanel(run: RunView) -> Element {
    let (status_label, status_cls) = run_status_badge(&run.status);
    let live = run.mode == "live";
    let mode_label = if live { "live fleet" } else { "scripted · token-free" };
    let sub = if live {
        "A real governed fleet (claude -p) under the gate. Stage and bounce events are reported as they happen."
    } else {
        "Token-free run: the agent is scripted, but the gate doing the deciding is the live one. Real deny/allow verdicts."
    };
    rsx! {
        div { class: "live-run",
            div { class: "live-run-head",
                span { class: "live-run-title", "Governed run" }
                span { class: "live-run-mode", "{mode_label}" }
                span { class: "live-run-status {status_cls}", "{status_label}" }
            }
            p { class: "panel-sub", "{sub}" }
            div { class: "live-events",
                for ev in run.events.iter() {
                    {
                        let vcls = match ev.verdict.as_str() {
                            "deny" => "live-event deny",
                            "allow" => "live-event allow",
                            _ => "live-event info",
                        };
                        let vlabel = ev.verdict.to_uppercase();
                        rsx! {
                            div { class: "{vcls}",
                                div { class: "live-event-head",
                                    span { class: "live-event-verdict", "{vlabel}" }
                                    if let Some(rule) = ev.rule.clone() {
                                        span { class: "live-event-rule", "{rule}" }
                                    }
                                }
                                p { class: "live-event-detail", "{ev.detail}" }
                            }
                        }
                    }
                }
                if run.events.is_empty() {
                    p { class: "live-events-empty", "Spinning up the fleet…" }
                }
            }
        }
    }
}

/// The clarify-bridge composer + thread: review a question, pick who to ask (the
/// per-question addressee picker), post it, and record the reply. Wired to the BFF
/// in-process; the live-tracker comment write-back is the provider phase.
#[component]
fn ClarifySection(story_id: String) -> Element {
    // Shared with the NEEDS YOU queue so posting/answering refetches both.
    let mut refresh = use_context::<Signal<u32>>();
    let sid_res = story_id.clone();
    let clars = use_resource(move || {
        let sid = sid_res.clone();
        let _dep = refresh();
        async move { fetch_clarifications(&sid).await }
    });

    let mut question = use_signal(|| {
        "Should the CSV export include archived members, or only currently active ones?"
            .to_string()
    });
    let mut addressee = use_signal(|| "@maria-pm".to_string());

    // Representative suggestions; on a live tracker these come from the ticket's
    // participants (assignee, reporter), plus "you" and a free-typed handle.
    let suggestions = ["@maria-pm", "@jdoe", "you"];

    let sid_post = story_id.clone();

    rsx! {
        div { class: "clarify",
            p { class: "clarify-h", "Ask the team" }
            p { class: "section-hint", "Review the question, pick who to ask, and post it. In-process now; this posts to the real tracker comment (with an @-mention) in the provider phase." }
            textarea {
                class: "clarify-q",
                value: "{question}",
                rows: "2",
                oninput: move |e| question.set(e.value()),
            }
            p { class: "clarify-label", "Ask:" }
            div { class: "clarify-addressees",
                for s in suggestions {
                    {
                        let sel = addressee() == s;
                        let cls = if sel { "addressee-chip sel" } else { "addressee-chip" };
                        rsx! {
                            button {
                                class: "{cls}",
                                onclick: move |_| addressee.set(s.to_string()),
                                "{s}"
                            }
                        }
                    }
                }
                input {
                    class: "addressee-input",
                    placeholder: "or type a handle…",
                    oninput: move |e| addressee.set(e.value()),
                }
            }
            button {
                class: "btn-run",
                onclick: move |_| {
                    let sid = sid_post.clone();
                    let q = question();
                    let a = addressee();
                    spawn(async move {
                        if post_clarification(&sid, &q, &a).await.is_some() {
                            refresh += 1;
                        }
                    });
                },
                "Post the question"
            }

            div { class: "clarify-thread",
                {
                    match clars() {
                        Some(Some(list)) if !list.is_empty() => rsx! {
                            for c in list {
                                ClarificationCard { clar: c, refresh }
                            }
                        },
                        Some(Some(_)) => rsx! { p { class: "section-hint", "No questions posted yet." } },
                        Some(None) => rsx! { p { class: "section-hint", "(Couldn't load the thread.)" } },
                        None => rsx! { p { class: "section-hint", "Loading…" } },
                    }
                }
            }
        }
    }
}

/// One clarification in the thread: shows the question + addressee, an answer input
/// while open, or the recorded reply once answered.
#[component]
fn ClarificationCard(clar: ClarificationView, refresh: Signal<u32>) -> Element {
    let mut refresh = refresh;
    let mut answer_text = use_signal(String::new);
    let open = clar.answer.is_none();
    let cid = clar.id.clone();
    let cls = if open { "clar-card open" } else { "clar-card answered" };

    rsx! {
        div { class: "{cls}",
            p { class: "clar-card-q", "{clar.question}" }
            p { class: "clar-card-meta", "to {clar.addressee}" }
            if open {
                div { class: "clar-answer-row",
                    input {
                        class: "addressee-input",
                        placeholder: "record the reply…",
                        value: "{answer_text}",
                        oninput: move |e| answer_text.set(e.value()),
                    }
                    button {
                        class: "btn-restart",
                        onclick: move |_| {
                            let cid = cid.clone();
                            let ans = answer_text();
                            spawn(async move {
                                if !ans.is_empty()
                                    && answer_clarification(&cid, &ans, "you").await.is_some()
                                {
                                    refresh += 1;
                                }
                            });
                        },
                        "Record answer"
                    }
                }
            } else {
                div { class: "clar-answered",
                    span { class: "clar-answer-by", "{clar.answered_by.clone().unwrap_or_default()} answered" }
                    p { class: "clar-answer-text", "{clar.answer.clone().unwrap_or_default()}" }
                }
            }
        }
    }
}

/// Decompose a parent story into component children: propose, edit titles, create.
/// Created children are real stories on the spine (visible in the left rail on the
/// next mount); the tracker write-back is the provider phase.
#[component]
fn DecomposeSection(story_id: String) -> Element {
    let mut proposed = use_signal(|| Option::<Vec<ProposedChildView>>::None);
    let mut child_refresh = use_signal(|| 0u32);
    let sid_children = story_id.clone();
    let children_res = use_resource(move || {
        let sid = sid_children.clone();
        let _dep = child_refresh();
        async move { fetch_children(&sid).await }
    });

    let sid_propose = story_id.clone();
    let sid_commit = story_id.clone();

    rsx! {
        div { class: "decompose",
            p { class: "clarify-h", "Decompose into component stories" }
            p { class: "section-hint", "Split this feature into the component stories your practice calls for (here: a UI story and an API story). Review and edit, then create. Creating writes them to the tracker as child work items in the provider phase." }
            button {
                class: "btn-run",
                onclick: move |_| {
                    let sid = sid_propose.clone();
                    spawn(async move {
                        if let Some(p) = fetch_proposal(&sid).await {
                            proposed.set(Some(p));
                        }
                    });
                },
                "Propose children"
            }

            {
                match proposed() {
                    Some(list) if !list.is_empty() => rsx! {
                        div { class: "proposed-list",
                            for (i , pc) in list.iter().enumerate() {
                                {
                                    let kind = pc.kind.clone();
                                    let title = pc.title.clone();
                                    rsx! {
                                        div { class: "proposed-child",
                                            span { class: "proposed-kind", "{kind}" }
                                            input {
                                                class: "addressee-input proposed-title",
                                                value: "{title}",
                                                oninput: move |e| {
                                                    if let Some(v) = proposed.write().as_mut() {
                                                        if let Some(item) = v.get_mut(i) {
                                                            item.title = e.value();
                                                        }
                                                    }
                                                },
                                            }
                                        }
                                    }
                                }
                            }
                            button {
                                class: "btn-run",
                                onclick: move |_| {
                                    let sid = sid_commit.clone();
                                    let children = proposed().unwrap_or_default();
                                    spawn(async move {
                                        if commit_children(&sid, &children).await.is_some() {
                                            proposed.set(None);
                                            child_refresh += 1;
                                        }
                                    });
                                },
                                "Create these stories"
                            }
                        }
                    },
                    _ => rsx! {},
                }
            }

            {
                let kids = children_res.read().clone().flatten().unwrap_or_default();
                if kids.is_empty() {
                    rsx! {}
                } else {
                    rsx! {
                        div { class: "children-list",
                            p { class: "clarify-label", "Component stories" }
                            for k in kids.iter() {
                                div { class: "child-row",
                                    span { class: "child-id", "{k.id}" }
                                    span { class: "child-title", "{k.title}" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
