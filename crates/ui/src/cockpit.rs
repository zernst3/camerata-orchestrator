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
    use_table, RowCellRenderer, RowCellRenderers, RowClass, Table,
};

/// One enforced gate rule, as returned by the BFF `/api/rules` endpoint (GOV-1 is
/// filtered out server-side). The cockpit just renders what the BFF returns.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
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

#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
struct RuleSelectionView {
    rule_id: String,
    #[serde(default)]
    chosen_option: Option<String>,
    #[serde(default)]
    repos: Vec<String>,
}

#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
struct CustomRuleView {
    name: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    domain: String,
}

#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Default)]
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

#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
struct ProjectView {
    id: String,
    name: String,
    #[serde(default)]
    repos: Vec<String>,
    #[serde(default)]
    ruleset: RulesetView,
    /// Repos that have been onboarded (`owner/repo`). A repo not here is "not yet onboarded".
    #[serde(default)]
    onboarded: Vec<String>,
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

#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
struct AppliedOptionView {
    id: String,
    label: String,
    #[serde(default)]
    directive: String,
}

#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
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

/// Fetch ALL corpus rules (the full rule library, with full context).
async fn fetch_corpus_rules() -> Option<Vec<ProposedRuleView>> {
    reqwest::get(format!("{}/api/corpus-rules", crate::BFF_URL))
        .await
        .ok()?
        .json::<Vec<ProposedRuleView>>()
        .await
        .ok()
}

/// Persist a full ruleset (read-modify-write). Always includes the existing `custom` array
/// unchanged — callers must pass it through from the current project.
async fn save_ruleset(project_id: &str, ruleset: serde_json::Value) -> bool {
    reqwest::Client::new()
        .post(format!("{}/api/projects/{}/ruleset", crate::BFF_URL, project_id))
        .json(&ruleset)
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
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
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

/// One repo's local-path resolution status (issue #33), from `/api/projects/:id/repo-health`.
#[derive(Clone, PartialEq, serde::Deserialize)]
struct RepoResolutionView {
    repo: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    resolved: bool,
    #[serde(default)]
    reason: String,
}

async fn fetch_repo_health(project_id: &str) -> Option<Vec<RepoResolutionView>> {
    let v: serde_json::Value = reqwest::get(format!(
        "{}/api/projects/{}/repo-health",
        crate::BFF_URL,
        project_id
    ))
    .await
    .ok()?
    .json()
    .await
    .ok()?;
    serde_json::from_value(v.get("repos")?.clone()).ok()
}

async fn set_repo_path(repo: &str, path: &str) -> bool {
    reqwest::Client::new()
        .post(format!("{}/api/repo-path", crate::BFF_URL))
        .json(&serde_json::json!({ "repo": repo, "path": path }))
        .send()
        .await
        .is_ok()
}

/// The broken-path health check (issue #33): for each of a project's repos, shows whether it
/// resolves to a local git checkout, with a per-repo "Resolve…" folder picker for the broken
/// ones. Refreshes on mount and after a resolve. Shown wherever a project's repos matter
/// (the Rules view today); the same data backs an import's "resolve paths" prompt.
#[component]
fn RepoHealthPanel(project_id: String) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let mut refresh = use_signal(|| 0u32);
    let pid = project_id.clone();
    let health = use_resource(move || {
        let pid = pid.clone();
        let _ = refresh();
        async move { fetch_repo_health(&pid).await }
    });
    let repos = health.read().clone().flatten().unwrap_or_default();
    if repos.is_empty() {
        return rsx! {};
    }
    let broken = repos.iter().filter(|r| !r.resolved).count();
    rsx! {
        div { class: "repo-health",
            if broken == 0 {
                p { class: "repo-health-ok", "✓ All {repos.len()} repo path(s) resolve to a local checkout." }
            } else {
                div { class: "repo-health-warn",
                    span { class: "repo-health-warn-h", "⚠ {broken} repo path(s) need resolving" }
                    p { class: "section-hint", "These repos don't point at a local git checkout on this machine (common right after importing a project). Resolve each before working on it." }
                }
            }
            for r in repos.iter() {
                {
                    let repo = r.repo.clone();
                    let resolved = r.resolved;
                    let reason = r.reason.clone();
                    let path = r.path.clone().unwrap_or_default();
                    rsx! {
                        div { class: "repo-health-row", key: "{r.repo}",
                            span { class: if resolved { "repo-health-icon ok" } else { "repo-health-icon bad" },
                                if resolved { "✓" } else { "⚠" }
                            }
                            span { class: "repo-health-repo", "{r.repo}" }
                            if resolved {
                                span { class: "repo-health-path", "{path}" }
                            } else {
                                span { class: "repo-health-reason", "{reason}" }
                                button {
                                    class: "btn-edit-sm",
                                    onclick: move |_| {
                                        let repo = repo.clone();
                                        spawn(async move {
                                            if let Some(folder) = rfd::AsyncFileDialog::new()
                                                .set_title("Choose this repo's local folder")
                                                .pick_folder()
                                                .await
                                            {
                                                let p = folder.path().to_string_lossy().to_string();
                                                if set_repo_path(&repo, &p).await {
                                                    refresh += 1;
                                                } else {
                                                    crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, "Couldn't save the repo path.");
                                                }
                                            }
                                        });
                                    },
                                    "Resolve…"
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

// ── Rules view: two editable chorale tables ────────────────────────────────

/// Build the ruleset JSON body for a `POST /api/projects/{id}/ruleset` call.
/// Merges the current project's selections with the provided override and
/// always preserves the custom rules array unchanged.
fn build_ruleset_json(project: &ProjectView) -> serde_json::Value {
    serde_json::json!({
        "selections": project.ruleset.selections.iter().map(|s| serde_json::json!({
            "rule_id": s.rule_id, "chosen_option": s.chosen_option, "repos": s.repos
        })).collect::<Vec<_>>(),
        "cross_repo": project.ruleset.cross_repo.iter().map(|s| serde_json::json!({
            "rule_id": s.rule_id, "chosen_option": s.chosen_option, "repos": s.repos
        })).collect::<Vec<_>>(),
        "process": project.ruleset.process.iter().map(|s| serde_json::json!({
            "rule_id": s.rule_id, "chosen_option": s.chosen_option, "repos": s.repos
        })).collect::<Vec<_>>(),
        "custom": project.ruleset.custom.iter().map(|c| serde_json::json!({
            "name": c.name, "body": c.body, "domain": c.domain
        })).collect::<Vec<_>>(),
    })
}

/// Which of the three lists a rule_id lives in (selections / cross_repo / process).
#[derive(Clone, Copy, PartialEq, Eq)]
enum SelectionBucket { Selections, CrossRepo, Process }

fn bucket_of(rule: &ProposedRuleView) -> SelectionBucket {
    match rule.scope.as_str() {
        "cross-repo" => SelectionBucket::CrossRepo,
        "process"    => SelectionBucket::Process,
        _            => SelectionBucket::Selections,
    }
}

/// A row for Table 1: a selection from the project's ruleset, joined with the full
/// corpus rule for title / domain / scope / options.
#[derive(Clone, PartialEq)]
struct AppliedRuleRow {
    /// From the ruleset selection.
    selection: RuleSelectionView,
    /// Scope bucket (selections / cross_repo / process) — drives "applies to all repos" label.
    bucket: SelectionBucket,
    /// Full corpus rule (may be None for custom / unknown ids).
    corpus: Option<ProposedRuleView>,
}

impl AppliedRuleRow {
    fn domain(&self) -> String {
        self.corpus.as_ref().map(|r| r.domain.clone()).filter(|s| !s.is_empty()).unwrap_or_else(|| "general".to_string())
    }
    fn title(&self) -> String {
        self.corpus.as_ref().map(|r| r.title.clone()).unwrap_or_else(|| self.selection.rule_id.clone())
    }
    fn scope_label(&self) -> &'static str {
        match self.bucket {
            SelectionBucket::Selections => "repo-local",
            SelectionBucket::CrossRepo  => "cross-repo",
            SelectionBucket::Process    => "process",
        }
    }
    fn chosen_label(&self) -> String {
        match (&self.selection.chosen_option, &self.corpus) {
            (Some(oid), Some(rule)) => rule.options.iter()
                .find(|o| &o.id == oid)
                .map(|o| o.label.clone())
                .unwrap_or_else(|| oid.clone()),
            _ => String::from("\u{2014}"),
        }
    }
}

fn applied_rule_columns() -> Vec<ColumnDef<AppliedRuleRow>> {
    let scope_badges = BadgeVariantMap::new()
        .with("repo-local", BadgeVariant::new("Repo-local", "green"))
        .with("cross-repo", BadgeVariant::new("Cross-repo", "yellow"))
        .with("process",    BadgeVariant::new("Process", "gray"));
    vec![
        ColumnDef::new(ColumnId("domain"), "Domain", |r: &AppliedRuleRow| {
            CellValue::Text(r.domain())
        })
        .sortable()
        .filter(FilterKind::Text)
        .initial_width(140.0),
        ColumnDef::new(ColumnId("rule"), "Rule", |r: &AppliedRuleRow| {
            CellValue::Text(format!("{} — {}", r.selection.rule_id, r.title()))
        })
        .sortable()
        .filter(FilterKind::Text)
        .initial_width(300.0),
        ColumnDef::new(ColumnId("scope"), "Scope", |r: &AppliedRuleRow| {
            CellValue::Text(r.scope_label().to_string())
        })
        .sortable()
        .render_kind(RenderKind::Badge(scope_badges))
        .initial_width(130.0),
        ColumnDef::new(ColumnId("repos"), "Applies to", |r: &AppliedRuleRow| {
            if r.bucket != SelectionBucket::Selections {
                CellValue::Text("all repos".to_string())
            } else {
                CellValue::Text(if r.selection.repos.is_empty() { "\u{2014}".to_string() } else { r.selection.repos.join(", ") })
            }
        })
        .filter(FilterKind::Text)
        .initial_width(200.0),
        ColumnDef::new(ColumnId("option"), "Chosen option", |r: &AppliedRuleRow| {
            CellValue::Text(r.chosen_label())
        })
        .sortable()
        .initial_width(180.0),
    ]
}

fn corpus_columns() -> Vec<ColumnDef<ProposedRuleView>> {
    let scope_badges = BadgeVariantMap::new()
        .with("repo-local", BadgeVariant::new("Repo-local", "green"))
        .with("cross-repo", BadgeVariant::new("Cross-repo", "yellow"))
        .with("process",    BadgeVariant::new("Process", "gray"));
    vec![
        ColumnDef::new(ColumnId("domain"), "Domain", |r: &ProposedRuleView| {
            CellValue::Text(if r.domain.is_empty() { "general".to_string() } else { r.domain.clone() })
        })
        .sortable()
        .filter(FilterKind::Text)
        .initial_width(140.0),
        ColumnDef::new(ColumnId("rule"), "Rule", |r: &ProposedRuleView| {
            CellValue::Text(format!("{} — {}", r.id, r.title))
        })
        .sortable()
        .filter(FilterKind::Text)
        .initial_width(300.0),
        ColumnDef::new(ColumnId("scope"), "Scope", |r: &ProposedRuleView| {
            CellValue::Text(r.scope.clone())
        })
        .sortable()
        .render_kind(RenderKind::Badge(scope_badges))
        .initial_width(130.0),
        ColumnDef::new(ColumnId("applied_to"), "Applied to", |r: &ProposedRuleView| {
            CellValue::Text(if r.repos.is_empty() { String::new() } else { r.repos.join(", ") })
        })
        .filter(FilterKind::Text)
        .initial_width(220.0),
    ]
}

/// Table 1 — the project's applied rules (selections + cross_repo + process).
/// Filterable by repo; each row is clickable to open the shared `RuleDetailModal`.
/// After an option pick in the modal the ruleset is POSTed. "Remove from repo"
/// removes the current repo from the selection's repos list (or drops the selection
/// when repos becomes empty).
#[component]
fn ProjectRulesTable(
    project: ProjectView,
    corpus: Vec<ProposedRuleView>,
    refresh: Signal<u32>,
    /// Which repo Table 2 wants Table 1 to jump to (set by "Go to repo rule").
    #[props(default)] goto_repo: Signal<Option<String>>,
) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let mut detail_rule = use_context::<Signal<Option<ProposedRuleView>>>();
    let mut chosen_ctx = use_context::<Signal<std::collections::HashMap<String, String>>>();

    // Current repo filter ("" = all repos).
    let mut repo_filter = use_signal(String::new);

    // Consume the goto_repo signal from Table 2: read the value, then clear in a second step.
    use_effect(move || {
        let maybe_repo = goto_repo.read().clone();
        if let Some(repo) = maybe_repo {
            repo_filter.set(repo);
            goto_repo.write().take();
        }
    });

    let corpus_by_id: std::collections::HashMap<String, ProposedRuleView> =
        corpus.iter().map(|r| (r.id.clone(), r.clone())).collect();

    // Build the flat list of applied rule rows.
    let rows_data: Vec<AppliedRuleRow> = {
        let mut out = Vec::new();
        for sel in &project.ruleset.selections {
            out.push(AppliedRuleRow {
                selection: sel.clone(),
                bucket: SelectionBucket::Selections,
                corpus: corpus_by_id.get(&sel.rule_id).cloned(),
            });
        }
        for sel in &project.ruleset.cross_repo {
            out.push(AppliedRuleRow {
                selection: sel.clone(),
                bucket: SelectionBucket::CrossRepo,
                corpus: corpus_by_id.get(&sel.rule_id).cloned(),
            });
        }
        for sel in &project.ruleset.process {
            out.push(AppliedRuleRow {
                selection: sel.clone(),
                bucket: SelectionBucket::Process,
                corpus: corpus_by_id.get(&sel.rule_id).cloned(),
            });
        }
        out
    };

    // Apply repo filter: cross_repo/process always show; selections filter by repo.
    let filtered: Vec<AppliedRuleRow> = {
        let rf = repo_filter();
        if rf.is_empty() {
            rows_data.clone()
        } else {
            rows_data
                .iter()
                .filter(|r| {
                    r.bucket != SelectionBucket::Selections
                        || r.selection.repos.iter().any(|rp| rp == &rf)
                })
                .cloned()
                .collect()
        }
    };

    // Mint RowIds ONCE per (project ruleset + filter) mount. Key the component on the
    // refresh tick so a project update remounts with fresh rows.
    let rows: Vec<(RowId, AppliedRuleRow)> = use_hook({
        let filtered = filtered.clone();
        move || filtered.iter().map(|r| (RowId::new(), r.clone())).collect()
    });
    let id_map: std::collections::HashMap<RowId, AppliedRuleRow> =
        rows.iter().map(|(id, r)| (*id, r.clone())).collect();
    let id_map_click = id_map.clone();
    let id_map_remove = id_map.clone();

    let handle = use_table(move || TableState::new(rows.clone(), applied_rule_columns()));
    use_hook(move || {
        handle.set_pagination_mode(PaginationMode::InfiniteScroll);
        let _ = handle.set_page_size(2000);
    });

    let project_repos = project.repos.clone();
    let project_id = project.id.clone();
    let project_for_remove = project.clone();

    // After the user picks an option in RuleDetailModal (via chosen_ctx), persist it.
    // We watch chosen_ctx and when it changes relative to what's saved, POST the ruleset.
    let project_for_opt = project.clone();
    let pid_opt = project_id.clone();
    let toasts_opt = toasts;
    let mut refresh_opt = refresh;
    use_effect(move || {
        // Build the updated ruleset with any chosen-option changes applied.
        let chosen = chosen_ctx.read().clone();
        let mut p = project_for_opt.clone();
        let mut changed = false;
        for sel in p.ruleset.selections.iter_mut() {
            if let Some(opt) = chosen.get(&sel.rule_id) {
                if sel.chosen_option.as_deref() != Some(opt.as_str()) {
                    sel.chosen_option = Some(opt.clone());
                    changed = true;
                }
            }
        }
        for sel in p.ruleset.cross_repo.iter_mut() {
            if let Some(opt) = chosen.get(&sel.rule_id) {
                if sel.chosen_option.as_deref() != Some(opt.as_str()) {
                    sel.chosen_option = Some(opt.clone());
                    changed = true;
                }
            }
        }
        for sel in p.ruleset.process.iter_mut() {
            if let Some(opt) = chosen.get(&sel.rule_id) {
                if sel.chosen_option.as_deref() != Some(opt.as_str()) {
                    sel.chosen_option = Some(opt.clone());
                    changed = true;
                }
            }
        }
        if changed {
            let body = build_ruleset_json(&p);
            let pid = pid_opt.clone();
            spawn(async move {
                if save_ruleset(&pid, body).await {
                    crate::toast::push_toast(toasts_opt, crate::toast::ToastKind::Info, "Option saved.");
                    refresh_opt += 1;
                } else {
                    crate::toast::push_toast(toasts_opt, crate::toast::ToastKind::Error, "Could not save the option choice.");
                }
            });
        }
    });

    let rf = repo_filter();
    rsx! {
        // Repo filter bar — mirrors the onboarding "Repo ruleset:" selector.
        div { class: "repo-select",
            label { class: "repo-select-label", "Filter by repo:" }
            select {
                class: "repo-select-input",
                value: "{rf}",
                onchange: move |e| repo_filter.set(e.value()),
                option { value: "", "All repos" }
                for repo in project_repos.iter() {
                    option { key: "{repo}", value: "{repo}", "{repo}" }
                }
            }
            span { class: "repo-select-hint",
                "Repo-local rules filter to the selected repo. Cross-repo and process rules always show (project-level)."
            }
        }

        Table {
            handle,
            sort_enabled: true,
            filter_enabled: true,
            selection_enabled: true,
            sticky_header: true,
            on_row_click: Callback::new(move |rid: RowId| {
                if let Some(row) = id_map_click.get(&rid) {
                    if let Some(corpus_rule) = &row.corpus {
                        detail_rule.set(Some(corpus_rule.clone()));
                        // Seed chosen_ctx so the modal shows the current selection.
                        if let Some(opt) = &row.selection.chosen_option {
                            chosen_ctx.write().insert(corpus_rule.id.clone(), opt.clone());
                        }
                    }
                }
            }),
        }

        // Remove-from-repo action: removes the currently-filtered repo from the selection's
        // repos list; if repos becomes empty, the selection is dropped entirely.
        div { class: "rules-table-toolbar",
            button {
                class: "btn-restart",
                title: if rf.is_empty() { "Select a repo filter first to remove a rule from a specific repo" } else { "Remove selected rules from the current repo filter" },
                disabled: rf.is_empty(),
                onclick: move |_| {
                    let sel = handle.selected_ids();
                    if sel.is_empty() { return; }
                    let repo_to_remove = repo_filter();
                    if repo_to_remove.is_empty() { return; }
                    let rows: Vec<AppliedRuleRow> = sel.iter()
                        .filter_map(|id| id_map_remove.get(id).cloned())
                        .collect();
                    let mut p = project_for_remove.clone();
                    let pid = project_id.clone();
                    let mut changed = false;
                    // Update selections.
                    for row in &rows {
                        if row.bucket == SelectionBucket::Selections {
                            if let Some(s) = p.ruleset.selections.iter_mut().find(|s| s.rule_id == row.selection.rule_id) {
                                s.repos.retain(|r| r != &repo_to_remove);
                                changed = true;
                            }
                        }
                    }
                    // Drop selections with no repos left.
                    p.ruleset.selections.retain(|s| !s.repos.is_empty());
                    if changed {
                        let body = build_ruleset_json(&p);
                        let mut refresh = refresh;
                        spawn(async move {
                            if save_ruleset(&pid, body).await {
                                crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("Removed rule(s) from {repo_to_remove}."));
                                refresh += 1;
                            } else {
                                crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, "Could not update the ruleset.");
                            }
                        });
                    }
                },
                "Remove selected from \u{201c}{rf}\u{201d}"
            }
            span { class: "rules-table-hint",
                "Click a row to edit the chosen option. Select rows + Remove to drop them from a repo."
            }
        }
    }
}

/// Table 2 — the full corpus, with "Applied to" chips and an "Add to repo" control
/// per row. Clicking "Go to repo rule" sets the Table-1 repo filter and focuses
/// that view.
#[component]
fn AllRulesTable(
    project: ProjectView,
    corpus: Vec<ProposedRuleView>,
    refresh: Signal<u32>,
    /// Signal Table 1 to switch its repo filter to this repo.
    goto_repo: Signal<Option<String>>,
) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let mut detail_rule = use_context::<Signal<Option<ProposedRuleView>>>();
    let mut chosen_ctx = use_context::<Signal<std::collections::HashMap<String, String>>>();

    // Build a map: rule_id -> Vec<repo> it's currently applied to.
    let applied_repos: std::collections::HashMap<String, Vec<String>> = {
        let mut m: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
        for sel in project.ruleset.selections.iter()
            .chain(project.ruleset.cross_repo.iter())
            .chain(project.ruleset.process.iter())
        {
            m.entry(sel.rule_id.clone()).or_default().extend(sel.repos.clone());
        }
        m
    };
    // Annotate the corpus with its currently-applied repos for the table column.
    let annotated: Vec<ProposedRuleView> = corpus.iter().map(|r| {
        let mut rv = r.clone();
        rv.repos = applied_repos.get(&r.id).cloned().unwrap_or_default();
        // deduplicate
        rv.repos.sort();
        rv.repos.dedup();
        rv
    }).collect();

    // Mint row ids ONCE per mount.
    let rows: Vec<(RowId, ProposedRuleView)> = use_hook({
        let annotated = annotated.clone();
        move || annotated.iter().map(|r| (RowId::new(), r.clone())).collect()
    });
    let id_map: std::collections::HashMap<RowId, ProposedRuleView> =
        rows.iter().map(|(id, r)| (*id, r.clone())).collect();
    let id_map_click = id_map.clone();

    let handle = use_table(move || TableState::new(rows.clone(), corpus_columns()));
    use_hook(move || {
        handle.set_pagination_mode(PaginationMode::InfiniteScroll);
        let _ = handle.set_page_size(5000);
    });

    let project_repos = project.repos.clone();
    let project_id = project.id.clone();

    // The "Add to repo" action, handled below the table (not in a row renderer — row
    // renderers are Arc<dyn Fn...> + Send + Sync and can't capture Dioxus signals).
    // Signal<Option<(rule_id, repo)>>: set by the per-rule selects; use_effect acts on it.
    let mut add_pending: Signal<Option<(String, String)>> = use_signal(|| None);
    let project_for_add = project.clone();
    let pid_add = project_id.clone();
    let mut refresh_add = refresh;
    use_effect(move || {
        let maybe = add_pending.read().clone();
        if let Some((rule_id, repo)) = maybe {
            add_pending.set(None);
            let mut p = project_for_add.clone();
            let pid = pid_add.clone();
            // Scope + default option come from the corpus.
            let corpus_rule = corpus.iter().find(|r| r.id == rule_id).cloned();
            let default_opt = corpus_rule.as_ref().and_then(|r| r.default_option.clone());
            let scope_bucket = corpus_rule.as_ref().map(bucket_of).unwrap_or(SelectionBucket::Selections);
            match scope_bucket {
                SelectionBucket::CrossRepo => {
                    if let Some(sel) = p.ruleset.cross_repo.iter_mut().find(|s| s.rule_id == rule_id) {
                        if !sel.repos.contains(&repo) { sel.repos.push(repo.clone()); }
                    } else {
                        p.ruleset.cross_repo.push(RuleSelectionView { rule_id: rule_id.clone(), chosen_option: default_opt, repos: vec![repo.clone()] });
                    }
                }
                SelectionBucket::Process => {
                    if let Some(sel) = p.ruleset.process.iter_mut().find(|s| s.rule_id == rule_id) {
                        if !sel.repos.contains(&repo) { sel.repos.push(repo.clone()); }
                    } else {
                        p.ruleset.process.push(RuleSelectionView { rule_id: rule_id.clone(), chosen_option: default_opt, repos: vec![repo.clone()] });
                    }
                }
                SelectionBucket::Selections => {
                    if let Some(sel) = p.ruleset.selections.iter_mut().find(|s| s.rule_id == rule_id) {
                        if !sel.repos.contains(&repo) { sel.repos.push(repo.clone()); }
                    } else {
                        p.ruleset.selections.push(RuleSelectionView { rule_id: rule_id.clone(), chosen_option: default_opt, repos: vec![repo.clone()] });
                    }
                }
            }
            let body = build_ruleset_json(&p);
            spawn(async move {
                if save_ruleset(&pid, body).await {
                    crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("Added rule to {repo}."));
                    refresh_add += 1;
                } else {
                    crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, "Could not update the ruleset.");
                }
            });
        }
    });

    rsx! {
        // The chorale table: "Applied to" column shows the repos (comma-joined text from
        // the accessor). Row click opens the rule detail modal.
        Table {
            handle,
            sort_enabled: true,
            filter_enabled: true,
            sticky_header: true,
            on_row_click: Callback::new(move |rid: RowId| {
                if let Some(r) = id_map_click.get(&rid) {
                    detail_rule.set(Some(r.clone()));
                    if let Some(opt) = r.default_option.as_ref() {
                        chosen_ctx.write().insert(r.id.clone(), opt.clone());
                    }
                }
            }),
        }

        // Per-rule actions below the table: "Add to repo" and "Go to repo rule".
        // These are here (not in a row renderer) because row renderers are Send+Sync
        // and cannot capture Dioxus signals. The list shows ALL corpus rules for
        // which the project has at least one repo they are not yet on.
        {
            let actionable: Vec<&ProposedRuleView> = annotated.iter().filter(|r| {
                project.repos.iter().any(|rp| !r.repos.contains(rp))
                || !r.repos.is_empty()  // also show go-to for applied rules
            }).collect();
            if actionable.is_empty() {
                rsx! {}
            } else {
                let unapplied_count = annotated.iter().filter(|r| {
                    project.repos.iter().any(|rp| !r.repos.contains(rp))
                }).count();
                rsx! {
                    details { class: "add-to-repo-details",
                        summary { class: "add-to-repo-summary",
                            "Rule actions \u{2014} add to repo, go to project rules ({unapplied_count} rules have repos they\u{2019}re not yet on)"
                        }
                        div { class: "add-to-repo-list",
                            for rule in annotated.iter() {
                                {
                                    let missing_repos: Vec<String> = project_repos.iter()
                                        .filter(|rp| !rule.repos.contains(rp))
                                        .cloned()
                                        .collect();
                                    let first_applied = rule.repos.first().cloned();
                                    let rule_id = rule.id.clone();
                                    let rule_title = rule.title.clone();
                                    let has_actions = !missing_repos.is_empty() || first_applied.is_some();
                                    if has_actions {
                                        let mut add_pending = add_pending;
                                        rsx! {
                                            div { key: "{rule_id}", class: "add-to-repo-row",
                                                span { class: "add-to-repo-rule-id", "{rule_id}" }
                                                span { class: "add-to-repo-rule-title", "{rule_title}" }
                                                if !missing_repos.is_empty() {
                                                    select {
                                                        class: "add-to-repo-select",
                                                        onchange: {
                                                            let rule_id = rule_id.clone();
                                                            move |e: Event<FormData>| {
                                                                let repo = e.value();
                                                                if repo.is_empty() { return; }
                                                                add_pending.set(Some((rule_id.clone(), repo)));
                                                            }
                                                        },
                                                        option { value: "", "Add to repo\u{2026}" }
                                                        for repo in missing_repos.iter() {
                                                            option { key: "{repo}", value: "{repo}", "{repo}" }
                                                        }
                                                    }
                                                }
                                                if let Some(first_repo) = first_applied {
                                                    button {
                                                        class: "btn-edit-sm go-to-repo-btn",
                                                        title: "Jump Table 1\u{2019}s filter to this repo",
                                                        onclick: move |_| { goto_repo.set(Some(first_repo.clone())); },
                                                        "\u{2197} View in project rules"
                                                    }
                                                }
                                            }
                                        }
                                    } else {
                                        rsx! {}
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

/// A thin modal wrapper for `RulesView`: provides the `detail_rule` + `chosen` contexts
/// that `RuleDetailModal` reads, and renders the modal outside the table subtree
/// (same ghost-click rationale as `ScanResults`).
///
/// After the user picks an option the caller's `on_option_picked` callback is invoked
/// with `(rule_id, option_id)` so the parent can POST the updated ruleset.
#[component]
fn RulesDetailModalHost(
    on_option_picked: EventHandler<(String, String)>,
) -> Element {
    let detail_rule = use_context::<Signal<Option<ProposedRuleView>>>();
    let chosen = use_context::<Signal<std::collections::HashMap<String, String>>>();
    let Some(r) = detail_rule() else {
        return rsx! {};
    };
    let mut detail_rule_mut = use_context::<Signal<Option<ProposedRuleView>>>();
    rsx! {
        div { class: "rule-modal-overlay", onclick: move |_| detail_rule_mut.set(None),
            div { class: "rule-modal", onclick: move |e| e.stop_propagation(),
                div { class: "rule-modal-head",
                    span { class: "rule-modal-id", "{r.id}" }
                    button { class: "rule-modal-close", onclick: move |_| detail_rule_mut.set(None), "\u{2715}" }
                }
                p { class: "rule-modal-title", "{r.title}" }
                div { class: "rule-modal-meta",
                    span { class: "rule-modal-tag", "domain \u{00b7} {r.domain}" }
                    span { class: "rule-modal-tag", "scope \u{00b7} {r.scope}" }
                    span { class: "rule-modal-tag", "kind \u{00b7} {r.kind}" }
                    if !r.enforcement.is_empty() {
                        span { class: "rule-modal-tag", "enforcement \u{00b7} {r.enforcement}" }
                    }
                }
                if let Some(q) = r.decision_question.as_ref().filter(|s| !s.is_empty()) {
                    div { class: "rule-modal-section",
                        span { class: "rule-modal-label", "The decision" }
                        p { class: "rule-modal-question", "{q}" }
                    }
                }
                if let Some(w) = r.decision_why.as_ref().filter(|s| !s.is_empty()) {
                    div { class: "rule-modal-section",
                        span { class: "rule-modal-label", "Why the default" }
                        p { class: "rule-modal-why", "{w}" }
                    }
                }
                if r.options.is_empty() {
                    p { class: "rule-modal-note", "Single-variant rule — nothing to choose; arm it as-is." }
                } else {
                    div { class: "rule-modal-section",
                        span { class: "rule-modal-label", "Choose the alternative to adopt" }
                        if r.default_option.is_none() {
                            p { class: "rule-modal-mustchoose", "No default — you must choose an alternative before arming." }
                        }
                        div { class: "rule-modal-opts",
                            for o in r.options.iter() {
                                {
                                    let rid = r.id.clone();
                                    let oid = o.id.clone();
                                    let cur = chosen.read().get(&r.id).cloned().or_else(|| r.default_option.clone());
                                    let picked = cur.as_deref() == Some(o.id.as_str());
                                    let is_default = r.default_option.as_deref() == Some(o.id.as_str());
                                    let cls = if picked { "rule-opt on" } else { "rule-opt" };
                                    let mut chosen = chosen;
                                    rsx! {
                                        button {
                                            key: "{o.id}",
                                            class: "{cls}",
                                            onclick: move |_| {
                                                chosen.write().insert(rid.clone(), oid.clone());
                                                on_option_picked.call((rid.clone(), oid.clone()));
                                            },
                                            div { class: "rule-opt-head",
                                                span { class: "rule-opt-label", "{o.label}" }
                                                if is_default {
                                                    span { class: "rule-opt-default-badge", "default" }
                                                }
                                                if picked {
                                                    span { class: "rule-opt-picked-badge", "\u{2713} adopted" }
                                                }
                                            }
                                            span { class: "rule-opt-directive", "{o.directive}" }
                                            if !o.why.is_empty() {
                                                span { class: "rule-opt-why", "Why: {o.why}" }
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
    // Fetch ALL corpus rules for Table 2 (and for Table 1 join).
    let corpus_res = use_resource(move || {
        let _ = refresh();
        async move { fetch_corpus_rules().await }
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

    // Shared context: the open rule in the detail modal (Tables 1 + 2 both write here).
    let detail_rule = use_signal(|| Option::<ProposedRuleView>::None);
    use_context_provider(|| detail_rule);
    // Shared context: the per-rule chosen option (rule id -> option id).
    let chosen: Signal<std::collections::HashMap<String, String>> = use_signal(std::collections::HashMap::new);
    use_context_provider(|| chosen);

    // Signal from Table 2 to Table 1: "go to this repo".
    let goto_repo: Signal<Option<String>> = use_signal(|| None);

    let proj = active.read().clone().flatten();
    let proj_list = projects.read().clone().flatten().unwrap_or_default();
    let corpus = corpus_res.read().clone().flatten().unwrap_or_default();

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
                    // Clone everything from `p` up-front so closures below can move owned values
                    // without borrowing the match-arm `p` ref (which doesn't live long enough
                    // for the 'static EventHandler / spawn closures).
                    let p_owned: ProjectView = p.clone();
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
                    let pid_health = p.id.clone();
                    let p_modal = p_owned.clone();
                    let p_t1 = p_owned.clone();
                    let p_t2 = p_owned.clone();
                    let corpus_t1 = corpus.clone();
                    let corpus_t2 = corpus.clone();
                    rsx! {
                        // The modal host is rendered at this subtree root (outside the chorale
                        // tables) to avoid the ghost-click-eater bug. option_picked persists
                        // the chosen option immediately on every pick.
                        RulesDetailModalHost {
                            on_option_picked: move |(rule_id, opt_id): (String, String)| {
                                // Build a ruleset with the new option applied to the right selection.
                                let mut p2 = p_modal.clone();
                                let mut saved = false;
                                for sel in p2.ruleset.selections.iter_mut()
                                    .chain(p2.ruleset.cross_repo.iter_mut())
                                    .chain(p2.ruleset.process.iter_mut())
                                {
                                    if sel.rule_id == rule_id {
                                        sel.chosen_option = Some(opt_id.clone());
                                        saved = true;
                                    }
                                }
                                if saved {
                                    let body = build_ruleset_json(&p2);
                                    let pid2 = p2.id.clone();
                                    let mut refresh = refresh;
                                    spawn(async move {
                                        if save_ruleset(&pid2, body).await {
                                            crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, "Option saved.");
                                            refresh += 1;
                                        } else {
                                            crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, "Could not save the option choice.");
                                        }
                                    });
                                }
                            }
                        }

                        // Broken-path health check (issue #33): up top so a path that doesn't
                        // resolve to a local checkout is the first thing the architect sees.
                        RepoHealthPanel { project_id: pid_health }

                        // Table 1: the project's applied rules, filterable by repo, with
                        // option-edit (via modal) and remove-from-repo actions.
                        p { class: "section-label", "Project rules (applied)" }
                        p { class: "section-hint", "Rules the project has selected. Filter by repo to focus on one repo\u{2019}s rules. Click a row to edit the chosen option; select rows and use Remove to drop them from a repo. Cross-repo and process rules are project-level (no repo owns them) and always show." }
                        // Keyed wrapper divs remount the table components (and their use_hook
                        // row ids) when the ruleset or corpus changes. The key is on the div
                        // itself, which IS the first node of each expression block below.
                        {
                            let t1_key = format!("pt-{}-{}-{}", refresh(), p_owned.ruleset.selections.len(), p_owned.ruleset.cross_repo.len());
                            rsx! {
                                div {
                                    key: "{t1_key}",
                                    ProjectRulesTable {
                                        project: p_t1,
                                        corpus: corpus_t1,
                                        refresh,
                                        goto_repo,
                                    }
                                }
                            }
                        }

                        // Table 2: the full corpus, with "Applied to" and "Add to repo".
                        p { class: "section-label", "All rules (corpus reference)" }
                        p { class: "section-hint", "Every rule in the corpus. \u{201c}Applied to\u{201d} shows which project repos already have it. Use the add-to-repo panel below the table to add a rule to a new repo. Click a rule row to read its full context. Use \u{201c}View in project rules\u{201d} to jump Table 1\u{2019}s filter to a repo." }
                        {
                            let t2_key = format!("at-{}-{}", refresh(), corpus.len());
                            rsx! {
                                div {
                                    key: "{t2_key}",
                                    AllRulesTable {
                                        project: p_t2,
                                        corpus: corpus_t2,
                                        refresh,
                                        goto_repo,
                                    }
                                }
                            }
                        }

                        div { class: "rules-sections",
                            RuleCount { label: "Repo-local rules", n: p_owned.ruleset.selections.len() }
                            RuleCount { label: "Cross-repo rules (API contracts)", n: p_owned.ruleset.cross_repo.len() }
                            RuleCount { label: "Process rules (commit/PR)", n: p_owned.ruleset.process.len() }
                            RuleCount { label: "Custom rules", n: p_owned.ruleset.custom.len() }
                        }

                        SuppressionsPanel { project_id: pid_sup }

                        CiRulesPanel { repos: p_owned.repos.clone() }

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
                            let pid_add = p_owned.id.clone();
                            let custom_rules = p_owned.ruleset.custom.clone();
                            let project_id_cr = p_owned.id.clone();
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
                                if !custom_rules.is_empty() {
                                    CustomRulesTable { key: "cr-{refresh()}-{custom_rules.len()}", custom: custom_rules, project_id: project_id_cr, refresh }
                                }
                            }
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
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
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
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
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
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
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

// ── Unit of Work (issue #39) ──────────────────────────────────────────────────

/// The dev status of a Unit of Work. Shown alongside the story's tracker status.
/// New = gray, In progress = accent, Done = green.
#[derive(Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize, Default, Debug)]
#[serde(rename_all = "snake_case")]
enum DevStatus {
    #[default]
    New,
    InProgress,
    Done,
}

impl DevStatus {
    fn label(self) -> &'static str {
        match self {
            Self::New => "New",
            Self::InProgress => "In progress",
            Self::Done => "Done",
        }
    }

    /// CSS modifier for the `uow-dev-badge` class.
    fn badge_cls(self) -> &'static str {
        match self {
            Self::New => "neutral",
            Self::InProgress => "accent",
            Self::Done => "green",
        }
    }

    /// Wire string for `POST /api/uow/:id/status`.
    fn wire_str(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::InProgress => "in_progress",
            Self::Done => "done",
        }
    }
}

/// A single entry in the AI development history.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
struct HistoryEntryView {
    ts: String,
    kind: String,
    text: String,
}

/// The Unit of Work as returned by `GET /api/uow/:story_id`.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
struct UowView {
    story_id: String,
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    dev_status: DevStatus,
    #[serde(default)]
    history: Vec<HistoryEntryView>,
    #[serde(default)]
    updated: String,
}

/// Fetch all UoWs from the BFF and return them indexed by `story_id`.
async fn fetch_uow_map() -> std::collections::HashMap<String, UowView> {
    let Ok(resp) = reqwest::get(format!("{}/api/uow", crate::BFF_URL)).await else {
        return std::collections::HashMap::new();
    };
    let Ok(list) = resp.json::<Vec<UowView>>().await else {
        return std::collections::HashMap::new();
    };
    list.into_iter().map(|u| (u.story_id.clone(), u)).collect()
}

/// Fetch the UoW for a single story (get-or-create semantics).
async fn fetch_uow(story_id: &str) -> Option<UowView> {
    reqwest::get(format!("{}/api/uow/{}", crate::BFF_URL, story_id))
        .await
        .ok()?
        .json::<UowView>()
        .await
        .ok()
}

/// POST a new dev-status for a story's UoW.
async fn post_uow_status(story_id: &str, status: DevStatus) -> Option<UowView> {
    reqwest::Client::new()
        .post(format!("{}/api/uow/{}/status", crate::BFF_URL, story_id))
        .json(&serde_json::json!({ "status": status.wire_str() }))
        .send()
        .await
        .ok()?
        .json::<UowView>()
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
    /// In-app documentation viewer: USER_GUIDE.md and TECHNICAL.md rendered as markdown.
    Docs,
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
async fn export_project_json(id: &str, name: &str) -> bool {
    let Ok(resp) = reqwest::get(format!("{}/api/projects/{}/export", crate::BFF_URL, id)).await
    else {
        return false;
    };
    let Ok(text) = resp.text().await else {
        return false;
    };
    // Slug the project name for the filename: lowercase, spaces → hyphens, strip non-alnum.
    let slug: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let filename = format!("camerata-project-{slug}.json");
    match rfd::AsyncFileDialog::new()
        .set_file_name(&filename)
        .save_file()
        .await
    {
        Some(file) => file.write(text.as_bytes()).await.is_ok(),
        None => false,
    }
}

/// Result of an import attempt (first pass with `overwrite: false`).
#[derive(Clone, PartialEq)]
enum ImportResult {
    /// The project was created or silently overwritten; the returned project is active.
    Imported(ProjectView),
    /// A project with the same name already exists; the user must confirm before we overwrite.
    /// Holds the name for display and the raw JSON body to re-POST with `overwrite: true`.
    Conflict { name: String, payload: String },
    /// Something went wrong (network, parse, etc.).
    Failed,
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

/// Open a file picker, read the JSON, and POST it to the import endpoint with
/// `overwrite: false`. Returns `ImportResult` so the caller can decide whether to
/// prompt for overwrite confirmation.
async fn import_project_json() -> ImportResult {
    let Some(file) = rfd::AsyncFileDialog::new()
        .add_filter("JSON", &["json"])
        .pick_file()
        .await
    else {
        return ImportResult::Failed;
    };
    let Ok(raw) = String::from_utf8(file.read().await) else {
        return ImportResult::Failed;
    };
    import_project_payload(&raw, false).await
}

/// POST `payload` to /api/projects/import with the given `overwrite` flag. Shared by
/// the first-pass attempt and the confirmed overwrite.
async fn import_project_payload(payload: &str, overwrite: bool) -> ImportResult {
    // Merge the `overwrite` flag into the payload without re-parsing the whole doc:
    // parse into a Value, set the flag, then re-serialise.
    let mut body: serde_json::Value = match serde_json::from_str(payload) {
        Ok(v) => v,
        Err(_) => return ImportResult::Failed,
    };
    if let Some(obj) = body.as_object_mut() {
        obj.insert("overwrite".to_string(), serde_json::Value::Bool(overwrite));
    }
    let Ok(body_str) = serde_json::to_string(&body) else {
        return ImportResult::Failed;
    };
    let Ok(resp) = reqwest::Client::new()
        .post(format!("{}/api/projects/import", crate::BFF_URL))
        .header("content-type", "application/json")
        .body(body_str)
        .send()
        .await
    else {
        return ImportResult::Failed;
    };
    let Ok(v) = resp.json::<serde_json::Value>().await else {
        return ImportResult::Failed;
    };
    if v.get("conflict").and_then(|b| b.as_bool()).unwrap_or(false) {
        let name = v
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_string();
        ImportResult::Conflict { name, payload: payload.to_string() }
    } else if v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) {
        match serde_json::from_value::<ProjectView>(v.get("project").cloned().unwrap_or_default()) {
            Ok(p) => ImportResult::Imported(p),
            Err(_) => ImportResult::Failed,
        }
    } else {
        ImportResult::Failed
    }
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
    // An import that hit a name collision: holds (project name, raw JSON payload).
    // While set, a confirm modal is visible.
    let mut pending_import_overwrite = use_signal(|| Option::<(String, String)>::None);
    // True when the just-opened project has an in-progress onboarding draft: show the
    // "continue or start over" prompt before entering (the project is already active on
    // the server, so the draft endpoints are scoped to it).
    let mut resume_prompt = use_signal(|| false);
    let list = projects.read().clone().flatten().unwrap_or_default();

    rsx! {
        // Overwrite-confirm modal — shown when an import collides with an existing name.
        if let Some((ref conflict_name, ref conflict_payload)) = pending_import_overwrite() {
            {
                let conflict_name = conflict_name.clone();
                let conflict_payload = conflict_payload.clone();
                rsx! {
                    div { class: "rule-modal-overlay", onclick: move |_| pending_import_overwrite.set(None),
                        div { class: "rule-modal", onclick: move |e| e.stop_propagation(),
                            div { class: "rule-modal-head",
                                span { class: "rule-modal-id", "Overwrite project?" }
                                button {
                                    class: "rule-modal-close",
                                    onclick: move |_| pending_import_overwrite.set(None),
                                    "\u{2715}"
                                }
                            }
                            p { class: "rule-modal-detail",
                                "A project named \u{201c}{conflict_name}\u{201d} already exists. \
                                 Overwriting will replace its repos, ruleset, and onboarded state \
                                 but keep its id. This cannot be undone."
                            }
                            div { class: "onboard-leave-actions",
                                button {
                                    class: "btn-edit-sm",
                                    onclick: move |_| pending_import_overwrite.set(None),
                                    "Cancel"
                                }
                                button {
                                    class: "btn-edit-sm pg-btn-danger",
                                    onclick: move |_| {
                                        pending_import_overwrite.set(None);
                                        let payload = conflict_payload.clone();
                                        spawn(async move {
                                            match import_project_payload(&payload, true).await {
                                                ImportResult::Imported(_) => {
                                                    crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, "Imported. Resolve the repo paths in the Rules view.");
                                                    refresh += 1;
                                                    screen.set(CockpitScreen::InProject);
                                                }
                                                _ => {
                                                    crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, "Import failed.");
                                                }
                                            }
                                        });
                                    },
                                    "Overwrite"
                                }
                            }
                        }
                    }
                }
            }
        }

        // Resume-onboarding prompt — shown when an opened project has an in-progress draft.
        if resume_prompt() {
            div { class: "rule-modal-overlay",
                div { class: "rule-modal", onclick: move |e| e.stop_propagation(),
                    div { class: "rule-modal-head",
                        span { class: "rule-modal-id", "Onboarding in progress" }
                    }
                    p { class: "rule-modal-detail",
                        "This project has an onboarding you didn't finish. Continue where you \
                         left off, or start over from a fresh scan?"
                    }
                    div { class: "onboard-leave-actions",
                        button {
                            // Secondary button sized to match the primary beside it (danger tint).
                            class: "btn-secondary danger",
                            onclick: move |_| {
                                spawn(async move {
                                    // Discard the saved draft, then enter for a fresh onboarding.
                                    clear_onboarding_draft().await;
                                    resume_prompt.set(false);
                                    screen.set(CockpitScreen::InProject);
                                });
                            },
                            "Start over"
                        }
                        button {
                            class: "btn-run",
                            onclick: move |_| {
                                // Keep the draft; OnboardView restores it when opened.
                                resume_prompt.set(false);
                                screen.set(CockpitScreen::InProject);
                            },
                            "Continue where you left off"
                        }
                    }
                }
            }
        }

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
                                let name_export = p.name.clone();
                                let id_open = p.id.clone();
                                let id_del = p.id.clone();
                                let name_del = p.name.clone();
                                let is_pending = pending_delete().as_deref() == Some(p.id.as_str());
                                rsx! {
                                    div { class: "pg-card", key: "{p.id}",
                                        div { class: "pg-card-main",
                                            span { class: "pg-card-name", "{p.name}" }
                                            span { class: "pg-card-meta", "{p.repos.len()} repo(s) · {p.ruleset.selections.len()} repo-rules" }
                                            {
                                                let n_on = p.onboarded.iter().filter(|r| p.repos.contains(r)).count();
                                                let total = p.repos.len();
                                                let cls = if total > 0 && n_on == total { "pg-onboard-badge done" } else { "pg-onboard-badge" };
                                                rsx! {
                                                    span { class: "{cls}",
                                                        if total > 0 && n_on == total { "✓ onboarded" }
                                                        else if n_on > 0 { "{n_on}/{total} onboarded" }
                                                        else { "not yet onboarded" }
                                                    }
                                                }
                                            }
                                        }
                                        div { class: "pg-card-actions",
                                            button {
                                                class: "pg-btn-secondary",
                                                onclick: move |_| {
                                                    let id = id_export.clone();
                                                    let name = name_export.clone();
                                                    spawn(async move { let _ = export_project_json(&id, &name).await; });
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
                                                            // If this project has an in-progress onboarding
                                                            // draft (server scopes the draft to the now-active
                                                            // project), ask before entering instead of silently
                                                            // resuming. Otherwise go straight in.
                                                            if load_onboarding_draft().await.is_some() {
                                                                resume_prompt.set(true);
                                                            } else {
                                                                screen.set(CockpitScreen::InProject);
                                                            }
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
                                match import_project_json().await {
                                    ImportResult::Imported(_) => {
                                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, "Imported. Resolve the repo paths in the Rules view.");
                                        refresh += 1;
                                        screen.set(CockpitScreen::InProject);
                                    }
                                    ImportResult::Conflict { name, payload } => {
                                        pending_import_overwrite.set(Some((name, payload)));
                                    }
                                    ImportResult::Failed => {}
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
    // Leaving is safe: onboarding state auto-saves per project, so navigating back just
    // leaves (the resume prompt restores it on return). No "you'll lose your work" warning.
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
                onclick: move |_| {
                    screen.set(CockpitScreen::Projects);
                },
                "← Projects"
            }
            button {
                class: cls(CockpitView::Onboard),
                onclick: move |_| view.set(CockpitView::Onboard),
                "Onboard repos"
            }
            button {
                class: cls(CockpitView::Stories),
                onclick: move |_| view.set(CockpitView::Stories),
                "Governed Development"
            }
            button {
                class: cls(CockpitView::Rules),
                onclick: move |_| view.set(CockpitView::Rules),
                "Rules"
            }
            button {
                class: cls(CockpitView::Routines),
                onclick: move |_| view.set(CockpitView::Routines),
                "Routines"
            }
            button {
                class: cls(CockpitView::Workspace),
                onclick: move |_| view.set(CockpitView::Workspace),
                "Repository Workspace"
            }
            button {
                class: cls(CockpitView::Docs),
                onclick: move |_| view.set(CockpitView::Docs),
                "Docs"
            }
        }
    }
}

#[component]
pub fn CockpitApp() -> Element {
    // Which cockpit view (control surface vs routines). Declared first so all hooks
    // below run unconditionally in a stable order regardless of the view.
    let mut view = use_signal(|| CockpitView::Stories);
    // Shared so a child (e.g. ScanResults' "Complete onboarding") can switch the tab.
    use_context_provider(|| view);
    // On open, land on the right view: Onboard while onboarding is incomplete, Governed
    // Development once every repo is onboarded. Set ONCE (a guard) so it never overrides the
    // user's manual nav after the first load.
    let active_proj = use_resource(fetch_active_project);
    let mut view_inited = use_signal(|| false);
    use_effect(move || {
        if view_inited() {
            return;
        }
        if let Some(maybe) = &*active_proj.read() {
            let fully_onboarded = matches!(
                maybe,
                Some(p) if !p.repos.is_empty() && p.repos.iter().all(|r| p.onboarded.contains(r))
            );
            view.set(if fully_onboarded {
                CockpitView::Stories
            } else {
                CockpitView::Onboard
            });
            view_inited.set(true);
        }
    });

    // Both data sets come from the BFF over HTTP. `use_resource` runs the fetch when
    // the cockpit mounts; the embedded server (see main.rs) is up by then.
    let stories_res = use_resource(fetch_stories);
    let rules_res = use_resource(fetch_rules);
    // The active connection (native vs GitHub), shown honestly in the topbar.
    let provider_res = use_resource(fetch_provider);

    // UoW refresh tick: bumped whenever the architect changes a UoW dev-status so the
    // spine row badges update immediately. The UoW map is fetched once on mount and
    // re-fetched whenever this tick bumps.
    let uow_refresh = use_signal(|| 0u32);
    let uow_res = use_resource(move || {
        let _dep = uow_refresh();
        async move { fetch_uow_map().await }
    });

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
    if view() == CockpitView::Docs {
        return rsx! {
            div { class: "cockpit",
                CockpitNav { view }
                div { class: "cockpit-scroll",
                    DocsView {}
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
                                {
                                    // Snapshot the UoW map once for the entire spine render.
                                    let uow_map = uow_res.read().clone().unwrap_or_default();
                                    rsx! {
                                        for (i , s) in story_list.iter().enumerate() {
                                            {
                                                let (badge, badge_cls) = status_badge(s.status);
                                                let sel = i == selected();
                                                let cls = if sel { "spine-item sel" } else { "spine-item" };
                                                // UoW dev-status for this story (default New if no UoW yet).
                                                let dev_status = uow_map
                                                    .get(&s.id)
                                                    .map(|u| u.dev_status)
                                                    .unwrap_or_default();
                                                let dev_label = dev_status.label();
                                                let dev_cls = dev_status.badge_cls();
                                                rsx! {
                                                    button {
                                                        class: "{cls}",
                                                        onclick: move |_| selected.set(i),
                                                        span { class: "spine-title", "{s.title}" }
                                                        // Story tracker status (existing).
                                                        span { class: "spine-badge {badge_cls}", "{badge}" }
                                                        // UoW dev status (new — shown alongside, visually distinct).
                                                        span { class: "uow-dev-badge {dev_cls}", "{dev_label}" }
                                                    }
                                                }
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

                                // ── UoW panel: dev status + branch + AI history ──────
                                // Shows the dev-side projection of the selected story.
                                // Branch and history are designed to be auto-populated by
                                // the governed run (Pillar 2); for now they are readable
                                // here and settable via the API endpoints.
                                UowPanel {
                                    story_id: current.id.clone(),
                                    uow_refresh,
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
    // Flat + lossless: one row per finding, every column. NOT grouped/merged — a machine
    // consumer (script, pivot, SIEM, compliance pipeline) groups/filters itself and needs
    // full fidelity. `also_matches` carries the other rule ids the location-merge folded in,
    // so no rule is dropped from the export (space-separated; the grouping the UI shows is
    // recoverable from rule_id + also_matches + path + line).
    let mut out =
        String::from("repo,severity,status,rule_id,also_matches,path,line,snippet,detail\n");
    for f in findings {
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{},{}\n",
            csv_field(&f.repo),
            csv_field(&f.severity),
            csv_field(&f.status),
            csv_field(&f.rule_id),
            csv_field(&f.also_matches.join(" ")),
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

/// Where a finding sits in onboarding triage. The architect moves each finding between these
/// three tables (a single-select switches the view) until nothing is Unresolved; then the
/// ignored and tech-debt buckets are processed. This is LOCAL triage state — the backend
/// commit (baseline waiver / ticket / dev-engine import) happens at Process, not on each move.
#[derive(Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
enum TriageState {
    #[default]
    Unresolved,
    Ignored,
    TechDebt,
}

impl TriageState {
    fn label(self) -> &'static str {
        match self {
            Self::Unresolved => "Unresolved",
            Self::Ignored => "Ignored",
            Self::TechDebt => "Tech debt",
        }
    }
}

/// Which tech-debt bucket a finding is in: resolve LATER (file a tracked ticket) or NOW (pull
/// into the dev engine as the first story). Only meaningful when state == TechDebt.
#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum TechDebtBucket {
    Later,
    Now,
}

/// One finding's triage disposition: its table, the (required) ignore reason, and its
/// tech-debt bucket. Absence from the dispositions map == Unresolved with defaults.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct Disposition {
    state: TriageState,
    reason: String,
    bucket: TechDebtBucket,
}

impl Default for Disposition {
    fn default() -> Self {
        Self { state: TriageState::Unresolved, reason: String::new(), bucket: TechDebtBucket::Later }
    }
}

/// Stable identity for a finding across the triage tables (repo + rule + location + snippet),
/// so its disposition survives table switches and re-sorts.
fn finding_key(f: &FindingView) -> String {
    format!("{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}", f.repo, f.rule_id, f.path, f.line, f.snippet)
}

/// The disposition state for a finding (Unresolved when absent from the map).
fn finding_state(
    dispositions: &std::collections::HashMap<String, Disposition>,
    f: &FindingView,
) -> TriageState {
    dispositions
        .get(&finding_key(f))
        .map(|d| d.state)
        .unwrap_or(TriageState::Unresolved)
}

/// Wire the mechanical (CI-tier) governance rules into a repo's CI as a governed dev run.
/// Returns `(run_id, mode)`.
/// Emit the "wire mechanical rules into CI" story as a GitHub issue. Returns the issue URL.
async fn wire_ci_rules(repo: &str) -> Option<String> {
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/api/onboard/ci-rules", crate::BFF_URL))
        .json(&serde_json::json!({ "repo": repo }))
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

/// The "add CI-enforced rules" panel: per repo, emit the CI-wiring task as a STORY (a
/// GitHub issue), not a dev run launched from onboarding. Onboarding produces stories; the
/// dev layer (Pillar 2) picks the issue up and does the work. Reused in onboarding + Rules.
#[component]
fn CiRulesPanel(repos: Vec<String>) -> Element {
    let mut msg = use_signal(String::new);
    let mut busy = use_signal(|| false);
    rsx! {
        div { class: "fix-panel",
            p { class: "scan-section-h", "Add CI-enforced rules" }
            p { class: "scan-section-sub", "Mechanical rules are declared in .camerata/ci-checks.json at arm time, but a config doesn't enforce itself. This files a story (a GitHub issue) to wire each declared check into CI (ESLint rule, query-plan/migration audit, AST lint). The dev layer picks it up and does the work — onboarding just writes the story." }
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
                                            Some(url) => msg.set(format!(
                                                "Filed a CI-wiring story for {repo}: {url}"
                                            )),
                                            None => msg.set(format!("Could not file the CI-wiring story for {repo} (is GitHub connected?).")),
                                        }
                                        busy.set(false);
                                    });
                                },
                                "Create CI-rules story"
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

#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
struct RuleOptionView {
    id: String,
    label: String,
    #[serde(default)]
    directive: String,
    #[serde(default)]
    why: String,
}

#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
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
    decision_question: Option<String>,
    #[serde(default)]
    decision_why: Option<String>,
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

#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
struct StackView {
    repo: String,
    #[serde(default)]
    languages: Vec<String>,
    #[serde(default)]
    frameworks: Vec<String>,
}

/// Real audit usage from the server (the actual half of actual-vs-estimated).
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Default)]
struct ActualUsageView {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cost_usd: f64,
    #[serde(default)]
    calls: u64,
    /// False when some calls didn't report a cost (the dollar figure is a partial sum).
    #[serde(default)]
    cost_complete: bool,
}

#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
struct ScanReportView {
    #[serde(default)]
    repos: Vec<String>,
    #[serde(default)]
    stacks: Vec<StackView>,
    files_scanned: usize,
    #[serde(default)]
    files_excluded: usize,
    #[serde(default)]
    code_chars: usize,
    /// Mechanical rule ids dropped from the code-only scan (enforced in CI instead).
    #[serde(default)]
    excluded_mechanical_rules: Vec<String>,
    #[serde(default)]
    actual_usage: Option<ActualUsageView>,
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

/// The persisted in-flight onboarding state (issue #27). Saved continuously so a brownfield
/// onboarding survives an app restart — the architect doesn't re-scan to keep testing the
/// post-scan features. The scan + audit (the expensive artifacts) plus the per-repo rule
/// selection, triage dispositions, and view state are all sticky.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct OnboardingDraft {
    scan: ScanReportView,
    #[serde(default)]
    audit: Option<ScanReportView>,
    #[serde(default)]
    repo_selection: std::collections::HashMap<String, Vec<String>>,
    // The architect's chosen alternative per rule (rule id -> option id). Persisted so a
    // non-default option survives reload — without this the choices reset to defaults.
    #[serde(default)]
    chosen: std::collections::HashMap<String, String>,
    #[serde(default)]
    dispositions: std::collections::HashMap<String, Disposition>,
    #[serde(default)]
    viewed_repo: String,
    #[serde(default)]
    triage_view: TriageState,
}

/// Load the saved onboarding draft, or None when nothing is in progress.
async fn load_onboarding_draft() -> Option<OnboardingDraft> {
    reqwest::Client::new()
        .get(format!("{}/api/onboard/draft", crate::BFF_URL))
        .send()
        .await
        .ok()?
        .json::<Option<OnboardingDraft>>()
        .await
        .ok()
        .flatten()
}

/// Persist the current onboarding draft (best-effort; failure is non-fatal).
async fn save_onboarding_draft(draft: &OnboardingDraft) {
    let _ = reqwest::Client::new()
        .post(format!("{}/api/onboard/draft", crate::BFF_URL))
        .json(draft)
        .send()
        .await;
}

/// Drop the saved draft (a fresh scan starts a new session; clearing avoids re-seeding the
/// previous run's audit/dispositions onto it).
async fn clear_onboarding_draft() {
    let _ = reqwest::Client::new()
        .post(format!("{}/api/onboard/draft/clear", crate::BFF_URL))
        .send()
        .await;
}

/// Finish onboarding for the active project: marks its repos onboarded and clears the
/// draft. The post-scan steps (audit / triage / apply / wire-CI) are all optional, so this
/// is the explicit "I'm done" action. Returns true on success.
async fn complete_onboarding() -> bool {
    let Ok(resp) = reqwest::Client::new()
        .post(format!("{}/api/onboard/complete", crate::BFF_URL))
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

/// A rule the user selected to audit against, carrying its per-repo binding. An empty
/// `repos` means PROJECT-LEVEL (every repo); a non-empty `repos` scopes the rule to just
/// those repos. The backend audits each repo against only the rules that apply to it, so a
/// multi-repo scan runs each repo against its own rules ∪ the project-level set.
#[derive(Clone, PartialEq)]
struct SelectedAuditRule {
    id: String,
    directive: String,
    repos: Vec<String>,
}

/// Serialize selected rules into the audit request shape (`{id, directive, repos}` each).
fn audit_rules_json(rules: &[SelectedAuditRule]) -> Vec<serde_json::Value> {
    rules
        .iter()
        .map(|r| serde_json::json!({ "id": r.id, "directive": r.directive, "repos": r.repos }))
        .collect()
}

/// Phase 2 — audit the repos against the selected rules (each carrying its repo binding).
async fn audit_against(
    repos: &[String],
    rules: &[SelectedAuditRule],
    model: &str,
    calibration_model: &str,
    mode: &str,
) -> Option<ScanReportView> {
    let rule_json = audit_rules_json(rules);
    reqwest::Client::new()
        .post(format!("{}/api/onboard/audit", crate::BFF_URL))
        .json(&serde_json::json!({ "repos": repos, "rules": rule_json, "model": model, "calibration_model": calibration_model, "mode": mode }))
        .send()
        .await
        .ok()?
        .json::<ScanReportView>()
        .await
        .ok()
}

/// One model the audit selector offers (`GET /api/models`).
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
struct AuditModelOption {
    label: String,
    id: String,
    /// USD per million tokens (input / output). Drives the pre-audit cost estimate.
    #[serde(default)]
    price_in: f64,
    #[serde(default)]
    price_out: f64,
}

#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
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

/// Rough pre-audit cost estimate, returned as (total_tokens, dollars, passes). Mirrors the
/// server's chunk/batch math (ai_audit) so the number tracks what the audit actually sends:
/// the digest is re-sent per rule-batch, so parallel/job (batches of 15) cost more tokens
/// than sequential (one batch). Input and output are priced SEPARATELY (output bills ~5×
/// input and dominates findings-heavy scans, so a flat blended rate would misprice exactly
/// the scans that matter).
///
/// Deliberately biased CONSERVATIVE (slightly high): an estimate that turns into a smaller
/// bill is a pleasant surprise; one that turns into a bigger bill is broken trust. Two
/// things push the real bill the other way and are NOT modeled, both safe: prompt-caching
/// of the repeated rules/repo-map prefix (cache reads bill ~0.1× input), and dedup shrinking
/// the calibration pass. The guarded risk is the OUTPUT undercount on findings-dense scans —
/// so output is modeled both per-pass AND proportional to the code scanned, not a flat
/// constant. Approximate by design (~4 chars/token); sized to size a scan, not to bill it.
#[allow(clippy::too_many_arguments)]
fn estimate_audit_cost(
    code_chars: usize,
    selected: usize,
    mode: &str,
    audit_in: f64,
    audit_out: f64,
    calib_in: f64,
    calib_out: f64,
) -> (u64, f64, usize) {
    const CHUNK_DIGEST_CHARS: usize = 350_000;
    const RULE_BATCH_SIZE: usize = 15;
    const CHARS_PER_TOKEN: f64 = 4.0;
    // Per-pass scaffolding re-sent every pass: the rules block (verbose directives) + the
    // repo map + the system prompt. Conservative.
    const OVERHEAD_CHARS_PER_PASS: usize = 10_000;
    // Output is findings: each is a paragraph of detail. A baseline per pass PLUS a term
    // that scales with the code each pass scans, so a findings-dense or large scan isn't
    // under-counted on output (the half that bites, since output bills ~5×).
    const OUT_TOKENS_PER_PASS: f64 = 2_200.0;
    const OUTPUT_PER_CODE_TOKEN: f64 = 0.02;
    // Resolution round + general conservatism. Biased HIGH on purpose: both logged real
    // runs (budget-mini ~2.24×, chorale ~1.75×) came in UNDER estimate, and an audit that
    // costs more than quoted is the bad surprise. Better to over-quote than under-quote.
    const FUDGE: f64 = 1.4;

    let chunks = code_chars.div_ceil(CHUNK_DIGEST_CHARS).max(1);
    let batches = if mode == "sequential" {
        1
    } else {
        selected.div_ceil(RULE_BATCH_SIZE).max(1)
    };
    let passes = chunks * batches;
    let code_tokens = code_chars as f64 / CHARS_PER_TOKEN;

    // ── Scan passes, priced at the AUDIT model ──
    let scan_in = (code_chars * batches + OVERHEAD_CHARS_PER_PASS * passes) as f64 / CHARS_PER_TOKEN;
    let scan_out =
        OUT_TOKENS_PER_PASS * passes as f64 + OUTPUT_PER_CODE_TOKEN * code_tokens * batches as f64;

    // ── Calibration: ONE pass over all findings, priced at the CALIBRATION model. It
    // re-reads roughly the scan's output (the findings) and, crucially, RE-EMITS each
    // finding with a corrected/verified body — not a short verdict. So its output rides
    // with the full findings volume, ~1× the scan's output, not a fraction of it. The
    // earlier 0.3× factor was the main structural reason real runs came in over estimate. ──
    let cal_in = scan_out;
    let cal_out = scan_out;

    let dollars = ((scan_in * audit_in + scan_out * audit_out)
        + (cal_in * calib_in + cal_out * calib_out))
        / 1_000_000.0
        * FUDGE;
    let total_tokens = ((scan_in + scan_out + cal_in + cal_out) * FUDGE) as u64;
    (total_tokens, dollars, passes)
}

/// Compact human token count: 2.0M / 350k / 900.
fn human_tokens(t: u64) -> String {
    if t >= 1_000_000 {
        format!("{:.1}M", t as f64 / 1_000_000.0)
    } else if t >= 1_000 {
        format!("{:.0}k", t as f64 / 1_000.0)
    } else {
        t.to_string()
    }
}

/// A polled async-audit job (`GET /api/onboard/audit/job/:id`).
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Default)]
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
    rules: &[SelectedAuditRule],
    model: &str,
    calibration_model: &str,
    exec_mode: &str,
) -> Option<String> {
    let rule_json = audit_rules_json(rules);
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/api/onboard/audit/start", crate::BFF_URL))
        .json(&serde_json::json!({ "repos": repos, "rules": rule_json, "model": model, "calibration_model": calibration_model, "mode": exec_mode }))
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
    /// Local working-copy path the governance files were written into (Apply step).
    #[serde(default)]
    path: Option<String>,
    /// The governance branch created/pushed (Apply step).
    #[serde(default)]
    branch: Option<String>,
}

/// Apply: write the selected rules onto a governance branch in each repo's LOCAL clone and
/// push it to origin — NO pull request. The architect opens the PR separately.
async fn apply_rules(rules: &[ArmRuleReq], findings: &[FindingView]) -> Option<(bool, String, Vec<ArmResultView>)> {
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/api/onboard/apply", crate::BFF_URL))
        .json(&serde_json::json!({ "rules": rules, "findings": findings }))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let ok = v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false);
    let message = v.get("message").and_then(|m| m.as_str()).unwrap_or_default().to_string();
    let results = v
        .get("results")
        .cloned()
        .and_then(|r| serde_json::from_value(r).ok())
        .unwrap_or_default();
    Some((ok, message, results))
}

/// Open the governance PR for each repo from the already-applied branch (separate step).
async fn open_governance_pr(repos: &[String]) -> Option<Vec<ArmResultView>> {
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/api/onboard/open-pr", crate::BFF_URL))
        .json(&serde_json::json!({ "repos": repos }))
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


/// Accept selected findings as tech debt: open a GitHub issue (the story). `title` lets the
/// caller distinguish resolve-later from resolve-now; `None` uses the server default.
async fn create_ticket(repo: &str, findings: &[FindingView], title: Option<&str>) -> Option<String> {
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/api/onboard/ticket", crate::BFF_URL))
        .json(&serde_json::json!({ "repo": repo, "findings": findings, "title": title }))
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

/// Split a finding's detail into (body, needs-review reason). The calibration pass appends
/// `[needs review: <reason>]` (or a bare `[needs review]`) to flag findings whose
/// APPLICABILITY is questionable — distinct from the trivial sense in which every finding
/// "needs review". Returns `Some(reason)` (reason may be empty) when the flag is present, so
/// the table can surface WHY this one is flagged (e.g. "premature for a mini/internal app").
fn split_needs_review(detail: &str) -> (String, Option<String>) {
    if let Some(start) = detail.rfind("[needs review") {
        if let Some(end_rel) = detail[start..].find(']') {
            let inside = &detail[start + 1..start + end_rel];
            let reason = inside
                .strip_prefix("needs review")
                .unwrap_or("")
                .trim_start_matches([':', ' '])
                .trim()
                .to_string();
            let body = detail[..start].trim_end().to_string();
            return (body, Some(reason));
        }
    }
    (detail.to_string(), None)
}

fn finding_columns(repos: Vec<String>, show_bucket: bool) -> Vec<ColumnDef<FindingView>> {
    // chorale 0.2.3's palette has a native orange, so each severity gets a distinct color
    // straight from RenderKind::Badge — no custom cell renderer needed (Critical = red,
    // High = orange, Medium = yellow, Low = gray).
    let sev = BadgeVariantMap::new()
        .with("critical", BadgeVariant::new("Critical", "red"))
        .with("high", BadgeVariant::new("High", "orange"))
        .with("medium", BadgeVariant::new("Medium", "yellow"))
        .with("low", BadgeVariant::new("Low", "gray"));
    let mut cols = vec![
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
                // chorale 0.2.3 added blue/purple to the palette, so the two authorities
                // read as distinct colors (no more gray fallback collision).
                .with("enforced", BadgeVariant::new("Rule · enforced", "green"))
                .with("advisory", BadgeVariant::new("AI · advisory", "blue")),
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
        // "Needs review": the calibration pass's applicability flag with its reason. Every
        // finding technically needs review; THESE are flagged for a specific reason (usually
        // over-engineering / YAGNI on a small codebase). Surfaced as its own column + reason
        // so the architect can triage the hedged ones at a glance. Text-filterable so you can
        // show only the flagged rows. Drawn by a cell renderer (orange chip + reason).
        ColumnDef::new(ColumnId("needs_review"), "Needs review", |f: &FindingView| {
            match split_needs_review(&f.detail).1 {
                Some(reason) if !reason.is_empty() => CellValue::Text(reason),
                Some(_) => CellValue::Text("needs review".to_string()),
                None => CellValue::Text(String::new()),
            }
        })
        .sortable()
        .filter(FilterKind::Text)
        .initial_width(300.0),
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
        // Second grouping level: the FILE (path only). The findings table groups by
        // rule → file, so a rule violated 4× across one file collapses under a
        // "handlers.rs (4)" sub-header instead of 4 loose rows. Path-only (not path:line)
        // so all sites in a file share one group; the line lives in the Line column.
        ColumnDef::new(ColumnId("file"), "File", |f: &FindingView| {
            CellValue::Text(f.path.clone())
        })
        .sortable()
        .filter(FilterKind::Text)
        .initial_width(260.0),
        ColumnDef::new(ColumnId("loc"), "Line", |f: &FindingView| {
            CellValue::Text(f.line.to_string())
        })
        .sortable()
        .initial_width(80.0),
        ColumnDef::new(ColumnId("snippet"), "Snippet", |f: &FindingView| {
            CellValue::Text(f.snippet.clone())
        })
        .initial_width(380.0),
    ];
    // The tech-debt bucket flag (resolve later / now). Drawn by a row renderer that reads the
    // live disposition map; the accessor is a placeholder. Only present in the tech-debt view.
    if show_bucket {
        cols.push(
            ColumnDef::new(ColumnId("bucket"), "Bucket", |_f: &FindingView| {
                CellValue::Text(String::new())
            })
            .initial_width(120.0),
        );
    }
    cols
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
        // (The "Applies to" column was removed: the repo this ruleset is for is already
        // chosen in the "Repo ruleset" selector above the table, so per-row repo was redundant.)
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
    // Which triage table this is (issue #26). Findings not in this state are filtered out;
    // the component is keyed on it by the parent, so a switch remounts with that table's set.
    #[props(default = TriageState::Unresolved)] triage_view: TriageState,
    // The lifted finding -> disposition map. Move actions write here; the row is then dropped
    // from this table (remove_rows) and reappears under its new table on the next switch.
    #[props(default)] dispositions: Signal<std::collections::HashMap<String, Disposition>>,
) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    // Keep only the findings in THIS table's triage state (absent from the map = Unresolved).
    let findings: Vec<FindingView> = {
        let d = dispositions.peek();
        findings
            .into_iter()
            .filter(|f| finding_state(&d, f) == triage_view)
            .collect()
    };
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
    let in_techdebt = triage_view == TriageState::TechDebt;
    let handle = use_table(move || {
        TableState::new(rows.clone(), finding_columns(filter_repos.clone(), in_techdebt))
    });
    // Subscribe to the disposition map so the bucket flag column re-renders when the architect
    // marks resolve-later/now (the renderer below captures this snapshot).
    let bucket_snapshot = dispositions.read().clone();
    // Two-level grouping: by RULE, then by FILE within each rule. chorale groups by an
    // ordered key list (it recurses through the Vec, one depth per key), so a rule violated
    // 4× across one file renders as "RULE (4)" → "handlers.rs (4)" → the 4 individual lines.
    // Counts come free on every header. This is a PRESENTATION view of the flat finding
    // list; the CSV export stays flat + lossless (one row per finding), unchanged.
    use_hook(move || {
        handle.set_grouping(vec![ColumnId("type"), ColumnId("file")]);
        // Load all groups first, then collapse all by default — the architect drills in
        // rule → file → lines. (collapse_all only collapses groups in the loaded view, so it
        // must run after the page size is raised.)
        handle.set_pagination_mode(PaginationMode::InfiniteScroll);
        let _ = handle.set_page_size(5000);
        handle.collapse_all_groups();
    });
    // A durable ignore requires a reason (the require-reason invariant), captured here and
    // stored on the disposition; it's committed to the baseline at Process.
    let mut ignore_reason = use_signal(String::new);
    // Two id_map clones: each triage table renders two move buttons, and the two closures in
    // an arm each move a clone. Match arms are mutually exclusive, so the same two clones
    // serve every arm.
    let id_map_a = id_map.clone();
    let id_map_b = id_map.clone();
    // Two more clones for the tech-debt bucket buttons (resolve later / now).
    let id_map_c = id_map.clone();
    let id_map_d = id_map.clone();
    // The (sorted) rows for CSV export.
    let csv_rows = findings.clone();


    // SECURITY findings (the deterministic floor — the only tier ranked "critical") get a
    // red full-row highlight so they're unmistakable beyond the badge text. This now uses
    // chorale 0.2.3's `row_class` hook on the Table (below), not a per-cell stripe renderer.
    let row_renderers = {
        let mut m: std::collections::HashMap<ColumnId, RowCellRenderer<FindingView>> =
            std::collections::HashMap::new();
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
        // "Needs review" flag + reason: an orange chip when the calibration pass hedged this
        // finding, followed by the specific reason. Blank when not flagged.
        m.insert(
            ColumnId("needs_review"),
            std::sync::Arc::new(move |f: &FindingView, _val: &CellValue| {
                match split_needs_review(&f.detail).1 {
                    Some(reason) => {
                        let reason = reason.clone();
                        rsx! {
                            span { class: "nr-flag", "Needs review" }
                            if !reason.is_empty() {
                                span { class: "nr-reason", " {reason}" }
                            }
                        }
                    }
                    None => rsx! {},
                }
            }) as RowCellRenderer<FindingView>,
        );
        // Tech-debt bucket flag: reads the live disposition snapshot for this finding and
        // renders a "Later" / "Now" badge. Present only in the tech-debt view.
        if in_techdebt {
            let snap = bucket_snapshot.clone();
            m.insert(
                ColumnId("bucket"),
                std::sync::Arc::new(move |f: &FindingView, _val: &CellValue| {
                    let bucket = snap.get(&finding_key(f)).map(|d| d.bucket).unwrap_or(TechDebtBucket::Later);
                    let (label, cls) = match bucket {
                        TechDebtBucket::Later => ("Later", "td-bucket later"),
                        TechDebtBucket::Now => ("Now", "td-bucket now"),
                    };
                    rsx! { span { class: "{cls}", "{label}" } }
                }) as RowCellRenderer<FindingView>,
            );
        }
        RowCellRenderers::new(m)
    };

    rsx! {
        // Key: what the red row highlight means. Security (deterministic, Critical) vs the rest.
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
            // View-specific triage actions. A move writes the new disposition for each
            // selected finding and drops it from this table; it reappears under its target
            // table on the next switch. Backend commit happens later, at Process.
            match triage_view {
                TriageState::Unresolved => rsx! {
                    input {
                        class: "addressee-input ignore-reason",
                        placeholder: "reason to ignore (required)",
                        value: "{ignore_reason}",
                        oninput: move |e| ignore_reason.set(e.value()),
                    }
                    button {
                        class: "btn-restart",
                        onclick: move |_| {
                            let sel = handle.selected_ids();
                            let picked: Vec<FindingView> = sel.iter().filter_map(|id| id_map_a.get(id).cloned()).collect();
                            if picked.is_empty() { return; }
                            let reason = ignore_reason();
                            if reason.trim().is_empty() {
                                crate::toast::push_toast(toasts, crate::toast::ToastKind::Warning, "A reason is required to ignore a finding (it's recorded in the baseline at Process).");
                                return;
                            }
                            let mut d = dispositions.peek().clone();
                            for f in &picked {
                                let e = d.entry(finding_key(f)).or_default();
                                e.state = TriageState::Ignored;
                                e.reason = reason.clone();
                            }
                            dispositions.set(d);
                            handle.remove_rows(&sel);
                            crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("Moved {} to Ignored.", picked.len()));
                        },
                        "Ignore with reason \u{2192}"
                    }
                    button {
                        class: "btn-run",
                        onclick: move |_| {
                            let sel = handle.selected_ids();
                            let picked: Vec<FindingView> = sel.iter().filter_map(|id| id_map_b.get(id).cloned()).collect();
                            if picked.is_empty() { return; }
                            let mut d = dispositions.peek().clone();
                            for f in &picked { d.entry(finding_key(f)).or_default().state = TriageState::TechDebt; }
                            dispositions.set(d);
                            handle.remove_rows(&sel);
                            crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("Moved {} to Tech debt.", picked.len()));
                        },
                        "Save as tech debt"
                    }
                },
                TriageState::Ignored => rsx! {
                    button {
                        class: "btn-restart",
                        onclick: move |_| {
                            let sel = handle.selected_ids();
                            let picked: Vec<FindingView> = sel.iter().filter_map(|id| id_map_a.get(id).cloned()).collect();
                            if picked.is_empty() { return; }
                            let mut d = dispositions.peek().clone();
                            for f in &picked { d.entry(finding_key(f)).or_default().state = TriageState::Unresolved; }
                            dispositions.set(d);
                            handle.remove_rows(&sel);
                            crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("Moved {} back to Unresolved.", picked.len()));
                        },
                        "Move to Unresolved"
                    }
                    button {
                        class: "btn-run",
                        onclick: move |_| {
                            let sel = handle.selected_ids();
                            let picked: Vec<FindingView> = sel.iter().filter_map(|id| id_map_b.get(id).cloned()).collect();
                            if picked.is_empty() { return; }
                            let mut d = dispositions.peek().clone();
                            for f in &picked { d.entry(finding_key(f)).or_default().state = TriageState::TechDebt; }
                            dispositions.set(d);
                            handle.remove_rows(&sel);
                            crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("Moved {} to Tech debt.", picked.len()));
                        },
                        "Move to Tech debt"
                    }
                },
                TriageState::TechDebt => rsx! {
                    // Bucket the selected tech-debt findings. These stay in the table; only the
                    // Bucket flag column changes. Default is Later (a tracked ticket); Now pulls
                    // the finding into the dev engine as a fix story at Process.
                    button {
                        class: "btn-edit-sm",
                        onclick: move |_| {
                            let sel = handle.selected_ids();
                            let picked: Vec<FindingView> = sel.iter().filter_map(|id| id_map_c.get(id).cloned()).collect();
                            if picked.is_empty() { return; }
                            let mut d = dispositions.peek().clone();
                            for f in &picked { d.entry(finding_key(f)).or_default().bucket = TechDebtBucket::Later; }
                            dispositions.set(d);
                            crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("Marked {} as resolve later.", picked.len()));
                        },
                        "Mark: resolve later"
                    }
                    button {
                        class: "btn-edit-sm",
                        onclick: move |_| {
                            let sel = handle.selected_ids();
                            let picked: Vec<FindingView> = sel.iter().filter_map(|id| id_map_d.get(id).cloned()).collect();
                            if picked.is_empty() { return; }
                            let mut d = dispositions.peek().clone();
                            for f in &picked { d.entry(finding_key(f)).or_default().bucket = TechDebtBucket::Now; }
                            dispositions.set(d);
                            crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("Marked {} as resolve now.", picked.len()));
                        },
                        "Mark: resolve now"
                    }
                    button {
                        class: "btn-restart",
                        onclick: move |_| {
                            let sel = handle.selected_ids();
                            let picked: Vec<FindingView> = sel.iter().filter_map(|id| id_map_a.get(id).cloned()).collect();
                            if picked.is_empty() { return; }
                            let mut d = dispositions.peek().clone();
                            for f in &picked { d.entry(finding_key(f)).or_default().state = TriageState::Unresolved; }
                            dispositions.set(d);
                            handle.remove_rows(&sel);
                            crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("Moved {} back to Unresolved.", picked.len()));
                        },
                        "Move to Unresolved"
                    }
                    button {
                        class: "btn-restart",
                        onclick: move |_| {
                            let sel = handle.selected_ids();
                            let picked: Vec<FindingView> = sel.iter().filter_map(|id| id_map_b.get(id).cloned()).collect();
                            if picked.is_empty() { return; }
                            let mut d = dispositions.peek().clone();
                            for f in &picked { d.entry(finding_key(f)).or_default().state = TriageState::Ignored; }
                            dispositions.set(d);
                            handle.remove_rows(&sel);
                            crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("Moved {} to Ignored.", picked.len()));
                        },
                        "Move to Ignored"
                    }
                },
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
            // 0.2.3: an expand-all / collapse-all control in the grouped header (the
            // findings table groups by rule -> file), so a long audit collapses at once.
            group_expand_toggle: true,
            row_cell_renderers: row_renderers,
            // Critical (security-floor) rows get a red full-row highlight via the 0.2.3
            // conditional row-styling hook — replaces the old per-cell stripe renderer.
            row_class: RowClass::new(|f: &FindingView| {
                (f.severity == "critical").then(|| "finding-row-critical".to_string())
            }),
            on_row_click: Callback::new(move |rid: RowId| {
                if let Some(f) = id_map_click.get(&rid) {
                    detail_finding.set(Some(f.clone()));
                }
            }),
        }
    }
}


/// A rule can be armed WITHOUT the architect picking an alternative when it has no options,
/// or its `default_option` resolves to a non-empty directive. Rules that DON'T satisfy this
/// ("needs a choice") are never pre-selected: auto-selecting one would block audit/arm out of
/// the box for a decision the architect never actively made. They still show (highlighted) and
/// can be selected manually, at which point the gate asks for the choice.
fn rule_has_usable_default(r: &ProposedRuleView) -> bool {
    if r.options.is_empty() {
        return true;
    }
    r.default_option
        .as_ref()
        .and_then(|o| r.options.iter().find(|x| &x.id == o))
        .map(|x| !x.directive.is_empty())
        .unwrap_or(false)
}

/// The proposed-rules table with SELECTION (chorale checkboxes) — accept/reject
/// each rule into the approved starter set.
///
/// Per-repo: `rules` is the subset bound to `view_repo` (the repo this table represents),
/// while `all_rules` is the full cross-repo set used to resolve the OTHER repos' saved
/// selections when building the audit/arm requests. `repo_selection` is the lifted, shared
/// `repo -> selected rule ids` map: this table seeds its checkboxes from `view_repo`'s saved
/// set (or the recommended rules on first view) and writes the live selection back to it, so
/// switching repos preserves each repo's own picks. An empty `view_repo` is the single-repo /
/// non-split case (no per-repo map; behaves like the original whole-set table).
#[component]
fn ProposedRulesTable(
    rules: Vec<ProposedRuleView>,
    #[props(default)] all_rules: Vec<ProposedRuleView>,
    #[props(default)] view_repo: String,
    #[props(default)] repo_selection: Signal<std::collections::HashMap<String, Vec<String>>>,
    findings: Vec<FindingView>,
    on_audit: EventHandler<Vec<SelectedAuditRule>>,
    auditing: bool,
) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let chosen = use_context::<Signal<std::collections::HashMap<String, String>>>();
    let placement = use_context::<Signal<std::collections::HashMap<String, Vec<String>>>>();
    // Full cross-repo rule lookup (by rule id) for building audit/arm requests that span
    // every repo's saved selection, not just the one this table is currently showing.
    let all_by_id: std::collections::HashMap<String, ProposedRuleView> = if all_rules.is_empty() {
        rules.iter().map(|r| (r.id.clone(), r.clone())).collect()
    } else {
        all_rules.iter().map(|r| (r.id.clone(), r.clone())).collect()
    };
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
    // The rows to PRE-SELECT on mount. If this repo already has a saved selection in the
    // lifted map, restore exactly that (so switching repos preserves each repo's picks);
    // otherwise fall back to the recommended rules (the first-view default). Derived BEFORE
    // use_table consumes `rows`.
    let suggested_ids: Vec<RowId> = {
        let saved: Option<std::collections::HashSet<String>> = if view_repo.is_empty() {
            None
        } else {
            repo_selection
                .peek()
                .get(&view_repo)
                .map(|ids| ids.iter().cloned().collect())
        };
        match saved {
            Some(ids) => rows
                .iter()
                .filter(|(_, p)| ids.contains(&p.id))
                .map(|(r, _)| *r)
                .collect(),
            // First view: pre-select the recommended rules, but NOT ones that still need an
            // alternative chosen — auto-selecting those would block audit/arm immediately for
            // a decision the architect never made. They stay visible (highlighted) to opt into.
            None => rows
                .iter()
                .filter(|(_, p)| p.recommended && rule_has_usable_default(p))
                .map(|(r, _)| *r)
                .collect(),
        }
    };
    let mut domain_rows: std::collections::BTreeMap<String, Vec<RowId>> = Default::default();
    for (rid, p) in &rows {
        let d = if p.domain.is_empty() { "general".to_string() } else { p.domain.clone() };
        domain_rows.entry(d).or_default().push(*rid);
    }
    // Distinct domains (sorted, "general" for blank — matches the cell value) for the
    // Domain column's multi-select filter options.
    let domain_options: Vec<String> = domain_rows.keys().cloned().collect();
    let handle = use_table(move || {
        TableState::new(
            rows.clone(),
            rule_columns(domain_options.clone()),
        )
    });
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
        // Load EVERY group first, THEN collapse — collapse_all_groups only collapses the
        // groups currently in the view, so collapsing before the page size is raised left
        // every group past the first page expanded. Order: group → load all → collapse all.
        handle.set_pagination_mode(PaginationMode::InfiniteScroll);
        let _ = handle.set_page_size(5000);
        handle.collapse_all_groups();
        for rid in &suggested_ids {
            handle.set_selection(*rid, true);
        }
    });
    // Publish the live selected-rule count to ScanResults (Step 2) so its cost estimate
    // tracks what the user has ticked, AND persist this repo's selection back to the lifted
    // map so a repo switch (which remounts this table) restores it. Reactive: re-runs
    // whenever the selection changes. ProposedRulesTable only mounts inside ScanResults,
    // which provides the count signal.
    let mut selected_count = use_context::<Signal<usize>>();
    let id_map_writeback = id_map.clone();
    let view_repo_wb = view_repo.clone();
    let mut repo_selection_wb = repo_selection;
    use_effect(move || {
        let live_ids: Vec<String> = handle
            .selected_ids()
            .iter()
            .filter_map(|rid| id_map_writeback.get(rid).map(|r| r.id.clone()))
            .collect();
        if view_repo_wb.is_empty() {
            // Single-repo / non-split: no per-repo map, count is just this table's picks.
            selected_count.set(live_ids.len());
        } else {
            // Write this repo's selection, then report the cross-repo total so the cost
            // estimate reflects every repo that will be scanned, not just the visible one.
            let mut map = repo_selection_wb.peek().clone();
            map.insert(view_repo_wb.clone(), live_ids);
            let total: usize = map.values().map(|v| v.len()).sum();
            repo_selection_wb.set(map);
            selected_count.set(total);
        }
    });
    let mut arming = use_signal(|| false);
    let mut opening_pr = use_signal(|| false);
    // Repos that the Apply step wrote a governance branch into (local + pushed). The "Open
    // governance PR" button targets exactly these, and is disabled until something is applied.
    let mut applied_repos = use_signal(Vec::<String>::new);
    let arm_findings = findings;
    // Export the FULL cross-repo rule set (every repo's proposed rules), not just the
    // currently-viewed repo's subset, so the CSV stays lossless for a multi-repo scan.
    let csv_rules = if all_rules.is_empty() { rules.clone() } else { all_rules.clone() };

    // Dedicated clones for the audit closure; the originals are consumed by the arm closure.
    let all_by_id_audit = all_by_id.clone();
    let view_repo_audit = view_repo.clone();

    // Rules whose alternative is still UNRESOLVED — they have options but no chosen choice
    // AND no usable default directive, so the architect must pick one before the rule can be
    // enforced. Recomputed each render (reads `chosen`), so picking an alternative clears it.
    let needs_choice: std::collections::HashSet<String> = {
        let chosen_map = chosen.read();
        id_map
            .values()
            .filter(|r| {
                if r.options.is_empty() {
                    return false;
                }
                let oid = chosen_map.get(&r.id).cloned().or_else(|| r.default_option.clone());
                oid.and_then(|o| r.options.iter().find(|x| x.id == o).map(|x| x.directive.clone()))
                    .filter(|s| !s.is_empty())
                    .is_none()
            })
            .map(|r| r.id.clone())
            .collect()
    };
    // The VIEWED table's live selection (rule ids). Drives the per-row highlight: a needs-a-
    // choice rule is yellow ONLY while selected-but-unresolved; unselected = no highlight,
    // selected-and-resolved = the normal blue selection.
    let selected_rule_ids: std::collections::HashSet<String> = handle
        .selected_ids()
        .iter()
        .filter_map(|rid| id_map.get(rid).map(|r| r.id.clone()))
        .collect();
    let needs_choice_hl = needs_choice.clone();
    let selected_rule_ids_hl = selected_rule_ids.clone();
    // The SELECTED unresolved rules (across every repo's picks, matching the arm guard). These
    // BLOCK both buttons: an unresolved rule you've selected can't be audited or armed. An
    // unresolved rule you HAVEN'T selected is only highlighted, not blocking. This is also why
    // audit no longer silently falls back to the rule title — it's gated the same as arm now.
    let unresolved_selected: Vec<String> = {
        let selected: std::collections::BTreeSet<String> = if view_repo.is_empty() {
            selected_rule_ids.iter().cloned().collect()
        } else {
            let mut map = repo_selection.peek().clone();
            map.insert(view_repo.clone(), selected_rule_ids.iter().cloned().collect());
            map.values().flatten().cloned().collect()
        };
        selected
            .into_iter()
            .filter(|id| needs_choice.contains(id))
            .collect()
    };
    let has_unresolved = !unresolved_selected.is_empty();
    let unresolved_hint = if has_unresolved {
        format!("Choose an alternative first for: {}", unresolved_selected.join(", "))
    } else {
        String::new()
    };

    rsx! {
        // Per-domain "select all" is now native: the table is grouped by domain and
        // chorale 0.2.3 renders a tri-state select-all checkbox in each group header
        // (selection_enabled + grouping), so the old custom "Select rules by domain"
        // dropdown is gone.
        Table {
            handle,
            sort_enabled: true,
            selection_enabled: true,
            filter_enabled: true,
            // 0.2.3: expand-all / collapse-all control in the grouped header. The table is
            // grouped by domain and mounts collapsed, so this lets the architect open every
            // domain's rules (and re-collapse them) in one click.
            group_expand_toggle: true,
            // Highlight a rule yellow ONLY while it's selected AND still needs an alternative
            // chosen — that's the state that blocks audit/arm. Unselected = no highlight;
            // selected-and-resolved = the normal blue selection. Clears when a choice is made.
            row_class: RowClass::new(move |r: &ProposedRuleView| {
                (selected_rule_ids_hl.contains(&r.id) && needs_choice_hl.contains(&r.id))
                    .then(|| "rule-row-needs-choice".to_string())
            }),
            on_row_click: Callback::new(move |rid: RowId| {
                if let Some(r) = id_map_click.get(&rid) {
                    detail_rule.set(Some(r.clone()));
                }
            }),
        }
        // Explain WHY the buttons below are disabled: one or more selected rules still need an
        // alternative chosen (highlighted yellow above). Click each to pick one; the buttons
        // re-enable once all are resolved.
        if has_unresolved {
            div { class: "rule-gate-warning", role: "alert",
                span { class: "rule-gate-warning-icon", "\u{26A0}" }
                span {
                    "These selected rules need an alternative chosen before you can audit or add them (highlighted yellow above): "
                    strong { "{unresolved_selected.join(\", \")}" }
                    ". Click each rule to pick an option."
                }
            }
        }
        div { class: "findings-toolbar",
            button {
                class: "btn-run",
                disabled: auditing || has_unresolved,
                title: unresolved_hint.clone(),
                onclick: move |_| {
                    // Build the audit request from EVERY repo's saved selection, so one scan
                    // covers all repos, each against its own chosen rules. The current repo's
                    // live table selection is authoritative for it (the write-back effect may
                    // not have flushed yet); other repos come from the lifted map.
                    let resolve_directive = |r: &ProposedRuleView| -> String {
                        if r.options.is_empty() {
                            r.title.clone()
                        } else {
                            let oid = chosen.read().get(&r.id).cloned().or_else(|| r.default_option.clone());
                            oid.and_then(|o| r.options.iter().find(|x| x.id == o).map(|x| x.directive.clone()))
                                .filter(|s| !s.is_empty())
                                .unwrap_or_else(|| r.title.clone())
                        }
                    };
                    let live_ids: Vec<String> = handle.selected_ids().iter()
                        .filter_map(|id| id_map_audit.get(id).map(|r| r.id.clone())).collect();
                    let chosen_rules: Vec<SelectedAuditRule> = if view_repo_audit.is_empty() {
                        // Single-repo / non-split: audit this table's picks, each carrying
                        // the rule's own repo binding (a rule bound to every repo = project-level).
                        live_ids.iter().filter_map(|id| all_by_id_audit.get(id)).map(|r| {
                            SelectedAuditRule { id: r.id.clone(), directive: resolve_directive(r), repos: r.repos.clone() }
                        }).collect()
                    } else {
                        // Per-repo: each (repo, selected rule) becomes one entry scoped to that
                        // repo. The backend audits each repo against only the rules bound to it.
                        let mut map = repo_selection.peek().clone();
                        map.insert(view_repo_audit.clone(), live_ids);
                        let mut out = Vec::new();
                        for (repo, ids) in &map {
                            for id in ids {
                                if let Some(r) = all_by_id_audit.get(id) {
                                    out.push(SelectedAuditRule { id: r.id.clone(), directive: resolve_directive(r), repos: vec![repo.clone()] });
                                }
                            }
                        }
                        out
                    };
                    if chosen_rules.is_empty() {
                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Warning, "Select at least one rule (tick its checkbox) to audit against.");
                        return;
                    }
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
                disabled: arming() || has_unresolved,
                title: unresolved_hint.clone(),
                onclick: move |_| {
                    // Arm across EVERY repo's selection: a rule selected in one or more repos'
                    // tables arms to exactly those repos (an explicit placement override still
                    // wins). The current repo's live table selection is authoritative for it.
                    let live_ids: Vec<String> = handle.selected_ids().iter()
                        .filter_map(|id| id_map.get(id).map(|r| r.id.clone())).collect();
                    // rule id -> the repos that selected it.
                    let mut rule_repos: std::collections::BTreeMap<String, Vec<String>> = Default::default();
                    if view_repo.is_empty() {
                        // Single-repo / non-split: each picked rule keeps its own repo binding.
                        for id in &live_ids {
                            if let Some(r) = all_by_id.get(id) {
                                rule_repos.insert(id.clone(), r.repos.clone());
                            }
                        }
                    } else {
                        let mut map = repo_selection.peek().clone();
                        map.insert(view_repo.clone(), live_ids);
                        for (repo, ids) in &map {
                            for id in ids {
                                rule_repos.entry(id.clone()).or_default().push(repo.clone());
                            }
                        }
                    }
                    if rule_repos.is_empty() {
                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Warning, "Select at least one rule (tick its checkbox) before arming.");
                        return;
                    }
                    // Resolve each selected rule to its adopted directive; a rule
                    // with alternatives and no choice yet blocks arming.
                    let mut arm_reqs = Vec::new();
                    let mut unresolved = Vec::new();
                    for (id, selected_repos) in &rule_repos {
                        let Some(r) = all_by_id.get(id) else { continue; };
                        let (directive, option) = if r.options.is_empty() {
                            (r.title.clone(), None)
                        } else {
                            let oid = chosen.read().get(&r.id).cloned().or_else(|| r.default_option.clone());
                            match oid.clone().and_then(|o| r.options.iter().find(|x| x.id == o).map(|x| x.directive.clone())) {
                                Some(d) if !d.is_empty() => (d, oid),
                                _ => { unresolved.push(r.id.clone()); continue; }
                            }
                        };
                        // Architect's explicit placement override wins; otherwise arm to the
                        // repos that selected this rule. A rule routed to zero repos is skipped.
                        let repos = placement.read().get(&r.id).cloned().unwrap_or_else(|| selected_repos.clone());
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
                        match apply_rules(&arm_reqs, &findings).await {
                            Some((ok, message, results)) => {
                                if !ok && results.is_empty() {
                                    crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, if message.is_empty() { "Apply failed.".to_string() } else { message });
                                } else {
                                    let mut done = Vec::new();
                                    for r in results {
                                        if r.ok {
                                            let branch = r.branch.unwrap_or_default();
                                            let path = r.path.unwrap_or_default();
                                            crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("{}: applied to branch '{branch}' (local + pushed, no PR) — {path}", r.repo));
                                            done.push(r.repo);
                                        } else {
                                            crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, format!("{}: {}", r.repo, r.message.unwrap_or_default()));
                                        }
                                    }
                                    if !done.is_empty() { applied_repos.set(done); }
                                }
                            }
                            None => crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, "Apply failed — set a workspace folder + connect GitHub (Contents write)."),
                        }
                        arming.set(false);
                    });
                },
                if arming() { "Applying…" } else { "Add rules to repo(s) (branch + push)" }
            }
            button {
                class: "btn-run",
                disabled: opening_pr() || applied_repos().is_empty(),
                title: if applied_repos().is_empty() { "Apply the rules first; then open the PR from the pushed branch." } else { "" },
                onclick: move |_| {
                    let repos = applied_repos();
                    if repos.is_empty() { return; }
                    opening_pr.set(true);
                    spawn(async move {
                        match open_governance_pr(&repos).await {
                            Some(results) => {
                                for r in results {
                                    if r.ok {
                                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("{}: governance PR → {}", r.repo, r.url.unwrap_or_default()));
                                    } else {
                                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, format!("{}: {}", r.repo, r.message.unwrap_or_default()));
                                    }
                                }
                            }
                            None => crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, "Open PR failed — needs PR write on the connected token."),
                        }
                        opening_pr.set(false);
                    });
                },
                if opening_pr() { "Opening PR…" } else { "Open governance PR" }
            }
        }
        p { class: "arm-note",
            "Add rules to repo(s) writes the governance files (AGENTS.md / CONVENTIONS.md / CI gate / baseline) onto a "
            code { "camerata/onboard-governance" }
            " branch in each repo's local clone AND pushes it to origin — no PR is opened. Edit the working copy as much as you want, then Open governance PR when ready."
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
                    if !r.enforcement.is_empty() {
                        span { class: "rule-modal-tag", "enforcement · {r.enforcement}" }
                    }
                }
                p { class: "rule-modal-placement", "Enforced via: {r.placement}" }
                // The decision this rule frames — what the architect is actually choosing between.
                if let Some(q) = r.decision_question.as_ref().filter(|s| !s.is_empty()) {
                    div { class: "rule-modal-section",
                        span { class: "rule-modal-label", "The decision" }
                        p { class: "rule-modal-question", "{q}" }
                    }
                }
                // The rationale for the adopted default (decision.why).
                if let Some(w) = r.decision_why.as_ref().filter(|s| !s.is_empty()) {
                    div { class: "rule-modal-section",
                        span { class: "rule-modal-label", "Why the default" }
                        p { class: "rule-modal-why", "{w}" }
                    }
                }
                if r.options.is_empty() {
                    p { class: "rule-modal-note", "Single-variant rule — nothing to choose; arm it as-is." }
                } else {
                    div { class: "rule-modal-section",
                        span { class: "rule-modal-label", "Choose the alternative to adopt" }
                        if r.default_option.is_none() {
                            p { class: "rule-modal-mustchoose", "No default — you must choose an alternative before arming." }
                        }
                        div { class: "rule-modal-opts",
                            for o in r.options.iter() {
                                {
                                    let rid = r.id.clone();
                                    let oid = o.id.clone();
                                    let cur = chosen.read().get(&r.id).cloned().or_else(|| r.default_option.clone());
                                    let picked = cur.as_deref() == Some(o.id.as_str());
                                    let is_default = r.default_option.as_deref() == Some(o.id.as_str());
                                    let cls = if picked { "rule-opt on" } else { "rule-opt" };
                                    let mut chosen = chosen;
                                    rsx! {
                                        button {
                                            key: "{o.id}",
                                            class: "{cls}",
                                            onclick: move |_| { chosen.write().insert(rid.clone(), oid.clone()); },
                                            div { class: "rule-opt-head",
                                                span { class: "rule-opt-label", "{o.label}" }
                                                if is_default {
                                                    span { class: "rule-opt-default-badge", "default" }
                                                }
                                                if picked {
                                                    span { class: "rule-opt-picked-badge", "✓ adopted" }
                                                }
                                            }
                                            span { class: "rule-opt-directive", "{o.directive}" }
                                            if !o.why.is_empty() {
                                                span { class: "rule-opt-why", "Why: {o.why}" }
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
    /// Derived `owner/repo` AND the local folder it lives in (recorded as the repo's path).
    Found { repo: String, path: String },
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
            Some(r) => RepoDetect::Found { repo: r.to_string(), path },
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

    // RESTORE a saved onboarding draft on first mount (issue #27): if there's no live scan
    // but a draft exists on disk, bring its scan back so the architect resumes exactly where
    // they left off — no re-scan. ScanResults then rehydrates its own selection/dispositions/
    // audit from the same draft.
    use_future(move || async move {
        if scan.peek().is_none() {
            if let Some(draft) = load_onboarding_draft().await {
                // Rehydrate the repos textarea too — otherwise it reloads empty (showing the
                // placeholder) even though the scan + rules restored, which reads as "the repos
                // were lost." The repo set is exactly the scanned repos.
                if repo.peek().trim().is_empty() {
                    repo.set(draft.scan.repos.join("\n"));
                }
                scan.set(Some(draft.scan));
            }
        }
    });

    let brownfield_cls = if path() == OnboardPath::Brownfield { "onboard-path on" } else { "onboard-path" };
    let greenfield_cls = if path() == OnboardPath::Greenfield { "onboard-path on" } else { "onboard-path" };

    // The flow steps differ slightly by path; both are gated on a connection.
    let steps: &[(&str, &str)] = match path() {
        OnboardPath::Brownfield => &[
            ("Point at the repo(s)", "Name the existing owner/repo(s) your token can reach, or browse to a local folder."),
            ("Scan + propose per-repo rules", "Camerata detects each repo's stack and proposes a starter ruleset per repo — you review, you don't author from scratch."),
            ("Pick rules", "Select rules per repo (project-level rules apply to all). Click a rule to read its options and choose an alternative."),
            ("Audit (optional) + triage", "Optionally scan the code against your selected rules + the security floor, then triage findings (Unresolved / Ignored / Tech debt). Not required to finish onboarding."),
            ("Add rules to repo(s)", "Write the governance files onto a camerata/onboard-governance branch in each local clone and push it — no PR (Open governance PR separately). Applying marks the repo onboarded."),
            ("Wire mechanical rules into CI", "The final step: add the selected mechanical rules to each repo's existing CI as enforced lint gates."),
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

            // No GitHub gate: onboarding reads LOCAL code only. A token is only needed LATER
            // (development time) to push the governance branch / open a PR — surface that when
            // it's missing, but never block onboarding on it.
            if !connected {
                div { class: "onboard-note",
                    "Onboarding works on your local repo folders — no GitHub connection needed here. (A token is only needed later, to push the governance branch and open a PR.)"
                }
            }

            // Repo input — a SET of LOCAL repos (a brownfield onboarding spans inter-related
            // repos). You add a repo by browsing to its local folder; the path is recorded so
            // the repo is immediately a workspace repo (scan/audit/apply all read it locally).
            div { class: "onboard-repo-block",
                label { class: "onboard-repo-label", "Repositories — browse to each repo's local folder (a feature often spans several)" }
                {
                    let names: Vec<String> = repo()
                        .lines()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                    rsx! {
                        if names.is_empty() {
                            p { class: "onboard-repos-empty", "No repos yet — browse to a local repo folder to add one." }
                        } else {
                            div { class: "onboard-repos-list",
                                for name in names {
                                    {
                                        let name_rm = name.clone();
                                        rsx! {
                                            div { class: "onboard-repo-chip", key: "{name}",
                                                span { class: "onboard-repo-chip-name", "{name}" }
                                                button {
                                                    class: "onboard-repo-chip-x",
                                                    title: "Remove",
                                                    onclick: move |_| {
                                                        let kept: Vec<String> = repo()
                                                            .lines()
                                                            .map(|s| s.trim().to_string())
                                                            .filter(|s| !s.is_empty() && s != &name_rm)
                                                            .collect();
                                                        repo.set(kept.join("\n"));
                                                    },
                                                    "\u{2715}"
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                button {
                    class: "btn-edit-sm onboard-browse",
                    onclick: move |_| {
                        spawn(async move {
                            match detect_local_repo().await {
                                RepoDetect::Cancelled => {}
                                RepoDetect::Found { repo: found, path: folder } => {
                                    // Record the local path FIRST so the repo is immediately a
                                    // workspace repo (scan/audit/apply read it locally), then add
                                    // it to the list.
                                    let saved = set_repo_path(&found, &folder).await;
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
                                        if saved {
                                            crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("Added {found} ({folder})"));
                                        } else {
                                            crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, format!("Added {found}, but couldn't record its local path."));
                                        }
                                    }
                                }
                                RepoDetect::Failed(msg) => {
                                    crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, format!("Couldn't read that folder: {msg}. It must be a local git repo with a GitHub origin remote."));
                                }
                            }
                        });
                    },
                    "Browse for a local repo folder\u{2026}"
                }
                button {
                    class: "onboard-cta",
                    disabled: repo().trim().is_empty() || scanning(),
                    // Brownfield scans the whole repo SET (audit + propose rules) from each
                    // repo's LOCAL working tree; greenfield scaffolding is next.
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
                            // A fresh scan starts a new session: clear any prior draft FIRST so
                            // ScanResults doesn't rehydrate the previous run's audit/dispositions
                            // onto these results. Awaited before the scan lands.
                            clear_onboarding_draft().await;
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
                    {
                        // Key by the SCAN's identity (repo set + proposed-rule count) so a
                        // RE-SCAN remounts ScanResults/ProposedRulesTable with fresh rows and a
                        // fresh "recommended -> selected" pass. Without this, the once-per-mount
                        // suggested-selection (and minted row ids) carried over from the first
                        // scan, so adding/removing repos never updated which rules were ticked.
                        // Stable across re-renders of the SAME scan (ticking a rule doesn't
                        // change onboard_scan), so it never wipes the user's selection mid-edit.
                        let scan_key = format!(
                            "{}|{}",
                            report.repos.join(","),
                            report.proposed_rules.len()
                        );
                        rsx! { ScanResults { key: "{scan_key}", report } }
                    }
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
    // Calibration model — its OWN picker (severity recalibration + confidence tagging). A
    // customer can run a cheap scan with a stronger verify, or keep it end-to-end. Defaults
    // to the scan model so "the model you picked" is genuinely used across the board unless
    // the user deliberately splits the tiers.
    let mut calibration_model = use_signal(String::new);
    if calibration_model().is_empty() && !audit_model().is_empty() {
        calibration_model.set(audit_model());
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
    // Selected-rule count, set by ProposedRulesTable and read here for the cost estimate
    // (the estimate also depends on the model + mode pickers, which live in this component).
    let selected_count = use_signal(|| 0usize);
    use_context_provider(|| selected_count);

    // Per-repo rule selection. For a multi-repo scan the architect views ONE repo's rule
    // table at a time (the single-select below) and each repo keeps its own picks. This
    // lifted `repo -> selected rule ids` map is the source of truth the per-repo tables seed
    // from and write back to, so switching repos preserves each repo's selection and one
    // audit covers every repo against its own rules. Empty for a single-repo scan (the table
    // then behaves as the original whole-set table).
    //
    // PRE-SEED every repo with its recommended rules so a repo the architect never opens
    // still audits against a sensible default set (not just the always-on security floor).
    let repo_seed = {
        let mut m = std::collections::HashMap::<String, Vec<String>>::new();
        if report.repos.len() > 1 {
            for repo in &report.repos {
                let ids: Vec<String> = report
                    .proposed_rules
                    .iter()
                    .filter(|r| {
                        r.recommended
                            && rule_has_usable_default(r)
                            && r.repos.iter().any(|rp| rp == repo)
                    })
                    .map(|r| r.id.clone())
                    .collect();
                m.insert(repo.clone(), ids);
            }
        }
        m
    };
    let repo_selection = use_signal(|| repo_seed);
    // Which repo's rule table is in view. Defaults to the first scanned repo.
    let mut viewed_repo = use_signal(|| report.repos.first().cloned().unwrap_or_default());
    let multi_repo = report.repos.len() > 1;
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

    // Per-rule repo placement OVERRIDE (rule id -> repos it installs into). Starts EMPTY:
    // an entry exists only when the architect explicitly overrides a rule's target repos.
    // With no entry, arm falls back to the per-repo SELECTION — i.e. the rule installs into
    // exactly the repos whose table checked it (matching what the audit scans). Seeding this
    // with each rule's scan binding (the old behavior) made the override always-present, so
    // arm ignored the per-repo selection and pushed every "available" rule to all repos.
    // (There's no placement-editor UI yet; this map is the seam for one.)
    let placement = use_signal(std::collections::HashMap::<String, Vec<String>>::new);
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

    // ── Triage state (issue #26) ──────────────────────────────────────────────
    // Each finding lives in one of three tables: Unresolved (the default), Ignored, or
    // Tech debt. The architect moves findings between them until nothing is Unresolved, then
    // Processes the ignored + tech-debt buckets. State is LOCAL until Process.
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    // The shared scan state (reset to None to "start over") + the cockpit view (so
    // "Complete onboarding" can switch the tab to Governed Development).
    let mut onboard_scan = use_context::<Signal<Option<ScanReportView>>>();
    let mut view = use_context::<Signal<CockpitView>>();
    // When the draft last auto-saved (shown with a check), the two-click "start over"
    // arm, and the in-flight "complete onboarding" flag.
    let mut last_saved = use_signal(|| Option::<String>::None);
    let mut restart_arm = use_signal(|| false);
    let mut finishing = use_signal(|| false);
    let dispositions = use_signal(std::collections::HashMap::<String, Disposition>::new);
    let mut triage_view = use_signal(|| TriageState::Unresolved);
    let mut processing = use_signal(|| false);

    // ── Auto-save / restore the onboarding draft (issue #27) ──────────────────
    // On mount, rehydrate this scan's audit / selection / dispositions / view from the saved
    // draft (only when it's the SAME scan). The `draft_loaded` gate keeps the save effect from
    // overwriting the draft with initial (un-rehydrated) state before the restore runs.
    let mut audit_w = audit;
    let mut repo_selection_w = repo_selection;
    let mut dispositions_w = dispositions;
    let mut chosen_w = chosen;
    let mut draft_loaded = use_signal(|| false);
    {
        let report_repos = report.repos.clone();
        use_future(move || {
            let report_repos = report_repos.clone();
            async move {
                if let Some(d) = load_onboarding_draft().await {
                    if d.scan.repos == report_repos {
                        if d.audit.is_some() {
                            audit_w.set(d.audit);
                        }
                        if !d.repo_selection.is_empty() {
                            repo_selection_w.set(d.repo_selection);
                        }
                        if !d.chosen.is_empty() {
                            chosen_w.set(d.chosen);
                        }
                        if !d.dispositions.is_empty() {
                            dispositions_w.set(d.dispositions);
                        }
                        if !d.viewed_repo.is_empty() {
                            viewed_repo.set(d.viewed_repo);
                        }
                        triage_view.set(d.triage_view);
                    }
                }
                draft_loaded.set(true);
            }
        });
    }
    {
        let report = report.clone();
        use_effect(move || {
            // Track every persisted slice so the effect re-runs on any change.
            let audit_v = audit.read().clone();
            let sel = repo_selection.read().clone();
            let cho = chosen.read().clone();
            let disp = dispositions.read().clone();
            let vr = viewed_repo();
            let tv = triage_view();
            if !draft_loaded() {
                return;
            }
            let draft = OnboardingDraft {
                scan: report.clone(),
                audit: audit_v,
                repo_selection: sel,
                chosen: cho,
                dispositions: disp,
                viewed_repo: vr,
                triage_view: tv,
            };
            spawn(async move {
                save_onboarding_draft(&draft).await;
                // Stamp the local time so the UI can show "auto-saved at HH:MM:SS".
                last_saved.set(Some(chrono::Local::now().format("%-I:%M:%S %p").to_string()));
            });
        });
    }

    // Live per-table counts (recompute reactively as dispositions change).
    let (n_unresolved, n_ignored, n_techdebt) = {
        let d = dispositions.read();
        let mut u = 0usize;
        let mut i = 0usize;
        let mut t = 0usize;
        for f in &findings {
            match finding_state(&d, f) {
                TriageState::Unresolved => u += 1,
                TriageState::Ignored => i += 1,
                TriageState::TechDebt => t += 1,
            }
        }
        (u, i, t)
    };

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
                            {
                                // Bold the calibration "[needs review: …]" flag so the reason it
                                // was hedged stands out from the explanation body.
                                let (body, nr) = split_needs_review(&f.detail);
                                rsx! {
                                    p { class: "rule-modal-detail",
                                        "{body}"
                                        if let Some(reason) = nr {
                                            " "
                                            b { class: "nr-inline",
                                                if reason.is_empty() { "[needs review]" } else { "[needs review: {reason}]" }
                                            }
                                        }
                                    }
                                }
                            }
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
                if report.files_excluded > 0 {
                    span { class: "scan-stat",
                        span { class: "scan-stat-n", "{report.files_excluded}" }
                        " excluded as noise"
                    }
                }
                if !report.excluded_mechanical_rules.is_empty() {
                    span { class: "scan-stat",
                        title: "{report.excluded_mechanical_rules.join(\", \")}",
                        span { class: "scan-stat-n", "{report.excluded_mechanical_rules.len()}" }
                        " mechanical rule(s) enforced in CI, not scanned"
                    }
                }
            }

            // Onboarding status + lifecycle actions. The post-scan steps (audit, triage,
            // apply, wire-CI) are all optional, so "Complete onboarding" is available here.
            div { class: "onboard-actionbar",
                if let Some(ts) = last_saved() {
                    span { class: "onboard-saved", "✓ Auto-saved at {ts}" }
                }
                div { class: "onboard-actionbar-spacer" }
                button {
                    class: "btn-secondary danger",
                    onclick: move |_| {
                        if restart_arm() {
                            spawn(async move {
                                clear_onboarding_draft().await;
                                restart_arm.set(false);
                                onboard_scan.set(None);
                            });
                        } else {
                            restart_arm.set(true);
                        }
                    },
                    if restart_arm() { "Confirm: discard & rescan?" } else { "Start over" }
                }
                button {
                    class: "btn-run",
                    disabled: finishing(),
                    onclick: move |_| {
                        finishing.set(true);
                        spawn(async move {
                            if complete_onboarding().await {
                                crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, "Onboarding complete. Rules saved to the project.");
                                onboard_scan.set(None);
                                view.set(CockpitView::Stories);
                            } else {
                                crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, "Could not complete onboarding.");
                            }
                            finishing.set(false);
                        });
                    },
                    if finishing() { "Finishing…" } else { "Complete onboarding" }
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
            // Per-repo view switch (multi-repo only): pick which repo's recommended-rule
            // table to view + select. Each repo keeps its own selection; the audit runs every
            // repo against its own picks.
            if multi_repo {
                div { class: "repo-select",
                    label { class: "repo-select-label", "Repo ruleset:" }
                    select {
                        class: "repo-select-input",
                        value: "{viewed_repo}",
                        onchange: move |e| viewed_repo.set(e.value()),
                        for repo in report.repos.iter() {
                            option { key: "{repo}", value: "{repo}", "{repo}" }
                        }
                    }
                    span { class: "repo-select-hint",
                        "Showing rules for this repo. Each repo has its own selection; the audit scans every repo against its own rules."
                    }
                }
            }
            {
                let repos_audit = report.repos.clone();
                // Per-repo binding drives RECOMMENDATION (pre-selection), NOT visibility: every
                // repo's table shows the WHOLE rule library so the architect can manually add
                // ANY rule to ANY repo. (Filtering visibility by the repo binding hid rules that
                // were auto-suggested for a sibling repo — e.g. ci-cd suggested for repo A never
                // appeared in repo B's table, so it couldn't be added there at all.) The viewed
                // repo only changes which rules are pre-checked, via the seeded per-repo selection.
                let view_repo = if multi_repo { viewed_repo() } else { String::new() };
                let all_rules = report.proposed_rules.clone();
                let visible_rules = all_rules.clone();
                rsx! {
                    ProposedRulesTable {
                        // Key on the viewed repo so switching remounts the table with that
                        // repo's seeded selection.
                        key: "{view_repo}",
                        rules: visible_rules,
                        all_rules,
                        view_repo,
                        repo_selection,
                        findings: findings.clone(),
                        auditing: auditing(),
                        on_audit: move |rules: Vec<SelectedAuditRule>| {
                            let repos = repos_audit.clone();
                            let model = audit_model();
                            let calib = calibration_model();
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
                                    let Some(jid) = audit_job_start(&repos, &rules, &model, &calib, "parallel").await else {
                                        auditing.set(false);
                                        return;
                                    };
                                    active_audit_job.set(Some(jid.clone()));
                                    poll_job(jid, audit, auditing, job_progress, active_audit_job).await;
                                });
                            } else {
                                // Synchronous: hold the request until the (shorter) run finishes.
                                spawn(async move {
                                    audit.set(audit_against(&repos, &rules, &model, &calib, &mode).await);
                                    auditing.set(false);
                                });
                            }
                        },
                    }
                }
            }

            // ── Phase 2: the audit runs from the table's "Audit selected" button ──
            div { class: "audit-cta",
                p { class: "scan-section-h", "Step 2 — audit the code against your selected rules (optional)" }
                p { class: "scan-section-sub", "The audit is OPTIONAL — you can Apply the rules above and finish onboarding without it. Run it when you want to see existing violations to triage. Tick the rules, then press “Audit code against selected rules”. The deterministic security rules (secrets / raw-SQL / secret-URLs) always run as the enforced floor; the AI checks the code against ONLY your selected rules AND flags anything else worth a look (advisory)." }
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
                    // Calibration model — its OWN tier. The scan finds; calibration
                    // recalibrates severity + tags confidence. Defaults to the scan model
                    // (end-to-end); split it to run a cheap scan with a stronger verify.
                    div { class: "audit-model-row",
                        label { class: "audit-model-label", "Calibration model" }
                        select {
                            class: "audit-model-select",
                            disabled: auditing(),
                            value: "{calibration_model}",
                            onchange: move |e| calibration_model.set(e.value()),
                            for opt in m.models.iter() {
                                option { key: "{opt.id}", value: "{opt.id}", "{opt.label}" }
                            }
                        }
                        span { class: "audit-model-hint", "Recalibrates severity + flags low-confidence findings. Default = the scan model; pick a stronger one for cheap-scan-plus-smart-verify." }
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
                // Cost: the pre-audit ESTIMATE for this configuration (model + calibration
                // model + mode + ticked rules), and — once the audit has run — the ACTUAL
                // billed usage beside it, so the estimate is verifiable, not a black box.
                if report.code_chars > 0 {
                    {
                        let price = |id: &str, fallback: (f64, f64)| {
                            models.as_ref()
                                .and_then(|m| m.models.iter().find(|o| o.id == id).map(|o| (o.price_in, o.price_out)))
                                .unwrap_or(fallback)
                        };
                        let (a_in, a_out) = price(&audit_model(), (3.0, 15.0));
                        let (c_in, c_out) = price(&calibration_model(), (a_in, a_out));
                        let sel = selected_count();
                        let (toks, dollars, passes) = estimate_audit_cost(report.code_chars, sel, &audit_mode(), a_in, a_out, c_in, c_out);
                        let code_toks = human_tokens((report.code_chars as f64 / 4.0) as u64);
                        let dollar_str = if dollars < 0.01 { "<$0.01".to_string() } else { format!("~${dollars:.2}") };
                        // ACTUAL, once the audit finished and the backend reported usage.
                        let actual = audited.as_ref().and_then(|a| a.actual_usage.clone()).filter(|u| u.calls > 0);
                        rsx! {
                            div { class: "audit-cost",
                                if let Some(u) = actual {
                                    {
                                        let act_toks = u.input_tokens + u.output_tokens;
                                        let act_dollar = if !u.cost_complete { "n/a".to_string() }
                                            else if u.cost_usd < 0.01 { "<$0.01".to_string() }
                                            else { format!("${:.2}", u.cost_usd) };
                                        rsx! {
                                            div { class: "audit-cost-main",
                                                span { class: "audit-cost-label", "Actual cost" }
                                                span { class: "audit-cost-val", "{act_dollar}" }
                                                span { class: "audit-cost-meta", "{human_tokens(act_toks)} tokens · {u.calls} calls · est. was {dollar_str}" }
                                            }
                                            p { class: "audit-cost-note",
                                                if u.cost_complete {
                                                    "Real billed usage for this run ({human_tokens(u.input_tokens)} in / {human_tokens(u.output_tokens)} out). "
                                                } else {
                                                    "Real token usage shown; a $ figure needs every call to report cost (some didn't, so it's omitted to avoid understating). "
                                                }
                                                "The deterministic security floor ran free. Next time you audit PR diffs — pennies."
                                            }
                                        }
                                    }
                                } else {
                                    div { class: "audit-cost-main",
                                        span { class: "audit-cost-label", "Estimated cost" }
                                        span { class: "audit-cost-val", "{dollar_str}" }
                                        span { class: "audit-cost-meta", "~{human_tokens(toks)} tokens · {passes} pass(es) · {sel} rule(s)" }
                                    }
                                    p { class: "audit-cost-note",
                                        "Approximate, biased high (input + output priced separately; output bills ~5× and dominates findings-heavy scans). "
                                        "One-time baseline over ~{code_toks} tokens of code ({report.files_scanned} files); prompt-caching can make the actual bill lower. "
                                        "The deterministic security floor (secrets / raw-SQL / secret-URLs) runs free. "
                                        "After this, you audit PR diffs — pennies. Cheaper model or Sequential mode lowers this."
                                    }
                                }
                            }
                        }
                    }
                }
                // Live progress for an async job: a determinate bar (it grows as repos are
                // discovered) + a findings-so-far count, so a walk-away scan shows life.
                if let Some((done, total, nf)) = job_progress() {
                    {
                        let pct = (done * 100).checked_div(total).unwrap_or(0).min(100);
                        let n_repos = report.repos.len();
                        rsx! {
                            div { class: "job-progress",
                                div { class: "job-progress-track",
                                    div { class: "job-progress-fill", style: "width: {pct}%" }
                                }
                                span { class: "job-progress-label", "{done}/{total} passes · {nf} finding(s) so far" }
                                // Multi-repo scans run ONE repo at a time, and the pass
                                // denominator grows as each repo is reached — so the agent
                                // activity below shows only the repo currently running, not all
                                // of them. Make the full scope explicit so it doesn't look like
                                // only one repo is being scanned.
                                if n_repos > 1 {
                                    div { class: "job-progress-scope",
                                        span { class: "job-progress-scope-h", "{n_repos} repos in scope, scanned one at a time:" }
                                        for repo in report.repos.iter() {
                                            span { key: "{repo}", class: "job-progress-repo", "{repo}" }
                                        }
                                        span { class: "job-progress-scope-note", "The pass count climbs as each repo is reached; agent activity shows the repo running now." }
                                    }
                                }
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
                // the audit (so you can trust it's really working, not hung). Shown ONLY for
                // a current-session audit (running or done THIS mount). The transcript lives
                // server-side, so without this gate a remount (e.g. switching cockpit tabs
                // and back) re-renders the PREVIOUS run's transcript while the findings —
                // which are client state — are gone: a confusing half-restored state. Gating
                // it on the same lifecycle as the findings keeps the two consistent (both
                // present during/after an audit, both absent on a fresh remount).
                if auditing() || audited.is_some() {
                    crate::agent_activity::AgentActivity { run_id: "scan-audit".to_string() }
                }
            }

            // ── Findings (after the audit runs) ────────────────────────────────
            if audited.is_some() {
                p { class: "scan-section-h", "Findings" }
                p { class: "scan-section-sub", "Triage every finding into one of three tables: leave it Unresolved, Ignore it (with a reason), or save it as Tech debt. Switch tables below; selected findings move between tables. When nothing is Unresolved, Process the ignored + tech-debt buckets." }

                // Single-select over the three triage tables, each with a live count.
                div { class: "triage-switch",
                    for st in [TriageState::Unresolved, TriageState::Ignored, TriageState::TechDebt] {
                        {
                            let count = match st { TriageState::Unresolved => n_unresolved, TriageState::Ignored => n_ignored, TriageState::TechDebt => n_techdebt };
                            let active = triage_view() == st;
                            rsx! {
                                button {
                                    key: "{st.label()}",
                                    class: if active { "triage-tab active" } else { "triage-tab" },
                                    onclick: move |_| triage_view.set(st),
                                    "{st.label()} "
                                    span { class: "triage-tab-count", "{count}" }
                                }
                            }
                        }
                    }
                }

                // Wrapped so the key is the first node in its block (Dioxus requirement);
                // keying on the view remounts the table so its frozen rows reflect the switch.
                {
                    rsx! {
                        FindingsTable {
                            key: "{triage_view().label()}",
                            findings: findings.clone(),
                            repos: report.repos.clone(),
                            descriptions: descriptions.clone(),
                            triage_view: triage_view(),
                            dispositions,
                        }
                    }
                }

                // Process: commit the ignored bucket to the baseline and file the tech-debt
                // bucket as tickets. Enabled only once nothing remains Unresolved.
                if triage_view() == TriageState::TechDebt || n_unresolved == 0 {
                    {
                    let findings_for_process = findings.clone();
                    rsx! {
                    div { class: "triage-process",
                        if n_unresolved > 0 {
                            p { class: "section-hint", "Resolve the {n_unresolved} remaining Unresolved finding(s) (Ignore or save as Tech debt) before Processing." }
                        }
                        button {
                            class: "btn-run",
                            disabled: processing() || n_unresolved > 0 || (n_ignored == 0 && n_techdebt == 0),
                            onclick: move |_| {
                                let d = dispositions.read().clone();
                                // Group ignored findings by (repo, reason) -> baseline waiver;
                                // group tech-debt by repo -> a tracked ticket.
                                let mut ignore_groups: std::collections::HashMap<(String, String), Vec<FindingView>> = Default::default();
                                // Tech-debt "resolve later" -> a tracked ticket (GitHub issue), grouped by repo.
                                let mut debt_later: std::collections::HashMap<String, Vec<FindingView>> = Default::default();
                                // Tech-debt "resolve now" -> ALSO a GitHub issue (the story), grouped by repo.
                                // The story makes it into GitHub now (Pillar 1); the dev-engine INGEST of a
                                // resolve-now story is Pillar 2 — same issue, flagged in its title for pickup.
                                let mut debt_now: std::collections::HashMap<String, Vec<FindingView>> = Default::default();
                                for f in &findings_for_process {
                                    let disp = d.get(&finding_key(f)).cloned().unwrap_or_default();
                                    match disp.state {
                                        TriageState::Ignored => {
                                            ignore_groups.entry((f.repo.clone(), disp.reason.clone())).or_default().push(f.clone());
                                        }
                                        TriageState::TechDebt => match disp.bucket {
                                            TechDebtBucket::Later => debt_later.entry(f.repo.clone()).or_default().push(f.clone()),
                                            TechDebtBucket::Now => debt_now.entry(f.repo.clone()).or_default().push(f.clone()),
                                        },
                                        TriageState::Unresolved => {}
                                    }
                                }
                                if ignore_groups.is_empty() && debt_later.is_empty() && debt_now.is_empty() { return; }
                                processing.set(true);
                                spawn(async move {
                                    let mut ok = 0usize;
                                    let mut failed = 0usize;
                                    for ((repo, reason), group) in &ignore_groups {
                                        let r = if reason.trim().is_empty() { "Accepted during onboarding triage".to_string() } else { reason.clone() };
                                        match ignore_findings(repo, group, &r, None).await {
                                            Some(_) => ok += group.len(),
                                            None => failed += group.len(),
                                        }
                                    }
                                    for (repo, group) in &debt_later {
                                        match create_ticket(repo, group, None).await {
                                            Some(_) => ok += group.len(),
                                            None => failed += group.len(),
                                        }
                                    }
                                    // Resolve-now -> a GitHub issue titled so the dev layer (Pillar 2) can pick
                                    // it up for ingest. For Pillar 1 the win is: the story lands in GitHub.
                                    for (repo, group) in &debt_now {
                                        let title = format!("Tech debt (resolve now): {} finding(s) for the dev engine", group.len());
                                        match create_ticket(repo, group, Some(&title)).await {
                                            Some(_) => ok += group.len(),
                                            None => failed += group.len(),
                                        }
                                    }
                                    let msg = format!("Processed {ok} finding(s): ignores → baseline; tech-debt → GitHub issues (resolve-now issues are flagged for the dev engine when Pillar 2 lands).");
                                    if failed == 0 {
                                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, msg);
                                    } else {
                                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Warning, format!("Processed {ok}; {failed} failed (needs GitHub Issues write)."));
                                    }
                                    processing.set(false);
                                });
                            },
                            if processing() { "Processing…" } else { "Process ignored + tech-debt buckets" }
                        }
                    }
                    }
                    }
                }

                // ── Optional: wire mechanical rules into CI (#32) ──────────────────
                // Files a STORY (GitHub issue) per repo to add the selected mechanical rules
                // to that repo's existing CI as enforced lint gates. This is OPTIONAL — it does
                // NOT gate "onboarded". Use "Complete onboarding" above to finish at any point;
                // the dev layer picks the CI story up later.
                if n_unresolved == 0 {
                    div { class: "onboard-final-step",
                        span { class: "onboard-step-eyebrow", "Optional: wire mechanical rules into CI" }
                        CiRulesPanel { repos: report.repos.clone() }
                        p { class: "section-hint", "Optional, and independent of the tech-debt work above. This files a CI-wiring story (a GitHub issue); it is not required to finish onboarding — use \u{201c}Complete onboarding\u{201d} whenever you're ready." }
                    }
                }
            }
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

/// The Unit of Work dev panel for a selected story.
///
/// Shows the dev-side projection alongside the story's tracker status:
/// - Dev status control (3-state segmented control: New / In progress / Done).
/// - Branch ref (if set, read-only here — auto-populated by the governed run).
/// - AI development history (HistoryEntry rows: ts · kind · text), read-only.
///
/// Fetch is keyed by `story_id` so switching stories reloads the UoW. A shared
/// `uow_refresh` tick lets the spine badges update after a status change.
///
/// NOTE: branch + history are designed to be auto-populated by the governed run
/// (Pillar 2). They are settable via the API endpoints; the UI shows them here.
#[component]
fn UowPanel(story_id: String, uow_refresh: Signal<u32>) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let sid = story_id.clone();
    let uow_data = use_resource(move || {
        let sid = sid.clone();
        async move { fetch_uow(&sid).await }
    });

    let uow = uow_data.read().clone().flatten();
    let dev_status = uow.as_ref().map(|u| u.dev_status).unwrap_or_default();
    let branch = uow.as_ref().and_then(|u| u.branch.clone());
    let history = uow.as_ref().map(|u| u.history.clone()).unwrap_or_default();

    // The three status options for the segmented control.
    const STATUS_OPTS: &[DevStatus] = &[DevStatus::New, DevStatus::InProgress, DevStatus::Done];

    rsx! {
        div { class: "uow-panel",
            p { class: "uow-panel-h", "UNIT OF WORK" }

            // ── Dev status: 3-state segmented control ──────────────────────────
            div { class: "uow-status-row",
                span { class: "uow-field-label", "Dev status" }
                div { class: "uow-seg",
                    for opt in STATUS_OPTS.iter().copied() {
                        {
                            let sid = story_id.clone();
                            let active = opt == dev_status;
                            let cls = if active { "uow-seg-btn active" } else { "uow-seg-btn" };
                            rsx! {
                                button {
                                    class: "{cls}",
                                    onclick: move |_| {
                                        let sid = sid.clone();
                                        let mut uow_refresh = uow_refresh;
                                        let toasts = toasts;
                                        spawn(async move {
                                            if post_uow_status(&sid, opt).await.is_some() {
                                                // Bump both: the panel re-fetches its own UoW,
                                                // and the spine badges refresh via the map.
                                                uow_refresh += 1;
                                            } else {
                                                crate::toast::push_toast(
                                                    toasts,
                                                    crate::toast::ToastKind::Warning,
                                                    "Could not update dev status.".to_string(),
                                                );
                                            }
                                        });
                                    },
                                    "{opt.label()}"
                                }
                            }
                        }
                    }
                }
            }

            // ── Branch ref (read-only; auto-populated by the governed run) ─────
            div { class: "uow-branch-row",
                span { class: "uow-field-label", "Branch" }
                if let Some(ref b) = branch {
                    span { class: "uow-branch-val", "{b}" }
                } else {
                    span { class: "uow-branch-none", "not set" }
                }
            }

            // ── AI development history ─────────────────────────────────────────
            div { class: "uow-history",
                p { class: "uow-history-h", "AI history" }
                if history.is_empty() {
                    p { class: "uow-history-empty", "No history yet — the governed run will append entries here." }
                } else {
                    div { class: "uow-history-list",
                        for entry in history.iter() {
                            div { class: "uow-history-row",
                                span { class: "uow-hist-ts", "{entry.ts}" }
                                span { class: "uow-hist-kind", "{entry.kind}" }
                                span { class: "uow-hist-text", "{entry.text}" }
                            }
                        }
                    }
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

// ── Docs view ─────────────────────────────────────────────────────────────────

/// In-app documentation viewer. Renders USER_GUIDE.md and TECHNICAL.md as
/// markdown, with a toggle to switch between them. Uses the same `md_to_html`
/// renderer and `.chat-turn-text.md` CSS that the chat bubble uses, so tables,
/// code blocks, and headings are styled consistently.
const DOCS_USER_GUIDE: &str = include_str!("../../../docs/USER_GUIDE.md");
const DOCS_TECHNICAL: &str = include_str!("../../../docs/TECHNICAL.md");

#[derive(Clone, Copy, PartialEq, Eq)]
enum DocsTab {
    UserGuide,
    Technical,
}

#[component]
fn DocsView() -> Element {
    let mut tab = use_signal(|| DocsTab::UserGuide);

    let content = match tab() {
        DocsTab::UserGuide => crate::md::md_to_html(DOCS_USER_GUIDE),
        DocsTab::Technical => crate::md::md_to_html(DOCS_TECHNICAL),
    };

    rsx! {
        div { class: "docs-view",
            div { class: "docs-tabs",
                button {
                    class: if tab() == DocsTab::UserGuide { "chat-mode-btn active" } else { "chat-mode-btn" },
                    onclick: move |_| tab.set(DocsTab::UserGuide),
                    "User Guide"
                }
                button {
                    class: if tab() == DocsTab::Technical { "chat-mode-btn active" } else { "chat-mode-btn" },
                    onclick: move |_| tab.set(DocsTab::Technical),
                    "Technical"
                }
            }
            div { class: "docs-body chat-turn-text md", dangerous_inner_html: content }
        }
    }
}
