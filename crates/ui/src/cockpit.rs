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
    BadgeVariant, BadgeVariantMap, CellValue, ColumnDef, ColumnId, FilterKind,
    PaginationMode, RenderKind, RowId, TableState,
};
use chorale_dioxus::{
    use_table, CellRenderer, CellRenderers, RowCellRenderer, RowCellRenderers, Table,
};

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
    // Mint row ids ONCE per mount (see ProposedRulesTable): minting in the render body
    // re-ids every render and desyncs selection. The call site keys this component on the
    // ruleset's refresh tick + count, so any add/edit/delete remounts it with fresh rows.
    let rows: Vec<(RowId, CustomRuleView)> = use_hook({
        let custom = custom.clone();
        move || custom.iter().map(|c| (RowId::new(), c.clone())).collect()
    });
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
/// One suppression in the audit registry (`GET /api/projects/:id/suppressions`).
#[derive(Clone, PartialEq, serde::Deserialize)]
struct SuppressionView {
    rule_id: String,
    path: String,
    #[serde(default)]
    line: Option<usize>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    ticket: Option<String>,
    source: String,
    #[serde(default)]
    accepted_by: Option<String>,
    stale: bool,
}

async fn fetch_suppressions(project_id: &str) -> Option<Vec<SuppressionView>> {
    reqwest::get(format!(
        "{}/api/projects/{}/suppressions",
        crate::BFF_URL,
        project_id
    ))
    .await
    .ok()?
    .json::<Vec<SuppressionView>>()
    .await
    .ok()
}

/// The central suppression audit view: everything waived across the project's repos
/// (inline waivers + baseline), with stale ones flagged. The require-indexing invariant.
#[component]
fn SuppressionsPanel(project_id: String) -> Element {
    let pid = project_id.clone();
    let mut loaded = use_signal(|| false);
    let sups = use_resource(move || {
        let pid = pid.clone();
        let _ = loaded();
        async move { fetch_suppressions(&pid).await }
    });
    let list = sups.read().clone().flatten();

    rsx! {
        div { class: "sups-panel",
            div { class: "sups-head",
                p { class: "section-label", "Suppressions — everything waived" }
                button {
                    class: "btn-edit-sm",
                    onclick: move |_| loaded.toggle(),
                    "Refresh"
                }
            }
            p { class: "section-hint", "Inline waivers + baseline entries across this project's repos. Stale ones (no live violation) should be removed." }
            match list {
                None => rsx! { p { class: "section-hint", "Loading… (needs GitHub connected)" } },
                Some(v) if v.is_empty() => rsx! { p { class: "section-hint", "No suppressions recorded." } },
                Some(v) => rsx! {
                    div { class: "sups-list",
                        for (i , s) in v.iter().enumerate() {
                            div { key: "{i}", class: if s.stale { "sup-row stale" } else { "sup-row" },
                                span { class: "sup-rule", "{s.rule_id}" }
                                span { class: "sup-source {s.source}", "{s.source}" }
                                span { class: "sup-loc",
                                    {
                                        match s.line {
                                            Some(l) => format!("{}:{}", s.path, l),
                                            None => s.path.clone(),
                                        }
                                    }
                                }
                                span { class: "sup-reason", {s.reason.clone().unwrap_or_default()} }
                                if let Some(t) = &s.ticket {
                                    span { class: "sup-ticket", "{t}" }
                                }
                                if let Some(who) = &s.accepted_by {
                                    span { class: "sup-who", "{who}" }
                                }
                                if s.stale {
                                    span { class: "sup-stale-tag", "stale" }
                                }
                            }
                        }
                    }
                },
            }
        }
    }
}

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
                    let pid_sup = p.id.clone();
                    rsx! {
                        div { class: "rules-sections",
                            RuleCount { label: "Repo-local rules", n: p.ruleset.selections.len() }
                            RuleCount { label: "Cross-repo rules (API contracts)", n: p.ruleset.cross_repo.len() }
                            RuleCount { label: "Process rules (commit/PR)", n: p.ruleset.process.len() }
                            RuleCount { label: "Custom rules", n: p.ruleset.custom.len() }
                        }

                        SuppressionsPanel { project_id: pid_sup }

                        CiRulesPanel { repos: p.repos.clone() }

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
                            CustomRulesTable { key: "cr-{refresh()}-{p.ruleset.custom.len()}", custom: p.ruleset.custom.clone(), project_id: p.id.clone(), refresh }
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

/// AI-suggested clarifying questions for a story (the engineer's genuine unknowns),
/// for review-then-post.
async fn suggest_clarifications(story_id: &str) -> Vec<String> {
    let Ok(resp) = reqwest::Client::new()
        .post(format!(
            "{}/api/stories/{}/clarify/suggest",
            crate::BFF_URL,
            story_id
        ))
        .send()
        .await
    else {
        return Vec::new();
    };
    resp.json::<Vec<String>>().await.unwrap_or_default()
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

/// The top-level screen of the enterprise app: the projects home, or inside a project.
/// A project CONTAINS everything (repos, ruleset, baseline, workspace), so nothing in
/// the cockpit is reachable until you open one.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CockpitScreen {
    /// The projects home: pick one to open, create a new one, or import.
    Projects,
    /// Inside the active project (the cockpit tabs).
    InProject,
}

/// The shell for the enterprise edition: shows the projects home first; the cockpit only
/// renders once a project is open. The screen is shared via context so the cockpit's nav
/// can navigate back to the projects list.
#[component]
pub fn CockpitShell() -> Element {
    let screen = use_signal(|| CockpitScreen::Projects);
    use_context_provider(|| screen);
    match screen() {
        CockpitScreen::Projects => rsx! { ProjectGate {} },
        CockpitScreen::InProject => rsx! { CockpitApp {} },
    }
}

/// Export a project as a JSON file (native save dialog). Returns true on success.
async fn export_project_json(id: &str) -> bool {
    let Ok(resp) = reqwest::get(format!("{}/api/projects/{}/export", crate::BFF_URL, id)).await
    else {
        return false;
    };
    let Ok(text) = resp.text().await else {
        return false;
    };
    match rfd::AsyncFileDialog::new()
        .set_file_name("camerata-project.json")
        .save_file()
        .await
    {
        Some(file) => file.write(text.as_bytes()).await.is_ok(),
        None => false,
    }
}

/// Delete a project by id. Returns true on success.
async fn delete_project(id: &str) -> bool {
    reqwest::Client::new()
        .delete(format!("{}/api/projects/{}", crate::BFF_URL, id))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Import a project from a JSON file (native open dialog → POST import). The server gives
/// it a fresh id and makes it active. Returns true on success.
async fn import_project_json() -> bool {
    let Some(file) = rfd::AsyncFileDialog::new()
        .add_filter("JSON", &["json"])
        .pick_file()
        .await
    else {
        return false;
    };
    let Ok(text) = String::from_utf8(file.read().await) else {
        return false;
    };
    let Ok(resp) = reqwest::Client::new()
        .post(format!("{}/api/projects/import", crate::BFF_URL))
        .header("content-type", "application/json")
        .body(text)
        .send()
        .await
    else {
        return false;
    };
    resp.json::<serde_json::Value>()
        .await
        .ok()
        .and_then(|v| v.get("ok").and_then(|b| b.as_bool()))
        .unwrap_or(false)
}

/// The projects home: the first thing you see. Open a stored project, create one, or
/// import one. Nothing else in the app is reachable until a project is open.
#[component]
fn ProjectGate() -> Element {
    let mut screen = use_context::<Signal<CockpitScreen>>();
    let mut refresh = use_signal(|| 0u32);
    let projects = use_resource(move || {
        let _ = refresh();
        async move { fetch_projects().await }
    });
    let mut new_name = use_signal(String::new);
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    // The project id awaiting a delete confirm (two-click, with a warning toast).
    let mut pending_delete = use_signal(|| Option::<String>::None);
    let list = projects.read().clone().flatten().unwrap_or_default();

    rsx! {
        div { class: "project-gate",
            div { class: "pg-inner",
                p { class: "eyebrow", "Camerata" }
                h1 { class: "h1", "Your projects" }
                p { class: "lede", "A project is the container for everything — its repos, ruleset, baseline, and workspace. Open one to begin, or create a new one." }

                if list.is_empty() {
                    p { class: "pg-empty", "No projects yet. Create one below to begin." }
                } else {
                    div { class: "pg-list",
                        for p in list.iter() {
                            {
                                let id_export = p.id.clone();
                                let id_open = p.id.clone();
                                let id_del = p.id.clone();
                                let name_del = p.name.clone();
                                let is_pending = pending_delete().as_deref() == Some(p.id.as_str());
                                rsx! {
                                    div { class: "pg-card", key: "{p.id}",
                                        div { class: "pg-card-main",
                                            span { class: "pg-card-name", "{p.name}" }
                                            span { class: "pg-card-meta", "{p.repos.len()} repo(s) · {p.ruleset.selections.len()} repo-rules" }
                                        }
                                        div { class: "pg-card-actions",
                                            button {
                                                class: "pg-btn-secondary",
                                                onclick: move |_| {
                                                    let id = id_export.clone();
                                                    spawn(async move { let _ = export_project_json(&id).await; });
                                                },
                                                "Export"
                                            }
                                            button {
                                                class: if is_pending { "pg-btn-danger confirm" } else { "pg-btn-danger" },
                                                onclick: move |_| {
                                                    let id = id_del.clone();
                                                    if pending_delete().as_deref() == Some(id.as_str()) {
                                                        // Second click — delete.
                                                        pending_delete.set(None);
                                                        spawn(async move {
                                                            if delete_project(&id).await {
                                                                crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, "Project deleted.");
                                                                refresh += 1;
                                                            } else {
                                                                crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, "Could not delete the project.");
                                                            }
                                                        });
                                                    } else {
                                                        // First click — warn + arm the confirm.
                                                        pending_delete.set(Some(id));
                                                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Warning, format!("Click Confirm to permanently delete \u{201c}{name_del}\u{201d}. This can't be undone."));
                                                    }
                                                },
                                                if is_pending { "Confirm delete" } else { "Delete" }
                                            }
                                            button {
                                                class: "btn-run",
                                                onclick: move |_| {
                                                    let id = id_open.clone();
                                                    spawn(async move {
                                                        if set_active_project(&id).await {
                                                            screen.set(CockpitScreen::InProject);
                                                        }
                                                    });
                                                },
                                                "Open"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                div { class: "pg-create",
                    p { class: "section-label", "Create a project" }
                    p { class: "section-hint", "A project starts empty — just a name. You add repos, rules, and everything else from inside it (the way an Azure resource group works)." }
                    div { class: "pg-create-row",
                        input { class: "addressee-input", placeholder: "project name", value: "{new_name}", oninput: move |e| new_name.set(e.value()) }
                        button {
                            class: "btn-run",
                            onclick: move |_| {
                                let name = new_name();
                                if name.trim().is_empty() { return; }
                                spawn(async move {
                                    if create_project(&name, Vec::new()).await.is_some() {
                                        screen.set(CockpitScreen::InProject);
                                    }
                                });
                            },
                            "Create & open"
                        }
                    }
                    button {
                        class: "btn-edit-sm pg-import",
                        onclick: move |_| {
                            spawn(async move {
                                if import_project_json().await {
                                    refresh += 1;
                                    screen.set(CockpitScreen::InProject);
                                }
                            });
                        },
                        "Import project (JSON)…"
                    }
                }
            }
        }
    }
}

/// The cockpit's internal nav: switch between the control surface (stories) and the
/// routine dashboard. Both are architect tools, so both live in the Enterprise app.
#[component]
fn CockpitNav(view: Signal<CockpitView>) -> Element {
    let mut view = view;
    let mut screen = use_context::<Signal<CockpitScreen>>();
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
                class: "cockpit-nav-tab back",
                onclick: move |_| screen.set(CockpitScreen::Projects),
                "← Projects"
            }
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

    // Onboard state lifted to app scope so it SURVIVES navigating between cockpit views:
    // the Phase-1 scan result, and the id of an in-flight async audit job. A background
    // job keeps running server-side regardless; these let the UI re-attach (resume the
    // poll, re-show the scan) when the user returns to Onboard instead of losing it.
    let onboard_scan = use_signal(|| Option::<ScanReportView>::None);
    use_context_provider(|| onboard_scan);
    let active_audit_job = use_signal(|| Option::<String>::None);
    use_context_provider(|| active_audit_job);
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

/// Quote a CSV field if it contains a comma, quote, or newline (RFC 4180).
fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Pop a native save dialog and write `content`. Returns true on success.
async fn save_csv(default_name: &str, content: String) -> bool {
    match rfd::AsyncFileDialog::new()
        .set_file_name(default_name)
        .save_file()
        .await
    {
        Some(file) => file.write(content.as_bytes()).await.is_ok(),
        None => false,
    }
}

/// Build CSV for the audit findings table.
fn findings_csv(findings: &[FindingView]) -> String {
    let mut out = String::from("repo,severity,status,rule_id,path,line,snippet,detail\n");
    for f in findings {
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{}\n",
            csv_field(&f.repo),
            csv_field(&f.severity),
            csv_field(&f.status),
            csv_field(&f.rule_id),
            csv_field(&f.path),
            f.line,
            csv_field(&f.snippet),
            csv_field(&f.detail),
        ));
    }
    out
}

/// Build CSV for the proposed-rules table.
fn rules_csv(rules: &[ProposedRuleView]) -> String {
    let mut out =
        String::from("rule_id,title,kind,scope,enforcement,placement,finding_count,repos\n");
    for r in rules {
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{}\n",
            csv_field(&r.id),
            csv_field(&r.title),
            csv_field(&r.kind),
            csv_field(&r.scope),
            csv_field(&r.enforcement),
            csv_field(&r.placement),
            r.finding_count,
            csv_field(&r.repos.join(" ")),
        ));
    }
    out
}

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
    /// `active` (enforced), `suppressed-inline`, or `suppressed-baseline`.
    #[serde(default = "default_finding_status")]
    status: String,
    /// Other rule ids this same location also violates (the server merged them into this
    /// row). Empty for an un-merged finding. Surfaced as a "+N" on the rule and listed in
    /// the detail modal.
    #[serde(default)]
    also_matches: Vec<String>,
}

fn default_finding_status() -> String {
    "active".to_string()
}

/// Wire the mechanical (CI-tier) governance rules into a repo's CI as a governed dev run.
/// Returns `(run_id, mode)`.
async fn wire_ci_rules(repo: &str) -> Option<(String, String)> {
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/api/onboard/ci-rules", crate::BFF_URL))
        .json(&serde_json::json!({ "repo": repo }))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let run = v.get("run_id")?.as_str()?.to_string();
    let mode = v
        .get("mode")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    Some((run, mode))
}

/// The "wire CI-enforced rules" panel: spawns a governed dev run per repo that
/// implements the declared mechanical rules' CI enforcement (checks what's already
/// enforced, adds what's missing). This is development work, so it runs the same
/// governed pipeline as any dev task. Reused in onboarding and the Rules view.
#[component]
fn CiRulesPanel(repos: Vec<String>) -> Element {
    let mut msg = use_signal(String::new);
    let mut busy = use_signal(|| false);
    rsx! {
        div { class: "fix-panel",
            p { class: "scan-section-h", "Add CI-enforced rules" }
            p { class: "scan-section-sub", "Mechanical rules are declared in .camerata/ci-checks.json at arm time, but a config doesn't enforce itself. This spawns a governed development run that checks what's already enforced in CI and implements the rest (ESLint rule, query-plan/migration audit, AST lint) — same gate-governed pipeline as any dev task." }
            for repo in repos.iter() {
                {
                    let repo = repo.clone();
                    let repo_click = repo.clone();
                    rsx! {
                        div { class: "fix-row", key: "{repo}",
                            span { class: "fix-repo", "{repo}" }
                            button {
                                class: "btn-run",
                                disabled: busy(),
                                onclick: move |_| {
                                    let repo = repo_click.clone();
                                    busy.set(true);
                                    msg.set(String::new());
                                    spawn(async move {
                                        match wire_ci_rules(&repo).await {
                                            Some((run, mode)) => msg.set(format!(
                                                "Started a governed {mode} run ({run}) to wire CI governance for {repo}. Watch it in the control surface (Agent activity)."
                                            )),
                                            None => msg.set(format!("Could not start the CI-wiring run for {repo}.")),
                                        }
                                        busy.set(false);
                                    });
                                },
                                "Wire CI rules (governed)"
                            }
                        }
                    }
                }
            }
            if !msg().is_empty() {
                p { class: "fix-msg", "{msg}" }
            }
        }
    }
}

/// The "fix the audited items" panel: one governed remediation run per repo. Fixing
/// runs through the SAME worktree → gate → layer-2 → bounce pipeline as any dev task.
#[component]
fn FixAuditedPanel(findings: Vec<FindingView>, repos: Vec<String>) -> Element {
    let mut msg = use_signal(String::new);
    let mut busy = use_signal(|| false);
    rsx! {
        div { class: "fix-panel",
            p { class: "scan-section-h", "Fix the audited items" }
            p { class: "scan-section-sub", "Remediation runs as a governed development task — the same worktree → gate → layer-2 checks → bounce loop as any dev work, not a special path. Arm the ruleset first so the fix is held to the rules it installs." }
            for repo in repos.iter() {
                {
                    let repo = repo.clone();
                    let repo_findings: Vec<FindingView> =
                        findings.iter().filter(|f| f.repo == repo).cloned().collect();
                    let n = repo_findings.len();
                    let repo_click = repo.clone();
                    rsx! {
                        div { class: "fix-row", key: "{repo}",
                            span { class: "fix-repo", "{repo}" }
                            span { class: "fix-count", "{n} findings" }
                            button {
                                class: "btn-run",
                                disabled: busy() || n == 0,
                                onclick: move |_| {
                                    let repo = repo_click.clone();
                                    let rf = repo_findings.clone();
                                    busy.set(true);
                                    msg.set(String::new());
                                    spawn(async move {
                                        match fix_audited(&repo, &rf).await {
                                            Some((run, mode)) => msg.set(format!(
                                                "Started a governed {mode} run ({run}) to fix {repo}. Watch each agent's prompt + output in the control surface (Agent activity)."
                                            )),
                                            None => msg.set(format!("Could not start the fix run for {repo}.")),
                                        }
                                        busy.set(false);
                                    });
                                },
                                "Fix (governed)"
                            }
                        }
                    }
                }
            }
            if !msg().is_empty() {
                p { class: "fix-msg", "{msg}" }
            }
        }
    }
}

/// Durable ignore: record the findings as reasoned baseline suppressions (governed PR).
/// Returns the PR URL.
async fn ignore_findings(
    repo: &str,
    findings: &[FindingView],
    reason: &str,
    ticket: Option<String>,
) -> Option<String> {
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/api/onboard/ignore", crate::BFF_URL))
        .json(&serde_json::json!({ "repo": repo, "findings": findings, "reason": reason, "ticket": ticket }))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    if !v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) {
        return None;
    }
    v.get("url").and_then(|u| u.as_str()).map(String::from)
}

/// Fix the audited findings for a repo as a GOVERNED development run (the same
/// worktree → gate → layer-2 pipeline as any dev task). Returns `(run_id, mode)`.
async fn fix_audited(repo: &str, findings: &[FindingView]) -> Option<(String, String)> {
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/api/onboard/fix", crate::BFF_URL))
        .json(&serde_json::json!({ "repo": repo, "findings": findings }))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let run = v.get("run_id")?.as_str()?.to_string();
    let mode = v
        .get("mode")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    Some((run, mode))
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
    domain: String,
    #[serde(default)]
    repos: Vec<String>,
    #[serde(default)]
    placement: String,
    #[serde(default)]
    finding_count: usize,
    #[serde(default)]
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

/// Phase 2 — audit the repos against the selected rules (`(id, directive)` each).
async fn audit_against(
    repos: &[String],
    rules: &[(String, String)],
    model: &str,
    mode: &str,
) -> Option<ScanReportView> {
    let rule_json: Vec<_> = rules
        .iter()
        .map(|(id, directive)| serde_json::json!({ "id": id, "directive": directive }))
        .collect();
    reqwest::Client::new()
        .post(format!("{}/api/onboard/audit", crate::BFF_URL))
        .json(&serde_json::json!({ "repos": repos, "rules": rule_json, "model": model, "mode": mode }))
        .send()
        .await
        .ok()?
        .json::<ScanReportView>()
        .await
        .ok()
}

/// One model the audit selector offers (`GET /api/models`).
#[derive(Clone, PartialEq, serde::Deserialize)]
struct AuditModelOption {
    label: String,
    id: String,
}

#[derive(Clone, PartialEq, serde::Deserialize)]
struct AuditModelsResp {
    models: Vec<AuditModelOption>,
    #[serde(default)]
    default: String,
}

async fn fetch_audit_models() -> Option<AuditModelsResp> {
    reqwest::get(format!("{}/api/models", crate::BFF_URL))
        .await
        .ok()?
        .json::<AuditModelsResp>()
        .await
        .ok()
}

/// A polled async-audit job (`GET /api/onboard/audit/job/:id`).
#[derive(Clone, PartialEq, serde::Deserialize, Default)]
struct JobStateView {
    #[serde(default)]
    status: String,
    #[serde(default)]
    done: usize,
    #[serde(default)]
    total: usize,
    #[serde(default)]
    findings: Vec<FindingView>,
    #[serde(default)]
    report: Option<ScanReportView>,
    #[serde(default)]
    message: Option<String>,
}

/// Mode 3: START an async audit job, returning its id (the request returns immediately).
async fn audit_job_start(
    repos: &[String],
    rules: &[(String, String)],
    model: &str,
    exec_mode: &str,
) -> Option<String> {
    let rule_json: Vec<_> = rules
        .iter()
        .map(|(id, directive)| serde_json::json!({ "id": id, "directive": directive }))
        .collect();
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/api/onboard/audit/start", crate::BFF_URL))
        .json(&serde_json::json!({ "repos": repos, "rules": rule_json, "model": model, "mode": exec_mode }))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    v.get("job_id").and_then(|j| j.as_str()).map(String::from)
}

/// Recommend a scan mode by the scanned codebase's SCALE (the design's auto-select):
/// multi-repo or a large codebase → Background job (decoupled, walk-away); otherwise
/// Parallel (fast enough to wait on). Sequential is never auto-recommended — it's a manual
/// gentle/debug override. The user can always change it.
fn recommend_scan_mode(report: &ScanReportView) -> String {
    if report.repos.len() > 1 || report.files_scanned > 150 {
        "job".to_string()
    } else {
        "parallel".to_string()
    }
}

/// Poll an async audit job for progress + incremental findings + the final report.
async fn audit_job_poll(job_id: &str) -> Option<JobStateView> {
    reqwest::get(format!("{}/api/onboard/audit/job/{}", crate::BFF_URL, job_id))
        .await
        .ok()?
        .json::<Option<JobStateView>>()
        .await
        .ok()
        .flatten()
}

/// Drive an async audit job to completion: poll every ~1.5s, update progress + (on done) the
/// final report, clearing the shared `active_audit_job` so a later mount doesn't re-resume.
/// Shared by the manual start AND the resume-on-mount path. Gives up after a few misses (the
/// job vanished, e.g. the server restarted) so it can't spin forever.
async fn poll_job(
    jid: String,
    mut audit: Signal<Option<ScanReportView>>,
    mut auditing: Signal<bool>,
    mut job_progress: Signal<Option<(usize, usize, usize)>>,
    mut active_audit_job: Signal<Option<String>>,
) {
    let mut misses = 0u32;
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        match audit_job_poll(&jid).await {
            Some(js) => {
                misses = 0;
                job_progress.set(Some((js.done, js.total, js.findings.len())));
                match js.status.as_str() {
                    "done" => {
                        audit.set(js.report);
                        auditing.set(false);
                        job_progress.set(None);
                        active_audit_job.set(None);
                        break;
                    }
                    "failed" => {
                        auditing.set(false);
                        job_progress.set(None);
                        active_audit_job.set(None);
                        break;
                    }
                    _ => {}
                }
            }
            None => {
                misses += 1;
                if misses >= 3 {
                    auditing.set(false);
                    job_progress.set(None);
                    active_audit_job.set(None);
                    break;
                }
            }
        }
    }
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
async fn arm_rules(rules: &[ArmRuleReq], findings: &[FindingView]) -> Option<Vec<ArmResultView>> {
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/api/onboard/arm", crate::BFF_URL))
        .json(&serde_json::json!({ "rules": rules, "findings": findings }))
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

fn finding_columns(repos: Vec<String>) -> Vec<ColumnDef<FindingView>> {
    // Fallback badge map only. The severity column is actually drawn by a custom cell
    // renderer in FindingsTable (which overrides RenderKind::Badge) so High can be ORANGE,
    // distinct from Critical's red — chorale's built-in palette has no orange and would
    // collapse both to red. This map is the safety net if that renderer is ever absent.
    let sev = BadgeVariantMap::new()
        .with("critical", BadgeVariant::new("Critical", "red"))
        .with("high", BadgeVariant::new("High", "red"))
        .with("medium", BadgeVariant::new("Medium", "yellow"))
        .with("low", BadgeVariant::new("Low", "gray"));
    vec![
        ColumnDef::new(ColumnId("repo"), "Repo", |f: &FindingView| {
            CellValue::Text(f.repo.clone())
        })
        .sortable()
        .filter(FilterKind::MultiSelect { options: repos })
        .initial_width(180.0),
        ColumnDef::new(ColumnId("severity"), "Severity", |f: &FindingView| {
            CellValue::Text(f.severity.clone())
        })
        .sortable()
        .filter(FilterKind::MultiSelect {
            options: vec![
                "critical".to_string(),
                "high".to_string(),
                "medium".to_string(),
                "low".to_string(),
            ],
        })
        .render_kind(RenderKind::Badge(sev))
        .initial_width(110.0),
        // AUTHORITY, not just provenance: a deterministic rule hit is ENFORCED
        // (high-confidence, gateable, auto-fix-eligible); an AI finding is ADVISORY
        // (investigative, review-only, never auto-blocks or auto-fixes without a human).
        // This is the enforcement-vs-convention split rendered as a column. AI findings
        // carry an `AI-` rule id.
        ColumnDef::new(ColumnId("authority"), "Authority", |f: &FindingView| {
            CellValue::Text(if f.rule_id.starts_with("AI-") {
                "advisory".to_string()
            } else {
                "enforced".to_string()
            })
        })
        .sortable()
        .filter(FilterKind::MultiSelect {
            options: vec!["enforced".to_string(), "advisory".to_string()],
        })
        .render_kind(RenderKind::Badge(
            BadgeVariantMap::new()
                // chorale badges support green/yellow/red/gray; blue/purple fell back to a
                // single default gray, making the two authorities indistinguishable.
                .with("enforced", BadgeVariant::new("Rule · enforced", "green"))
                .with("advisory", BadgeVariant::new("AI · advisory", "yellow")),
        ))
        .initial_width(170.0),
        ColumnDef::new(ColumnId("type"), "Finding type", |f: &FindingView| {
            CellValue::Text(f.rule_id.clone())
        })
        .sortable()
        // String lookup, not multi-select: rule ids are many and the architect typically
        // wants "show me everything matching ARCH-" or a specific id, not a checkbox list.
        .filter(FilterKind::Text)
        .initial_width(250.0),
        // The ratchet: enforced (active = new/changed) vs suppressed (baseline debt or
        // an inline waiver). Report shows all; the gate blocks only the enforced ones.
        ColumnDef::new(ColumnId("status"), "Enforcement", |f: &FindingView| {
            CellValue::Text(match f.status.as_str() {
                "suppressed-baseline" => "baseline".to_string(),
                "suppressed-inline" => "waived".to_string(),
                _ => "enforced".to_string(),
            })
        })
        .sortable()
        .render_kind(RenderKind::Badge(
            BadgeVariantMap::new()
                .with("enforced", BadgeVariant::new("Enforced", "red"))
                .with("baseline", BadgeVariant::new("Baseline debt", "gray"))
                .with("waived", BadgeVariant::new("Waived", "yellow")),
        ))
        .initial_width(150.0),
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

fn rule_columns(domains: Vec<String>) -> Vec<ColumnDef<ProposedRuleView>> {
    let kind = BadgeVariantMap::new()
        .with("mechanical", BadgeVariant::new("Mechanical", "green"))
        .with("review", BadgeVariant::new("Review", "yellow"));
    let scope = BadgeVariantMap::new()
        .with("repo-local", BadgeVariant::new("Repo-local", "green"))
        .with("cross-repo", BadgeVariant::new("Cross-repo", "yellow"))
        .with("process", BadgeVariant::new("Process", "gray"));
    vec![
        // The group-by column (the table groups on this). A rule's corpus domain —
        // sql / api-layer / ui / security / architecture / process / integration.
        ColumnDef::new(ColumnId("domain"), "Domain", |r: &ProposedRuleView| {
            CellValue::Text(if r.domain.is_empty() {
                "general".to_string()
            } else {
                r.domain.clone()
            })
        })
        .sortable()
        // Multi-select: a rule has exactly one domain, so exact-match MultiSelect lets the
        // architect tick the domains they care about (sql + api-layer, say) and see only those.
        .filter(FilterKind::MultiSelect { options: domains })
        .initial_width(150.0),
        // Suggested = the rule's domain matched the scanned stack; the rest are the full
        // library, available to arm but not recommended for this stack.
        ColumnDef::new(ColumnId("suggested"), "For this stack", |r: &ProposedRuleView| {
            CellValue::Text(if r.recommended {
                "suggested".to_string()
            } else {
                "available".to_string()
            })
        })
        .sortable()
        .render_kind(RenderKind::Badge(
            BadgeVariantMap::new()
                .with("suggested", BadgeVariant::new("Suggested", "green"))
                .with("available", BadgeVariant::new("Available", "gray")),
        ))
        .initial_width(130.0),
        ColumnDef::new(ColumnId("id"), "Rule", |r: &ProposedRuleView| {
            CellValue::Text(r.id.clone())
        })
        .sortable()
        .filter(FilterKind::Text)
        .initial_width(280.0),
        ColumnDef::new(ColumnId("scope"), "Scope", |r: &ProposedRuleView| {
            CellValue::Text(r.scope.clone())
        })
        .sortable()
        .render_kind(RenderKind::Badge(scope))
        .initial_width(130.0),
        // Show EVERY repo this rule applies to (comma-joined), not a "N repos" collapse —
        // the Text filter matches by substring, so typing one repo surfaces every row that
        // references it regardless of which OTHER repos share the cell ("contains", not the
        // exact-combo match a MultiSelect would impose). That's the per-repo "show anywhere
        // this repo is referenced" behavior the multi-repo case needs.
        ColumnDef::new(ColumnId("repos"), "Applies to", |r: &ProposedRuleView| {
            CellValue::Text(if r.repos.is_empty() {
                "—".to_string()
            } else {
                r.repos.join(", ")
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
    // Default order leads triage with what matters: enforced (new) before suppressed
    // (debt/waived), then by severity (critical → high → medium → low). A flat 200-row dump
    // is paralysis; this floats the exploitable-bug criticals to the very top so a
    // hardcoded secret can never sit below "no mappers crate."
    let mut findings = findings;
    findings.sort_by_key(|f| {
        let enforced = if f.status == "active" { 0 } else { 1 };
        let sev = match f.severity.as_str() {
            "critical" => 0,
            "high" => 1,
            "medium" => 2,
            _ => 3,
        };
        (enforced, sev)
    });
    // Distinct repos for the repo multi-select filter. (Finding type is a Text/contains
    // filter now, so it needs no precomputed option list.)
    let mut filter_repos: Vec<String> = findings.iter().map(|f| f.repo.clone()).collect();
    filter_repos.sort();
    filter_repos.dedup();
    // Mint row ids ONCE per mount (see ProposedRulesTable): `RowId::new()` in the render
    // body would re-id every render and desync the Table from id_map/selection. This
    // component remounts on each new audit (it's gated behind `audited.is_some()` and the
    // re-audit clears it first), so freezing rows per mount tracks fresh findings while
    // keeping ids stable within a mount.
    let rows: Vec<(RowId, FindingView)> = use_hook({
        let findings = findings.clone();
        move || findings.iter().map(|f| (RowId::new(), f.clone())).collect()
    });
    let id_map: std::collections::HashMap<RowId, FindingView> =
        rows.iter().map(|(r, f)| (*r, f.clone())).collect();
    let id_map_click = id_map.clone();
    // Row click opens the finding-detail modal (hosted by ScanResults, OUTSIDE this table's
    // subtree — same reason as the rule modal). Shows the violated rule's full directive +
    // the complete, untruncated explanation that the row cell clips.
    let mut detail_finding = use_context::<Signal<Option<FindingView>>>();
    let handle = use_table(move || {
        TableState::new(rows.clone(), finding_columns(filter_repos.clone()))
    });
    // Group findings BY FINDING TYPE so same-kind violations sit together (a flat 200-row
    // dump is paralysis); severity / authority / repo / type filter via multi-select.
    use_hook(move || {
        handle.set_grouping(vec![ColumnId("type")]);
        handle.set_pagination_mode(PaginationMode::InfiniteScroll);
        let _ = handle.set_page_size(5000);
    });
    let mut busy = use_signal(|| false);
    // A durable ignore requires a reason (the require-reason invariant); optional ticket.
    let mut ignore_reason = use_signal(String::new);
    let mut ignore_ticket = use_signal(String::new);
    // Separate clones for the ignore button (the tech-debt button moves the originals).
    let id_map_ig = id_map.clone();
    let repo_ig = target_repo.clone();
    // The (sorted) rows for CSV export.
    let csv_rows = findings.clone();

    let renderers = {
        let mut m: std::collections::HashMap<ColumnId, CellRenderer> =
            std::collections::HashMap::new();
        // Severity badge with a per-level palette. chorale's badge map only has
        // green/yellow/red/gray, so Critical AND High both landed on red and looked
        // identical (the whole reason for the red row-stripe). A custom renderer (which
        // overrides the column's RenderKind::Badge) gives High its own ORANGE, keeping
        // Critical the strongest red. Orange routes through a --chorale-badge-orange-*
        // var with an orange fallback, so it works today and auto-adopts a future
        // chorale orange (rust-chorale#33).
        m.insert(
            ColumnId("severity"),
            std::sync::Arc::new(move |val: &CellValue| {
                let sev = match val {
                    CellValue::Text(s) => s.clone(),
                    _ => String::new(),
                };
                let (label, bg, fg): (&str, &str, &str) = match sev.as_str() {
                    "critical" => (
                        "Critical",
                        "var(--chorale-badge-red-bg, #fee2e2)",
                        "var(--chorale-badge-red-text, #991b1b)",
                    ),
                    "high" => (
                        "High",
                        "var(--chorale-badge-orange-bg, #ffedd5)",
                        "var(--chorale-badge-orange-text, #9a3412)",
                    ),
                    "medium" => (
                        "Medium",
                        "var(--chorale-badge-yellow-bg, #fef3c7)",
                        "var(--chorale-badge-yellow-text, #92400e)",
                    ),
                    "low" => (
                        "Low",
                        "var(--chorale-badge-gray-bg, #f3f4f6)",
                        "var(--chorale-badge-gray-text, #374151)",
                    ),
                    other => (
                        other,
                        "var(--chorale-badge-default-bg, #e5e7eb)",
                        "var(--chorale-badge-default-text, #1f2937)",
                    ),
                };
                let style = format!(
                    "display:inline-block;padding:0.125rem 0.5rem;border-radius:9999px;\
                     background:{bg};color:{fg};font-size:0.75rem;font-weight:500;"
                );
                rsx! { span { style: "{style}", "{label}" } }
            }) as CellRenderer,
        );
        CellRenderers::new(m)
    };

    // SECURITY findings (the deterministic floor — the only tier ranked "critical") get a
    // bold red full-height stripe on the left of their row, so they're unmistakable beyond
    // the badge text. A row-aware renderer on the first column ("repo") draws an
    // absolutely-positioned bar (the chorale <td> is position:relative) for critical rows,
    // leaving every other cell — including the Severity badge — untouched.
    let row_renderers = {
        let mut m: std::collections::HashMap<ColumnId, RowCellRenderer<FindingView>> =
            std::collections::HashMap::new();
        m.insert(
            ColumnId("repo"),
            std::sync::Arc::new(move |f: &FindingView, val: &CellValue| {
                let repo = match val {
                    CellValue::Text(s) => s.clone(),
                    _ => String::new(),
                };
                let is_security = f.severity == "critical";
                rsx! {
                    if is_security {
                        span { class: "crit-row-stripe" }
                    }
                    "{repo}"
                }
            }) as RowCellRenderer<FindingView>,
        );
        // "Finding type": the primary rule id, with a hover tooltip of what it enforces, and
        // a "+N" chip when the server merged N other rules at this same location into the row
        // (the also_matches set). Row-aware so it can read both the description map and the
        // also_matches off the FindingView. Tooltip lists the demoted rule ids too.
        let desc = descriptions.clone();
        m.insert(
            ColumnId("type"),
            std::sync::Arc::new(move |f: &FindingView, val: &CellValue| {
                let rid = match val {
                    CellValue::Text(s) => s.clone(),
                    _ => String::new(),
                };
                let mut tip = desc.get(&rid).cloned().unwrap_or_else(|| rid.clone());
                if !f.also_matches.is_empty() {
                    tip = format!("{tip}\n\nAlso violates here: {}", f.also_matches.join(", "));
                }
                let extra = f.also_matches.len();
                rsx! {
                    span { title: "{tip}", "{rid}" }
                    if extra > 0 {
                        span { class: "finding-also-count", title: "{tip}", " +{extra}" }
                    }
                }
            }) as RowCellRenderer<FindingView>,
        );
        RowCellRenderers::new(m)
    };

    rsx! {
        // Key: what the red stripe means. Security (deterministic, Critical) vs the rest.
        div { class: "findings-key",
            span { class: "findings-key-item",
                span { class: "findings-key-swatch crit" }
                "Security findings (deterministic, stop-the-line)"
            }
            span { class: "findings-key-item",
                span { class: "findings-key-swatch arch" }
                "Architectural findings (everything else)"
            }
        }
        div { class: "findings-toolbar",
            input {
                class: "addressee-input ignore-reason",
                placeholder: "reason to ignore (required)",
                value: "{ignore_reason}",
                oninput: move |e| ignore_reason.set(e.value()),
            }
            input {
                class: "addressee-input ignore-ticket",
                placeholder: "ticket (optional)",
                value: "{ignore_ticket}",
                oninput: move |e| ignore_ticket.set(e.value()),
            }
            button {
                class: "btn-restart",
                disabled: busy(),
                onclick: move |_| {
                    let sel = handle.selected_ids();
                    let picked: Vec<FindingView> = sel.iter().filter_map(|id| id_map_ig.get(id).cloned()).collect();
                    if picked.is_empty() { return; }
                    let reason = ignore_reason();
                    if reason.trim().is_empty() {
                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Warning, "A reason is required to ignore a finding (it's recorded in the baseline).");
                        return;
                    }
                    let repo = repo_ig.clone();
                    let ticket = { let t = ignore_ticket(); if t.trim().is_empty() { None } else { Some(t) } };
                    busy.set(true);
                    spawn(async move {
                        match ignore_findings(&repo, &picked, &reason, ticket).await {
                            Some(url) => {
                                crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("Recorded {} ignore(s) in the baseline (PR): {url}", picked.len()));
                                handle.remove_rows(&sel);
                            }
                            None => crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, "Couldn't record the ignore — needs GitHub + Contents/PR write."),
                        }
                        busy.set(false);
                    });
                },
                "Ignore selected (with reason)"
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
            button {
                class: "btn-edit-sm",
                onclick: move |_| {
                    let csv = findings_csv(&csv_rows);
                    spawn(async move { let _ = save_csv("camerata-findings.csv", csv).await; });
                },
                "Export CSV"
            }
        }
        Table {
            handle,
            sort_enabled: true,
            filter_enabled: true,
            selection_enabled: true,
            resize_enabled: true,
            // Pin the column header to the top of the table's scroll viewport so it
            // stays visible while scrolling a long findings list.
            sticky_header: true,
            cell_renderers: renderers,
            row_cell_renderers: row_renderers,
            on_row_click: Callback::new(move |rid: RowId| {
                if let Some(f) = id_map_click.get(&rid) {
                    detail_finding.set(Some(f.clone()));
                }
            }),
        }
    }
}


/// The proposed-rules table with SELECTION (chorale checkboxes) — accept/reject
/// each rule into the approved starter set.
#[component]
fn ProposedRulesTable(
    rules: Vec<ProposedRuleView>,
    findings: Vec<FindingView>,
    on_audit: EventHandler<Vec<(String, String)>>,
    auditing: bool,
) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let chosen = use_context::<Signal<std::collections::HashMap<String, String>>>();
    let placement = use_context::<Signal<std::collections::HashMap<String, Vec<String>>>>();
    // Mint the row ids ONCE and persist them. `RowId::new()` is random, so doing this
    // in the render body would generate fresh ids every re-render: the Table keeps the
    // ids from its first render (use_table runs its initializer once), while id_map /
    // domain_rows / selected_set would rebuild with NEW ids that no longer match. That
    // desync is exactly why the multi-select and the table didn't share selection and
    // why "select at least one rule" fired with rules visibly ticked. use_hook persists
    // the same Vec (same RowIds) for the life of the component.
    let rows: Vec<(RowId, ProposedRuleView)> = use_hook({
        let rules = rules.clone();
        move || rules.iter().map(|r| (RowId::new(), r.clone())).collect()
    });
    let id_map: std::collections::HashMap<RowId, ProposedRuleView> =
        rows.iter().map(|(r, p)| (*r, p.clone())).collect();
    let id_map_audit = id_map.clone();
    // Suggested rows (pre-selected on load) and a domain -> rows map (per-domain
    // "select all" chips). Both derived BEFORE use_table consumes `rows`.
    let suggested_ids: Vec<RowId> =
        rows.iter().filter(|(_, p)| p.recommended).map(|(r, _)| *r).collect();
    let mut domain_rows: std::collections::BTreeMap<String, Vec<RowId>> = Default::default();
    for (rid, p) in &rows {
        let d = if p.domain.is_empty() { "general".to_string() } else { p.domain.clone() };
        domain_rows.entry(d).or_default().push(*rid);
    }
    // Distinct domains (sorted, "general" for blank — matches the cell value) for the
    // Domain column's multi-select filter options.
    let domain_options: Vec<String> = domain_rows.keys().cloned().collect();
    let handle =
        use_table(move || TableState::new(rows.clone(), rule_columns(domain_options.clone())));
    // The row-detail modal is hosted by ScanResults (OUTSIDE this table's subtree)
    // via a shared signal: a row click writes the rule here, ScanResults renders the
    // modal. Hosting the full-screen overlay outside the Table avoids a Dioxus-desktop
    // quirk where mounting it as a SIBLING of the table left a ghost node that
    // swallowed the next click, so the modal wouldn't reopen after being closed.
    let mut detail_rule = use_context::<Signal<Option<ProposedRuleView>>>();
    let id_map_click = id_map.clone();
    // Group BY DOMAIN, switch to INFINITE SCROLL (not paginated), LOAD EVERY rule so
    // selection/audit cover all domains (paginated select-all only grabbed the first
    // page — that's why whole domains like api-layer were missing from the audit),
    // and PRE-SELECT the suggested rules. Once, on mount.
    use_hook(move || {
        handle.set_grouping(vec![ColumnId("domain")]);
        // Start every domain group COLLAPSED: the rule list is long and the architect
        // scans domain-by-domain, expanding only the ones they're triaging. Must run
        // AFTER set_grouping so the group keys exist to collapse.
        handle.collapse_all_groups();
        handle.set_pagination_mode(PaginationMode::InfiniteScroll);
        let _ = handle.set_page_size(5000);
        for rid in &suggested_ids {
            handle.set_selection(*rid, true);
        }
    });
    let mut arming = use_signal(|| false);
    let mut domain_panel_open = use_signal(|| false);
    let arm_findings = findings;
    let csv_rules = rules.clone();

    // Current selection as a set, read reactively so each domain's checkbox reflects
    // whether ALL of that domain's rows are selected (tri-state-ish: checked only when
    // every row in the domain is selected).
    let selected_set: std::collections::HashSet<RowId> =
        handle.selected_ids().into_iter().collect();

    rsx! {
        // Per-domain "select all" — styled like a column's multi-select filter: a
        // trigger that opens a FIXED-HEIGHT, scrollable checkbox list (so 100 domains
        // don't blow up the layout). Checking a domain selects every rule in it across
        // all pages; unchecking clears them.
        div { class: "domain-select",
            button {
                class: "domain-select-trigger",
                onclick: move |_| { let o = domain_panel_open(); domain_panel_open.set(!o); },
                "Select rules by domain "
                span { class: "domain-select-caret", if domain_panel_open() { "\u{25B4}" } else { "\u{25BE}" } }
            }
            if domain_panel_open() {
                div { class: "domain-select-panel",
                    for (domain , rids) in domain_rows.iter() {
                        {
                            let rids = rids.clone();
                            let all_selected = !rids.is_empty()
                                && rids.iter().all(|r| selected_set.contains(r));
                            rsx! {
                                label { key: "{domain}", class: "domain-select-item",
                                    input {
                                        r#type: "checkbox",
                                        checked: all_selected,
                                        onchange: move |_| {
                                            let target = !all_selected;
                                            for rid in &rids { handle.set_selection(*rid, target); }
                                        },
                                    }
                                    span { class: "domain-select-name", "{domain}" }
                                    span { class: "domain-select-count", "{rids.len()}" }
                                }
                            }
                        }
                    }
                }
            }
        }
        Table {
            handle,
            sort_enabled: true,
            selection_enabled: true,
            filter_enabled: true,
            on_row_click: Callback::new(move |rid: RowId| {
                if let Some(r) = id_map_click.get(&rid) {
                    detail_rule.set(Some(r.clone()));
                }
            }),
        }
        div { class: "findings-toolbar",
            button {
                class: "btn-run",
                disabled: auditing,
                onclick: move |_| {
                    // Read the SELECTED rows at click time and audit ONLY those (their
                    // chosen/default directive). The audit must scan against the picked
                    // subset, never all proposed rules.
                    let sel = handle.selected_ids();
                    let picked: Vec<ProposedRuleView> = sel.iter().filter_map(|id| id_map_audit.get(id).cloned()).collect();
                    if picked.is_empty() {
                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Warning, "Select at least one rule (tick its checkbox) to audit against.");
                        return;
                    }
                    let chosen_rules: Vec<(String, String)> = picked.iter().map(|r| {
                        let directive = if r.options.is_empty() {
                            r.title.clone()
                        } else {
                            let oid = chosen.read().get(&r.id).cloned().or_else(|| r.default_option.clone());
                            oid.and_then(|o| r.options.iter().find(|x| x.id == o).map(|x| x.directive.clone()))
                                .filter(|s| !s.is_empty())
                                .unwrap_or_else(|| r.title.clone())
                        };
                        (r.id.clone(), directive)
                    }).collect();
                    on_audit.call(chosen_rules);
                },
                if auditing {
                    span { class: "spinner" }
                    "Auditing\u{2026}"
                } else {
                    "Audit code against selected rules"
                }
            }
            button {
                class: "btn-edit-sm",
                onclick: move |_| {
                    let csv = rules_csv(&csv_rules);
                    spawn(async move { let _ = save_csv("camerata-proposed-rules.csv", csv).await; });
                },
                "Export CSV"
            }
            button {
                class: "btn-run",
                disabled: arming(),
                onclick: move |_| {
                    let sel = handle.selected_ids();
                    let picked: Vec<ProposedRuleView> = sel.iter().filter_map(|id| id_map.get(id).cloned()).collect();
                    if picked.is_empty() {
                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Warning, "Select at least one rule (tick its checkbox) before arming.");
                        return;
                    }
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
                    let findings = arm_findings.clone();
                    spawn(async move {
                        match arm_rules(&arm_reqs, &findings).await {
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

/// The row-click detail modal: full rule context + alternative picker. Hosted by
/// `ScanResults` so it renders OUTSIDE the chorale table's subtree — see the note in
/// `ProposedRulesTable` for why (a sibling-of-table overlay left a ghost click-eater
/// in the desktop webview, breaking modal reopen). Reads the open rule and the
/// per-rule choice from context.
#[component]
fn RuleDetailModal() -> Element {
    let mut detail_rule = use_context::<Signal<Option<ProposedRuleView>>>();
    let chosen = use_context::<Signal<std::collections::HashMap<String, String>>>();
    let Some(r) = detail_rule() else {
        return rsx! {};
    };
    rsx! {
        div { class: "rule-modal-overlay", onclick: move |_| detail_rule.set(None),
            div { class: "rule-modal", onclick: move |e| e.stop_propagation(),
                div { class: "rule-modal-head",
                    span { class: "rule-modal-id", "{r.id}" }
                    button { class: "rule-modal-close", onclick: move |_| detail_rule.set(None), "\u{2715}" }
                }
                p { class: "rule-modal-title", "{r.title}" }
                div { class: "rule-modal-meta",
                    span { class: "rule-modal-tag", "domain · {r.domain}" }
                    span { class: "rule-modal-tag", "scope · {r.scope}" }
                    span { class: "rule-modal-tag", "kind · {r.kind}" }
                }
                p { class: "rule-modal-placement", "Enforced via: {r.placement}" }
                if r.options.is_empty() {
                    p { class: "rule-modal-note", "Single-variant rule — nothing to choose; arm it as-is." }
                } else {
                    p { class: "rule-modal-label", "Choose the alternative to adopt" }
                    div { class: "rule-modal-opts",
                        for o in r.options.iter() {
                            {
                                let rid = r.id.clone();
                                let oid = o.id.clone();
                                let cur = chosen.read().get(&r.id).cloned().or_else(|| r.default_option.clone());
                                let picked = cur.as_deref() == Some(o.id.as_str());
                                let cls = if picked { "rule-opt on" } else { "rule-opt" };
                                let mut chosen = chosen;
                                rsx! {
                                    button {
                                        key: "{o.id}",
                                        class: "{cls}",
                                        onclick: move |_| { chosen.write().insert(rid.clone(), oid.clone()); },
                                        span { class: "rule-opt-label", "{o.label}" }
                                        span { class: "rule-opt-directive", "{o.directive}" }
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
/// The outcome of trying to derive a repo from a navigated-to folder.
enum RepoDetect {
    /// The user cancelled the dialog.
    Cancelled,
    /// Derived `owner/repo`.
    Found(String),
    /// Couldn't derive one — carries a human reason for a toast.
    Failed(String),
}

/// Let the user NAVIGATE to a local repo folder; derive its `owner/repo` from the git
/// origin remote (server-side).
async fn detect_local_repo() -> RepoDetect {
    let Some(folder) = rfd::AsyncFileDialog::new()
        .set_title("Choose a local repo folder")
        .pick_folder()
        .await
    else {
        return RepoDetect::Cancelled;
    };
    let path = folder.path().to_string_lossy().to_string();
    let resp = match reqwest::Client::new()
        .post(format!("{}/api/git/detect-repo", crate::BFF_URL))
        .json(&serde_json::json!({ "path": path }))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return RepoDetect::Failed(format!("couldn't reach the local server ({e})")),
    };
    let v: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => return RepoDetect::Failed("unexpected response from the server".to_string()),
    };
    if v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) {
        match v.get("repo").and_then(|r| r.as_str()) {
            Some(r) => RepoDetect::Found(r.to_string()),
            None => RepoDetect::Failed("no repo in the response".to_string()),
        }
    } else {
        RepoDetect::Failed(
            v.get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("could not detect a repo in that folder")
                .to_string(),
        )
    }
}

#[component]
fn OnboardView(connection: Option<ProviderView>) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let mut path = use_signal(|| OnboardPath::Brownfield);
    let mut repo = use_signal(String::new);
    // The scan result lives in app-scope context so it survives leaving + returning to this
    // view (a running audit job keeps going server-side; the scan shouldn't vanish).
    let mut scan = use_context::<Signal<Option<ScanReportView>>>();
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
                    class: "btn-edit-sm onboard-browse",
                    onclick: move |_| {
                        spawn(async move {
                            match detect_local_repo().await {
                                RepoDetect::Cancelled => {}
                                RepoDetect::Found(found) => {
                                    let mut cur = repo();
                                    let exists = cur.split([',', '\n']).any(|s| s.trim() == found);
                                    if exists {
                                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("{found} is already in the list."));
                                    } else {
                                        if !cur.trim().is_empty() && !cur.ends_with('\n') {
                                            cur.push('\n');
                                        }
                                        cur.push_str(&found);
                                        repo.set(cur);
                                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("Added {found} from the folder."));
                                    }
                                }
                                RepoDetect::Failed(msg) => {
                                    crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, format!("Couldn't read that folder: {msg}. It must be a git repo cloned from GitHub — or paste owner/repo above instead."));
                                }
                            }
                        });
                    },
                    "Browse for a local repo folder\u{2026}"
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
    // Phase 2 audit result (findings against the selected rules); None until the
    // architect picks rules and runs the audit.
    let mut audit = use_signal(|| Option::<ScanReportView>::None);
    let mut auditing = use_signal(|| false);
    // The model the user picks for the audit — they own the thoroughness/speed trade-off.
    // Company-agnostic list comes from the server (`/api/models`); seed from its default.
    let models_res = use_resource(fetch_audit_models);
    let models = models_res.read().clone().flatten();
    let mut audit_model = use_signal(String::new);
    if audit_model().is_empty() {
        if let Some(m) = &models {
            if !m.default.is_empty() {
                audit_model.set(m.default.clone());
            }
        }
    }
    // Scan mode (user-facing): "parallel" (default), "sequential" (gentle), or "job"
    // (async — submit, walk away, poll). Job uses parallel execution + async delivery.
    // AUTO-SELECTED by the scan's scale; the user can override.
    let recommended_mode = recommend_scan_mode(&report);
    let mut audit_mode = use_signal(|| recommended_mode.clone());
    // Live progress for an async job: (passes done, passes total, findings so far).
    let mut job_progress = use_signal(|| Option::<(usize, usize, usize)>::None);
    // The in-flight async job id (app-scope, survives navigation). RESUME: if a job was
    // already running when this view (re)mounted, re-attach the poll instead of losing it.
    let active_audit_job = use_context::<Signal<Option<String>>>();
    use_future(move || async move {
        if let Some(jid) = active_audit_job.peek().clone() {
            auditing.set(true);
            poll_job(jid, audit, auditing, job_progress, active_audit_job).await;
        }
    });
    let audited = audit.read().clone();
    let findings: Vec<FindingView> = audited
        .as_ref()
        .map(|a| a.findings.clone())
        .unwrap_or_default();

    // "High severity" stat covers the top two tiers (critical + high) so the exploitable
    // criticals are never invisible in the summary.
    let high = findings
        .iter()
        .filter(|f| f.severity == "critical" || f.severity == "high")
        .count();
    let enforced = findings.iter().filter(|f| f.status == "active").count();
    let suppressed = findings.len().saturating_sub(enforced);

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

    // The open row-detail rule, shared with ProposedRulesTable. Provided HERE (not in
    // the table) so the modal renders at this subtree's root, outside the chorale
    // table — see RuleDetailModal / ProposedRulesTable for the reopen-bug rationale.
    let detail_rule = use_signal(|| Option::<ProposedRuleView>::None);
    use_context_provider(|| detail_rule);

    // The open finding (row-click) — same host-outside-the-table pattern, shared with
    // FindingsTable. The modal shows the violated rule's directive + the full detail.
    let detail_finding = use_signal(|| Option::<FindingView>::None);
    use_context_provider(|| detail_finding);

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

    let descriptions_modal = descriptions.clone();
    rsx! {
        // Row-detail modal: hosted here, at the results-subtree root, so it is NOT a
        // sibling of the chorale table (see RuleDetailModal for why).
        RuleDetailModal {}
        // Finding-detail modal: click any findings row to read the violated rule's full
        // directive + the complete explanation the row cell truncates.
        if let Some(f) = detail_finding() {
            {
                let directive = descriptions_modal.get(&f.rule_id).cloned();
                let mut detail_finding = detail_finding;
                rsx! {
                    div { class: "rule-modal-overlay", onclick: move |_| detail_finding.set(None),
                        div { class: "rule-modal", onclick: move |e| e.stop_propagation(),
                            div { class: "rule-modal-head",
                                span { class: "rule-modal-id", "{f.rule_id}" }
                                button { class: "rule-modal-close", onclick: move |_| detail_finding.set(None), "\u{2715}" }
                            }
                            div { class: "rule-modal-meta",
                                span { class: "rule-modal-tag", "severity · {f.severity}" }
                                span { class: "rule-modal-tag", "{f.path}:{f.line}" }
                                span { class: "rule-modal-tag", "{f.status}" }
                            }
                            if let Some(d) = directive {
                                p { class: "rule-modal-label", "Rule violated" }
                                p { class: "rule-modal-detail", "{d}" }
                            }
                            p { class: "rule-modal-label", "Finding" }
                            p { class: "rule-modal-title", "{f.snippet}" }
                            p { class: "rule-modal-label", "Explanation" }
                            p { class: "rule-modal-detail", "{f.detail}" }
                            if !f.also_matches.is_empty() {
                                p { class: "rule-modal-label", "Also violates at this location" }
                                div { class: "rule-modal-meta",
                                    for rid in f.also_matches.iter() {
                                        span { class: "rule-modal-tag", "{rid}" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
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
                    span { class: "scan-stat-n", "{findings.len()}" }
                    " findings"
                }
                span { class: "scan-stat",
                    span { class: "scan-stat-n high", "{high}" }
                    " high severity"
                }
                span { class: "scan-stat",
                    span { class: "scan-stat-n high", "{enforced}" }
                    " enforced (new)"
                }
                span { class: "scan-stat",
                    span { class: "scan-stat-n", "{suppressed}" }
                    " suppressed (debt/waived)"
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

            // ── Phase 1 result: the proposed ruleset to pick from ──────────────
            p { class: "scan-section-h", "Step 1 — proposed starter ruleset" }
            p { class: "scan-section-sub", "Camerata mapped the stack and proposes these rules. Pick the ones to enforce and choose alternatives; you own the final set (arming generates the governance PR)." }
            p { class: "scan-section-sub", "Click a rule row to read its full context and choose its alternative." }
            {
                let repos_audit = report.repos.clone();
                rsx! {
                    ProposedRulesTable {
                        rules: report.proposed_rules.clone(),
                        findings: findings.clone(),
                        auditing: auditing(),
                        on_audit: move |rules: Vec<(String, String)>| {
                            let repos = repos_audit.clone();
                            let model = audit_model();
                            let mode = audit_mode();
                            // Clear the PREVIOUS run's findings so a re-audit starts from a
                            // blank Findings table instead of showing stale results while
                            // the new audit runs (the server also clears the transcript).
                            audit.set(None);
                            job_progress.set(None);
                            auditing.set(true);
                            if mode == "job" {
                                // Async job: submit, record the id (app-scope, so a later
                                // mount can resume), then poll. The server runs it decoupled
                                // from any single request.
                                let mut active_audit_job = active_audit_job;
                                spawn(async move {
                                    let Some(jid) = audit_job_start(&repos, &rules, &model, "parallel").await else {
                                        auditing.set(false);
                                        return;
                                    };
                                    active_audit_job.set(Some(jid.clone()));
                                    poll_job(jid, audit, auditing, job_progress, active_audit_job).await;
                                });
                            } else {
                                // Synchronous: hold the request until the (shorter) run finishes.
                                spawn(async move {
                                    audit.set(audit_against(&repos, &rules, &model, &mode).await);
                                    auditing.set(false);
                                });
                            }
                        },
                    }
                }
            }

            // ── Phase 2: the audit runs from the table's "Audit selected" button ──
            div { class: "audit-cta",
                p { class: "scan-section-h", "Step 2 — audit the code against your selected rules" }
                p { class: "scan-section-sub", "Tick the rules above, then press “Audit code against selected rules”. The deterministic security rules (secrets / raw-SQL / secret-URLs) always run as the enforced floor; the AI checks the code against ONLY your selected rules AND flags anything else worth a look (advisory)." }
                // Model picker — the user owns the speed/thoroughness trade-off. List is
                // company-agnostic, served by /api/models.
                if let Some(m) = models.as_ref() {
                    div { class: "audit-model-row",
                        label { class: "audit-model-label", "Audit model" }
                        select {
                            class: "audit-model-select",
                            disabled: auditing(),
                            value: "{audit_model}",
                            onchange: move |e| audit_model.set(e.value()),
                            for opt in m.models.iter() {
                                option { key: "{opt.id}", value: "{opt.id}", "{opt.label}" }
                            }
                        }
                        span { class: "audit-model-hint", "Faster models finish sooner; stronger models catch more." }
                    }
                }
                // Execution mode — speed/scale knob, separate from the model (quality) and
                // the rule selection (coverage). Parallel is the recommended default.
                div { class: "audit-model-row",
                    label { class: "audit-model-label", "Scan mode" }
                    select {
                        class: "audit-model-select",
                        disabled: auditing(),
                        value: "{audit_mode}",
                        onchange: move |e| audit_mode.set(e.value()),
                        option { value: "parallel", "Parallel" }
                        option { value: "sequential", "Sequential (slower, gentlest)" }
                        option { value: "job", "Background job (walk away)" }
                    }
                    if audit_mode() == recommended_mode {
                        span { class: "audit-mode-rec", "✓ auto-selected for this scan's size" }
                    }
                    span { class: "audit-model-hint", "Parallel runs rule-batches concurrently. Background job runs server-side so you can leave and watch findings stream in — best for huge / multi-repo scans." }
                }
                // Live progress for an async job: a determinate bar (it grows as repos are
                // discovered) + a findings-so-far count, so a walk-away scan shows life.
                if let Some((done, total, nf)) = job_progress() {
                    {
                        let pct = (done * 100).checked_div(total).unwrap_or(0).min(100);
                        rsx! {
                            div { class: "job-progress",
                                div { class: "job-progress-track",
                                    div { class: "job-progress-fill", style: "width: {pct}%" }
                                }
                                span { class: "job-progress-label", "{done}/{total} passes · {nf} finding(s) so far" }
                            }
                        }
                    }
                }
                // While the audit runs, the Bombe turns — a visible "the AI is thinking"
                // cue so a multi-second audit doesn't look hung.
                if auditing() {
                    div { class: "audit-thinking",
                        crate::bombe::BombeSpinner { title: "Camerata is auditing\u{2026}".to_string() }
                        span { class: "audit-thinking-label", "Camerata is auditing your code\u{2026}" }
                    }
                }
                // Live feedback: open this to watch the AI's actual prompt + output for
                // the audit (so you can trust it's really working, not hung).
                crate::agent_activity::AgentActivity { run_id: "scan-audit".to_string() }
            }

            // ── Findings (after the audit runs) ────────────────────────────────
            if audited.is_some() {
                p { class: "scan-section-h", "Findings" }
                p { class: "scan-section-sub", "Select rows to Ignore, Resolve, or Accept as tech debt. Sort by severity/authority; filter by type or location." }
                p { class: "scan-domains-note",
                    b { "Two domains, two authorities. " }
                    "“Rule · enforced” findings are deterministic rule violations — high-confidence, gateable, eligible for auto-fix. “AI · advisory” findings are the investigative layer — the agent thinks something's worth a look. They are review-only: they never auto-block work or auto-fix without you confirming. Enforcement is mechanical; advice needs the architect."
                }
                FindingsTable { findings: findings.clone(), repos: report.repos.clone(), descriptions: descriptions.clone() }

                FixAuditedPanel { findings: findings.clone(), repos: report.repos.clone() }
            }

            CiRulesPanel { repos: report.repos.clone() }
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

    // AI-suggested clarifying questions (the engineer's genuine unknowns), reviewed
    // before any is dropped into the composer and posted.
    let mut ai_questions = use_signal(Vec::<String>::new);
    let mut suggesting = use_signal(|| false);
    let sid_suggest = story_id.clone();

    let sid_post = story_id.clone();

    rsx! {
        div { class: "clarify",
            p { class: "clarify-h", "Ask the team" }
            p { class: "section-hint", "Review the question, pick who to ask, and post it. In-process now; this posts to the real tracker comment (with an @-mention) in the provider phase." }
            div { class: "clarify-suggest-row",
                button {
                    class: "btn-edit-sm",
                    disabled: suggesting(),
                    onclick: move |_| {
                        let sid = sid_suggest.clone();
                        suggesting.set(true);
                        spawn(async move {
                            ai_questions.set(suggest_clarifications(&sid).await);
                            suggesting.set(false);
                        });
                    },
                    if suggesting() { "Thinking…" } else { "Suggest questions (AI)" }
                }
                span { class: "section-hint", "The lead engineer lists what it genuinely needs answered — click one to load it." }
            }
            if !ai_questions().is_empty() {
                div { class: "clarify-suggestions",
                    for (i , q) in ai_questions().iter().enumerate() {
                        {
                            let qq = q.clone();
                            rsx! {
                                button {
                                    key: "{i}",
                                    class: "clarify-suggestion",
                                    onclick: move |_| question.set(qq.clone()),
                                    "{q}"
                                }
                            }
                        }
                    }
                }
            }
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
