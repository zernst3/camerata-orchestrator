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

// Chorale (crates.io, headless table) backs the brownfield audit-findings and
// proposed-rules tables — the surfaces where the data genuinely scales.
use chorale_core::{
    BadgeVariant, BadgeVariantMap, CellValue, ColumnDef, ColumnId, FilterKind, PaginationMode,
    RenderKind, RowId, TableState,
};
use chorale_dioxus::{use_table, RowCellRenderer, RowCellRenderers, RowClass, Table};

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
    /// Max developer→checker bounce-and-revise iterations a stage may take before the
    /// fleet stops the loop and raises the outstanding violations for human review (#29).
    /// Defaults to 1.
    #[serde(default = "default_max_iterations")]
    max_iterations: usize,
    /// The project's model tier map: fast/balanced/strongest -> model id.
    /// Serde default fills in the fleet defaults when the field is absent (back-compat).
    #[serde(default)]
    tier_map: TierMapView,
}

/// UI mirror of `camerata_fleet::tier::TierMap`. Three model-id slots, one per
/// capability band. Serde defaults match the fleet defaults.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
struct TierMapView {
    #[serde(default = "default_fast_model_str")]
    fast: String,
    #[serde(default = "default_balanced_model_str")]
    balanced: String,
    #[serde(default = "default_strongest_model_str")]
    strongest: String,
}

fn default_fast_model_str() -> String {
    // BUG-UI-1: align with the fleet canonical id in crates/fleet/src/tier.rs::default_fast_model().
    // The previous value "claude-haiku-4-5" (without a date suffix) would cause the settings panel
    // to show and save a different id than the fleet actually uses ("claude-haiku-4-5-20251001"),
    // so a user hitting "Save" without changing anything would pin the wrong model.
    "claude-haiku-4-5-20251001".to_string()
}
fn default_balanced_model_str() -> String {
    "claude-sonnet-4-6".to_string()
}
fn default_strongest_model_str() -> String {
    "claude-opus-4-8".to_string()
}

impl Default for TierMapView {
    fn default() -> Self {
        Self {
            fast: default_fast_model_str(),
            balanced: default_balanced_model_str(),
            strongest: default_strongest_model_str(),
        }
    }
}

fn default_max_iterations() -> usize {
    1
}

/// GET a JSON resource from the BFF, retrying on a connection failure so a fetch that
/// races the embedded server's startup is not rendered as "empty". The desktop app boots
/// its server in-process; the first request(s) can land before it accepts connections,
/// which previously showed the projects list empty until a remount re-fetched. Retries for
/// ~2.5s, then gives up (returns None) so a genuinely-down server still fails. A successful
/// request whose body is empty / `null` is NOT retried — that is real data, not a race.
async fn bff_get_json<T: serde::de::DeserializeOwned>(path: &str) -> Option<T> {
    let url = format!("{}{}", crate::BFF_URL, path);
    for attempt in 0..10u32 {
        match reqwest::get(url.as_str()).await {
            Ok(resp) => return resp.json::<T>().await.ok(),
            Err(_) if attempt < 9 => {
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
            Err(_) => return None,
        }
    }
    None
}

async fn fetch_projects() -> Option<Vec<ProjectView>> {
    bff_get_json::<Vec<ProjectView>>("/api/projects").await
}

async fn fetch_active_project() -> Option<ProjectView> {
    bff_get_json::<Option<ProjectView>>("/api/projects/active")
        .await
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

/// Update the project's model-tier map (fast / balanced / strongest model ids).
/// Uses the `POST /api/projects/:id/tier-map` endpoint added in #63. Patch semantics:
/// all three bands are always sent so a single round-trip sets the whole map.
async fn set_project_tier_map(id: &str, map: &TierMapView) -> bool {
    reqwest::Client::new()
        .post(format!("{}/api/projects/{}/tier-map", crate::BFF_URL, id))
        .json(&serde_json::json!({
            "fast":     map.fast,
            "balanced": map.balanced,
            "strongest": map.strongest,
        }))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Update a project's loop-guard ceiling (#29): the max developer→checker
/// bounce-and-revise iterations a stage may take before the fleet stops and
/// raises the outstanding violations for human review.
async fn set_max_iterations(id: &str, max_iterations: usize) -> bool {
    reqwest::Client::new()
        .post(format!(
            "{}/api/projects/{}/max-iterations",
            crate::BFF_URL,
            id
        ))
        .json(&serde_json::json!({ "max_iterations": max_iterations }))
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
        .post(format!(
            "{}/api/projects/{}/emit",
            crate::BFF_URL,
            project_id
        ))
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
        .post(format!(
            "{}/api/projects/{}/custom",
            crate::BFF_URL,
            project_id
        ))
        .json(&serde_json::json!({ "name": name, "body": body, "domain": domain }))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Delete a custom rule by name (the only way a custom rule leaves a project).
async fn delete_custom_rule(project_id: &str, name: &str) -> bool {
    reqwest::Client::new()
        .post(format!(
            "{}/api/projects/{}/custom/delete",
            crate::BFF_URL,
            project_id
        ))
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
            CellValue::Text(if c.domain.is_empty() {
                "*".to_string()
            } else {
                c.domain.clone()
            })
        })
        .sortable()
        .initial_width(150.0),
        ColumnDef::new(ColumnId("body"), "Directive", |c: &CustomRuleView| {
            CellValue::Text(c.body.clone())
        })
        .initial_width(460.0),
        // Type: custom rules are free-text directives with no formal enforcement
        // classification. They are prose or structured in practice (prose when
        // written as a principle; structured when they express a concrete contract),
        // but cannot be mechanical or architectural without a dev task to build a
        // checker. We show "prose / structured" honestly rather than fabricating a
        // single modality. No per-cell tooltip is wired here because the value is
        // constant across all rows; see the legend below the table for definitions.
        ColumnDef::new(ColumnId("enf_type"), "Type", |_c: &CustomRuleView| {
            CellValue::Text("prose / structured".to_string())
        })
        .initial_width(140.0),
    ]
}

/// The custom-rules editor: a chorale table of the project's custom rules grouped
/// by domain, with selection -> delete. The add/edit form lives in the parent.
#[component]
fn CustomRulesTable(
    custom: Vec<CustomRuleView>,
    project_id: String,
    refresh: Signal<u32>,
) -> Element {
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
        // Type modality legend: custom rules are always "prose / structured" (free-text
        // directives). The four modalities are defined here so the architect understands
        // what they mean without hovering each row (all rows show the same value).
        details { class: "modality-legend",
            summary { class: "modality-legend-summary", "Type modality key" }
            dl { class: "modality-legend-list",
                dt { "Prose" }
                dd { "A principle or idiom a human judges \u{2014} a matter of degree (rendered to AGENTS.md)." }
                dt { "Structured" }
                dd { "A concrete design contract with a clear conform/violate answer; human-verified, not lint-able (CONVENTIONS.md)." }
                dt { "Mechanical" }
                dd { "An existing off-the-shelf linter decides it (clippy, eslint, ruff, golangci-lint, \u{2026})." }
                dt { "Architectural" }
                dd { "Deterministic, but needs a bespoke custom checker \u{2014} no off-the-shelf linter expresses it." }
            }
        }
    }
}

/// Import a ruleset JSON (upsert base rules; the server preserves custom).
async fn import_ruleset(project_id: &str, json: String) -> bool {
    reqwest::Client::new()
        .post(format!(
            "{}/api/projects/{}/ruleset",
            crate::BFF_URL,
            project_id
        ))
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
        .post(format!(
            "{}/api/projects/{}/ruleset",
            crate::BFF_URL,
            project_id
        ))
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
enum SelectionBucket {
    Selections,
    CrossRepo,
    Process,
}

fn bucket_of(rule: &ProposedRuleView) -> SelectionBucket {
    match rule.scope.as_str() {
        "cross-repo" => SelectionBucket::CrossRepo,
        "process" => SelectionBucket::Process,
        _ => SelectionBucket::Selections,
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
        self.corpus
            .as_ref()
            .map(|r| r.domain.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "general".to_string())
    }
    fn title(&self) -> String {
        self.corpus
            .as_ref()
            .map(|r| r.title.clone())
            .unwrap_or_else(|| self.selection.rule_id.clone())
    }
    fn scope_label(&self) -> &'static str {
        match self.bucket {
            SelectionBucket::Selections => "repo-local",
            SelectionBucket::CrossRepo => "cross-repo",
            SelectionBucket::Process => "process",
        }
    }
    fn chosen_label(&self) -> String {
        match (&self.selection.chosen_option, &self.corpus) {
            (Some(oid), Some(rule)) => rule
                .options
                .iter()
                .find(|o| &o.id == oid)
                .map(|o| o.label.clone())
                .unwrap_or_else(|| oid.clone()),
            _ => String::from("\u{2014}"),
        }
    }
}

/// Map an enforcement modality string to its badge variant (label + color).
/// Shared by applied_rule_columns, corpus_columns, and the RowCellRenderer tooltips
/// so every table uses the same visual language.
fn enforcement_badges() -> BadgeVariantMap {
    BadgeVariantMap::new()
        .with("prose",        BadgeVariant::new("Prose",        "gray"))
        .with("structured",   BadgeVariant::new("Structured",   "blue"))
        .with("mechanical",   BadgeVariant::new("Mechanical",   "green"))
        .with("architectural",BadgeVariant::new("Architectural","yellow"))
        .with_fallback(BadgeVariant::new("\u{2014}", "gray"))
}

/// Return the modality definition tooltip text for a given enforcement value.
/// Used both in modal `title` attributes and in the per-cell RowCellRenderer tooltips.
fn enforcement_tooltip(enforcement: &str) -> &'static str {
    match enforcement {
        "prose"        => "A principle or idiom a human judges \u{2014} a matter of degree (rendered to AGENTS.md).",
        "structured"   => "A concrete design contract with a clear conform/violate answer; human-verified, not lint-able (CONVENTIONS.md).",
        "mechanical"   => "An existing off-the-shelf linter decides it (clippy, eslint, ruff, golangci-lint, \u{2026}).",
        "architectural"=> "Deterministic, but needs a bespoke custom checker \u{2014} no off-the-shelf linter expresses it.",
        _              => "The enforcement modality for this rule is not yet classified.",
    }
}

fn applied_rule_columns() -> Vec<ColumnDef<AppliedRuleRow>> {
    let scope_badges = BadgeVariantMap::new()
        .with("repo-local", BadgeVariant::new("Repo-local", "green"))
        .with("cross-repo", BadgeVariant::new("Cross-repo", "yellow"))
        .with("process", BadgeVariant::new("Process", "gray"));
    let verif_badges = BadgeVariantMap::new()
        .with("verified",      BadgeVariant::new("\u{2713} Verified",  "green"))
        .with("grounded",      BadgeVariant::new("\u{29bf} Grounded", "blue"))
        .with("needs_recheck", BadgeVariant::new("Needs re-check",    "yellow"))
        .with("draft",         BadgeVariant::new("Draft",             "gray"));
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
        // Provenance badge sourced from the corpus join. Falls back to `draft` for
        // unknown / custom rule ids that have no corpus entry.
        ColumnDef::new(ColumnId("verif"), "Provenance", |r: &AppliedRuleRow| {
            CellValue::Text(
                r.corpus
                    .as_ref()
                    .map(|c| c.verification.clone())
                    .unwrap_or_else(|| "draft".to_string()),
            )
        })
        .sortable()
        .render_kind(RenderKind::Badge(verif_badges))
        .initial_width(140.0),
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
                CellValue::Text(if r.selection.repos.is_empty() {
                    "\u{2014}".to_string()
                } else {
                    r.selection.repos.join(", ")
                })
            }
        })
        .filter(FilterKind::Text)
        .initial_width(200.0),
        ColumnDef::new(ColumnId("option"), "Chosen option", |r: &AppliedRuleRow| {
            CellValue::Text(r.chosen_label())
        })
        .sortable()
        .initial_width(180.0),
        // Type (enforcement modality): prose / structured / mechanical / architectural.
        // Sourced from the corpus join; falls back to empty (shown as "—" by the badge
        // fallback) when the rule id has no corpus entry (custom / unknown ids).
        // The RowCellRenderer in ProjectRulesTable adds a `title` tooltip explaining
        // the modality definition; see `enforcement_tooltip()`.
        ColumnDef::new(ColumnId("enf_type"), "Type", |r: &AppliedRuleRow| {
            CellValue::Text(
                r.corpus
                    .as_ref()
                    .map(|c| c.enforcement.clone())
                    .unwrap_or_default(),
            )
        })
        .sortable()
        .render_kind(RenderKind::Badge(enforcement_badges()))
        .initial_width(130.0),
    ]
}

fn corpus_columns() -> Vec<ColumnDef<ProposedRuleView>> {
    let scope_badges = BadgeVariantMap::new()
        .with("repo-local", BadgeVariant::new("Repo-local", "green"))
        .with("cross-repo", BadgeVariant::new("Cross-repo", "yellow"))
        .with("process", BadgeVariant::new("Process", "gray"));
    let verif_badges = BadgeVariantMap::new()
        .with("verified",      BadgeVariant::new("\u{2713} Verified",  "green"))
        .with("grounded",      BadgeVariant::new("\u{29bf} Grounded", "blue"))
        .with("needs_recheck", BadgeVariant::new("Needs re-check",    "yellow"))
        .with("draft",         BadgeVariant::new("Draft",             "gray"));
    vec![
        ColumnDef::new(ColumnId("domain"), "Domain", |r: &ProposedRuleView| {
            CellValue::Text(if r.domain.is_empty() {
                "general".to_string()
            } else {
                r.domain.clone()
            })
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
        // Provenance / verification badge: lets the architect see which rules are
        // human-confirmed, which are grounded in a cited source, and which are still
        // AI drafts before deciding to apply them. See `verif_badge()`.
        ColumnDef::new(ColumnId("verif"), "Provenance", |r: &ProposedRuleView| {
            CellValue::Text(r.verification.clone())
        })
        .sortable()
        .render_kind(RenderKind::Badge(verif_badges))
        .initial_width(140.0),
        ColumnDef::new(ColumnId("scope"), "Scope", |r: &ProposedRuleView| {
            CellValue::Text(r.scope.clone())
        })
        .sortable()
        .render_kind(RenderKind::Badge(scope_badges))
        .initial_width(130.0),
        ColumnDef::new(
            ColumnId("applied_to"),
            "Applied to",
            |r: &ProposedRuleView| {
                CellValue::Text(if r.repos.is_empty() {
                    String::new()
                } else {
                    r.repos.join(", ")
                })
            },
        )
        .filter(FilterKind::Text)
        .initial_width(220.0),
        // Type (enforcement modality): prose / structured / mechanical / architectural.
        // The RowCellRenderer in AllRulesTable adds a `title` tooltip explaining the
        // modality; see `enforcement_tooltip()`.
        ColumnDef::new(ColumnId("enf_type"), "Type", |r: &ProposedRuleView| {
            CellValue::Text(r.enforcement.clone())
        })
        .sortable()
        .render_kind(RenderKind::Badge(enforcement_badges()))
        .initial_width(130.0),
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
    #[props(default)]
    goto_repo: Signal<Option<String>>,
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
                    crate::toast::push_toast(
                        toasts_opt,
                        crate::toast::ToastKind::Info,
                        "Option saved.",
                    );
                    refresh_opt += 1;
                } else {
                    crate::toast::push_toast(
                        toasts_opt,
                        crate::toast::ToastKind::Error,
                        "Could not save the option choice.",
                    );
                }
            });
        }
    });

    let rf = repo_filter();

    // Row-cell renderer for the Type (enforcement modality) column: adds a native
    // browser `title` tooltip with the modality definition. The renderer captures
    // no signals (only a free function), so it satisfies Send + Sync.
    let applied_type_renderers = {
        let mut m: std::collections::HashMap<ColumnId, RowCellRenderer<AppliedRuleRow>> =
            std::collections::HashMap::new();
        m.insert(
            ColumnId("enf_type"),
            std::sync::Arc::new(move |r: &AppliedRuleRow, _val: &CellValue| {
                let enf = r.corpus.as_ref().map(|c| c.enforcement.as_str()).unwrap_or("");
                let tip = enforcement_tooltip(enf);
                let label = if enf.is_empty() { "\u{2014}" } else { enf };
                rsx! { span { title: "{tip}", "{label}" } }
            }) as RowCellRenderer<AppliedRuleRow>,
        );
        RowCellRenderers::new(m)
    };

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
            row_cell_renderers: applied_type_renderers,
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
        let mut m: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for sel in project
            .ruleset
            .selections
            .iter()
            .chain(project.ruleset.cross_repo.iter())
            .chain(project.ruleset.process.iter())
        {
            m.entry(sel.rule_id.clone())
                .or_default()
                .extend(sel.repos.clone());
        }
        m
    };
    // Annotate the corpus with its currently-applied repos for the table column.
    let annotated: Vec<ProposedRuleView> = corpus
        .iter()
        .map(|r| {
            let mut rv = r.clone();
            rv.repos = applied_repos.get(&r.id).cloned().unwrap_or_default();
            // deduplicate
            rv.repos.sort();
            rv.repos.dedup();
            rv
        })
        .collect();

    // Mint row ids ONCE per mount.
    let rows: Vec<(RowId, ProposedRuleView)> = use_hook({
        let annotated = annotated.clone();
        move || {
            annotated
                .iter()
                .map(|r| (RowId::new(), r.clone()))
                .collect()
        }
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
            let scope_bucket = corpus_rule
                .as_ref()
                .map(bucket_of)
                .unwrap_or(SelectionBucket::Selections);
            match scope_bucket {
                SelectionBucket::CrossRepo => {
                    if let Some(sel) = p
                        .ruleset
                        .cross_repo
                        .iter_mut()
                        .find(|s| s.rule_id == rule_id)
                    {
                        if !sel.repos.contains(&repo) {
                            sel.repos.push(repo.clone());
                        }
                    } else {
                        p.ruleset.cross_repo.push(RuleSelectionView {
                            rule_id: rule_id.clone(),
                            chosen_option: default_opt,
                            repos: vec![repo.clone()],
                        });
                    }
                }
                SelectionBucket::Process => {
                    if let Some(sel) = p.ruleset.process.iter_mut().find(|s| s.rule_id == rule_id) {
                        if !sel.repos.contains(&repo) {
                            sel.repos.push(repo.clone());
                        }
                    } else {
                        p.ruleset.process.push(RuleSelectionView {
                            rule_id: rule_id.clone(),
                            chosen_option: default_opt,
                            repos: vec![repo.clone()],
                        });
                    }
                }
                SelectionBucket::Selections => {
                    if let Some(sel) = p
                        .ruleset
                        .selections
                        .iter_mut()
                        .find(|s| s.rule_id == rule_id)
                    {
                        if !sel.repos.contains(&repo) {
                            sel.repos.push(repo.clone());
                        }
                    } else {
                        p.ruleset.selections.push(RuleSelectionView {
                            rule_id: rule_id.clone(),
                            chosen_option: default_opt,
                            repos: vec![repo.clone()],
                        });
                    }
                }
            }
            let body = build_ruleset_json(&p);
            spawn(async move {
                if save_ruleset(&pid, body).await {
                    crate::toast::push_toast(
                        toasts,
                        crate::toast::ToastKind::Info,
                        format!("Added rule to {repo}."),
                    );
                    refresh_add += 1;
                } else {
                    crate::toast::push_toast(
                        toasts,
                        crate::toast::ToastKind::Error,
                        "Could not update the ruleset.",
                    );
                }
            });
        }
    });

    // Row-cell renderer for the Type (enforcement modality) column in the corpus
    // (all-rules) table: adds a native browser `title` tooltip with the definition.
    let corpus_type_renderers = {
        let mut m: std::collections::HashMap<ColumnId, RowCellRenderer<ProposedRuleView>> =
            std::collections::HashMap::new();
        m.insert(
            ColumnId("enf_type"),
            std::sync::Arc::new(move |r: &ProposedRuleView, _val: &CellValue| {
                let enf = r.enforcement.as_str();
                let tip = enforcement_tooltip(enf);
                let label = if enf.is_empty() { "\u{2014}" } else { enf };
                rsx! { span { title: "{tip}", "{label}" } }
            }) as RowCellRenderer<ProposedRuleView>,
        );
        RowCellRenderers::new(m)
    };

    rsx! {
        // The chorale table: "Applied to" column shows the repos (comma-joined text from
        // the accessor). Row click opens the rule detail modal.
        Table {
            handle,
            sort_enabled: true,
            filter_enabled: true,
            sticky_header: true,
            row_cell_renderers: corpus_type_renderers,
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
fn RulesDetailModalHost(on_option_picked: EventHandler<(String, String)>) -> Element {
    let detail_rule = use_context::<Signal<Option<ProposedRuleView>>>();
    let chosen = use_context::<Signal<std::collections::HashMap<String, String>>>();
    let Some(r) = detail_rule() else {
        return rsx! {};
    };
    let mut detail_rule_mut = use_context::<Signal<Option<ProposedRuleView>>>();
    let (vbadge_label, vbadge_cls) = verif_badge(&r.verification);
    let vsources_tip = verif_sources_tooltip(&r.sources);
    rsx! {
        div { class: "rule-modal-overlay", onclick: move |_| detail_rule_mut.set(None),
            div { class: "rule-modal", onclick: move |e| e.stop_propagation(),
                div { class: "rule-modal-head",
                    span { class: "rule-modal-id", "{r.id}" }
                    button { class: "rule-modal-close", onclick: move |_| detail_rule_mut.set(None), "\u{2715}" }
                }
                div { class: "rule-modal-title-row",
                    p { class: "rule-modal-title", "{r.title}" }
                    span {
                        class: "verif-badge verif-badge-{vbadge_cls}",
                        title: "{vsources_tip}",
                        "{vbadge_label}"
                    }
                }
                div { class: "rule-modal-meta",
                    span { class: "rule-modal-tag", "domain \u{00b7} {r.domain}" }
                    span { class: "rule-modal-tag", "scope \u{00b7} {r.scope}" }
                    span { class: "rule-modal-tag", "kind \u{00b7} {r.kind}" }
                    if !r.enforcement.is_empty() {
                        span {
                            class: "rule-modal-tag",
                            title: "{enforcement_tooltip(&r.enforcement)}",
                            "enforcement \u{00b7} {r.enforcement}"
                        }
                    }
                }
                // Sources: the cited corpus source(s) backing this rule's grounding, shown
                // as a real panel (not only a badge hover) and extensible to multiple sources.
                if !r.sources.is_empty() {
                    div { class: "rule-modal-section",
                        span { class: "rule-modal-label", "Sources" }
                        for s in r.sources.iter() {
                            div { style: "margin:4px 0 8px;",
                                a {
                                    href: "{s.url}",
                                    style: "color:#2563eb; text-decoration:underline; word-break:break-all;",
                                    "{s.title}"
                                }
                                if let Some(linter) = s.linter.as_ref().filter(|l| !l.is_empty()) {
                                    span { style: "color:#666; margin-left:6px;", "[{linter}]" }
                                }
                                div { style: "color:#888; font-size:0.85em; word-break:break-all;", "{s.url}" }
                            }
                        }
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
                        // "Why the default" only makes sense when the rule HAS a default; a rule
                        // with no default has a rationale for the decision itself, not for a
                        // default that doesn't exist. Label it plainly "Why" in that case.
                        span { class: "rule-modal-label",
                            if r.default_option.is_some() { "Why the default" } else { "Why" }
                        }
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

/// Model-tier editor (#63): a compact dev-console for the project's fast / balanced /
/// strongest model bindings. Reads from `project.tier_map` and POSTs to
/// `PATCH /api/projects/:id/tier-map` (patch semantics: all three bands sent each save).
///
/// Placed in the Rules window as a distinct settings section — it is NOT part of the
/// ruleset (no rule ids, no options, no emit target). It controls which model the fleet
/// uses per-task-tier.
#[component]
fn TierMapEditor(project: ProjectView) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let pid = project.id.clone();
    // Local editable copy of the three model strings. Seeded from the project's tier map.
    let mut fast = use_signal(|| project.tier_map.fast.clone());
    let mut balanced = use_signal(|| project.tier_map.balanced.clone());
    let mut strongest = use_signal(|| project.tier_map.strongest.clone());
    let mut saving = use_signal(|| false);

    rsx! {
        div { class: "tier-map-editor",
            p { class: "tier-map-heading", "Model tier map" }
            p { class: "section-hint tier-map-hint",
                "Maps each capability band to a concrete model id. The fleet resolves every task's \
                 band (Fast / Balanced / Strongest) to the model id here at runtime. Changing this \
                 affects all governed runs for this project from the next run onward."
            }
            div { class: "tier-map-rows",
                // Fast band
                div { class: "tier-map-row",
                    label { class: "tier-map-band-label tier-map-fast", "Fast" }
                    span { class: "tier-map-band-desc", "(throughput — tests, simple edits)" }
                    input {
                        class: "tier-map-input addressee-input",
                        r#type: "text",
                        placeholder: "e.g. claude-haiku-4-5-20251001",
                        value: "{fast}",
                        oninput: move |e| fast.set(e.value()),
                    }
                }
                // Balanced band
                div { class: "tier-map-row",
                    label { class: "tier-map-band-label tier-map-balanced", "Balanced" }
                    span { class: "tier-map-band-desc", "(mid-tier — most tasks)" }
                    input {
                        class: "tier-map-input addressee-input",
                        r#type: "text",
                        placeholder: "e.g. claude-sonnet-4-6",
                        value: "{balanced}",
                        oninput: move |e| balanced.set(e.value()),
                    }
                }
                // Strongest band
                div { class: "tier-map-row",
                    label { class: "tier-map-band-label tier-map-strongest", "Strongest" }
                    span { class: "tier-map-band-desc", "(frontier-class — architecture, security)" }
                    input {
                        class: "tier-map-input addressee-input",
                        r#type: "text",
                        placeholder: "e.g. claude-opus-4-8",
                        value: "{strongest}",
                        oninput: move |e| strongest.set(e.value()),
                    }
                }
            }
            button {
                class: "btn-run",
                disabled: saving(),
                onclick: move |_| {
                    let pid = pid.clone();
                    let map = TierMapView {
                        fast: fast().trim().to_string(),
                        balanced: balanced().trim().to_string(),
                        strongest: strongest().trim().to_string(),
                    };
                    if map.fast.is_empty() || map.balanced.is_empty() || map.strongest.is_empty() {
                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Warning, "All three tier model ids are required.");
                        return;
                    }
                    saving.set(true);
                    spawn(async move {
                        if set_project_tier_map(&pid, &map).await {
                            crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, "Tier map saved.");
                        } else {
                            crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, "Could not save tier map.");
                        }
                        saving.set(false);
                    });
                },
                if saving() { "Saving\u{2026}" } else { "Save tier map" }
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
    let chosen: Signal<std::collections::HashMap<String, String>> =
        use_signal(std::collections::HashMap::new);
    use_context_provider(|| chosen);

    // Signal from Table 2 to Table 1: "go to this repo".
    let goto_repo: Signal<Option<String>> = use_signal(|| None);

    // pw/cockpit-ui: single-rule editor — the rule currently open for editing (None = closed).
    let mut single_edit_rule: Signal<Option<ProposedRuleView>> = use_signal(|| None);

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
                    let pid_drift = p.id.clone();
                    let p_modal = p_owned.clone();
                    let p_t1 = p_owned.clone();
                    let p_t2 = p_owned.clone();
                    let p_edit = p_owned.clone();
                    let corpus_t1 = corpus.clone();
                    let corpus_t2 = corpus.clone();
                    rsx! {
                        // pw/cockpit-ui Feature 3: single-rule editor overlay. Rendered at
                        // the subtree root (same ghost-click-eater rationale as RulesDetailModalHost).
                        if let Some(rule_for_edit) = single_edit_rule() {
                            SingleRuleEditor {
                                project: p_edit.clone(),
                                rule: rule_for_edit,
                                on_close: move |_| single_edit_rule.set(None),
                                on_saved: move |_| {
                                    single_edit_rule.set(None);
                                    refresh += 1;
                                },
                            }
                        }

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

                        // pw/cockpit-ui Feature 2: rule-drift notice. Shows rules whose corpus
                        // entry changed since they were adopted, with inline diffs and "Update".
                        RuleDriftNotice { project_id: pid_drift }

                        // pw/cockpit-ui Feature 3: "Edit rule" entry point — opens the
                        // SingleRuleEditor for the currently-selected corpus rule. Placed above
                        // Table 1 so it's visible without scrolling past the table.
                        if !corpus.is_empty() {
                            {
                                let corpus_for_edit = corpus.clone();
                                rsx! {
                                    div { class: "single-rule-edit-entry",
                                        p { class: "section-hint",
                                            "To edit a single rule's option (project-level or repo override), "
                                            "select a rule in the corpus table below and click the button."
                                        }
                                        select {
                                            class: "single-rule-edit-select",
                                            onchange: move |e: Event<FormData>| {
                                                let id = e.value();
                                                if id.is_empty() { return; }
                                                if let Some(r) = corpus_for_edit.iter().find(|r| r.id == id) {
                                                    single_edit_rule.set(Some(r.clone()));
                                                }
                                            },
                                            option { value: "", "Choose a rule to edit…" }
                                            for r in corpus.iter() {
                                                option { key: "{r.id}", value: "{r.id}", "{r.id} — {r.title}" }
                                            }
                                        }
                                    }
                                }
                            }
                        }

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

                        CiRulesPanel {
                            repos: p_owned.repos.clone(),
                            rules: ci_rule_items_from_selections(&p_owned.ruleset.selections, &corpus),
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

                        // ── SETTINGS: Model tier map (#63) ────────────────────────────
                        // NOT a ruleset concern — controls which model the fleet uses
                        // per-task-tier at runtime. Labeled SETTINGS to distinguish from
                        // the rule tables above.
                        p { class: "section-label settings-label", "SETTINGS: Model tier map" }
                        TierMapEditor { project: p_owned.clone() }

                        // ── SETTINGS: Commit / PR gate settings (#65) ──────────────
                        // The `VcsGateSettings` component owns the `process-rule-config`
                        // surface: bypass mode + per-rule on/off toggles. It talks
                        // directly to `/api/projects/:id/process-rule-config`.
                        // Labeled SETTINGS (not rules) — it is a process configuration
                        // surface, not part of the emitted ruleset.
                        p { class: "section-label settings-label", "SETTINGS: Commit / PR gate" }
                        crate::vcs_settings::VcsGateSettings { project_id: p_owned.id.clone() }

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

/// The outcome of attempting to start a governed run. The no-code-first gate (Pillar 2)
/// can BLOCK the start with a precise reason (server 409), which the cockpit surfaces as
/// a toast instead of silently doing nothing.
enum StartRunOutcome {
    /// The run started; carries its id for polling.
    Started(String),
    /// The development gate blocked the start; carries the server's reason.
    Blocked(String),
    /// Transport / decode failure.
    Failed,
}

/// Start a governed DEVELOPMENT run for a story (the build phase, run from the
/// `DecisionsApproved` step).
///
/// Sends the per-UoW tier map so the fleet's orchestrator (the strongest tier)
/// leads and delegates simpler work to the balanced / fast tiers:
/// `POST /api/stories/:id/run` with body
/// `{ "tier_map": { "strongest": "<id>", "balanced": "<id>", "fast": "<id>" } }`.
///
/// Returns [`StartRunOutcome::Started`] with the run id on success, or
/// [`StartRunOutcome::Blocked`] (with the gate's reason) when the no-code-first gate
/// refuses the start because the story's decisions are not all approved. A transport
/// or decode failure maps to [`StartRunOutcome::Failed`].
/// Build the request body for a development run:
/// `{ "tier_map": { "strongest": "<id>", "balanced": "<id>", "fast": "<id>" } }`,
/// plus `"skip_layer2": true` ONLY when the one-time bootstrap toggle is on (omitted
/// otherwise, so the default-off behaviour is exactly today's). `skip_layer2` is the
/// bootstrap escape hatch: it skips ONLY the post-task layer-2 lint/test bounce so a
/// brownfield repo can install the tooling layer-2 needs. Layer 1 (the security gate)
/// and the no-code-first decisions gate still apply. Extracted as a pure fn so the wire
/// shape is unit-testable.
fn dev_run_body(tier_map: &TierMapView, skip_layer2: bool) -> serde_json::Value {
    let mut body = serde_json::json!({
        "tier_map": {
            "strongest": tier_map.strongest,
            "balanced": tier_map.balanced,
            "fast": tier_map.fast,
        }
    });
    // Only include the flag when set, so a normal run's body is byte-for-byte today's.
    if skip_layer2 {
        body["skip_layer2"] = serde_json::Value::Bool(true);
    }
    body
}

/// Percent-encode a value for use as a single URL PATH SEGMENT.
///
/// UoW / story ids are `owner/repo#num`. Used raw in a path, the `/` breaks
/// single-segment routing and the `#` is even dropped by the client as a URL
/// fragment (so the server never sees it). Encode everything outside the RFC 3986
/// unreserved set; axum's `Path` extractor percent-decodes it back on the server.
fn enc_seg(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

async fn start_dev_run(
    story_id: &str,
    tier_map: &TierMapView,
    skip_layer2: bool,
) -> StartRunOutcome {
    let body = dev_run_body(tier_map, skip_layer2);
    let resp = match reqwest::Client::new()
        .post(format!("{}/api/stories/{}/run", crate::BFF_URL, enc_seg(story_id)))
        .json(&body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return StartRunOutcome::Failed,
    };
    if resp.status().as_u16() == 409 {
        let reason = resp
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v.get("reason").and_then(|r| r.as_str().map(String::from)))
            .unwrap_or_else(|| "The development gate blocked this run.".to_string());
        return StartRunOutcome::Blocked(reason);
    }
    let Ok(v) = resp.json::<serde_json::Value>().await else {
        return StartRunOutcome::Failed;
    };
    match v.get("run_id").and_then(|r| r.as_str()) {
        Some(id) => StartRunOutcome::Started(id.to_string()),
        None => StartRunOutcome::Failed,
    }
}

/// Start an INVESTIGATION run for a story (the intake → investigating transition,
/// run from the `Intake` step).
///
/// `POST /api/uow/:story_id/begin-investigation` with body `{ "model": "<id>" }`;
/// the server transitions the stage and returns `{ "run_id", "story_id" }`. Returns
/// the run id on success (to drive the live agent activity) or `None` on any
/// transport / decode failure or a missing `run_id`.
async fn begin_investigation_run(story_id: &str, model: &str) -> Option<String> {
    reqwest::Client::new()
        .post(format!(
            "{}/api/uow/{}/begin-investigation",
            crate::BFF_URL,
            enc_seg(story_id)
        ))
        .json(&serde_json::json!({ "model": model }))
        .send()
        .await
        .ok()?
        .json::<serde_json::Value>()
        .await
        .ok()?
        .get("run_id")
        .and_then(|r| r.as_str())
        .map(String::from)
}

/// The branches a UoW can merge FROM (`POST /api/uow/:story_id/branches`), split by
/// where they live. Populates the "Update branch" picker.
#[derive(Clone, Default, PartialEq, serde::Deserialize)]
struct MergeSourceBranchesView {
    #[serde(default)]
    local: Vec<String>,
    #[serde(default)]
    origin: Vec<String>,
}

/// Fetch the mergeable branches for a UoW. Empty lists on any failure / no clone.
async fn fetch_uow_branches(story_id: &str) -> MergeSourceBranchesView {
    let resp = reqwest::Client::new()
        .post(format!(
            "{}/api/uow/{}/branches",
            crate::BFF_URL,
            enc_seg(story_id)
        ))
        .send()
        .await;
    match resp {
        Ok(r) => r
            .json::<MergeSourceBranchesView>()
            .await
            .unwrap_or_default(),
        Err(_) => MergeSourceBranchesView::default(),
    }
}

/// Start an AI-assisted update-branch run for a UoW: merge `source_branch` (from
/// `source` = "local"/"origin") INTO the UoW's branch. Returns the run id to poll, or
/// a `Blocked` reason (server 4xx, e.g. no branch yet) surfaced as a toast.
async fn start_update_branch_run(
    story_id: &str,
    source_branch: &str,
    source: &str,
    model: &str,
) -> StartRunOutcome {
    let resp = match reqwest::Client::new()
        .post(format!(
            "{}/api/uow/{}/update-branch",
            crate::BFF_URL,
            enc_seg(story_id)
        ))
        .json(&serde_json::json!({
            "source_branch": source_branch,
            "source": source,
            "model": model,
        }))
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return StartRunOutcome::Failed,
    };
    if resp.status().as_u16() == 400 {
        let reason = resp
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v.get("error").and_then(|r| r.as_str().map(String::from)))
            .unwrap_or_else(|| "The update-branch request was rejected.".to_string());
        return StartRunOutcome::Blocked(reason);
    }
    let Ok(v) = resp.json::<serde_json::Value>().await else {
        return StartRunOutcome::Failed;
    };
    match v.get("run_id").and_then(|r| r.as_str()) {
        Some(id) => StartRunOutcome::Started(id.to_string()),
        None => StartRunOutcome::Failed,
    }
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

/// A run's provenance summary as the BFF reports it (`GET /api/runs/:id/provenance`):
/// the rules in force, the gate deny/allow tallies, and total bounces (issue #21).
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
struct RunProvenanceView {
    #[serde(default)]
    run_id: String,
    #[serde(default)]
    story_id: String,
    #[serde(default)]
    mode: String,
    #[serde(default)]
    rules_in_force: Vec<String>,
    #[serde(default)]
    deny_count: usize,
    #[serde(default)]
    allow_count: usize,
    #[serde(default)]
    total_bounces: usize,
    #[serde(default)]
    rules_fired: Vec<String>,
}

/// Fetch the provenance summary for a run.
async fn fetch_provenance(run_id: &str) -> Option<RunProvenanceView> {
    reqwest::get(format!("{}/api/runs/{}/provenance", crate::BFF_URL, run_id))
        .await
        .ok()?
        .json::<RunProvenanceView>()
        .await
        .ok()
}

/// Sign off a run (issue #21). The architect's explicit gate after reviewing the
/// provenance; persists on the story's UoW. Returns the updated UoW on success.
async fn sign_off_run(run_id: &str, by: &str, note: Option<&str>) -> Option<UowView> {
    reqwest::Client::new()
        .post(format!("{}/api/runs/{}/sign-off", crate::BFF_URL, run_id))
        .json(&serde_json::json!({ "by": by, "note": note }))
        .send()
        .await
        .ok()?
        .json::<UowView>()
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

    /// Wire string for `POST /api/uow/:id/status`.
    fn wire_str(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::InProgress => "in_progress",
            Self::Done => "done",
        }
    }
}

/// The governed-development lifecycle stage of a Unit of Work (Pillar 2). Mirrors
/// `camerata_server::lifecycle::UowStage`; orthogonal to (and richer than) `DevStatus`.
#[derive(Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize, Default, Debug)]
#[serde(rename_all = "snake_case")]
enum UowStage {
    #[default]
    Intake,
    Investigating,
    DecisionsApproved,
    Development,
    AwaitingQa,
    SignedOff,
}

impl UowStage {
    /// A short display label for the lifecycle strip.
    fn label(self) -> &'static str {
        match self {
            Self::Intake => "Intake",
            Self::Investigating => "Investigating",
            Self::DecisionsApproved => "Decisions approved",
            Self::Development => "Development",
            Self::AwaitingQa => "Awaiting QA",
            Self::SignedOff => "Signed off",
        }
    }

    /// Monotonic ordinal (0 = Intake .. 5 = SignedOff), for "has reached" comparisons.
    fn ordinal(self) -> usize {
        match self {
            Self::Intake => 0,
            Self::Investigating => 1,
            Self::DecisionsApproved => 2,
            Self::Development => 3,
            Self::AwaitingQa => 4,
            Self::SignedOff => 5,
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

/// The frozen gate provenance stamped onto a UoW after a governed run finishes.
/// Mirrors `camerata_server::uow::GateProvenance`.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
struct GateProvenanceView {
    run_id: String,
    mode: String,
    allow_count: usize,
    deny_count: usize,
    total_bounces: usize,
    #[serde(default)]
    rules_fired: Vec<String>,
    #[serde(default)]
    recorded: String,
}

/// An architect's sign-off on a story's governed run (issue #21).
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
struct SignOffView {
    ts: String,
    by: String,
    run_id: String,
    #[serde(default)]
    note: Option<String>,
}

/// The Unit of Work as returned by `GET /api/uow/:story_id`.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
struct UowView {
    story_id: String,
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    dev_status: DevStatus,
    /// The governed-development lifecycle stage (Pillar 2).
    #[serde(default)]
    stage: UowStage,
    #[serde(default)]
    history: Vec<HistoryEntryView>,
    /// The frozen gate provenance from the most recent completed run, if any.
    #[serde(default)]
    gate_provenance: Option<GateProvenanceView>,
    #[serde(default)]
    sign_off: Option<SignOffView>,
    #[serde(default)]
    updated: String,
}

/// Fetch the UoW for a single story (get-or-create semantics).
async fn fetch_uow(story_id: &str) -> Option<UowView> {
    reqwest::get(format!("{}/api/uow/{}", crate::BFF_URL, enc_seg(story_id)))
        .await
        .ok()?
        .json::<UowView>()
        .await
        .ok()
}

/// POST a new dev-status for a story's UoW. Returns `Some(())` on a 2xx. The server
/// responds with the full `UnitOfWork` (not the UI's `UowView`), so we DO NOT try to
/// deserialize the body — the caller just bumps the refresh tick and re-fetches the UoW.
/// (Deserializing into `UowView` here was the bug behind a false "Could not update dev
/// status" toast even when the server succeeded.)
async fn post_uow_status(story_id: &str, status: DevStatus) -> Option<()> {
    let resp = reqwest::Client::new()
        .post(format!("{}/api/uow/{}/status", crate::BFF_URL, enc_seg(story_id)))
        .json(&serde_json::json!({ "status": status.wire_str() }))
        .send()
        .await
        .ok()?;
    resp.status().is_success().then_some(())
}

/// The outcome of a lifecycle transition POST. `Ok` carries the updated UoW; `Blocked`
/// carries the server's human-readable reason (a 409); `Failed` is a transport error.
enum TransitionOutcome {
    /// The transition succeeded; the panel re-fetches the updated UoW via the refresh
    /// tick, so the updated body is not carried here.
    Ok,
    Blocked(String),
    Failed,
}

/// POST a lifecycle transition (`begin-investigation` / `approve-decisions`) and map the
/// response: 2xx → the updated UoW, 409 → the block reason, anything else → Failed.
async fn post_uow_transition(story_id: &str, action: &str) -> TransitionOutcome {
    let url = format!("{}/api/uow/{}/{}", crate::BFF_URL, enc_seg(story_id), action);
    let resp = match reqwest::Client::new().post(url).send().await {
        Ok(r) => r,
        Err(_) => return TransitionOutcome::Failed,
    };
    if resp.status().is_success() {
        TransitionOutcome::Ok
    } else if resp.status().as_u16() == 409 {
        // The server returns { "reason": "<why>" } for a blocked transition.
        let reason = resp
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v.get("reason").and_then(|r| r.as_str().map(String::from)))
            .unwrap_or_else(|| "Transition blocked.".to_string());
        TransitionOutcome::Blocked(reason)
    } else {
        TransitionOutcome::Failed
    }
}

// ── Provider-agnostic Work-Item + Unit-of-Work surface (Governed Development) ────
//
// PROVIDER-ADAPTER SEAM. The types and rendering in this block are deliberately
// provider-AGNOSTIC: they operate only on the normalized `WorkItem` DTO (a stable id,
// a repo, a number, title/body/state/url/labels). The *connection + pull* is the only
// GitHub-specific piece, isolated in `GithubConnection` / `IssueManagementPanel`. To add
// a Jira / Azure-DevOps adapter later, drop in a sibling connection component that pulls
// the same `WorkItem` shape; the table, the UoW card list, the detail view, and every dev
// control downstream keep working unchanged because they never name a provider.

/// A normalized work item from any tracker provider (`POST /api/workitems/pull`,
/// `POST /api/workitems/refresh`). The server maps a provider's native issue (today:
/// the worktracker GitHub adapter's `CanonicalStory`) into this shape so the UI never
/// touches a provider-specific payload.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Debug)]
struct WorkItem {
    /// Stable cross-provider id, e.g. `"github:OWNER/REPO#123"`. The dedup key for UoWs.
    id: String,
    /// The provider that owns this item (today always `"github"`).
    #[serde(default)]
    provider: String,
    /// `OWNER/REPO` the item belongs to. Each pulled item carries its own repo.
    #[serde(default)]
    repo: String,
    #[serde(default)]
    number: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    body: String,
    /// `"open"` | `"closed"`.
    #[serde(default)]
    state: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    labels: Vec<String>,
}

/// App-lifetime cache of the last work-item pull, keyed by project id (so switching
/// projects never shows stale items). A `GlobalSignal` persists for the lifetime of the
/// process, so navigating away from Governed Development and back does NOT require a
/// re-pull — the pull is held in memory until Camerata closes or the user pulls again.
/// Manual pull only; there is no auto-poll.
static PULLED_WORK_ITEMS: GlobalSignal<Option<(String, Vec<WorkItem>)>> =
    Signal::global(|| None);

/// The `POST /api/workitems/pull` envelope.
#[derive(Clone, PartialEq, serde::Deserialize, Default)]
struct PullWorkItemsResult {
    #[serde(default)]
    items: Vec<WorkItem>,
}

/// One Unit of Work as `GET /api/uows` reports it: the UoW id, the WorkItem it
/// references, and its lifecycle stage. The `id` doubles as the key the existing
/// governed-dev endpoints are keyed by (the server reconciles UoW id ↔ story id), so
/// the reused dev controls below address this UoW through it.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Debug)]
struct UowListEntry {
    id: String,
    work_item: WorkItem,
    #[serde(default)]
    stage: UowStage,
}

/// The `GET /api/uows` envelope.
#[derive(Clone, PartialEq, serde::Deserialize, Default)]
struct UowsResult {
    #[serde(default)]
    uows: Vec<UowListEntry>,
}

/// The `POST /api/uow/from-workitem` result. `created=false` means a UoW already
/// existed for that work item (dedup by external ref) and was returned as-is.
#[derive(Clone, PartialEq, serde::Deserialize, Default)]
struct FromWorkItemResult {
    #[serde(default)]
    uow_id: String,
    #[serde(default)]
    created: bool,
}

/// One comment on a work item (`POST /api/workitems/comments`), flattened for the
/// modal. Mirrors the server's `IssueComment` shape.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Debug, Default)]
struct WorkItemComment {
    #[serde(default)]
    author: String,
    #[serde(default)]
    body: String,
    /// ISO-8601 created-at as the tracker returns it; the UI shows it as-is.
    #[serde(default)]
    created_at: String,
}

/// The `POST /api/workitems/comments` envelope.
#[derive(Clone, PartialEq, serde::Deserialize, Default)]
struct WorkItemCommentsResult {
    #[serde(default)]
    comments: Vec<WorkItemComment>,
}

/// The `POST /api/workitems/assignees` envelope: the repo's assignable user logins
/// (the practical @-mention set).
#[derive(Clone, PartialEq, serde::Deserialize, Default)]
struct WorkItemAssigneesResult {
    #[serde(default)]
    users: Vec<String>,
}

// ── Pure helpers (unit-tested) ──────────────────────────────────────────────────

/// The label a work item's State badge shows. Pure mapping over the wire string so
/// any casing / unknown value still renders sensibly. Returns (display, css-modifier).
fn work_item_state_badge(state: &str) -> (&'static str, &'static str) {
    match state.to_ascii_lowercase().as_str() {
        "open" => ("OPEN", "active"),
        "closed" => ("CLOSED", "done"),
        _ => ("UNKNOWN", "neutral"),
    }
}

/// The compact label one work item's row shows in the table's Labels column. Joins
/// with commas; empty -> an em-dash placeholder. Pure so it is unit-testable.
fn labels_summary(labels: &[String]) -> String {
    if labels.is_empty() {
        "—".to_string()
    } else {
        labels.join(", ")
    }
}

/// Whether a work item already has a UoW (dedup display logic). When true, the detail
/// view shows "Open Unit of Work" (and the existing UoW id) instead of a Create button.
/// Matching is by the work item's stable id against each UoW's referenced work item id.
fn existing_uow_for<'a>(uows: &'a [UowListEntry], work_item_id: &str) -> Option<&'a UowListEntry> {
    uows.iter().find(|u| u.work_item.id == work_item_id)
}

/// The button label for the create/open affordance, given whether a UoW already exists.
/// Pure: drives both the table-row action and the detail view consistently.
fn create_or_open_label(has_uow: bool) -> &'static str {
    if has_uow {
        "Open Unit of Work"
    } else {
        "Create Unit of Work from this issue"
    }
}

// ── Client functions for the work-item / UoW endpoints ──────────────────────────

/// Pull ALL open issues across ALL the active project's repos (`POST /api/workitems/pull`).
/// Manual / user-triggered; no cache. Body is empty (the server uses the active project).
async fn pull_work_items() -> Option<Vec<WorkItem>> {
    reqwest::Client::new()
        .post(format!("{}/api/workitems/pull", crate::BFF_URL))
        .json(&serde_json::json!({}))
        .send()
        .await
        .ok()?
        .json::<PullWorkItemsResult>()
        .await
        .ok()
        .map(|r| r.items)
}

/// List all Units of Work with their referenced WorkItem + lifecycle stage
/// (`GET /api/uows`).
async fn fetch_uows() -> Option<Vec<UowListEntry>> {
    reqwest::get(format!("{}/api/uows", crate::BFF_URL))
        .await
        .ok()?
        .json::<UowsResult>()
        .await
        .ok()
        .map(|r| r.uows)
}

/// Create a UoW referencing a work item (`POST /api/uow/from-workitem`). Dedups by
/// external ref server-side: an existing UoW comes back with `created=false`.
async fn create_uow_from_work_item(work_item_id: &str) -> Option<FromWorkItemResult> {
    reqwest::Client::new()
        .post(format!("{}/api/uow/from-workitem", crate::BFF_URL))
        .json(&serde_json::json!({ "work_item_id": work_item_id }))
        .send()
        .await
        .ok()?
        .json::<FromWorkItemResult>()
        .await
        .ok()
}

/// Re-pull a single work item (`POST /api/workitems/refresh`).
async fn refresh_work_item(work_item_id: &str) -> Option<WorkItem> {
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/api/workitems/refresh", crate::BFF_URL))
        .json(&serde_json::json!({ "work_item_id": work_item_id }))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    serde_json::from_value(v.get("item")?.clone()).ok()
}

/// Comment back onto the source issue (`POST /api/workitems/comment`). Returns the
/// comment url on success.
async fn comment_on_work_item(work_item_id: &str, body: &str) -> Option<String> {
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/api/workitems/comment", crate::BFF_URL))
        .json(&serde_json::json!({ "work_item_id": work_item_id, "body": body }))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    // `ok` must be truthy; surface the url it returns.
    if v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) {
        Some(
            v.get("url")
                .and_then(|u| u.as_str())
                .unwrap_or_default()
                .to_string(),
        )
    } else {
        None
    }
}

/// Read the COMMENTS on a work item (`POST /api/workitems/comments`). Degrades to an
/// empty list (server returns `{ comments: [] }` token-less / on error).
async fn fetch_work_item_comments(work_item_id: &str) -> Vec<WorkItemComment> {
    let res = reqwest::Client::new()
        .post(format!("{}/api/workitems/comments", crate::BFF_URL))
        .json(&serde_json::json!({ "work_item_id": work_item_id }))
        .send()
        .await
        .ok();
    match res {
        Some(r) => r
            .json::<WorkItemCommentsResult>()
            .await
            .ok()
            .map(|r| r.comments)
            .unwrap_or_default(),
        None => Vec::new(),
    }
}

/// Read the assignable users for a work item's repo (`POST /api/workitems/assignees`).
/// Degrades to an empty list (server returns `{ users: [] }` token-less / on error).
async fn fetch_work_item_assignees(work_item_id: &str) -> Vec<String> {
    let res = reqwest::Client::new()
        .post(format!("{}/api/workitems/assignees", crate::BFF_URL))
        .json(&serde_json::json!({ "work_item_id": work_item_id }))
        .send()
        .await
        .ok();
    match res {
        Some(r) => r
            .json::<WorkItemAssigneesResult>()
            .await
            .ok()
            .map(|r| r.users)
            .unwrap_or_default(),
        None => Vec::new(),
    }
}

/// Detect an ACTIVE `@<partial>` mention token at the END of the comment text, for the
/// autocomplete dropdown. Pragmatic approach: we look only at the LAST whitespace-
/// separated token of the whole value. If it starts with `@` and contains no further
/// `@`, the part after the `@` is the active partial (possibly empty, right after
/// typing `@`). Returns `None` when there is no active token (the dropdown stays hidden).
///
/// KNOWN LIMITATION: this tracks the tail of the value, not the caret. Editing a mention
/// in the MIDDLE of already-typed text does not re-open the dropdown. Full mid-text caret
/// tracking is a follow-up; the tail case covers the overwhelming common path (type then
/// mention).
fn active_mention_partial(value: &str) -> Option<&str> {
    // A trailing whitespace means the user just finished a token (e.g. a completed
    // mention + space): there is no ACTIVE token, so the dropdown closes.
    if value.is_empty() || value.ends_with(char::is_whitespace) {
        return None;
    }
    // The last whitespace-delimited token is the active one.
    let last = value.rsplit(char::is_whitespace).next()?;
    let partial = last.strip_prefix('@')?;
    // A second `@` inside the token (e.g. an email-ish `a@b`) is not a mention token.
    if partial.contains('@') {
        return None;
    }
    Some(partial)
}

/// Replace the trailing active `@<partial>` token in `value` with `@<login> ` (trailing
/// space so the user keeps typing after the completed mention). Pure; used when the user
/// clicks a dropdown suggestion. If there is no active token, appends `@<login> `.
fn apply_mention_selection(value: &str, login: &str) -> String {
    match active_mention_partial(value) {
        Some(partial) => {
            // Trim exactly the trailing `@partial` (its byte length) off the value.
            let token_len = 1 + partial.len(); // the `@` plus the partial
            let keep = &value[..value.len() - token_len];
            format!("{keep}@{login} ")
        }
        None => {
            let mut out = value.to_string();
            if !out.is_empty() && !out.ends_with(' ') && !out.ends_with('\n') {
                out.push(' ');
            }
            out.push('@');
            out.push_str(login);
            out.push(' ');
            out
        }
    }
}

/// Filter the assignable logins by the active partial (case-insensitive prefix-ish
/// `contains` match), capped to a short dropdown. An empty partial returns the first
/// few (so typing a bare `@` shows the set). Pure; unit-tested.
fn filter_mention_candidates(users: &[String], partial: &str) -> Vec<String> {
    let needle = partial.to_lowercase();
    users
        .iter()
        .filter(|u| needle.is_empty() || u.to_lowercase().contains(&needle))
        .take(8)
        .cloned()
        .collect()
}

/// Which sub-view of the Governed Development page is selected in the left nav:
/// the top-level Issue Management panel, or a specific UoW's dev controls.
#[derive(Clone, PartialEq, Eq)]
enum GovDevSel {
    /// The Issue Management panel (connection summary + pull + work-item table).
    IssueManagement,
    /// A selected UoW (by its id), showing that UoW's dev controls.
    Uow(String),
}

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
        ImportResult::Conflict {
            name,
            payload: payload.to_string(),
        }
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

/// The `/api/gate-probe` result (#14): the end-to-end gate-loop go/no-go.
#[derive(Clone, PartialEq, serde::Deserialize)]
struct GateProbeView {
    #[serde(default)]
    go: bool,
    #[serde(default)]
    layer1_denied: usize,
    #[serde(default)]
    layer1_total: usize,
    #[serde(default)]
    layer1_clean_allowed: bool,
    #[serde(default)]
    layer2_bounced: bool,
    #[serde(default)]
    layer2_clean: bool,
}

async fn fetch_gate_probe() -> Option<GateProbeView> {
    reqwest::Client::new()
        .post(format!("{}/api/gate-probe", crate::BFF_URL))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()
}

/// In-app gate self-check (#14): runs the deterministic end-to-end gate-loop probe and shows a
/// GO/NO-GO — deny-before-execute denied a forbidden write, and the bounce-and-revise loop
/// resolved a planted violation. The thesis, verifiable in one click (no model, no network out).
#[component]
fn GateSelfCheck() -> Element {
    let mut running = use_signal(|| false);
    let mut result = use_signal(|| Option::<GateProbeView>::None);
    rsx! {
        div { class: "gate-selfcheck",
            div { class: "gate-selfcheck-head",
                span { class: "gate-selfcheck-title", "Gate self-check" }
                span { class: "gate-selfcheck-sub", "Deny-before-execute + bounce-and-revise, end to end — deterministic, no model." }
                button {
                    class: "btn-edit-sm",
                    disabled: running(),
                    onclick: move |_| {
                        running.set(true);
                        spawn(async move {
                            result.set(fetch_gate_probe().await);
                            running.set(false);
                        });
                    },
                    if running() { "Running…" } else { "Run gate self-check" }
                }
            }
            if let Some(r) = result() {
                {
                    let l1 = format!(
                        "Layer 1: deny-before-execute — {}/{} floor rules enforced · clean write {}",
                        r.layer1_denied,
                        r.layer1_total,
                        if r.layer1_clean_allowed { "allowed" } else { "DENIED (deny-all!)" }
                    );
                    let l2 = format!("Layer 2: bounced={}, revise resolved={}", r.layer2_bounced, r.layer2_clean);
                    let (badge, cls) = if r.go { ("GO", "gate-selfcheck-verdict go") } else { ("NO-GO", "gate-selfcheck-verdict nogo") };
                    rsx! {
                        div { class: "{cls}",
                            span { class: "gate-selfcheck-badge", "{badge}" }
                            div { class: "gate-selfcheck-lines",
                                span { "{l1}" }
                                span { "{l2}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The loop-guard control (#29): adjust the active project's max developer→checker
/// bounce-and-revise iterations. Reads the current value from the active project and
/// writes it back via `POST /api/projects/:id/max-iterations`. On reaching the cap a
/// dirty stage stops and surfaces its outstanding violations for human review; the
/// shipped default is 1 (a single bounce).
#[component]
fn LoopGuardControl() -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    // Fetch the active project so we know the project id + current ceiling.
    let active = use_resource(fetch_active_project);
    // Local edit state, seeded once from the fetched value.
    let mut value = use_signal(|| 1usize);
    let mut seeded = use_signal(|| false);
    let mut saving = use_signal(|| false);

    let proj = active.read().clone().flatten();
    if let Some(p) = &proj {
        if !seeded() {
            value.set(p.max_iterations.max(1));
            seeded.set(true);
        }
    }

    let Some(project) = proj else {
        return rsx! {
            div { class: "loop-guard",
                span { class: "loop-guard-title", "Loop guard" }
                span { class: "loop-guard-sub", "Create or select a project to configure the bounce-and-revise ceiling." }
            }
        };
    };
    let pid = project.id.clone();

    rsx! {
        div { class: "loop-guard",
            div { class: "loop-guard-head",
                span { class: "loop-guard-title", "Loop guard — max revise iterations" }
                span { class: "loop-guard-sub",
                    "How many developer→checker bounce-and-revise passes a dirty stage may take before the loop stops and the outstanding violations are raised for review. Default: 1."
                }
            }
            div { class: "loop-guard-row",
                button {
                    class: "btn-edit-sm",
                    disabled: value() <= 1 || saving(),
                    onclick: move |_| {
                        let v = value().saturating_sub(1).max(1);
                        value.set(v);
                    },
                    "−"
                }
                input {
                    class: "loop-guard-input",
                    r#type: "number",
                    min: "1",
                    max: "20",
                    value: "{value}",
                    oninput: move |e| {
                        if let Ok(n) = e.value().parse::<usize>() {
                            value.set(n.clamp(1, 20));
                        }
                    },
                }
                button {
                    class: "btn-edit-sm",
                    disabled: value() >= 20 || saving(),
                    onclick: move |_| {
                        let v = (value() + 1).min(20);
                        value.set(v);
                    },
                    "+"
                }
                button {
                    class: "btn-edit-sm loop-guard-save",
                    disabled: saving(),
                    onclick: move |_| {
                        let pid = pid.clone();
                        let n = value();
                        saving.set(true);
                        spawn(async move {
                            let ok = set_max_iterations(&pid, n).await;
                            saving.set(false);
                            if ok {
                                crate::toast::push_toast(
                                    toasts,
                                    crate::toast::ToastKind::Info,
                                    &format!("Loop guard set to {n} iteration(s)."),
                                );
                            } else {
                                crate::toast::push_toast(
                                    toasts,
                                    crate::toast::ToastKind::Error,
                                    "Could not update the loop guard.",
                                );
                            }
                        });
                    },
                    if saving() { "Saving…" } else { "Save" }
                }
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

    // The active connection (native vs GitHub), shown honestly in the Onboard view.
    let provider_res = use_resource(fetch_provider);

    // Ask-a-finding (#54): the signal is provided by App (in main.rs) so that
    // ChatBubble — which is a sibling of CockpitShell, not a descendant — can
    // read it. We just consume it here so children can get it from context.
    // (App guarantees it's provided before CockpitApp mounts.)
    let _ask_finding_present = use_context::<Signal<Option<crate::chat::FindingContext>>>();

    // Onboard state lifted to app scope so it SURVIVES navigating between cockpit views:
    // the Phase-1 scan result, and the id of an in-flight async audit job. A background
    // job keeps running server-side regardless; these let the UI re-attach (resume the
    // poll, re-show the scan) when the user returns to Onboard instead of losing it.
    let onboard_scan = use_signal(|| Option::<ScanReportView>::None);
    use_context_provider(|| onboard_scan);
    let active_audit_job = use_signal(|| Option::<String>::None);
    use_context_provider(|| active_audit_job);

    // Routines + Onboard live inside the cockpit (architect tools). All hooks above
    // have run, so branching here is safe.
    if view() == CockpitView::Onboard {
        let conn = provider_res.read().clone().flatten();
        return rsx! {
            div { class: "cockpit",
                AppUpdateBanner {}
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
                AppUpdateBanner {}
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
                AppUpdateBanner {}
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
                AppUpdateBanner {}
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
                AppUpdateBanner {}
                CockpitNav { view }
                div { class: "cockpit-scroll",
                    DocsView {}
                }
            }
        };
    }

    // The Governed Development page (work-item / UoW surface). It owns its own data
    // fetching and selection state; CockpitApp just hosts it inside the shell chrome.
    rsx! {
        div { class: "cockpit",
            AppUpdateBanner {}
            CockpitNav { view }
            div { class: "cockpit-scroll",
                GovernedDevPage {}
            }
        }
    }
}

// ── Governed Development page ────────────────────────────────────────────────────
//
// The page is split into three provider-AGNOSTIC pieces plus one GitHub-specific one:
//   - `GovernedDevPage`     — the shell: left nav (Issue Management + UoW cards) + main.
//   - `IssueManagementPanel`— GITHUB-SPECIFIC connection + pull (the adapter seam), then
//                             a provider-agnostic `WorkItemTable` + `WorkItemDetail`.
//   - `WorkItemTable` / `WorkItemDetail` — operate purely on the `WorkItem` DTO.
//   - `UowDevControls`      — the existing governed-dev mechanisms (run-through-the-gate,
//                             clarify back-and-forth, sign-off) keyed to the selected UoW,
//                             plus comment-to-issue + pull-latest. Provider-agnostic.

/// A gear-icon button that opens the project-settings popup.
///
/// Contains project-scoped settings that must NOT live inline in a UoW:
///   - Loop guard (max revise iterations)
///   - Default tier-map (fast / balanced / strongest model ids)
#[component]
fn ProjectSettingsGear() -> Element {
    let mut open = use_signal(|| false);
    let active = use_resource(fetch_active_project);
    let proj = active.read().clone().flatten();

    rsx! {
        // The gear trigger button.
        button {
            class: "btn-edit-sm govdev-gear-btn",
            title: "Project settings",
            onclick: move |_| open.set(true),
            // Unicode gear character
            "\u{2699}\u{FE0F} Settings"
        }

        // The popup modal — only rendered when open AND we have a project.
        if open() {
            if let Some(p) = proj {
                div { class: "rule-modal-overlay", onclick: move |_| open.set(false),
                    div { class: "rule-modal proj-settings-modal", onclick: move |e| e.stop_propagation(),
                        div { class: "rule-modal-head",
                            span { class: "rule-modal-id", "Project settings" }
                            button {
                                class: "rule-modal-close",
                                onclick: move |_| open.set(false),
                                "\u{2715}"
                            }
                        }
                        p { class: "proj-settings-scope-note",
                            "These settings apply to the entire project and affect all governed runs."
                        }

                        // ── Loop guard ────────────────────────────────────────────
                        LoopGuardControl {}

                        // ── Default tier-map ──────────────────────────────────────
                        div { class: "proj-settings-section",
                            TierMapEditor { project: p }
                        }
                    }
                }
            } else {
                // No active project: show a minimal modal with instructions.
                div { class: "rule-modal-overlay", onclick: move |_| open.set(false),
                    div { class: "rule-modal proj-settings-modal", onclick: move |e| e.stop_propagation(),
                        div { class: "rule-modal-head",
                            span { class: "rule-modal-id", "Project settings" }
                            button {
                                class: "rule-modal-close",
                                onclick: move |_| open.set(false),
                                "\u{2715}"
                            }
                        }
                        p { class: "proj-settings-scope-note",
                            "Create or select a project to configure project-level settings."
                        }
                    }
                }
            }
        }
    }
}

/// The Governed Development page. Left: "Issue Management" entry + a card per UoW.
/// Right (main): the issue-management panel, or the selected UoW's dev controls.
#[component]
fn GovernedDevPage() -> Element {
    // Selection in the left nav. Defaults to the Issue Management panel.
    let mut sel = use_signal(|| GovDevSel::IssueManagement);

    // The UoW list (left-nav cards). Re-fetched whenever this tick bumps (e.g. after a
    // UoW is created from a work item).
    let uows_refresh = use_signal(|| 0u32);
    let uows_res = use_resource(move || {
        let _dep = uows_refresh();
        async move { fetch_uows().await }
    });

    let uows = uows_res.read().clone().flatten().unwrap_or_default();

    rsx! {
        div { class: "govdev",
            // ── LEFT NAV: Issue Management + one card per UoW ──────────────────
            aside { class: "govdev-nav",
                // Gear button: opens the project-settings popup (loop guard + tier map).
                // Lives at the top of the nav so it's always reachable regardless of UoW selection.
                div { class: "govdev-gear-row",
                    ProjectSettingsGear {}
                }
                button {
                    class: if sel() == GovDevSel::IssueManagement { "govdev-nav-top on" } else { "govdev-nav-top" },
                    onclick: move |_| sel.set(GovDevSel::IssueManagement),
                    span { class: "govdev-nav-top-title", "Issue Management" }
                    span { class: "govdev-nav-top-sub", "Pull issues · create Units of Work" }
                }
                p { class: "govdev-nav-label", "UNITS OF WORK ({uows.len()})" }
                div { class: "govdev-uow-list",
                    if uows.is_empty() {
                        p { class: "govdev-uow-empty", "No Units of Work yet. Pull work items and create one from an issue." }
                    }
                    for u in uows.iter() {
                        {
                            let uid = u.id.clone();
                            let selected = sel() == GovDevSel::Uow(uid.clone());
                            let cls = if selected { "govdev-uow-card sel" } else { "govdev-uow-card" };
                            let title = u.work_item.title.clone();
                            let repo = u.work_item.repo.clone();
                            let stage = u.stage.label();
                            rsx! {
                                button {
                                    class: "{cls}",
                                    onclick: move |_| sel.set(GovDevSel::Uow(uid.clone())),
                                    span { class: "govdev-uow-title", "{title}" }
                                    div { class: "govdev-uow-meta",
                                        span { class: "govdev-uow-repo", "{repo}" }
                                        span { class: "govdev-uow-stage", "{stage}" }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ── MAIN: the issue-management panel, or a UoW's dev controls ──────
            section { class: "govdev-main",
                match sel() {
                    GovDevSel::IssueManagement => rsx! {
                        IssueManagementPanel { uows: uows.clone(), uows_refresh, sel }
                    },
                    GovDevSel::Uow(uid) => {
                        match uows.iter().find(|u| u.id == uid).cloned() {
                            // Key by the UoW id so switching UoWs REMOUNTS the controls with
                            // fresh per-UoW state. Without the key, Dioxus reused one instance and
                            // just swapped the prop, so the first UoW's use_signal/use_resource
                            // state (dev status, stage, run model, etc.) bled into every other UoW.
                            Some(u) => rsx! { UowDevControls { key: "{u.id}", uow: u } },
                            // The UoW vanished from the list (e.g. between refreshes): fall back.
                            None => rsx! {
                                p { class: "section-hint", "That Unit of Work is no longer available." }
                            },
                        }
                    }
                }
            }
        }
    }
}

/// The Issue Management panel: a GitHub-specific connection summary + a "Pull work items"
/// button, then a provider-agnostic table of pulled `WorkItem`s and a row-detail view.
///
/// PROVIDER-ADAPTER SEAM: the connection summary + the pull action are the only
/// GitHub-aware pieces here (the BFF resolves the active project's GitHub repos). The
/// table and the detail view operate purely on `WorkItem`, so a future Jira/ADO panel
/// reuses them verbatim.
#[component]
fn IssueManagementPanel(
    uows: Vec<UowListEntry>,
    uows_refresh: Signal<u32>,
    sel: Signal<GovDevSel>,
) -> Element {
    let provider_res = use_resource(fetch_provider);
    let active_proj = use_resource(fetch_active_project);

    let mut pulling = use_signal(|| false);
    // The work item whose detail is open (by stable id), if any.
    let mut detail_id = use_signal(|| Option::<String>::None);
    // Bumped on every pull and used as the table's `key`, so the Chorale work-item table
    // remounts with fresh rows (it initializes its rows once per mount via use_table).
    let mut pull_seq = use_signal(|| 0u32);

    let conn = provider_res.read().clone().flatten();
    let proj = active_proj.read().clone().flatten();
    let repos = proj.as_ref().map(|p| p.repos.clone()).unwrap_or_default();

    // GITHUB-SPECIFIC connection summary.
    let (conn_cls, conn_label) = match &conn {
        Some(p) if p.live => ("conn-ok", format!("● {} connected", p.provider)),
        Some(p) => ("conn-warn", format!("● {} (no GitHub token)", p.provider)),
        None => ("conn-warn", "● connecting…".to_string()),
    };

    // The pulled work items come from an APP-LIFETIME cache (survives navigating away and
    // back), keyed by project id so a project switch never shows stale items. None = not
    // pulled yet for the active project.
    let proj_id = proj.as_ref().map(|p| p.id.clone()).unwrap_or_default();
    let item_list: Option<Vec<WorkItem>> = match PULLED_WORK_ITEMS.read().clone() {
        Some((pid, list)) if !proj_id.is_empty() && pid == proj_id => Some(list),
        _ => None,
    };
    // Resolve the open detail item against the current pull.
    let open_item = match (&item_list, detail_id()) {
        (Some(list), Some(id)) => list.iter().find(|it| it.id == id).cloned(),
        _ => None,
    };

    rsx! {
        div { class: "issue-mgmt",
            p { class: "govdev-h", "Issue Management" }

            // ── Connection summary (GitHub adapter) ───────────────────────────
            div { class: "issue-conn",
                div { class: "issue-conn-line",
                    span { class: "issue-conn-label", "Provider" }
                    span { class: "issue-conn-prov", "GitHub" }
                    span { class: "{conn_cls}", "{conn_label}" }
                }
                div { class: "issue-conn-line",
                    span { class: "issue-conn-label", "Repositories" }
                    if repos.is_empty() {
                        span { class: "issue-conn-none", "No repos on the active project." }
                    } else {
                        span { class: "issue-conn-repos", "{repos.join(\", \")}" }
                    }
                }
            }

            div { class: "issue-pull-row",
                button {
                    class: "btn-run",
                    disabled: pulling(),
                    onclick: {
                        let proj_id = proj_id.clone();
                        move |_| {
                            let proj_id = proj_id.clone();
                            pulling.set(true);
                            spawn(async move {
                                let pulled = pull_work_items().await.unwrap_or_default();
                                *PULLED_WORK_ITEMS.write() = Some((proj_id, pulled));
                                detail_id.set(None);
                                pull_seq += 1;
                                pulling.set(false);
                            });
                        }
                    },
                    if pulling() { "Pulling…" } else { "Pull work items" }
                }
                span { class: "section-hint", "Pulls all open issues across the active project's repos. Manual; no cache." }
            }

            // ── The work-item table (provider-agnostic) ───────────────────────
            match item_list {
                None => rsx! {
                    p { class: "section-hint", "No work items pulled yet — press \u{201c}Pull work items\u{201d}." }
                },
                Some(list) if list.is_empty() => rsx! {
                    p { class: "section-hint", "No open work items found across the active project's repos." }
                },
                Some(list) => rsx! {
                    WorkItemTable {
                        key: "{pull_seq}",
                        items: list,
                        on_open: EventHandler::new(move |id: String| detail_id.set(Some(id))),
                    }
                },
            }

            // ── The detail view for a clicked row (provider-agnostic) ─────────
            if let Some(it) = open_item {
                WorkItemDetail {
                    item: it,
                    uows: uows.clone(),
                    on_close: EventHandler::new(move |_| detail_id.set(None)),
                    uows_refresh,
                    sel,
                }
            }
        }
    }
}

/// Chorale column set for the work-item table: Repo, #, Title, State (badge), Labels.
fn work_item_columns() -> Vec<ColumnDef<WorkItem>> {
    let state_badges = BadgeVariantMap::new()
        .with("open", BadgeVariant::new("OPEN", "green"))
        .with("closed", BadgeVariant::new("CLOSED", "gray"))
        .with_fallback(BadgeVariant::new("Unknown", "gray"));
    vec![
        ColumnDef::new(ColumnId("repo"), "Repo", |it: &WorkItem| {
            CellValue::Text(it.repo.clone())
        })
        .sortable()
        .filter(FilterKind::Text)
        .initial_width(180.0),
        ColumnDef::new(ColumnId("num"), "#", |it: &WorkItem| {
            CellValue::Text(format!("#{}", it.number))
        })
        .sortable()
        .initial_width(80.0),
        ColumnDef::new(ColumnId("title"), "Title", |it: &WorkItem| {
            CellValue::Text(it.title.clone())
        })
        .sortable()
        .filter(FilterKind::Text)
        .initial_width(420.0),
        ColumnDef::new(ColumnId("state"), "State", |it: &WorkItem| {
            CellValue::Text(it.state.to_ascii_lowercase())
        })
        .sortable()
        .render_kind(RenderKind::Badge(state_badges))
        .initial_width(110.0),
        ColumnDef::new(ColumnId("labels"), "Labels", |it: &WorkItem| {
            CellValue::Text(labels_summary(&it.labels))
        })
        .filter(FilterKind::Text)
        .initial_width(240.0),
    ]
}

/// A provider-agnostic CHORALE table of `WorkItem`s: columns Repo, #, Title, State, Labels.
/// Clicking a row opens its detail MODAL via `on_open` — the parent (IssueManagementPanel)
/// hosts the modal, outside this table's subtree. Create/open-UoW lives in that modal, not
/// per-row, so the table stays a clean read surface.
#[component]
fn WorkItemTable(items: Vec<WorkItem>, on_open: EventHandler<String>) -> Element {
    let rows: Vec<(RowId, WorkItem)> = use_hook({
        let items = items.clone();
        move || items.iter().map(|it| (RowId::new(), it.clone())).collect()
    });
    let id_map: std::collections::HashMap<RowId, String> =
        rows.iter().map(|(r, it)| (*r, it.id.clone())).collect();
    let handle = use_table(move || TableState::new(rows.clone(), work_item_columns()));
    rsx! {
        Table {
            handle,
            sort_enabled: true,
            filter_enabled: true,
            sticky_header: true,
            on_row_click: Callback::new(move |rid: RowId| {
                if let Some(id) = id_map.get(&rid) {
                    on_open.call(id.clone());
                }
            }),
        }
    }
}

/// The detail view for one work item: full title + body + state + a link to the issue,
/// the comments thread, plus (optionally) the create/open-UoW affordance (dedup-aware).
/// Provider-agnostic.
///
/// `show_uow_action` defaults to true (the work-item TABLE opens it to create/open a
/// UoW). When opened from INSIDE an existing UoW's dev controls, pass `false` to hide the
/// redundant create/open-UoW button — the UoW already exists.
#[component]
fn WorkItemDetail(
    item: WorkItem,
    uows: Vec<UowListEntry>,
    on_close: EventHandler<()>,
    uows_refresh: Signal<u32>,
    sel: Signal<GovDevSel>,
    #[props(default = true)] show_uow_action: bool,
) -> Element {
    let (state_label, state_cls) = work_item_state_badge(&item.state);
    let existing = existing_uow_for(&uows, &item.id).cloned();
    // Fetch this work item's comments once per item id (re-fetches if the id changes).
    let comments_res = {
        let wid = item.id.clone();
        use_resource(move || {
            let wid = wid.clone();
            async move { fetch_work_item_comments(&wid).await }
        })
    };
    let comments = comments_res.read().clone();
    rsx! {
        // Modal overlay (click backdrop to close); the inner box stops propagation so
        // clicks inside don't dismiss. Same overlay/box pattern as the rule detail modal.
        div { class: "rule-modal-overlay", onclick: move |_| on_close.call(()),
            div { class: "rule-modal wi-detail-modal", onclick: move |e| e.stop_propagation(),
                div { class: "wi-detail-head",
                    span { class: "wi-detail-repo", "{item.repo}" }
                    span { class: "wi-detail-num", "#{item.number}" }
                    span { class: "wi-state {state_cls}", "{state_label}" }
                    button {
                        class: "rule-modal-close",
                        onclick: move |_| on_close.call(()),
                        "\u{2715}"
                    }
                }
                p { class: "wi-detail-title", "{item.title}" }
                if item.body.is_empty() {
                    p { class: "wi-detail-body empty", "(no description)" }
                } else {
                    // GitHub issue bodies are Markdown — render to HTML (same renderer as
                    // the chat bubble and docs view), not raw text.
                    div {
                        class: "wi-detail-body md chat-turn-text",
                        dangerous_inner_html: crate::md::md_to_html(&item.body),
                    }
                }
                if !item.url.is_empty() {
                    a { class: "wi-detail-link", href: "{item.url}", target: "_blank", "Open issue \u{2197}" }
                }

                // ── Comments thread (read-only, fetched for this item) ────────────
                div { class: "wi-comments",
                    p { class: "wi-comments-h", "Comments" }
                    match comments {
                        // Still loading the comments fetch.
                        None => rsx! { p { class: "section-hint", "Loading comments…" } },
                        Some(list) if list.is_empty() => rsx! {
                            p { class: "wi-comments-empty section-hint", "No comments." }
                        },
                        Some(list) => rsx! {
                            for (i , c) in list.into_iter().enumerate() {
                                div { key: "{i}", class: "wi-comment",
                                    div { class: "wi-comment-meta",
                                        span { class: "wi-comment-author", "{c.author}" }
                                        if !c.created_at.is_empty() {
                                            span { class: "wi-comment-date", "{c.created_at}" }
                                        }
                                    }
                                    if c.body.is_empty() {
                                        p { class: "wi-comment-body empty", "(empty comment)" }
                                    } else {
                                        div {
                                            class: "wi-comment-body md chat-turn-text",
                                            dangerous_inner_html: crate::md::md_to_html(&c.body),
                                        }
                                    }
                                }
                            }
                        },
                    }
                }

                if show_uow_action {
                    div { class: "wi-detail-actions",
                        CreateOrOpenUow { item: item.clone(), existing, uows_refresh, sel, compact: false }
                    }
                }
            }
        }
    }
}

/// The dedup-aware create/open-UoW button shared by the table rows and the detail view.
/// If a UoW already exists for the work item, it shows "Open Unit of Work" and selects it;
/// otherwise it creates one (`POST /api/uow/from-workitem`), bumps the UoW list, and opens
/// the new UoW. `compact` renders the small in-row variant.
#[component]
fn CreateOrOpenUow(
    item: WorkItem,
    existing: Option<UowListEntry>,
    uows_refresh: Signal<u32>,
    sel: Signal<GovDevSel>,
    compact: bool,
) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let mut working = use_signal(|| false);
    let has_uow = existing.is_some();
    let label = create_or_open_label(has_uow);
    let base_cls = if compact { "btn-edit-sm" } else { "btn-run" };

    rsx! {
        button {
            class: "{base_cls}",
            disabled: working(),
            // Stop the row's onclick (open-detail) from also firing.
            onclick: move |evt| {
                evt.stop_propagation();
                let mut sel = sel;
                let mut uows_refresh = uows_refresh;
                let toasts = toasts;
                if let Some(ref u) = existing {
                    sel.set(GovDevSel::Uow(u.id.clone()));
                    return;
                }
                let wid = item.id.clone();
                working.set(true);
                spawn(async move {
                    match create_uow_from_work_item(&wid).await {
                        Some(res) => {
                            uows_refresh += 1;
                            sel.set(GovDevSel::Uow(res.uow_id.clone()));
                            if !res.created {
                                crate::toast::push_toast(
                                    toasts,
                                    crate::toast::ToastKind::Info,
                                    "A Unit of Work already existed for this issue — opened it.".to_string(),
                                );
                            }
                        }
                        None => {
                            crate::toast::push_toast(
                                toasts,
                                crate::toast::ToastKind::Warning,
                                "Could not create a Unit of Work from this issue.".to_string(),
                            );
                        }
                    }
                    working.set(false);
                });
            },
            if working() { "Working…" } else { "{label}" }
        }
    }
}

/// The dev controls for a selected Unit of Work. Reuses the EXISTING governed-dev
/// mechanisms — run the governed fleet THROUGH THE GATE, the clarify back-and-forth, and
/// sign-off — keyed to this UoW's id (the same key the existing endpoints use). Adds an
/// "Add comment to issue" box (`POST /api/workitems/comment`) and a "Pull latest work item"
/// button (`POST /api/workitems/refresh`). Provider-agnostic: it only reads the WorkItem DTO.
#[component]
fn UowDevControls(uow: UowListEntry) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    // The UoW id keys every reused governed-dev endpoint (run, sign-off, UoW panel).
    let uow_key = uow.id.clone();

    // A local copy of the work item so "Pull latest" can refresh the displayed metadata
    // without re-fetching the whole UoW list.
    let mut item = use_signal(|| uow.work_item.clone());
    // Re-sync the displayed item when the selected UoW changes (prop change).
    use_effect(use_reactive(&uow.work_item, move |wi| item.set(wi)));

    // The reused per-UoW UoW panel / run live behind a refresh tick, same as the old page.
    let uow_refresh = use_signal(|| 0u32);

    // Increment 1: runs live ON THE STEPS, not a standalone button. We fetch the UoW
    // here (keyed on the same refresh tick the panel uses) so we know the current
    // lifecycle `stage` and can render the run control for the ACTIVE phase inline with
    // the lifecycle strip. The downstream `UowPanel` re-fetches the same UoW for its own
    // read-out, so the two stay in sync without sharing a fetch.
    let uow_for_stage = {
        let sid = uow.id.clone();
        use_resource(move || {
            let sid = sid.clone();
            let _dep = uow_refresh();
            async move { fetch_uow(&sid).await }
        })
    };
    let stage = uow_for_stage
        .read()
        .clone()
        .flatten()
        .map(|u| u.stage)
        .unwrap_or_default();

    // Live run state for THIS UoW (governed fleet through the gate).
    let active_run = use_signal(|| Option::<RunView>::None);

    // Model option list for every per-step selector.
    let run_models_res = use_resource(fetch_audit_models);
    let run_models_snap = run_models_res.read().clone().flatten();

    // The active project's tier map seeds the per-phase model defaults: investigation
    // defaults to the strongest tier; development pre-fills all three tiers. Each is
    // editable per-UoW for the run, without mutating the saved project tier map.
    let project_res = use_resource(fetch_active_project);
    let project_tier_map = project_res
        .read()
        .clone()
        .flatten()
        .map(|p| p.tier_map)
        .unwrap_or_default();

    // INVESTIGATION model (single select; default = project strongest).
    let mut invest_model = use_signal(String::new);
    // DEVELOPMENT tier models (three selects; defaults from the project tier map).
    let mut dev_strongest = use_signal(String::new);
    let mut dev_balanced = use_signal(String::new);
    let mut dev_fast = use_signal(String::new);
    // Seed the per-phase selectors from the project tier map once it loads, before the
    // user has touched them. Re-seeds if the active project changes.
    {
        let tm = project_tier_map.clone();
        use_effect(use_reactive(&tm, move |tm| {
            if invest_model.peek().is_empty() {
                invest_model.set(tm.strongest.clone());
            }
            if dev_strongest.peek().is_empty() {
                dev_strongest.set(tm.strongest.clone());
            }
            if dev_balanced.peek().is_empty() {
                dev_balanced.set(tm.balanced.clone());
            }
            if dev_fast.peek().is_empty() {
                dev_fast.set(tm.fast.clone());
            }
        }));
    }

    // Comment-to-issue composer (+ GitHub-style @-mention autocomplete).
    let mut comment_body = use_signal(String::new);
    let mut commenting = use_signal(|| false);
    // Pull-latest state.
    let mut refreshing = use_signal(|| false);

    // ── Work-item modal (opened from inside the UoW) ───────────────────────────
    // A local flag toggles the WorkItemDetail modal for THIS UoW's work item. The
    // modal's create/open-UoW action is hidden (the UoW already exists), so the
    // uows / sel / uows_refresh it requires are local throwaways here.
    let mut wi_modal_open = use_signal(|| false);
    let modal_uows_refresh = use_signal(|| 0u32);
    let modal_sel = use_signal(|| GovDevSel::IssueManagement);

    // ── @-mention autocomplete state ───────────────────────────────────────────
    // The repo's assignable users, fetched once per work item (the practical mention
    // set). Degrades to empty (no token / error) → the dropdown simply never shows.
    let assignees_res = {
        let wid = uow.work_item.id.clone();
        use_resource(move || {
            let wid = wid.clone();
            async move { fetch_work_item_assignees(&wid).await }
        })
    };
    let assignees = assignees_res.read().clone().unwrap_or_default();
    // Whether the dropdown is showing (an active `@token` exists and matches).
    let mut mention_open = use_signal(|| false);

    let it = item.read().clone();
    let (state_label, state_cls) = work_item_state_badge(&it.state);

    rsx! {
        div { class: "uow-dev",
            // ── Work-item header (provider-agnostic read of the DTO) ───────────
            div { class: "uow-dev-head",
                span { class: "uow-dev-repo", "{it.repo}" }
                span { class: "uow-dev-num", "#{it.number}" }
                span { class: "wi-state {state_cls}", "{state_label}" }
                // Open the full work-item modal (title + body + ALL comments) in-app.
                button {
                    class: "btn-edit-sm",
                    onclick: move |_| wi_modal_open.set(true),
                    "Open work item"
                }
                // RETAINED: the direct link to the issue on the tracker.
                if !it.url.is_empty() {
                    a { class: "wi-detail-link", href: "{it.url}", target: "_blank", "Open issue ↗" }
                }
            }
            p { class: "uow-dev-title", "{it.title}" }

            // The work-item modal (with comments), opened from the head button above.
            // Its create/open-UoW action is hidden — this UoW already exists.
            if wi_modal_open() {
                WorkItemDetail {
                    item: it.clone(),
                    uows: Vec::new(),
                    on_close: EventHandler::new(move |_| wi_modal_open.set(false)),
                    uows_refresh: modal_uows_refresh,
                    sel: modal_sel,
                    show_uow_action: false,
                }
            }

            // ── Pull latest work item ─────────────────────────────────────────
            div { class: "uow-dev-pull-row",
                button {
                    class: "btn-edit-sm",
                    disabled: refreshing(),
                    onclick: move |_| {
                        let wid = item.read().id.clone();
                        refreshing.set(true);
                        spawn(async move {
                            if let Some(updated) = refresh_work_item(&wid).await {
                                item.set(updated);
                            }
                            refreshing.set(false);
                        });
                    },
                    if refreshing() { "Pulling…" } else { "Pull latest work item" }
                }
                span { class: "section-hint", "Re-pull this issue from the tracker." }
            }

            // ── Gate self-check (reused) ──────────────────────────────────────
            GateSelfCheck {}

            // NOTE: Loop guard (max revise iterations) is a PROJECT-level setting and
            // has been moved to the gear-icon project-settings popup in GovernedDevPage.
            // It no longer lives here (per-UoW) to avoid implying it is per-UoW state.

            // ── Lifecycle steps with the ACTIVE phase's run control inline ────
            // Increment 1: runs live ON THE STEPS. The lifecycle strip shows the
            // ordered stages + the architect transitions ("Approve decisions"), and
            // the run control for the current phase is rendered inline beneath it —
            // it REPLACES the prior phase's control rather than stacking. The
            // investigation control owns the Intake → Investigating transition (with
            // its own model select); the development control runs the gated build with
            // a per-tier model map. The strongest tier leads and delegates simpler work.
            UowStepRunControls {
                story_id: uow_key.clone(),
                stage,
                uow_refresh,
                active_run,
                models: run_models_snap.clone(),
                invest_model,
                dev_strongest,
                dev_balanced,
                dev_fast,
            }

            // ── Agent activity for the active run (reused) ────────────────────
            {
                let rid = match active_run() {
                    Some(ref r) => r.id.clone(),
                    None => String::new(),
                };
                rsx! { crate::agent_activity::AgentActivity { run_id: rid } }
            }

            // ── AI-assisted "Update branch" (GitHub PR "Update branch", gated) ─
            // Targets THIS UoW's branch: pick a source branch (local or origin) and
            // merge it INTO the UoW branch. A clean merge commits; a conflict is
            // resolved by ONE gated agent (drives its own AgentActivity). Per-UoW
            // because it operates on this UoW's working branch.
            UowUpdateBranchControl {
                story_id: uow_key.clone(),
                uow_refresh,
                models: run_models_snap.clone(),
            }

            // ── The UoW panel (reused), keyed to this UoW ─────────────────────
            UowPanel { story_id: uow.id.clone(), uow_refresh }

            // ── The live run + provenance + sign-off (reused) ─────────────────
            if let Some(r) = active_run() {
                LiveRunPanel { run: r, uow_refresh }
            }

            // ── Add comment to the source issue (with @-mention autocomplete) ──
            // A comment with an @-mention IS how you loop a teammate in. As you type an
            // `@<partial>` token, a dropdown of the repo's ASSIGNABLE users (the practical
            // mention set GitHub resolves) appears; clicking one completes the @handle.
            // SCOPE: the candidate set is GitHub's assignees for the repo. A per-provider
            // mention wrapper (Jira/ADO user search) is the future generalization.
            div { class: "uow-comment",
                p { class: "clarify-h", "Add comment to issue" }
                p { class: "section-hint", "Posts a comment back onto the source issue via the tracker adapter. Type @ to mention an assignable teammate (GitHub resolves @handle)." }
                // The textarea wrapper is position-relative so the dropdown anchors to it.
                div { class: "uow-comment-box",
                    textarea {
                        class: "clarify-q",
                        value: "{comment_body}",
                        rows: "3",
                        placeholder: "Write a comment to post on the issue… (type @ to mention)",
                        oninput: move |e| {
                            let v = e.value();
                            // Recompute whether an active @token exists with matches.
                            let show = match active_mention_partial(&v) {
                                Some(p) => !filter_mention_candidates(&assignees, p).is_empty(),
                                None => false,
                            };
                            comment_body.set(v);
                            mention_open.set(show);
                        },
                    }
                    // The autocomplete dropdown: shown only when an active @token matches.
                    if mention_open() {
                        {
                            let partial = active_mention_partial(&comment_body()).unwrap_or("").to_string();
                            let candidates = filter_mention_candidates(&assignees, &partial);
                            rsx! {
                                div { class: "uow-mention-dropdown",
                                    for login in candidates {
                                        button {
                                            key: "{login}",
                                            class: "uow-mention-option",
                                            onclick: {
                                                let login = login.clone();
                                                move |_| {
                                                    let next = apply_mention_selection(&comment_body(), &login);
                                                    comment_body.set(next);
                                                    mention_open.set(false);
                                                }
                                            },
                                            "@{login}"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                button {
                    class: "btn-run",
                    disabled: commenting(),
                    onclick: move |_| {
                        let wid = item.read().id.clone();
                        let body = comment_body();
                        if body.trim().is_empty() {
                            return;
                        }
                        let toasts = toasts;
                        commenting.set(true);
                        spawn(async move {
                            match comment_on_work_item(&wid, &body).await {
                                Some(_url) => {
                                    comment_body.set(String::new());
                                    crate::toast::push_toast(
                                        toasts,
                                        crate::toast::ToastKind::Info,
                                        "Comment posted to the issue.".to_string(),
                                    );
                                }
                                None => {
                                    crate::toast::push_toast(
                                        toasts,
                                        crate::toast::ToastKind::Warning,
                                        "Could not post the comment.".to_string(),
                                    );
                                }
                            }
                            commenting.set(false);
                        });
                    },
                    if commenting() { "Posting…" } else { "Post comment" }
                }
            }
        }
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
    // `preview`/`preview_tool` carry the scan-time deterministic-preview provenance (Part B):
    // a preview finding is deterministic but NOT enforced until the CI story wires it.
    let mut out = String::from(
        "repo,severity,status,rule_id,also_matches,path,line,snippet,detail,preview,preview_tool\n",
    );
    for f in findings {
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{},{}\n",
            csv_field(&f.repo),
            csv_field(&f.severity),
            csv_field(&f.status),
            csv_field(&f.rule_id),
            csv_field(&f.also_matches.join(" ")),
            csv_field(&f.path),
            f.line,
            csv_field(&f.snippet),
            csv_field(&f.detail),
            f.preview,
            csv_field(f.preview_tool.as_deref().unwrap_or("")),
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
    /// PREVIEW (CI-security Part B): the server's scan-time deterministic preview pass ran
    /// the rule's underlying tool ITSELF and produced this finding, even though the rule is
    /// NOT yet wired into the repo's gate. Deterministic (stable tool rule-id) but ADVISORY:
    /// "preview — not enforced until wired". Defaults to `false` (back-compatible).
    #[serde(default)]
    preview: bool,
    /// For a preview finding, the tool that produced it (`clippy` | `ruff` | `eslint` |
    /// `semgrep`). `None` for non-preview findings. Shown in the Authority badge label.
    #[serde(default)]
    preview_tool: Option<String>,
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
        Self {
            state: TriageState::Unresolved,
            reason: String::new(),
            bucket: TechDebtBucket::Later,
        }
    }
}

/// Stable identity for a finding across the triage tables (repo + rule + location + snippet),
/// so its disposition survives table switches and re-sorts.
fn finding_key(f: &FindingView) -> String {
    format!(
        "{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}",
        f.repo, f.rule_id, f.path, f.line, f.snippet
    )
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

/// A CI-tier rule item for display in `CiRulesPanel` and for posting to the server.
/// Constructed at each call site from `ProposedRuleView` (onboarding) or from the
/// corpus + project selections (Rules panel). Only `enforcement == "mechanical"` or
/// `enforcement == "architectural"` items are CI-tier; structured/prose are excluded.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct CiRuleItem {
    id: String,
    title: String,
    enforcement: String,
    #[serde(default)]
    linter: Option<String>,
}

/// Extract the first linter hint from a `ProposedRuleView`'s sources, if any.
fn first_linter(rule: &ProposedRuleView) -> Option<String> {
    rule.sources
        .iter()
        .find_map(|s| s.linter.clone().filter(|l| !l.is_empty()))
}

/// Build `CiRuleItem`s from a proposed-rules list, keeping only CI-tier enforcement
/// levels ("mechanical" and "architectural"). Used at the onboarding call sites where
/// `proposed_rules` is already available on the scan report.
fn ci_rule_items_from_proposed(rules: &[ProposedRuleView]) -> Vec<CiRuleItem> {
    rules
        .iter()
        .filter(|r| r.enforcement == "mechanical" || r.enforcement == "architectural")
        .map(|r| CiRuleItem {
            id: r.id.clone(),
            title: r.title.clone(),
            enforcement: r.enforcement.clone(),
            linter: first_linter(r),
        })
        .collect()
}

/// Build `CiRuleItem`s from the project's applied selections joined with the corpus.
/// Used at the Rules-panel call site where we have `RuleSelectionView`s (rule_id only)
/// and must look up enforcement + title from the corpus `Vec<ProposedRuleView>`.
fn ci_rule_items_from_selections(
    selections: &[RuleSelectionView],
    corpus: &[ProposedRuleView],
) -> Vec<CiRuleItem> {
    let corpus_map: std::collections::HashMap<&str, &ProposedRuleView> =
        corpus.iter().map(|r| (r.id.as_str(), r)).collect();
    selections
        .iter()
        .filter_map(|s| corpus_map.get(s.rule_id.as_str()).copied())
        .filter(|r| r.enforcement == "mechanical" || r.enforcement == "architectural")
        .map(|r| CiRuleItem {
            id: r.id.clone(),
            title: r.title.clone(),
            enforcement: r.enforcement.clone(),
            linter: first_linter(r),
        })
        .collect()
}

/// POST /api/onboard/ci-rules for a single tier. Returns the GitHub issue URL on success.
async fn wire_ci_rules_tier(
    repo: &str,
    tier: &str,
    rules: Vec<CiRuleItem>,
) -> Result<String, String> {
    let payload = serde_json::json!({
        "repo": repo,
        "tier": tier,
        "rules": rules,
    });
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/api/onboard/ci-rules", crate::BFF_URL))
        .json(&payload)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?
        .json()
        .await
        .map_err(|e| format!("invalid response: {e}"))?;
    let ok = v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false);
    if !ok {
        let msg = v
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown error");
        return Err(msg.to_string());
    }
    v.get("url")
        .and_then(|u| u.as_str())
        .map(String::from)
        .ok_or_else(|| "server returned ok but no url".to_string())
}

/// The "add CI-enforced rules" panel, split by enforcement tier.
///
/// Mechanical and architectural rules are both deterministic CI-tier checks. Mechanical
/// rules map to an existing off-the-shelf linter (simple to wire). Architectural rules
/// require a custom checker and team refinement before implementing.
///
/// The panel renders TWO separate "Create story" buttons — one per tier — so the two
/// tracks land as distinct GitHub issues and can be scheduled independently. A button
/// is shown only when that tier has at least one rule. Both buttons are per-repo.
#[component]
fn CiRulesPanel(repos: Vec<String>, rules: Vec<CiRuleItem>) -> Element {
    let mut msg = use_signal(String::new);
    let mut busy = use_signal(|| false);

    let mechanical: Vec<CiRuleItem> = rules
        .iter()
        .filter(|r| r.enforcement == "mechanical")
        .cloned()
        .collect();
    let architectural: Vec<CiRuleItem> = rules
        .iter()
        .filter(|r| r.enforcement == "architectural")
        .cloned()
        .collect();

    let has_mechanical = !mechanical.is_empty();
    let has_architectural = !architectural.is_empty();

    rsx! {
        div { class: "fix-panel",
            p { class: "scan-section-h", "Add CI-enforced rules" }
            p { class: "scan-section-sub",
                "Mechanical and architectural rules are both deterministic CI-tier checks. \
                 Mechanical rules map to an existing off-the-shelf linter (simple to wire). \
                 Architectural rules require a custom checker and team refinement before implementing. \
                 Each tier files a separate GitHub issue so the two tracks can be scheduled independently."
            }
            for repo in repos.iter() {
                {
                    let repo = repo.clone();
                    let mech_rules = mechanical.clone();
                    let arch_rules = architectural.clone();
                    rsx! {
                        div { class: "fix-row", key: "{repo}",
                            span { class: "fix-repo", "{repo}" }
                            if has_mechanical {
                                {
                                    let repo_m = repo.clone();
                                    let rules_m = mech_rules.clone();
                                    rsx! {
                                        button {
                                            class: "btn-run",
                                            disabled: busy(),
                                            onclick: move |_| {
                                                let r = repo_m.clone();
                                                let rules = rules_m.clone();
                                                busy.set(true);
                                                msg.set(String::new());
                                                spawn(async move {
                                                    match wire_ci_rules_tier(&r, "mechanical", rules).await {
                                                        Ok(url) => msg.set(format!(
                                                            "Filed mechanical CI-rules story for {r}: {url}"
                                                        )),
                                                        Err(e) => msg.set(format!(
                                                            "Could not file mechanical story for {r}: {e}"
                                                        )),
                                                    }
                                                    busy.set(false);
                                                });
                                            },
                                            "Create mechanical-rules CI story"
                                        }
                                    }
                                }
                            }
                            if has_architectural {
                                {
                                    let repo_a = repo.clone();
                                    let rules_a = arch_rules.clone();
                                    rsx! {
                                        button {
                                            class: "btn-run",
                                            disabled: busy(),
                                            onclick: move |_| {
                                                let r = repo_a.clone();
                                                let rules = rules_a.clone();
                                                busy.set(true);
                                                msg.set(String::new());
                                                spawn(async move {
                                                    match wire_ci_rules_tier(&r, "architectural", rules).await {
                                                        Ok(url) => msg.set(format!(
                                                            "Filed architectural CI-rules story for {r}: {url}"
                                                        )),
                                                        Err(e) => msg.set(format!(
                                                            "Could not file architectural story for {r}: {e}"
                                                        )),
                                                    }
                                                    busy.set(false);
                                                });
                                            },
                                            "Create architectural-rules CI story"
                                        }
                                    }
                                }
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

/// One authoritative source backing a rule's grounding (mirrors `RuleSourceView`
/// from the server DTO). Used in `ProposedRuleView.sources`.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Default)]
struct RuleSourceView {
    url: String,
    title: String,
    #[serde(default)]
    linter: Option<String>,
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
    /// Server-side auto-recommend flag (pw/cockpit-ui product wave). The server
    /// emits `is_auto_recommended: true` for rules whose `verification` is
    /// `grounded` or `verified` (the two rungs that have been reviewed against a
    /// real source). `draft` and `needs_recheck` rules arrive with it `false`.
    /// Falls back to `recommended` when the field is absent so old server payloads
    /// continue to work.
    #[serde(default)]
    is_auto_recommended: bool,
    /// Provenance / verification status: `draft` | `grounded` | `verified` |
    /// `needs_recheck`. Defaults to `draft` for any rule that omits the field
    /// (pre-schema corpus rules, AI-discovered rules). See
    /// `docs/decisions/2026-06-20_rule_provenance_schema.md`.
    #[serde(default = "default_draft")]
    verification: String,
    /// Authoritative sources backing this rule's grounding (empty for `draft`).
    #[serde(default)]
    sources: Vec<RuleSourceView>,
}

fn default_draft() -> String {
    "draft".to_string()
}

impl ProposedRuleView {
    /// True when this rule should be pre-checked on first view of the proposed-rules
    /// table. Prefers the explicit `is_auto_recommended` field (set by the pw/cockpit-ui
    /// server wave); falls back to `recommended` for older server payloads.
    ///
    /// Only `grounded` and `verified` rules are auto-recommended because those are the
    /// two rungs of the provenance ladder that have been reviewed against a real source.
    /// `draft` and `needs_recheck` appear LISTED but unchecked so the architect must
    /// explicitly opt them in.
    fn effective_auto_recommended(&self) -> bool {
        // If the server sent an explicit `is_auto_recommended` flag, honour it.
        // Otherwise derive from `verification`: grounded/verified → recommended.
        if self.is_auto_recommended {
            return true;
        }
        // Back-compat: use `recommended` only when the rule is also grounded/verified.
        // A `recommended: true` on a `draft` rule was previously possible; we now
        // treat those as "available (not auto-recommended)" to match the new UX contract.
        self.recommended
            && matches!(self.verification.as_str(), "grounded" | "verified")
    }
}

/// Map a verification string to `(badge_label, css_modifier)`.
///
/// Used in every table that shows rules (proposed-rules in onboarding, corpus
/// in the Rules window, applied in the Rules window) and in the rule detail
/// modal. Pure function so it is unit-testable without a DOM.
///
/// CSS modifiers correspond to `.verif-badge.<modifier>` rules in GLOBAL_CSS:
/// - `verified`     -> green checkmark badge ("Verified")
/// - `grounded`     -> blue source-dot badge ("\u{29bf} Grounded")
/// - `draft`        -> muted italic badge ("Draft")
/// - `needs_recheck`-> amber warning badge ("Needs re-check")
/// - anything else  -> same as `draft`
fn verif_badge(verif: &str) -> (&'static str, &'static str) {
    match verif {
        "verified"     => ("\u{2713} Verified",    "verified"),
        // Grounded carries its OWN distinct glyph (a circled source-dot, ⦿) so it reads as a
        // clear status on the rule tables, not just a faint blue tint — and is visually
        // distinct from the verified checkmark and the symbol-less draft / needs-re-check
        // badges. See `docs/decisions/2026-06-20_ui_bugfixes.md`.
        "grounded"     => ("\u{29bf} Grounded",    "grounded"),
        "needs_recheck"=> ("Needs re-check",       "needs-recheck"),
        _              => ("Draft",                "draft"),
    }
}

/// Build a human-readable tooltip string from a list of sources.
/// Returns an empty string when sources is empty (badge has no hover text in that case).
fn verif_sources_tooltip(sources: &[RuleSourceView]) -> String {
    if sources.is_empty() {
        return String::new();
    }
    sources
        .iter()
        .map(|s| {
            if let Some(ref linter) = s.linter {
                format!("{} [{}]", s.title, linter)
            } else {
                s.title.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
struct StackView {
    repo: String,
    #[serde(default)]
    languages: Vec<String>,
    #[serde(default)]
    frameworks: Vec<String>,
}

/// Custom-rule helpers for onboarding. `domain` routes a rule: a repo's `owner/repo` =
/// repo-scoped (the "Custom" domain, shown only in that repo's table); `*` = all repos (the
/// "Custom Global" domain, shown everywhere like a project-level rule). `body` is the free-text
/// directive — the architect owns its wording. (Struct defined above; this adds onboarding methods.)
impl CustomRuleView {
    /// True for a Custom Global rule (applies to every repo).
    fn is_global(&self) -> bool {
        let d = self.domain.trim();
        d == "*" || d.is_empty()
    }
    /// Stable table id for this custom rule.
    fn rule_id(&self) -> String {
        format!("CUSTOM-{}", self.name)
    }
    /// Render as a proposed-rule row so it lives in the table alongside corpus rules. A single
    /// option carries the body, so directive resolution (audit/arm) returns the body, not the
    /// name, and it never reads as "needs a choice".
    fn to_proposed(&self, all_repos: &[String]) -> ProposedRuleView {
        let repos = if self.is_global() {
            all_repos.to_vec()
        } else {
            vec![self.domain.clone()]
        };
        ProposedRuleView {
            id: self.rule_id(),
            title: self.name.clone(),
            kind: "review".to_string(),
            enforcement: "prose".to_string(),
            options: vec![RuleOptionView {
                id: "custom".to_string(),
                label: self.name.clone(),
                directive: self.body.clone(),
                why: String::new(),
            }],
            default_option: Some("custom".to_string()),
            decision_question: None,
            decision_why: None,
            scope: "repo-local".to_string(),
            domain: if self.is_global() {
                "Custom Global".to_string()
            } else {
                "Custom".to_string()
            },
            repos,
            placement: "Guidance in AGENTS.md, reviewed at PR (custom · prose)".to_string(),
            finding_count: 0,
            recommended: true,
            // Custom rules are user-authored; they don't go through the corpus grounding
            // ladder. Emit them as `verified` (the architect authored + trusts them) so
            // they show the checkmark badge alongside corpus-verified rules, and
            // is_auto_recommended = true so they are pre-checked on the proposed-rules table.
            is_auto_recommended: true,
            verification: "verified".to_string(),
            sources: Vec::new(),
        }
    }
}

/// True when a rule id is a user-authored custom rule (so apply routes it through the project's
/// `ruleset.custom` / CUSTOM-block emit, not the regular arm-request path).
fn is_custom_rule_id(id: &str) -> bool {
    id.starts_with("CUSTOM-")
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
    /// Tokens served from the prompt cache (billed at ~0.1x input). Nonzero only when
    /// the API backend ran with prompt caching active (multi-batch parallel scans).
    #[serde(default)]
    cache_read_input_tokens: u64,
    /// Tokens written to the prompt cache (billed at ~1.25x input, one-time per TTL).
    #[serde(default)]
    cache_creation_input_tokens: u64,
}

/// One SOC-2 control gap entry from the deep tier, mirroring `DeepReport.soc2_gaps`.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
struct Soc2GapView {
    control: String,
    #[serde(default)]
    title: String,
    /// One of "met" | "partial" | "gap" | "unknown".
    status: String,
    #[serde(default)]
    observed: String,
    #[serde(default)]
    gap: String,
}

/// One deep-tier lens result (SOC-2 gap / deep-security / threat-model).
/// Mirrors `ai_audit::DeepLensResult` on the wire.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
struct DeepLensResultView {
    /// Stable id: "soc2-gap" | "deep-security" | "threat-model".
    lens: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    soc2_gaps: Vec<Soc2GapView>,
    /// Extra security / threat findings (deep-security + threat-model free-text content).
    #[serde(default)]
    detail: String,
    /// Always `true` for the deep tier — the whole tier is model-inferred, advisory.
    #[serde(default)]
    advisory: bool,
    /// Per-lens honesty disclaimer surfaced in the UI.
    #[serde(default)]
    disclaimer: String,
}

/// The top-level deep-tier output attached to a scan report when `deep: true` was sent.
/// Mirrors `ai_audit::DeepReport` on the wire.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
struct DeepReportView {
    lenses: Vec<DeepLensResultView>,
    /// Always `true` — the whole tier is advisory.
    #[serde(default)]
    advisory: bool,
    /// Honesty disclaimer for the whole tier.
    #[serde(default)]
    disclaimer: String,
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
    /// OPT-IN deep compliance & security tier output (#55). `None` unless the audit
    /// request sent `deep: true`. Everything inside is ADVISORY + model-inferred.
    #[serde(default)]
    deep: Option<DeepReportView>,
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
    // User-authored custom rules created during onboarding (Custom + Custom Global). Persisted
    // so they survive reload; written to the project's ruleset.custom on apply/complete.
    #[serde(default)]
    custom: Vec<CustomRuleView>,
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
/// When `deep` is true, the server also runs the three deep-tier lenses (SOC-2 gap,
/// deep security, threat model) and attaches the results to `report.deep`.
#[allow(clippy::too_many_arguments)]
async fn audit_against(
    repos: &[String],
    rules: &[SelectedAuditRule],
    model: &str,
    calibration_model: &str,
    mode: &str,
    thorough: bool,
    incremental: bool,
    deep: bool,
) -> Option<ScanReportView> {
    let rule_json = audit_rules_json(rules);
    reqwest::Client::new()
        .post(format!("{}/api/onboard/audit", crate::BFF_URL))
        .json(&serde_json::json!({
            "repos": repos,
            "rules": rule_json,
            "model": model,
            "calibration_model": calibration_model,
            "mode": mode,
            "thorough": thorough,
            "incremental": incremental,
            "deep": deep,
        }))
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
/// server's chunk/batch math (ai_audit) so the number tracks what the audit actually sends.
///
/// Input and output are priced SEPARATELY (output bills ~5× input and dominates
/// findings-heavy scans). The estimate is deliberately biased slightly CONSERVATIVE (high):
/// an estimate that turns into a smaller bill is a pleasant surprise; one that turns into
/// a bigger bill is broken trust.
///
/// PROMPT CACHING: for multi-batch parallel scans (the default), the codebase prefix (repo
/// map + chunk digest) is the same across every rule-batch for a given chunk. When the API
/// backend is in use the server marks this prefix with `cache_control: ephemeral` so the
/// provider caches it after the first batch and reads it at ~0.1× for subsequent batches.
/// The estimate models this:
///   - batch 0 per chunk: full input price + 1.25× cache-write surcharge on the digest
///   - batches 1..N per chunk: digest tokens read from cache at 0.1× instead of 1.0×
/// Sequential mode (one batch per chunk) has no prefix reuse across batches, so no caching
/// discount applies. CLI backend also skips caching (no-op there).
///
/// The FUDGE factor keeps the estimate conservative overall even after the cache discount,
/// since the calibration pass (over aggregated findings) and the resolution round are
/// modeled at full price.
/// `code_chars` is the in-scope code size. The caller is responsible for passing the size of
/// the SCANNED file set: the whole repo for a full scan. For an incremental re-scan only the
/// CHANGED files are actually sent to the AI, but the client has no per-file / changed-file
/// token breakdown today (`ScanReportView` carries only the repo-total `code_chars`), so we
/// price the FULL set and flag `incremental` in the readout as a known over-estimate. See the
/// followup in `docs/decisions/2026-06-20_ui_bugfixes.md`.
///
/// `deep` (the SOC-2 / deep-security / threat-model tier) adds three EXTRA whole-repo prose
/// passes at the AUDIT model on top of the standard scan + calibration: each re-reads the full
/// `code_chars` as input and emits a long prose report. Deep is therefore the priciest option
/// and the returned dollar figure reflects that, not just a prose warning.
#[allow(clippy::too_many_arguments)]
fn estimate_audit_cost(
    code_chars: usize,
    selected: usize,
    mode: &str,
    audit_in: f64,
    audit_out: f64,
    calib_in: f64,
    calib_out: f64,
    thorough: bool,
    incremental: bool,
    deep: bool,
) -> (u64, f64, usize) {
    const CHUNK_DIGEST_CHARS: usize = 350_000;
    const RULE_BATCH_SIZE: usize = 15;
    const CHARS_PER_TOKEN: f64 = 4.0;
    // Per-pass overhead (rules block + system prompt) that varies per batch and is never
    // cached. The digest + repo map form the cached prefix, so only this remainder is
    // re-sent at full price for subsequent batches.
    const OVERHEAD_CHARS_PER_PASS: usize = 10_000;
    // Output is findings: a baseline per pass plus a term that scales with code scanned
    // (so a findings-dense or large scan isn't under-counted on the half that bites most).
    const OUT_TOKENS_PER_PASS: f64 = 2_200.0;
    const OUTPUT_PER_CODE_TOKEN: f64 = 0.02;
    // Resolution round + general conservatism. Biased HIGH on purpose: logged real runs
    // (budget-mini ~2.24×, chorale ~1.75×) came in UNDER estimate even before caching, and
    // an audit that costs more than quoted is the bad surprise.
    const FUDGE: f64 = 1.4;
    // Prompt-cache pricing multipliers (Anthropic list pricing as of 2024-07):
    //   write (first batch per chunk): 1.25× input
    //   read  (subsequent batches):    0.10× input
    const CACHE_WRITE_MULT: f64 = 1.25;
    const CACHE_READ_MULT: f64 = 0.10;
    // Deep tier (#55): three EXTRA whole-repo passes (SOC-2 gap, deep security, threat model).
    // Each reads the full code once and emits a long prose report. Priced at the audit model.
    const DEEP_PASSES: f64 = 3.0;
    // A deep pass emits far more prose than a per-rule finding pass (full report per lens).
    const DEEP_OUT_TOKENS_PER_PASS: f64 = 8_000.0;

    // Batch mode (#61): the Anthropic Message Batches API charges a flat 50% discount on
    // ALL input and output tokens for the SCAN passes (which are submitted as a batch).
    // The calibration pass always runs real-time (a single call over aggregated findings
    // — not batched), so calib pricing is NOT discounted.
    let batch_discount = if mode == "batch" { 0.5 } else { 1.0 };
    let (eff_audit_in, eff_audit_out) = (audit_in * batch_discount, audit_out * batch_discount);
    // Calibration is real-time even in batch mode: one call over the aggregated findings.
    let (eff_calib_in, eff_calib_out) = (calib_in, calib_out);

    let chunks = code_chars.div_ceil(CHUNK_DIGEST_CHARS).max(1);
    let batches = if mode == "sequential" {
        1
    } else {
        selected.div_ceil(RULE_BATCH_SIZE).max(1)
    };
    let passes = chunks * batches;
    let code_tokens = code_chars as f64 / CHARS_PER_TOKEN;

    // ── Scan passes, priced at the AUDIT model (with batch discount applied) ──
    //
    // Without caching: the full digest is re-sent at full input price every pass.
    // With caching (parallel/batch mode, batches > 1): per chunk, batch 0 pays full input
    // + the one-time 1.25× cache-write surcharge; batches 1..N read the cached digest at
    // 0.1×. Sequential (batches == 1) has no reuse, so no discount.
    //
    // Overhead tokens (rules block, system prompt) are always sent at full price since they
    // vary per batch.
    let scan_in = if batches <= 1 {
        // No caching benefit: every batch pays full price for the digest.
        (code_chars * batches + OVERHEAD_CHARS_PER_PASS * passes) as f64 / CHARS_PER_TOKEN
    } else {
        // Batch 0 per chunk: full digest price + cache-write surcharge.
        // Batches 1..N per chunk: digest at cache-read rate (0.1×).
        let digest_tokens_per_chunk = code_chars as f64 / chunks as f64 / CHARS_PER_TOKEN;
        let write_cost = digest_tokens_per_chunk * CACHE_WRITE_MULT * chunks as f64;
        let read_cost = digest_tokens_per_chunk
            * CACHE_READ_MULT
            * (batches.saturating_sub(1)) as f64
            * chunks as f64;
        // Overhead (never cached) is full price for every pass.
        let overhead_cost = OVERHEAD_CHARS_PER_PASS as f64 / CHARS_PER_TOKEN * passes as f64;
        write_cost + read_cost + overhead_cost
    };
    let scan_out =
        OUT_TOKENS_PER_PASS * passes as f64 + OUTPUT_PER_CODE_TOKEN * code_tokens * batches as f64;

    // ── Calibration: ONE pass over all findings, priced at the CALIBRATION model. It
    // re-reads roughly the scan's output (the findings) and RE-EMITS each finding with a
    // corrected/verified body. So its output rides with the full findings volume, ~1× the
    // scan's output. Thorough mode (#51) runs ~3× for multi-vote consensus.
    let cal_passes = if thorough { 3.0 } else { 1.0 };
    let cal_in = scan_out * cal_passes;
    let cal_out = scan_out * cal_passes;

    // ── Deep tier: three EXTRA whole-repo prose passes at the AUDIT model. Each reads the
    // full code (no per-rule batching, no caching discount — distinct prompts per lens) and
    // emits a long prose report. This is the dominant cost when enabled, which is why deep is
    // surfaced as the priciest option in the readout. Batch discount does NOT apply (these run
    // real-time as part of the deep lens flow, not in the scan batch).
    let (deep_in, deep_out) = if deep {
        let full_code_tokens = code_chars as f64 / CHARS_PER_TOKEN;
        let din = full_code_tokens * DEEP_PASSES;
        let dout = DEEP_OUT_TOKENS_PER_PASS * DEEP_PASSES;
        (din, dout)
    } else {
        (0.0, 0.0)
    };

    // Incremental scope (only changed files actually billed) would lower the scan portion, but
    // the client has no changed-file token breakdown today (see fn doc + followup), so we keep
    // the full-scan price and let the readout flag incremental as an over-estimate. Bind the
    // flag so its role is explicit even though the number is unchanged here.
    let _ = incremental;

    let dollars = ((scan_in * eff_audit_in + scan_out * eff_audit_out)
        + (cal_in * eff_calib_in + cal_out * eff_calib_out)
        + (deep_in * audit_in + deep_out * audit_out))
        / 1_000_000.0
        * FUDGE;
    let total_tokens =
        ((scan_in + scan_out + cal_in + cal_out + deep_in + deep_out) * FUDGE) as u64;
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
    /// Batch mode (#61): the Anthropic Message Batch id currently being processed
    /// (`msgbatch_01...`). Surfaced in the job-progress status line so the user can
    /// look it up in the Anthropic console. `None` for parallel/sequential mode jobs.
    #[serde(default)]
    batch_id: Option<String>,
}

/// Mode 3: START an async audit job, returning its id (the request returns immediately).
/// `deep` forwards the opt-in deep compliance & security tier (#55); the server
/// runs the three lenses after the standard audit completes and attaches the result
/// to the final job report's `deep` field.
#[allow(clippy::too_many_arguments)]
async fn audit_job_start(
    repos: &[String],
    rules: &[SelectedAuditRule],
    model: &str,
    calibration_model: &str,
    exec_mode: &str,
    thorough: bool,
    incremental: bool,
    deep: bool,
) -> Option<String> {
    let rule_json = audit_rules_json(rules);
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/api/onboard/audit/start", crate::BFF_URL))
        .json(&serde_json::json!({
            "repos": repos,
            "rules": rule_json,
            "model": model,
            "calibration_model": calibration_model,
            "mode": exec_mode,
            "thorough": thorough,
            "incremental": incremental,
            "deep": deep,
        }))
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
    reqwest::get(format!(
        "{}/api/onboard/audit/job/{}",
        crate::BFF_URL,
        job_id
    ))
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
async fn apply_rules(
    rules: &[ArmRuleReq],
    custom: &[CustomRuleView],
    findings: &[FindingView],
) -> Option<(bool, String, Vec<ArmResultView>)> {
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/api/onboard/apply", crate::BFF_URL))
        .json(&serde_json::json!({ "rules": rules, "custom": custom, "findings": findings }))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let ok = v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false);
    let message = v
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or_default()
        .to_string();
    let results = v
        .get("results")
        .cloned()
        .and_then(|r| serde_json::from_value(r).ok())
        .unwrap_or_default();
    Some((ok, message, results))
}

/// One repo's set of governance files that ALREADY EXIST and would be OVERWRITTEN by Apply.
#[derive(Clone, serde::Deserialize)]
struct ApplyPreflightRepo {
    repo: String,
    #[serde(default)]
    existing_files: Vec<String>,
}

/// Preflight for Apply: ask the server which governance files Camerata is about to write
/// ALREADY EXIST in each repo's local clone (and would be clobbered). Returns the per-repo
/// list (empty when Apply is safe). `None` only on a transport/parse failure — the caller
/// treats that as "could not check" and falls through to the normal apply path rather than
/// blocking the architect on a preflight outage.
async fn preflight_apply(
    rules: &[ArmRuleReq],
    custom: &[CustomRuleView],
    findings: &[FindingView],
) -> Option<Vec<ApplyPreflightRepo>> {
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/api/onboard/apply/preflight", crate::BFF_URL))
        .json(&serde_json::json!({ "rules": rules, "custom": custom, "findings": findings }))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    if !v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) {
        return None;
    }
    v.get("repos")
        .cloned()
        .and_then(|r| serde_json::from_value(r).ok())
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
async fn create_ticket(
    repo: &str,
    findings: &[FindingView],
    title: Option<&str>,
) -> Option<String> {
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

/// The deterministic security-floor rule ids — the gate's content arms. A finding carrying one
/// of these is ENFORCED: pure regex/logic, repeatable (same code → same id, same line), gateable,
/// and its id is canonical. EVERYTHING else is ADVISORY — whether an AI-invented `AI-*` id OR the
/// AI judging code against a corpus rule (e.g. `RUST-DIOXUS-11`). Advisory findings are
/// model-inferred and their id / severity / very presence can drift run-to-run, so the UI must not
/// present them as fixed, enforced rules. (Keep in sync with the gate's content arms server-side.)
const FLOOR_RULE_IDS: &[&str] = &[
    "SEC-NO-HARDCODED-SECRETS-1",
    "SEC-NO-RAW-SQL-CONCAT-1",
    "ARCH-NO-SECRETS-IN-URL-1",
];

/// True when a finding is from the deterministic floor (enforced/stable) vs the AI audit (advisory).
fn is_enforced_floor(rule_id: &str) -> bool {
    FLOOR_RULE_IDS.contains(&rule_id)
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
        // AUTHORITY, not just provenance: a DETERMINISTIC-FLOOR hit is ENFORCED (regex/logic,
        // repeatable, gateable, stable id); EVERY other finding is ADVISORY (model-inferred,
        // review-only, id/severity may drift run-to-run, never auto-blocks). This is the
        // enforcement-vs-convention split rendered as a column. NOTE: advisory covers BOTH
        // `AI-*` invented ids AND the AI judging code against a corpus rule (e.g. RUST-DIOXUS-11)
        // — the old `AI-` prefix check mislabeled the latter as enforced. Keyed on the floor set.
        // PREVIEW is a THIRD authority tier between enforced and advisory: a scan-time
        // deterministic-tool finding (stable rule-id, no model in the trust path) that is NOT
        // yet wired into the repo's gate. It must read DISTINCTLY from an enforced floor hit
        // ("preview — not enforced until wired") AND from an AI-advisory finding (deterministic,
        // not model-inferred). The CI story still has to wire it for the gate to block on it.
        ColumnDef::new(ColumnId("authority"), "Authority", |f: &FindingView| {
            CellValue::Text(if f.preview {
                "preview".to_string()
            } else if is_enforced_floor(&f.rule_id) {
                "enforced".to_string()
            } else {
                "advisory".to_string()
            })
        })
        .sortable()
        .filter(FilterKind::MultiSelect {
            options: vec![
                "enforced".to_string(),
                "preview".to_string(),
                "advisory".to_string(),
            ],
        })
        .render_kind(RenderKind::Badge(
            BadgeVariantMap::new()
                // chorale 0.2.3 added blue/purple to the palette, so the authorities
                // read as distinct colors (no more gray fallback collision).
                .with("enforced", BadgeVariant::new("Rule · enforced", "green"))
                .with("preview", BadgeVariant::new("Preview · not enforced until wired", "purple"))
                .with("advisory", BadgeVariant::new("AI · advisory", "blue")),
        ))
        .initial_width(220.0),
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
        // The cell VALUE is just yes/no ("does this need review?") so the filter is a simple
        // two-option toggle, not a free-text box over every distinct reason. The visible chip +
        // reason are drawn by the row renderer (which reads f.detail), so the reason still shows.
        ColumnDef::new(
            ColumnId("needs_review"),
            "Needs review",
            |f: &FindingView| {
                CellValue::Text(if split_needs_review(&f.detail).1.is_some() {
                    "yes".to_string()
                } else {
                    "no".to_string()
                })
            },
        )
        .sortable()
        .filter(FilterKind::MultiSelect {
            options: vec!["yes".to_string(), "no".to_string()],
        })
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
    // The enforcement LANE (coarse automated-vs-human axis): mechanical/architectural
    // rules are CI-enforced ("Automated (CI)"), prose/structured are human-reviewed.
    // Labels deliberately avoid reusing "Mechanical" so this column doesn't collide
    // with the four-modality "Type" column.
    let kind = BadgeVariantMap::new()
        .with("mechanical", BadgeVariant::new("Automated (CI)", "green"))
        .with("review", BadgeVariant::new("Human review", "yellow"));
    let scope = BadgeVariantMap::new()
        .with("repo-local", BadgeVariant::new("Repo-local", "green"))
        .with("cross-repo", BadgeVariant::new("Cross-repo", "yellow"))
        .with("process", BadgeVariant::new("Process", "gray"));
    // Verification badge map: four rungs of the provenance ladder.
    // `verified`      -> green  (human-confirmed, most trusted)
    // `grounded`      -> blue   (cited source, machine-grounded)
    // `needs_recheck` -> yellow (was verified; source drifted)
    // `draft`         -> gray   (AI-generated, not yet grounded; default)
    let verif_badges = BadgeVariantMap::new()
        .with("verified",      BadgeVariant::new("\u{2713} Verified",  "green"))
        .with("grounded",      BadgeVariant::new("\u{29bf} Grounded", "blue"))
        .with("needs_recheck", BadgeVariant::new("Needs re-check",    "yellow"))
        .with("draft",         BadgeVariant::new("Draft",             "gray"));
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
        // Auto-recommend status: `recommended` = grounded/verified rules pre-checked for
        // this stack; `available` = draft/needs_recheck rules listed but unchecked.
        // The badge makes the provenance tier immediately visible alongside the rule row.
        ColumnDef::new(
            ColumnId("suggested"),
            "Recommendation",
            |r: &ProposedRuleView| {
                CellValue::Text(if r.effective_auto_recommended() {
                    "recommended".to_string()
                } else {
                    "available".to_string()
                })
            },
        )
        .sortable()
        .render_kind(RenderKind::Badge(
            BadgeVariantMap::new()
                .with("recommended", BadgeVariant::new("\u{2713} Recommended", "green"))
                .with("available", BadgeVariant::new("Available", "gray")),
        ))
        .initial_width(150.0),
        ColumnDef::new(ColumnId("id"), "Rule", |r: &ProposedRuleView| {
            CellValue::Text(r.id.clone())
        })
        .sortable()
        .filter(FilterKind::Text)
        .initial_width(280.0),
        // Type (enforcement modality): prose / structured / mechanical / architectural —
        // WHAT kind of conformance check the rule needs. The RowCellRenderer below adds a
        // `title` tooltip with the modality definition; see `enforcement_tooltip()`.
        ColumnDef::new(ColumnId("enf_type"), "Type", |r: &ProposedRuleView| {
            CellValue::Text(r.enforcement.clone())
        })
        .sortable()
        .render_kind(RenderKind::Badge(enforcement_badges()))
        .initial_width(130.0),
        // Provenance / verification state — displayed next to the rule name so the
        // architect immediately sees whether a rule is human-verified, grounded in a
        // cited source, a draft (AI-generated, not yet grounded), or flagged for
        // re-check. See `verif_badge()` and `docs/decisions/2026-06-20_ui_verification_badges.md`.
        ColumnDef::new(ColumnId("verif"), "Provenance", |r: &ProposedRuleView| {
            CellValue::Text(r.verification.clone())
        })
        .sortable()
        .render_kind(RenderKind::Badge(verif_badges))
        .initial_width(140.0),
        ColumnDef::new(ColumnId("scope"), "Scope", |r: &ProposedRuleView| {
            CellValue::Text(r.scope.clone())
        })
        .sortable()
        .render_kind(RenderKind::Badge(scope))
        .initial_width(130.0),
        // (The "Applies to" column was removed: the repo this ruleset is for is already
        // chosen in the "Repo ruleset" selector above the table, so per-row repo was redundant.)
        ColumnDef::new(
            ColumnId("placement"),
            "Where enforced",
            |r: &ProposedRuleView| CellValue::Text(r.placement.clone()),
        )
        .initial_width(300.0),
        ColumnDef::new(ColumnId("kind"), "Enforced by", |r: &ProposedRuleView| {
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
    // Ask-a-finding (#54): the app-level lifted signal, provided by CockpitApp via context.
    // When the architect selects a finding and presses "Ask", we build a FindingContext
    // and write it here; ChatBubble in main.rs reads from the same signal.
    let mut ask_finding =
        use_context::<Signal<Option<crate::chat::FindingContext>>>();
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
        TableState::new(
            rows.clone(),
            finding_columns(filter_repos.clone(), in_techdebt),
        )
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
    // Ask-a-finding (#54): one more id_map clone for the "Ask" button.
    let id_map_ask = id_map.clone();
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
            std::sync::Arc::new(
                move |f: &FindingView, _val: &CellValue| match split_needs_review(&f.detail).1 {
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
                },
            ) as RowCellRenderer<FindingView>,
        );
        // Tech-debt bucket flag: reads the live disposition snapshot for this finding and
        // renders a "Later" / "Now" badge. Present only in the tech-debt view.
        if in_techdebt {
            let snap = bucket_snapshot.clone();
            m.insert(
                ColumnId("bucket"),
                std::sync::Arc::new(move |f: &FindingView, _val: &CellValue| {
                    let bucket = snap
                        .get(&finding_key(f))
                        .map(|d| d.bucket)
                        .unwrap_or(TechDebtBucket::Later);
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
            // Ask-a-finding (#54): builds a FindingContext from the FIRST selected finding
            // and writes it to the app-level `ask_finding` signal. The ChatBubble (mounted
            // in App above this subtree) reads the signal and auto-opens in Project mode
            // focused on that finding.
            button {
                class: "ask-finding-btn",
                title: "Open the research chat focused on this finding (Project mode)",
                onclick: move |_| {
                    let sel = handle.selected_ids();
                    // Use the FIRST selected row; selecting multiple and asking about all
                    // is deferred — one coherent conversation per finding is the better UX.
                    let Some(first_id) = sel.into_iter().next() else { return; };
                    let Some(f) = id_map_ask.get(&first_id).cloned() else { return; };
                    // Map FindingView -> FindingContext (pub in chat.rs).
                    // FindingView fields: rule_id, path, line (usize), severity,
                    // snippet, detail, repo.
                    ask_finding.set(Some(crate::chat::FindingContext {
                        rule_id: f.rule_id.clone(),
                        severity: f.severity.clone(),
                        path: f.path.clone(),
                        line: f.line,
                        snippet: f.snippet.clone(),
                        detail: f.detail.clone(),
                        repo: f.repo.clone(),
                    }));
                },
                "Ask AI about this finding"
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

/// Composite key for the per-repo "chosen alternative" map. Option choices are INDEPENDENT
/// per repo: picking an alternative for a rule while viewing one repo must not change another
/// repo's choice for the same rule. The NUL byte separates the parts (it can't appear in an
/// `owner/repo` or a rule id), and the key stays a plain string so the map still serializes to
/// JSON for the auto-saved draft.
fn chosen_key(repo: &str, rule_id: &str) -> String {
    format!("{repo}\u{0}{rule_id}")
}

/// Sentinel key under which the SINGLE-repo scan stores its rule selection in the lifted
/// `repo -> selected rule ids` map. A real `owner/repo` can never contain a NUL byte, so this
/// can't collide with a multi-repo entry. Using a stable map key (instead of skipping the map
/// entirely when `view_repo` is empty) is what lets a single-repo selection survive a remount:
/// the map is what's serialized into the auto-saved onboarding draft, so without an entry here
/// the architect's manual (non-recommended) picks were dropped on navigate-away-and-back and
/// the table re-seeded to recommended-only. See `docs/decisions/2026-06-20_ui_bugfixes.md`.
const SINGLE_REPO_SELECTION_KEY: &str = "\u{0}__single_repo__";

/// The map key a `ProposedRulesTable` reads/writes its selection under. Multi-repo tables key
/// by their `view_repo`; the single-repo case (`view_repo` empty) uses the sentinel so its
/// picks persist through the draft like every other repo's do. Pure + unit-tested.
fn selection_key(view_repo: &str) -> String {
    if view_repo.is_empty() {
        SINGLE_REPO_SELECTION_KEY.to_string()
    } else {
        view_repo.to_string()
    }
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
    // The repo whose table is in view — option choices (`chosen`) are keyed per repo, so the
    // highlight + needs-a-choice check read THIS repo's picks.
    let viewed_repo = use_context::<Signal<String>>();
    let placement = use_context::<Signal<std::collections::HashMap<String, Vec<String>>>>();
    // Full cross-repo rule lookup (by rule id) for building audit/arm requests that span
    // every repo's saved selection, not just the one this table is currently showing.
    let all_by_id: std::collections::HashMap<String, ProposedRuleView> = if all_rules.is_empty() {
        rules.iter().map(|r| (r.id.clone(), r.clone())).collect()
    } else {
        all_rules
            .iter()
            .map(|r| (r.id.clone(), r.clone()))
            .collect()
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
        // Look up THIS table's saved selection under its map key (the sentinel for the
        // single-repo case, the repo name otherwise). Both paths persist into the same lifted
        // map, so both survive a remount via the auto-saved draft. Manual non-recommended picks
        // are restored exactly, not re-derived from `recommended`.
        let saved: Option<std::collections::HashSet<String>> = repo_selection
            .peek()
            .get(&selection_key(&view_repo))
            .map(|ids| ids.iter().cloned().collect());
        match saved {
            Some(ids) => rows
                .iter()
                .filter(|(_, p)| ids.contains(&p.id))
                .map(|(r, _)| *r)
                .collect(),
            // First view: pre-select all auto-recommended rules — grounded/verified rules
            // whose provenance has been reviewed against a real source. Draft + needs_recheck
            // rules appear listed but unchecked so the architect must opt them in explicitly.
            // Rules that are selected but still need an alternative highlighted yellow and
            // gate audit/arm until the architect picks an alternative (or deselects them).
            None => rows
                .iter()
                .filter(|(_, p)| p.effective_auto_recommended())
                .map(|(r, _)| *r)
                .collect(),
        }
    };
    let mut domain_rows: std::collections::BTreeMap<String, Vec<RowId>> = Default::default();
    for (rid, p) in &rows {
        let d = if p.domain.is_empty() {
            "general".to_string()
        } else {
            p.domain.clone()
        };
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
        // ALWAYS write this table's live selection into the lifted map (under its key), then
        // report the cross-repo total for the cost estimate. The single-repo case writes under
        // the sentinel key so its picks — recommended AND manual — persist into the auto-saved
        // draft and are restored on remount, instead of resetting to recommended-only.
        let mut map = repo_selection_wb.peek().clone();
        map.insert(selection_key(&view_repo_wb), live_ids);
        let total: usize = map.values().map(|v| v.len()).sum();
        repo_selection_wb.set(map);
        selected_count.set(total);
    });
    let mut arming = use_signal(|| false);
    let mut opening_pr = use_signal(|| false);
    // Apply-overwrite confirm gate: when the pre-apply preflight finds hand-written governance
    // files that Apply would clobber, we stash the conflict list + the resolved apply payload
    // here and show a confirm modal. The architect must explicitly acknowledge before we
    // overwrite. `None` = no pending confirm (Apply either ran or hasn't been requested).
    #[allow(clippy::type_complexity)]
    let mut pending_apply_overwrite: Signal<
        Option<(
            Vec<ApplyPreflightRepo>,
            Vec<ArmRuleReq>,
            Vec<CustomRuleView>,
            Vec<FindingView>,
        )>,
    > = use_signal(|| None);
    // Repos that the Apply step wrote a governance branch into (local + pushed). The "Open
    // governance PR" button targets exactly these, and is disabled until something is applied.
    let mut applied_repos = use_signal(Vec::<String>::new);
    // Fire the actual Apply with a resolved payload and surface the per-repo results. Shared by
    // the Apply button's "no conflicts → go straight through" path and the overwrite-confirm
    // modal's "Overwrite & apply" button, so both run identical result handling.
    let run_apply = use_callback(
        move |(arm_reqs, custom_reqs, findings): (
            Vec<ArmRuleReq>,
            Vec<CustomRuleView>,
            Vec<FindingView>,
        )| {
            arming.set(true);
            spawn(async move {
                match apply_rules(&arm_reqs, &custom_reqs, &findings).await {
                    Some((ok, message, results)) => {
                        if !ok && results.is_empty() {
                            crate::toast::push_toast(
                                toasts,
                                crate::toast::ToastKind::Error,
                                if message.is_empty() {
                                    "Apply failed.".to_string()
                                } else {
                                    message
                                },
                            );
                        } else {
                            let mut done = Vec::new();
                            for r in results {
                                if r.ok {
                                    let branch = r.branch.unwrap_or_default();
                                    let path = r.path.unwrap_or_default();
                                    crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("{}: applied to branch '{branch}' (local + pushed, no PR) — {path}", r.repo));
                                    done.push(r.repo);
                                } else {
                                    crate::toast::push_toast(
                                        toasts,
                                        crate::toast::ToastKind::Error,
                                        format!("{}: {}", r.repo, r.message.unwrap_or_default()),
                                    );
                                }
                            }
                            if !done.is_empty() {
                                applied_repos.set(done);
                            }
                        }
                    }
                    None => crate::toast::push_toast(
                        toasts,
                        crate::toast::ToastKind::Error,
                        "Apply failed — set a workspace folder + connect GitHub (Contents write).",
                    ),
                }
                arming.set(false);
            });
        },
    );
    let arm_findings = findings;
    // Export the FULL cross-repo rule set (every repo's proposed rules), not just the
    // currently-viewed repo's subset, so the CSV stays lossless for a multi-repo scan.
    let csv_rules = if all_rules.is_empty() {
        rules.clone()
    } else {
        all_rules.clone()
    };

    // Dedicated clones for the audit closure; the originals are consumed by the arm closure.
    let all_by_id_audit = all_by_id.clone();
    let view_repo_audit = view_repo.clone();

    // Rules whose alternative is still UNRESOLVED — they have options but no chosen choice
    // AND no usable default directive, so the architect must pick one before the rule can be
    // enforced. Recomputed each render (reads `chosen`), so picking an alternative clears it.
    let needs_choice: std::collections::HashSet<String> = {
        let chosen_map = chosen.read();
        let cur_repo = viewed_repo();
        id_map
            .values()
            .filter(|r| {
                if r.options.is_empty() {
                    return false;
                }
                let oid = chosen_map
                    .get(&chosen_key(&cur_repo, &r.id))
                    .cloned()
                    .or_else(|| r.default_option.clone());
                oid.and_then(|o| {
                    r.options
                        .iter()
                        .find(|x| x.id == o)
                        .map(|x| x.directive.clone())
                })
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
            map.insert(
                view_repo.clone(),
                selected_rule_ids.iter().cloned().collect(),
            );
            map.values().flatten().cloned().collect()
        };
        selected
            .into_iter()
            .filter(|id| needs_choice.contains(id))
            .collect()
    };
    let has_unresolved = !unresolved_selected.is_empty();
    let unresolved_hint = if has_unresolved {
        format!(
            "Choose an alternative first for: {}",
            unresolved_selected.join(", ")
        )
    } else {
        String::new()
    };

    // Row-cell renderer for the Type (enforcement modality) column: a native `title`
    // tooltip with the modality definition. Mirrors ProjectRulesTable / AllRulesTable.
    let rule_type_renderers = {
        let mut m: std::collections::HashMap<ColumnId, RowCellRenderer<ProposedRuleView>> =
            std::collections::HashMap::new();
        m.insert(
            ColumnId("enf_type"),
            std::sync::Arc::new(move |r: &ProposedRuleView, _val: &CellValue| {
                let enf = r.enforcement.as_str();
                let tip = enforcement_tooltip(enf);
                let label = if enf.is_empty() { "\u{2014}" } else { enf };
                rsx! { span { title: "{tip}", "{label}" } }
            }) as RowCellRenderer<ProposedRuleView>,
        );
        RowCellRenderers::new(m)
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
            row_cell_renderers: rule_type_renderers,
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
                    // Resolve a rule's adopted directive for a SPECIFIC repo (choices are per-repo).
                    let resolve_directive = |repo: &str, r: &ProposedRuleView| -> String {
                        if r.options.is_empty() {
                            r.title.clone()
                        } else {
                            let oid = chosen.read().get(&chosen_key(repo, &r.id)).cloned().or_else(|| r.default_option.clone());
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
                            let repo = r.repos.first().cloned().unwrap_or_default();
                            SelectedAuditRule { id: r.id.clone(), directive: resolve_directive(&repo, r), repos: r.repos.clone() }
                        }).collect()
                    } else {
                        // Per-repo: each (repo, selected rule) becomes one entry scoped to that
                        // repo, with THAT repo's chosen directive. The backend audits each repo
                        // against only the rules bound to it.
                        let mut map = repo_selection.peek().clone();
                        map.insert(view_repo_audit.clone(), live_ids);
                        let mut out = Vec::new();
                        for (repo, ids) in &map {
                            for id in ids {
                                if let Some(r) = all_by_id_audit.get(id) {
                                    out.push(SelectedAuditRule { id: r.id.clone(), directive: resolve_directive(repo, r), repos: vec![repo.clone()] });
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
                    let mut custom_reqs: Vec<CustomRuleView> = Vec::new();
                    let mut unresolved = Vec::new();
                    for (id, selected_repos) in &rule_repos {
                        let Some(r) = all_by_id.get(id) else { continue; };
                        // Custom rules route through the project's ruleset.custom (rendered as
                        // CUSTOM-<name> blocks), NOT the arm-request path — a base RuleSelection
                        // with a CUSTOM- id has no corpus rule to resolve on reconcile.
                        if is_custom_rule_id(id) {
                            let is_global = r.domain == "Custom Global";
                            let body = r.options.first().map(|o| o.directive.clone()).unwrap_or_default();
                            let domain = if is_global {
                                "*".to_string()
                            } else {
                                selected_repos.first().cloned().unwrap_or_default()
                            };
                            if !custom_reqs.iter().any(|c| c.name == r.title) {
                                custom_reqs.push(CustomRuleView { name: r.title.clone(), body, domain });
                            }
                            continue;
                        }
                        // Architect's explicit placement override wins; otherwise arm to the
                        // repos that selected this rule. A rule routed to zero repos is skipped.
                        let target_repos = placement.read().get(&r.id).cloned().unwrap_or_else(|| selected_repos.clone());
                        // One arm request PER REPO, each carrying THAT repo's chosen directive —
                        // option choices are per-repo, so a rule armed to two repos can adopt a
                        // different alternative in each.
                        for repo in &target_repos {
                            let (directive, option) = if r.options.is_empty() {
                                (r.title.clone(), None)
                            } else {
                                let oid = chosen.read().get(&chosen_key(repo, &r.id)).cloned().or_else(|| r.default_option.clone());
                                match oid.clone().and_then(|o| r.options.iter().find(|x| x.id == o).map(|x| x.directive.clone())) {
                                    Some(d) if !d.is_empty() => (d, oid),
                                    _ => { unresolved.push(format!("{} ({repo})", r.id)); continue; }
                                }
                            };
                            arm_reqs.push(ArmRuleReq {
                                id: r.id.clone(),
                                title: r.title.clone(),
                                directive,
                                option,
                                enforcement: r.enforcement.clone(),
                                scope: r.scope.clone(),
                                repos: vec![repo.clone()],
                            });
                        }
                    }
                    if !unresolved.is_empty() {
                        unresolved.sort();
                        unresolved.dedup();
                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Warning, format!("Choose an alternative first for: {}", unresolved.join(", ")));
                        return;
                    }
                    // Preflight FIRST: detect any hand-written governance files Apply would
                    // overwrite, and require explicit acknowledgement before clobbering them. If
                    // nothing would be overwritten (or the preflight can't run), apply directly —
                    // no nagging on the safe path.
                    let findings = arm_findings.clone();
                    arming.set(true);
                    spawn(async move {
                        let conflicts = preflight_apply(&arm_reqs, &custom_reqs, &findings).await;
                        arming.set(false);
                        match conflicts {
                            Some(repos) if !repos.is_empty() => {
                                // Stash the conflicts + the resolved payload; the modal confirms.
                                pending_apply_overwrite.set(Some((repos, arm_reqs, custom_reqs, findings)));
                            }
                            // No conflicts, or preflight unavailable: proceed straight to Apply.
                            _ => run_apply.call((arm_reqs, custom_reqs, findings)),
                        }
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

        // Overwrite-confirm modal — shown when the pre-apply preflight finds hand-written
        // governance files that Apply would clobber. Lists EXACTLY which files in which repos
        // will be overwritten; the architect must explicitly confirm before we proceed.
        if let Some((conflicts, _, _, _)) = pending_apply_overwrite() {
            div { class: "rule-modal-overlay", onclick: move |_| pending_apply_overwrite.set(None),
                div { class: "rule-modal", onclick: move |e| e.stop_propagation(),
                    div { class: "rule-modal-head",
                        span { class: "rule-modal-id", "Overwrite existing files?" }
                        button {
                            class: "rule-modal-close",
                            onclick: move |_| pending_apply_overwrite.set(None),
                            "\u{2715}"
                        }
                    }
                    p { class: "rule-modal-detail",
                        "Some of these repos already have governance files that Apply will "
                        strong { "overwrite" }
                        " on the "
                        code { "camerata/onboard-governance" }
                        " branch. Review what will be replaced, then confirm to proceed."
                    }
                    div { class: "apply-overwrite-list",
                        for repo in conflicts.iter() {
                            div { class: "apply-overwrite-repo", key: "{repo.repo}",
                                span { class: "apply-overwrite-repo-name", "{repo.repo}" }
                                ul { class: "apply-overwrite-files",
                                    for f in repo.existing_files.iter() {
                                        li { key: "{f}", code { "{f}" } }
                                    }
                                }
                            }
                        }
                    }
                    div { class: "onboard-leave-actions",
                        button {
                            class: "btn-edit-sm",
                            onclick: move |_| pending_apply_overwrite.set(None),
                            "Cancel"
                        }
                        button {
                            class: "btn-edit-sm pg-btn-danger",
                            onclick: move |_| {
                                // Take the stashed payload and run the actual Apply.
                                if let Some((_, arm_reqs, custom_reqs, findings)) = pending_apply_overwrite.take() {
                                    run_apply.call((arm_reqs, custom_reqs, findings));
                                }
                            },
                            "Overwrite & apply"
                        }
                    }
                }
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
    // The repo whose table is in view — option choices are recorded per repo.
    let viewed_repo = use_context::<Signal<String>>();
    let Some(r) = detail_rule() else {
        return rsx! {};
    };
    let (vbadge_label, vbadge_cls) = verif_badge(&r.verification);
    let vsources_tip = verif_sources_tooltip(&r.sources);
    rsx! {
        div { class: "rule-modal-overlay", onclick: move |_| detail_rule.set(None),
            div { class: "rule-modal", onclick: move |e| e.stop_propagation(),
                div { class: "rule-modal-head",
                    span { class: "rule-modal-id", "{r.id}" }
                    button { class: "rule-modal-close", onclick: move |_| detail_rule.set(None), "\u{2715}" }
                }
                div { class: "rule-modal-title-row",
                    p { class: "rule-modal-title", "{r.title}" }
                    span {
                        class: "verif-badge verif-badge-{vbadge_cls}",
                        title: "{vsources_tip}",
                        "{vbadge_label}"
                    }
                }
                div { class: "rule-modal-meta",
                    span { class: "rule-modal-tag", "domain · {r.domain}" }
                    span { class: "rule-modal-tag", "scope · {r.scope}" }
                    span { class: "rule-modal-tag", "kind · {r.kind}" }
                    if !r.enforcement.is_empty() {
                        span {
                            class: "rule-modal-tag",
                            title: "{enforcement_tooltip(&r.enforcement)}",
                            "enforcement · {r.enforcement}"
                        }
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
                        // "Why the default" only makes sense when the rule HAS a default; a rule
                        // with no default has a rationale for the decision itself, not for a
                        // default that doesn't exist. Label it plainly "Why" in that case.
                        span { class: "rule-modal-label",
                            if r.default_option.is_some() { "Why the default" } else { "Why" }
                        }
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
                                    let oid = o.id.clone();
                                    let key = chosen_key(&viewed_repo(), &r.id);
                                    let cur = chosen.read().get(&key).cloned().or_else(|| r.default_option.clone());
                                    let picked = cur.as_deref() == Some(o.id.as_str());
                                    let is_default = r.default_option.as_deref() == Some(o.id.as_str());
                                    let cls = if picked { "rule-opt on" } else { "rule-opt" };
                                    let mut chosen = chosen;
                                    rsx! {
                                        button {
                                            key: "{o.id}",
                                            class: "{cls}",
                                            onclick: move |_| { chosen.write().insert(key.clone(), oid.clone()); },
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

/// Create / edit / delete the architect's custom rules during onboarding (#49). Custom rules
/// also show in the proposed-rules table (Custom / Custom Global domain groups) and are
/// selectable there; this panel manages their lifecycle. A "Custom rule" is scoped to the
/// viewed repo; a "Global custom rule" applies to every repo. Reads/writes the shared
/// `custom_rules` + `repo_selection` contexts so a new rule appears in the table auto-selected.
#[component]
fn CustomRulesPanel(all_repos: Vec<String>) -> Element {
    let mut custom_rules = use_context::<Signal<Vec<CustomRuleView>>>();
    let mut repo_selection =
        use_context::<Signal<std::collections::HashMap<String, Vec<String>>>>();
    let viewed_repo = use_context::<Signal<String>>();
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    // Open editor: (original_name | None for a new rule, name, body, is_global).
    let mut editor = use_signal(|| Option::<(Option<String>, String, String, bool)>::None);

    rsx! {
        div { class: "custom-rules",
            div { class: "custom-rules-head",
                span { class: "custom-rules-title", "Custom rules" }
                button {
                    class: "btn-edit-sm",
                    onclick: move |_| editor.set(Some((None, String::new(), String::new(), false))),
                    "+ Custom rule (this repo)"
                }
                button {
                    class: "btn-edit-sm",
                    onclick: move |_| editor.set(Some((None, String::new(), String::new(), true))),
                    "+ Global custom rule"
                }
            }
            p { class: "custom-rules-sub",
                "Free-text rules you author. The text IS the directive (you own its wording); name the rule so it reads in the table. They appear under the Custom / Custom Global groups, are selectable like any rule, and are written into AGENTS.md on apply."
            }
            {
                let rules = custom_rules.read().clone();
                rsx! {
                    if rules.is_empty() {
                        p { class: "custom-rules-empty", "No custom rules yet." }
                    } else {
                        div { class: "custom-rules-list",
                            for c in rules {
                                {
                                    let c_edit = c.clone();
                                    let c_del = c.clone();
                                    let scope_label = if c.is_global() { "global".to_string() } else { c.domain.clone() };
                                    rsx! {
                                        div { class: "custom-rule-row", key: "{c.name}",
                                            span { class: "custom-rule-name", "{c.name}" }
                                            span { class: "custom-rule-scope", "{scope_label}" }
                                            button {
                                                class: "btn-edit-sm",
                                                onclick: move |_| editor.set(Some((Some(c_edit.name.clone()), c_edit.name.clone(), c_edit.body.clone(), c_edit.is_global()))),
                                                "Edit"
                                            }
                                            button {
                                                class: "btn-delete-sm",
                                                onclick: move |_| {
                                                    let id = c_del.rule_id();
                                                    custom_rules.write().retain(|x| x.name != c_del.name);
                                                    let mut m = repo_selection.peek().clone();
                                                    for v in m.values_mut() { v.retain(|x| x != &id); }
                                                    repo_selection.set(m);
                                                },
                                                "Delete"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if let Some((orig, name, body, global)) = editor() {
                {
                    let all_repos = all_repos.clone();
                    let cur_repo = viewed_repo();
                    let scope_text = if global { "applies to all repos".to_string() } else { format!("applies to {cur_repo}") };
                    rsx! {
                        div { class: "custom-rule-editor",
                            input {
                                class: "addressee-input",
                                placeholder: "rule name",
                                value: "{name}",
                                oninput: move |e| { if let Some(ed) = editor.write().as_mut() { ed.1 = e.value(); } },
                            }
                            textarea {
                                class: "routine-intent-input",
                                rows: "3",
                                placeholder: "the directive the agent should follow…",
                                value: "{body}",
                                oninput: move |e| { if let Some(ed) = editor.write().as_mut() { ed.2 = e.value(); } },
                            }
                            div { class: "custom-rule-editor-actions",
                                span { class: "custom-rule-scope", "{scope_text}" }
                                button { class: "btn-secondary", onclick: move |_| editor.set(None), "Cancel" }
                                button {
                                    class: "btn-run",
                                    onclick: move |_| {
                                        let name = name.trim().to_string();
                                        if name.is_empty() {
                                            crate::toast::push_toast(toasts, crate::toast::ToastKind::Warning, "Name the custom rule.");
                                            return;
                                        }
                                        let domain = if global { "*".to_string() } else { cur_repo.clone() };
                                        let rule = CustomRuleView { name: name.clone(), body: body.clone(), domain };
                                        let id = rule.rule_id();
                                        {
                                            let mut list = custom_rules.write();
                                            match &orig {
                                                Some(old) => {
                                                    if let Some(slot) = list.iter_mut().find(|x| &x.name == old) { *slot = rule; }
                                                    else { list.push(rule); }
                                                }
                                                None => list.push(rule),
                                            }
                                        }
                                        // Auto-select the rule for its repo(s) so it's included by default.
                                        // The single-repo case (one repo) stores its selection under the
                                        // sentinel key — same key the single-repo ProposedRulesTable seeds
                                        // from — so the new rule is actually pre-checked on the table's
                                        // remount (and survives the draft) instead of being written under a
                                        // repo name the table never reads.
                                        let multi_repo = all_repos.len() > 1;
                                        let repos: Vec<String> = if !multi_repo {
                                            vec![SINGLE_REPO_SELECTION_KEY.to_string()]
                                        } else if global {
                                            all_repos.clone()
                                        } else {
                                            vec![cur_repo.clone()]
                                        };
                                        let mut m = repo_selection.peek().clone();
                                        for r in &repos {
                                            let e = m.entry(r.clone()).or_default();
                                            if !e.contains(&id) { e.push(id.clone()); }
                                        }
                                        repo_selection.set(m);
                                        editor.set(None);
                                    },
                                    "Save"
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
    /// `onboarded_in` names the project that already onboarded this repo, if any (#50): onboarding
    /// is one-time, so the caller blocks a re-onboard and routes the user to the workspace.
    Found {
        repo: String,
        path: String,
        onboarded_in: Option<String>,
    },
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
            Some(r) => RepoDetect::Found {
                repo: r.to_string(),
                path,
                onboarded_in: v
                    .get("onboarded_project")
                    .and_then(|p| p.as_str())
                    .map(|s| s.to_string()),
            },
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

/// The result of a greenfield scaffold call, as returned by `POST /api/onboard/greenfield`.
#[derive(Clone, PartialEq, serde::Deserialize)]
struct GreenfieldScaffoldResult {
    ok: bool,
    #[serde(default)]
    path: String,
    #[serde(default)]
    files_written: Vec<String>,
    #[serde(default)]
    commit_sha: String,
    #[serde(default)]
    message: String,
}

/// Resolve the adopted directive for a corpus rule: uses the default option's
/// directive, falling back to the rule title when options are absent or the default
/// is unset. Mirrors the resolve logic in the brownfield apply path.
fn resolve_gf_directive(r: &ProposedRuleView) -> String {
    if r.options.is_empty() {
        return r.title.clone();
    }
    r.default_option
        .as_ref()
        .and_then(|oid| r.options.iter().find(|o| &o.id == oid))
        .map(|o| o.directive.clone())
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| r.title.clone())
}

/// Call `POST /api/onboard/greenfield` with the given name, local directory path,
/// and selected arm rules. Resolves each `ProposedRuleView` into an `ArmRuleReq`
/// (directive resolved from the default option). Returns `None` on network failure.
async fn scaffold_greenfield_api(
    name: &str,
    dest_path: &str,
    rules: &[ProposedRuleView],
) -> Option<GreenfieldScaffoldResult> {
    // Resolve each corpus rule to its ArmRuleReq shape (id + resolved directive).
    let arm_rules: Vec<ArmRuleReq> = rules
        .iter()
        .filter(|r| r.scope != "cross-repo" && r.scope != "process")
        .map(|r| ArmRuleReq {
            id: r.id.clone(),
            title: r.title.clone(),
            directive: resolve_gf_directive(r),
            option: r.default_option.clone(),
            enforcement: r.enforcement.clone(),
            scope: "repo-local".to_string(),
            repos: vec![name.to_string()],
        })
        .collect();
    reqwest::Client::new()
        .post(format!("{}/api/onboard/greenfield", crate::BFF_URL))
        .json(&serde_json::json!({
            "name": name,
            "path": dest_path,
            "rules": arm_rules,
        }))
        .send()
        .await
        .ok()?
        .json::<GreenfieldScaffoldResult>()
        .await
        .ok()
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

    // Greenfield-specific state: the new repo's name, the local directory to create,
    // the selected corpus rules to bake in, the in-progress flag, and the scaffold result.
    // These signals are mutated inside the GreenfieldForm child component — the parent
    // holds them in scope so they survive path switches.
    let gf_name = use_signal(String::new);
    let gf_path = use_signal(String::new);
    // Set of corpus rule ids the user selected to bake into the new repo.
    let gf_selected_ids = use_signal(|| std::collections::BTreeSet::<String>::new());
    let gf_scaffolding = use_signal(|| false);
    let gf_result = use_signal(|| Option::<GreenfieldScaffoldResult>::None);
    // Load corpus rules for the greenfield picker (the full library, no scan needed).
    let corpus_res = use_resource(fetch_corpus_rules);
    let corpus_rules: Vec<ProposedRuleView> = corpus_res.read().clone().flatten().unwrap_or_default();

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

    let brownfield_cls = if path() == OnboardPath::Brownfield {
        "onboard-path on"
    } else {
        "onboard-path"
    };
    let greenfield_cls = if path() == OnboardPath::Greenfield {
        "onboard-path on"
    } else {
        "onboard-path"
    };

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

            // ── Brownfield path: browse existing repos + scan ─────────────────
            if path() == OnboardPath::Brownfield {
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
                                    // #50: block re-onboarding a repo that's already onboarded.
                                    RepoDetect::Found { repo: found, onboarded_in: Some(project), .. } => {
                                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, format!("{found} is already onboarded (project \u{201c}{project}\u{201d}). Onboarding is one-time — add it to your workspace to work on it, instead of re-onboarding."));
                                    }
                                    RepoDetect::Found { repo: found, path: folder, .. } => {
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
                        onclick: move |_| {
                            let repos: Vec<String> = repo()
                                .lines()
                                .flat_map(|l| l.split(','))
                                .map(|s| s.trim().to_string())
                                .filter(|s| !s.is_empty())
                                .collect();
                            if repos.is_empty() { return; }
                            scanning.set(true);
                            spawn(async move {
                                clear_onboarding_draft().await;
                                scan.set(scan_repos(&repos).await);
                                scanning.set(false);
                            });
                        },
                        if scanning() { "Scanning\u{2026}" } else { "Scan repos" }
                    }
                }
            }

            // ── Greenfield path: name + directory + starter ruleset ────────────
            if path() == OnboardPath::Greenfield {
                GreenfieldForm {
                    gf_name,
                    gf_path,
                    gf_selected_ids,
                    gf_scaffolding,
                    gf_result,
                    corpus_rules: corpus_rules.clone(),
                    toasts,
                }
            }

            // Brownfield-only: scan results + flow steps.
            if path() == OnboardPath::Brownfield {
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
                            // RE-SCAN remounts ScanResults/ProposedRulesTable with fresh rows and
                            // a fresh "recommended -> selected" pass.
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

            // Greenfield: flow steps (shown until scaffolding, handled inside GreenfieldForm).
            if path() == OnboardPath::Greenfield && gf_result().is_none() && !gf_scaffolding() {
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

/// Greenfield onboarding form: name the new repo, pick a local directory, select
/// starter rules from the corpus, and scaffold the repo with governance baked in
/// from commit zero.
///
/// Reuses the same arm emit path as brownfield apply — the governance files emitted
/// here are identical in structure to what a brownfield onboarding would write.
#[allow(clippy::too_many_arguments)]
#[component]
fn GreenfieldForm(
    mut gf_name: Signal<String>,
    mut gf_path: Signal<String>,
    mut gf_selected_ids: Signal<std::collections::BTreeSet<String>>,
    mut gf_scaffolding: Signal<bool>,
    mut gf_result: Signal<Option<GreenfieldScaffoldResult>>,
    corpus_rules: Vec<ProposedRuleView>,
    toasts: Signal<Vec<crate::toast::Toast>>,
) -> Element {
    let can_scaffold = !gf_name().trim().is_empty() && !gf_path().trim().is_empty();
    // Split corpus into recommended (suggested for the greenfield starter set) and the rest.
    let (recommended, available): (Vec<_>, Vec<_>) =
        corpus_rules.iter().partition(|r| r.recommended);

    rsx! {
        div { class: "gf-form",
            // Step 1: name the repo.
            div { class: "gf-field",
                label { class: "gf-label", "New repo name" }
                input {
                    class: "gf-input",
                    r#type: "text",
                    placeholder: "my-project",
                    value: "{gf_name}",
                    oninput: move |e| {
                        gf_name.set(e.value());
                        // Clear a prior scaffold result when the name changes.
                        gf_result.set(None);
                    },
                }
                p { class: "gf-hint", "Used as the initial commit label. You can connect a GitHub remote later." }
            }

            // Step 2: choose the local directory.
            div { class: "gf-field",
                label { class: "gf-label", "Local directory" }
                div { class: "gf-dir-row",
                    span { class: "gf-dir-path",
                        if gf_path().is_empty() {
                            span { class: "gf-dir-empty", "No directory chosen yet" }
                        } else {
                            "{gf_path()}"
                        }
                    }
                    button {
                        class: "btn-edit-sm",
                        onclick: move |_| {
                            spawn(async move {
                                let Some(folder) = rfd::AsyncFileDialog::new()
                                    .set_title("Choose the PARENT folder — Camerata creates the repo directory inside it")
                                    .pick_folder()
                                    .await
                                else {
                                    return;
                                };
                                let parent = folder.path().to_string_lossy().to_string();
                                let name = gf_name.peek().trim().to_string();
                                let dest = if name.is_empty() {
                                    parent.clone()
                                } else {
                                    format!("{}/{}", parent.trim_end_matches('/'), name)
                                };
                                gf_path.set(dest);
                                gf_result.set(None);
                            });
                        },
                        "Choose parent folder\u{2026}"
                    }
                }
                p { class: "gf-hint", "Camerata creates a new directory here for the repo. The directory must not already exist." }
            }

            // Step 3: starter ruleset picker.
            div { class: "gf-field",
                label { class: "gf-label", "Starter ruleset" }
                p { class: "gf-hint", "Select the rules to bake in from the first commit. Recommended rules are pre-ticked. You can change these after onboarding." }
                if corpus_rules.is_empty() {
                    p { class: "gf-hint", "Loading corpus rules\u{2026}" }
                } else {
                    div { class: "gf-rules-list",
                        // Recommended rules first.
                        if !recommended.is_empty() {
                            p { class: "gf-rules-group-h", "Recommended" }
                            for rule in &recommended {
                                {
                                    let rid = rule.id.clone();
                                    let rid2 = rid.clone();
                                    let checked = gf_selected_ids().contains(&rid);
                                    rsx! {
                                        label { class: "gf-rule-row", key: "{rid}",
                                            input {
                                                r#type: "checkbox",
                                                checked,
                                                onchange: move |_| {
                                                    let mut ids = gf_selected_ids();
                                                    if ids.contains(&rid2) { ids.remove(&rid2); } else { ids.insert(rid2.clone()); }
                                                    gf_selected_ids.set(ids);
                                                },
                                            }
                                            span { class: "gf-rule-id", "{rule.id}" }
                                            span { class: "gf-rule-title", " \u{2014} {rule.title}" }
                                            if !rule.domain.is_empty() && rule.domain != "*" {
                                                span { class: "gf-rule-domain", " [{rule.domain}]" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        // Available (not pre-ticked) rules.
                        if !available.is_empty() {
                            p { class: "gf-rules-group-h", "Available" }
                            for rule in &available {
                                {
                                    let rid = rule.id.clone();
                                    let rid2 = rid.clone();
                                    let checked = gf_selected_ids().contains(&rid);
                                    rsx! {
                                        label { class: "gf-rule-row", key: "{rid}",
                                            input {
                                                r#type: "checkbox",
                                                checked,
                                                onchange: move |_| {
                                                    let mut ids = gf_selected_ids();
                                                    if ids.contains(&rid2) { ids.remove(&rid2); } else { ids.insert(rid2.clone()); }
                                                    gf_selected_ids.set(ids);
                                                },
                                            }
                                            span { class: "gf-rule-id", "{rule.id}" }
                                            span { class: "gf-rule-title", " \u{2014} {rule.title}" }
                                            if !rule.domain.is_empty() && rule.domain != "*" {
                                                span { class: "gf-rule-domain", " [{rule.domain}]" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Step 4: scaffold CTA.
            button {
                class: "onboard-cta",
                disabled: !can_scaffold || gf_scaffolding(),
                onclick: move |_| {
                    let name = gf_name().trim().to_string();
                    let dest = gf_path().trim().to_string();
                    if name.is_empty() {
                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Warning, "Enter a name for the new repo.");
                        return;
                    }
                    if dest.is_empty() {
                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Warning, "Choose a directory for the new repo.");
                        return;
                    }
                    // Resolve the selected corpus rules from the full list.
                    let ids = gf_selected_ids();
                    let selected: Vec<ProposedRuleView> = corpus_rules
                        .iter()
                        .filter(|r| ids.contains(&r.id))
                        .cloned()
                        .collect();
                    gf_scaffolding.set(true);
                    gf_result.set(None);
                    spawn(async move {
                        let result = scaffold_greenfield_api(&name, &dest, &selected).await;
                        gf_scaffolding.set(false);
                        gf_result.set(result);
                    });
                },
                if gf_scaffolding() { "Scaffolding\u{2026}" } else { "Scaffold repo" }
            }

            // Step 5: result.
            if let Some(result) = gf_result() {
                GreenfieldResultView { result }
            }
        }
    }
}

/// Displays the outcome of a greenfield scaffold: success with file list and commit
/// sha, or an error message.
#[component]
fn GreenfieldResultView(result: GreenfieldScaffoldResult) -> Element {
    if result.ok {
        rsx! {
            div { class: "gf-result gf-result-ok",
                p { class: "gf-result-h", "Repo scaffolded" }
                p { class: "gf-result-msg", "{result.message}" }
                div { class: "gf-result-files",
                    p { class: "gf-result-files-h", "Files committed:" }
                    ul {
                        for f in &result.files_written {
                            li { class: "gf-result-file", "{f}" }
                        }
                    }
                }
                if !result.commit_sha.is_empty() {
                    p { class: "gf-result-sha", "Initial commit: {result.commit_sha}" }
                }
                p { class: "gf-result-path", "Location: {result.path}" }
                p { class: "gf-result-next",
                    "The repo is governed from the first commit. Connect it to a remote \
                     (e.g. create a GitHub repo and add it as origin) whenever you are ready."
                }
            }
        }
    } else {
        rsx! {
            div { class: "gf-result gf-result-err",
                p { class: "gf-result-h", "Scaffold failed" }
                p { class: "gf-result-msg", "{result.message}" }
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
    // Thorough calibration (#51): opt-in, costs more AI. Off by default.
    let mut audit_thorough = use_signal(|| false);
    // Full scan: when ON, ignore the incremental cache and re-audit every file. Off by default
    // (so re-scans are incremental — only changed files cost AI tokens). The first scan of a
    // project is full regardless (no cache yet).
    let mut audit_full_scan = use_signal(|| false);
    // Deep compliance & security tier (#55): opt-in, the most expensive tier.
    // Runs three extra whole-repo passes (SOC-2 gap analysis, deep security audit,
    // threat model) after the standard audit and attaches the results as `report.deep`.
    // Output is ADVISORY — never a SOC-2 report or a penetration test.
    let mut audit_deep = use_signal(|| false);
    // pw/cockpit-ui Feature 5: feature-flag map. Controls per-feature affordances —
    // SOC-2 section visibility, deep-export scope. Fetched once on mount; degrades
    // gracefully (all flags default to false) when the server is old.
    let feature_flags_res = use_resource(fetch_feature_flags);
    let feature_flags = feature_flags_res
        .read()
        .clone()
        .unwrap_or_default();
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
                    .filter(|r| r.effective_auto_recommended() && r.repos.iter().any(|rp| rp == repo))
                    .map(|r| r.id.clone())
                    .collect();
                m.insert(repo.clone(), ids);
            }
        }
        m
    };
    let repo_selection = use_signal(|| repo_seed);
    // Shared so the custom-rules panel can auto-select a newly created rule for its repo(s).
    use_context_provider(|| repo_selection);
    // Which repo's rule table is in view. Defaults to the first scanned repo. Provided as
    // context so the rule-detail modal + the table key the per-repo `chosen` map by it.
    let viewed_repo = use_signal(|| report.repos.first().cloned().unwrap_or_default());
    use_context_provider(|| viewed_repo);
    let mut viewed_repo = viewed_repo;
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

    // The architect's PER-REPO alternative choices, keyed `chosen_key(repo, rule_id)` ->
    // option id. Per-repo so picking an alternative for a rule in one repo doesn't change
    // another repo's choice. Seeded with each rule's default for every scanned repo.
    let chosen = use_signal(|| {
        let mut m = std::collections::HashMap::<String, String>::new();
        for repo in &report.repos {
            for r in &report.proposed_rules {
                if let Some(d) = &r.default_option {
                    m.insert(chosen_key(repo, &r.id), d.clone());
                }
            }
        }
        m
    });
    use_context_provider(|| chosen);

    // User-authored custom rules (Custom + Custom Global), shared via context so the table, the
    // create/edit/delete modal, and the audit/arm closures all read/write the same list. Seeded
    // from the active project's existing custom rules (so re-opening shows them); the draft
    // restore below overlays any in-flight onboarding additions.
    let custom_rules = use_signal(Vec::<CustomRuleView>::new);
    use_context_provider(|| custom_rules);

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
            let picked = chosen
                .read()
                .get(&chosen_key(&viewed_repo(), &r.id))
                .cloned()
                .or_else(|| r.default_option.clone());
            let desc = picked
                .and_then(|oid| {
                    r.options
                        .iter()
                        .find(|o| o.id == oid)
                        .map(|o| o.directive.clone())
                })
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
    let mut custom_rules_w = custom_rules;
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
                        if !d.custom.is_empty() {
                            custom_rules_w.set(d.custom);
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
            let cust = custom_rules.read().clone();
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
                custom: cust,
                dispositions: disp,
                viewed_repo: vr,
                triage_view: tv,
            };
            spawn(async move {
                save_onboarding_draft(&draft).await;
                // Stamp the local time so the UI can show "auto-saved at HH:MM:SS".
                last_saved.set(Some(
                    chrono::Local::now().format("%-I:%M:%S %p").to_string(),
                ));
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
                                // Authority: deterministic floor (stable, gateable) vs AI-advisory
                                // (model-inferred, id/severity may vary run-to-run).
                                if is_enforced_floor(&f.rule_id) {
                                    span { class: "rule-modal-tag", "enforced · deterministic (stable id)" }
                                } else {
                                    span { class: "rule-modal-tag", "AI · advisory (id may vary run-to-run)" }
                                }
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
            // Author custom rules (#49) — they appear in the Custom / Custom Global groups below.
            CustomRulesPanel { all_repos: report.repos.clone() }
            {
                let repos_audit = report.repos.clone();
                // Per-repo binding drives RECOMMENDATION (pre-selection), NOT visibility: every
                // repo's table shows the WHOLE rule library so the architect can manually add
                // ANY rule to ANY repo. (Filtering visibility by the repo binding hid rules that
                // were auto-suggested for a sibling repo — e.g. ci-cd suggested for repo A never
                // appeared in repo B's table, so it couldn't be added there at all.) The viewed
                // repo only changes which rules are pre-checked, via the seeded per-repo selection.
                let view_repo = if multi_repo { viewed_repo() } else { String::new() };
                // Merge in the user's custom rules (#49). VISIBLE in this repo's table = corpus
                // rules + Custom Global + this repo's own Custom rules (repo-scoped customs don't
                // leak into sibling repos). The cross-repo `all_rules` lookup gets EVERY custom
                // (all repos) so arm/audit can resolve them. The table is re-keyed on a custom
                // signature so creating/editing/deleting a custom rule remounts it with the change.
                let actual_repo = viewed_repo();
                let (visible_customs, all_customs, custom_sig) = {
                    let cust = custom_rules.read();
                    let all_repos = report.repos.clone();
                    let visible: Vec<ProposedRuleView> = cust
                        .iter()
                        .filter(|c| c.is_global() || c.domain.trim() == actual_repo)
                        .map(|c| c.to_proposed(&all_repos))
                        .collect();
                    let all: Vec<ProposedRuleView> = cust.iter().map(|c| c.to_proposed(&all_repos)).collect();
                    let sig = cust.iter().map(|c| format!("{}\u{1}{}", c.name, c.domain)).collect::<Vec<_>>().join("\u{2}");
                    (visible, all, sig)
                };
                let mut all_rules = report.proposed_rules.clone();
                all_rules.extend(all_customs);
                let mut visible_rules = report.proposed_rules.clone();
                visible_rules.extend(visible_customs);
                rsx! {
                    ProposedRulesTable {
                        // Key on the viewed repo + custom signature so switching repos OR adding/
                        // editing/deleting a custom rule remounts the table with the change.
                        key: "{view_repo}\u{1f}{custom_sig}",
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
                            let thorough = audit_thorough();
                            let deep = audit_deep();
                            // Full scan forces a clean pass; otherwise the scan is incremental
                            // (only files changed since the last scan cost AI tokens).
                            let incremental = !audit_full_scan();
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
                                    let Some(jid) = audit_job_start(&repos, &rules, &model, &calib, "parallel", thorough, incremental, deep).await else {
                                        auditing.set(false);
                                        return;
                                    };
                                    active_audit_job.set(Some(jid.clone()));
                                    poll_job(jid, audit, auditing, job_progress, active_audit_job).await;
                                });
                            } else {
                                // Synchronous: hold the request until the (shorter) run finishes.
                                spawn(async move {
                                    audit.set(audit_against(&repos, &rules, &model, &calib, &mode, thorough, incremental, deep).await);
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
                        option { value: "batch", "Batch (50% off — async, API key required)" }
                    }
                    if audit_mode() == recommended_mode {
                        span { class: "audit-mode-rec", "✓ auto-selected for this scan's size" }
                    }
                    span { class: "audit-model-hint", "Parallel runs rule-batches concurrently. Background job runs server-side so you can leave and watch findings stream in — best for huge / multi-repo scans. Batch uses the Anthropic Message Batches API for a flat 50% discount on all tokens; requires ANTHROPIC_API_KEY and the api backend; results arrive asynchronously (seconds to minutes, up to 24h on very large scans)." }
                }
                // Thorough calibration (#51) — opt-in, costs more AI.
                div { class: "audit-model-row",
                    label { class: "audit-model-label", "Thorough calibration" }
                    label { class: "audit-thorough-toggle",
                        input {
                            r#type: "checkbox",
                            checked: audit_thorough(),
                            disabled: auditing(),
                            onchange: move |e| audit_thorough.set(e.checked()),
                        }
                        span { "Cross-check the calibration (uses more AI)" }
                    }
                    span { class: "audit-model-hint",
                        "Calibration is the step AFTER the scan that recalibrates each finding's severity and flags debatable ones for review — it never drops a finding. Thorough mode runs that pass ~3× and keeps the conservative consensus (so one over-confident pass can't push a debatable architectural preference to HIGH), and judges findings proportionally to the repo's size. Noticeably more AI calls. Optional — the standard single-pass calibration is on by default."
                    }
                }
                // Incremental scan — on by default; re-scans only pay AI for changed files.
                // The checkbox forces a full re-scan over every file.
                div { class: "audit-model-row",
                    label { class: "audit-model-label", "Full scan" }
                    label { class: "audit-thorough-toggle",
                        input {
                            r#type: "checkbox",
                            checked: audit_full_scan(),
                            disabled: auditing(),
                            onchange: move |e| audit_full_scan.set(e.checked()),
                        }
                        span { "Re-scan every file (ignore the incremental cache)" }
                    }
                    span { class: "audit-model-hint",
                        "By default a re-scan is INCREMENTAL: only files whose content changed since the last scan of this project are sent to the AI, and findings for unchanged files are reused from cache — so re-running after a small edit costs a fraction of the tokens. The first scan of a project is always full (no cache yet). Tick this to ignore the cache and re-audit the whole codebase from scratch (e.g. after changing your rule selection, or to refresh every finding)."
                    }
                }
                // Deep compliance & security tier (#55): opt-in, ADVISORY, expensive.
                // Gated on the `soc2` feature flag — hidden entirely when soc2 is disabled
                // (this is the SOC-2-headlined surface; set via .camerata/features.toml or
                // CAMERATA_FEATURE_SOC2=false). The server also skips the lens when off.
                if feature_flags.soc2 {
                    div { class: "audit-model-row",
                        label { class: "audit-model-label", "Deep compliance & security (opt-in)" }
                        label { class: "audit-thorough-toggle",
                            input {
                                r#type: "checkbox",
                                checked: audit_deep(),
                                disabled: auditing(),
                                onchange: move |e| audit_deep.set(e.checked()),
                            }
                            span { "Run SOC-2 gap analysis, deep security audit, and threat model" }
                        }
                        span { class: "audit-model-hint deep-tier-warning",
                            "ADVISORY ONLY — not a SOC-2 report and not a penetration test. \
                             Camerata sees static code only; controls that depend on org-level evidence \
                             (HR policies, vendor contracts, access reviews) cannot be assessed from code. \
                             Three extra whole-repo passes run after the standard audit. \
                             This is the MOST EXPENSIVE tier (~3 extra whole-repo passes). \
                             Enable only when you explicitly want compliance gap analysis for this codebase."
                        }
                    }
                }
                // If the architect SKIPS the audit, the post-scan section below (which hosts the
                // CI-wiring story) never renders because `code_chars == 0`. But wiring mechanical
                // rules into CI only needs the SELECTED rules, not a code scan — so offer the
                // CI-story affordance here too, so it's reachable straight after rule selection.
                if report.code_chars == 0 {
                    div { class: "onboard-final-step",
                        span { class: "onboard-step-eyebrow", "Optional: wire CI rules into CI" }
                        CiRulesPanel {
                            repos: report.repos.clone(),
                            rules: ci_rule_items_from_proposed(&report.proposed_rules),
                        }
                        p { class: "section-hint", "You can file the CI-wiring stories (GitHub issues) from your selected rules without running the audit. Optional, and not required to finish onboarding." }
                    }
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
                        // Mirror the request flags the audit will actually send: incremental
                        // scope (only changed files cost tokens unless Full scan is ticked) and
                        // the deep SOC-2/security tier (three extra whole-repo passes).
                        let incremental = !audit_full_scan();
                        let deep = audit_deep();
                        let (toks, dollars, passes) = estimate_audit_cost(report.code_chars, sel, &audit_mode(), a_in, a_out, c_in, c_out, audit_thorough(), incremental, deep);
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
                                        // Show cache savings line when the API backend ran with
                                        // prompt caching active (cache_read > 0 means the cache
                                        // was hit at least once; creation > 0 means the cache
                                        // was written at least once).
                                        let cache_active = u.cache_read_input_tokens > 0
                                            || u.cache_creation_input_tokens > 0;
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
                                                if cache_active {
                                                    "Prompt cache: {human_tokens(u.cache_creation_input_tokens)} tok written (1.25x), {human_tokens(u.cache_read_input_tokens)} tok read (0.1x). "
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
                                        if incremental {
                                            "Scope: INCREMENTAL — only files changed since the last scan are billed, so the real cost is usually well below this whole-repo figure (priced over ~{code_toks} tokens, {report.files_scanned} files). Tick Full scan to re-audit everything. "
                                        } else {
                                            "Scope: FULL — every file is re-audited (~{code_toks} tokens, {report.files_scanned} files). "
                                        }
                                        "Prompt-caching can make the actual bill lower. "
                                        "The deterministic security floor (secrets / raw-SQL / secret-URLs) runs free. "
                                        "After this, you audit PR diffs — pennies. Cheaper model or Sequential mode lowers this."
                                        if deep {
                                            span { class: "audit-cost-deep-note",
                                                " Deep tier is ON and INCLUDED above: ~3 extra whole-repo passes (SOC-2 gap, deep security, threat model) at the audit model. This is the MOST EXPENSIVE option — it dominates the figure. Untick Deep to drop it."
                                            }
                                        }
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

                // ── Deep compliance & security tier output (#55) ──────────────────────
                // Shown only when the audit ran with deep:true and the server returned the
                // three-lens report. Everything here is ADVISORY — never a SOC-2 report or
                // a penetration test. The disclaimer at the top of each lens makes this explicit.
                //
                // pw/cockpit-ui Feature 5: the SOC-2 lens (soc2-gap) is gated by the `soc2`
                // feature flag. When the flag is off, the SOC-2 section is hidden entirely;
                // deep-security and threat-model still render. The flag state comes from the
                // `feature_flags` map fetched at mount time.
                {
                    let soc2_on = feature_flags.soc2;
                    if let Some(deep) = audited.as_ref().and_then(|a| a.deep.clone()) {
                        rsx! {
                        div { class: "deep-tier-panel",
                            p { class: "deep-tier-heading", "Deep compliance & security tier (ADVISORY)" }
                            // Tier-level disclaimer — surfaced prominently before any findings.
                            if !deep.disclaimer.is_empty() {
                                p { class: "deep-tier-disclaimer", "{deep.disclaimer}" }
                            } else {
                                p { class: "deep-tier-disclaimer",
                                    "ADVISORY ONLY. This output is model-inferred from static code. \
                                     It is not a SOC-2 report, not a certification, and not a penetration test. \
                                     Controls that require organisational evidence (policies, HR, vendor contracts) \
                                     cannot be assessed from code alone. A qualified professional must review and \
                                     validate these findings before any compliance or security claim is made."
                                }
                            }
                            // Feature 5: when soc2 flag is OFF, show a notice that the SOC-2
                            // affordance is disabled for this workspace, but do NOT hide the
                            // deep-security or threat-model sections.
                            if !soc2_on {
                                p { class: "deep-soc2-disabled-notice",
                                    "\u{1F512} SOC-2 gap analysis is disabled for this workspace \
                                     (feature flag \u{2018}soc2\u{2019} is off). \
                                     Deep security and threat model results are shown below."
                                }
                            }
                            for lens in deep.lenses.iter() {
                                {
                                    // Feature 5: skip the soc2-gap lens when the flag is off.
                                    if lens.lens == "soc2-gap" && !soc2_on {
                                        rsx! {}
                                    } else {
                                    let (heading, description) = match lens.lens.as_str() {
                                        "soc2-gap"       => ("SOC-2 Readiness Gap Analysis",
                                                              "Maps the repo's detectable practices against SOC-2 Common Criteria and reports gaps. \
                                                               This is a gap analysis, not a SOC-2 report. \
                                                               Controls needing organisational evidence are marked unknown."),
                                        "deep-security"  => ("Deep Security Audit",
                                                              "Authorization, authentication, sensitive-data handling, and injection paths beyond the \
                                                               deterministic floor. Every finding is advisory — a human must validate each one."),
                                        "threat-model"   => ("Threat Model",
                                                              "Entry points, trust boundaries, sensitive-data paths, and STRIDE-flavoured threats with \
                                                               mitigation directions. Model-inferred from the repo structure."),
                                        other            => (other, ""),
                                    };
                                    let lens = lens.clone();
                                    rsx! {
                                        div { class: "deep-lens", key: "{lens.lens}",
                                            p { class: "deep-lens-heading", "{heading}" }
                                            p { class: "deep-lens-desc", "{description}" }
                                            if !lens.disclaimer.is_empty() {
                                                p { class: "deep-lens-disclaimer", "{lens.disclaimer}" }
                                            }
                                            if !lens.summary.is_empty() {
                                                p { class: "deep-lens-summary", "{lens.summary}" }
                                            }
                                            // SOC-2 gap table (only rendered when soc2 flag is on,
                                            // which is guaranteed by the lens filter above — belt + suspenders).
                                            if !lens.soc2_gaps.is_empty() && soc2_on {
                                                div { class: "soc2-gap-table",
                                                    div { class: "soc2-gap-row header",
                                                        span { class: "soc2-col-ctrl", "Control" }
                                                        span { class: "soc2-col-title", "Title" }
                                                        span { class: "soc2-col-status", "Status" }
                                                        span { class: "soc2-col-obs", "Observed" }
                                                        span { class: "soc2-col-gap", "Gap / Remediation" }
                                                    }
                                                    for (i, gap) in lens.soc2_gaps.iter().enumerate() {
                                                        div { key: "{i}", class: "soc2-gap-row soc2-status-{gap.status}",
                                                            span { class: "soc2-col-ctrl", "{gap.control}" }
                                                            span { class: "soc2-col-title", "{gap.title}" }
                                                            span { class: "soc2-col-status soc2-badge-{gap.status}", "{gap.status}" }
                                                            span { class: "soc2-col-obs", "{gap.observed}" }
                                                            span { class: "soc2-col-gap", "{gap.gap}" }
                                                        }
                                                    }
                                                }
                                            }
                                            // Free-text detail (deep-security + threat-model)
                                            if !lens.detail.is_empty() {
                                                pre { class: "deep-lens-detail", "{lens.detail}" }
                                            }
                                        }
                                    }
                                    }
                                }
                            }

                            // pw/cockpit-ui Feature 4: deep-report export button.
                            // Placed at the bottom of the deep-tier panel so it's visible after
                            // reviewing the findings. Project id comes from the active project.
                            {
                                let pid_export = report.repos.first().cloned().unwrap_or_default();
                                rsx! {
                                    DeepReportExportPanel {
                                        project_id: pid_export,
                                        soc2_enabled: soc2_on,
                                    }
                                }
                            }
                        }
                        }
                    } else {
                        rsx! {}
                    }
                }

                // ── Optional: wire CI rules into CI (#32) ─────────────────────────
                // Files per-tier STORIES (GitHub issues) per repo to add the selected CI-tier
                // rules to that repo's existing CI as enforced lint gates. Mechanical and
                // architectural land as SEPARATE issues — architectural needs team refinement
                // first. This is OPTIONAL — it does NOT gate "onboarded". Use "Complete
                // onboarding" above to finish at any point; the dev layer picks up each CI
                // story independently.
                if n_unresolved == 0 {
                    div { class: "onboard-final-step",
                        span { class: "onboard-step-eyebrow", "Optional: wire CI rules into CI" }
                        CiRulesPanel {
                            repos: report.repos.clone(),
                            rules: ci_rule_items_from_proposed(&report.proposed_rules),
                        }
                        p { class: "section-hint", "Optional, and independent of the tech-debt work above. This files per-tier CI-wiring stories (GitHub issues — one mechanical, one architectural); neither is required to finish onboarding — use \u{201c}Complete onboarding\u{201d} whenever you're ready." }
                    }
                }
            }
        }
    }
}

/// The live governed run: the real gate verdicts from the BFF run engine, streamed
/// in as the run walks to completion.
#[component]
#[component]
fn LiveRunPanel(run: RunView, uow_refresh: Signal<u32>) -> Element {
    let (status_label, status_cls) = run_status_badge(&run.status);
    let live = run.mode == "live";
    let mode_label = if live {
        "live fleet"
    } else {
        "scripted · token-free"
    };
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

            // ── Provenance + sign-off (issue #21): the write-back floor ──────────
            // Once the run reaches its terminal stage, show the provenance summary
            // (rules in force, deny/allow tallies, bounces) and the EXPLICIT sign-off
            // action. Camerata never auto-signs-off; this is the human gate.
            if run.done {
                RunProvenancePanel { run_id: run.id.clone(), uow_refresh }
            }
        }
    }
}

/// The provenance summary for a completed run plus the architect's sign-off action
/// (issue #21). Fetches `GET /api/runs/:id/provenance`; the sign-off button posts to
/// `POST /api/runs/:id/sign-off` and bumps `uow_refresh` so the UoW panel reflects it.
#[component]
fn RunProvenancePanel(run_id: String, uow_refresh: Signal<u32>) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let rid = run_id.clone();
    let prov_res = use_resource(move || {
        let rid = rid.clone();
        async move { fetch_provenance(&rid).await }
    });
    let mut signing = use_signal(|| false);
    let mut signed = use_signal(|| false);

    let prov = prov_res.read().clone().flatten();

    rsx! {
        div { class: "run-provenance",
            p { class: "run-provenance-h", "PROVENANCE" }
            match prov {
                Some(p) => rsx! {
                    div { class: "provenance-tallies",
                        span { class: "provenance-tally",
                            span { class: "provenance-num", "{p.allow_count}" }
                            " allowed"
                        }
                        span { class: "provenance-tally deny",
                            span { class: "provenance-num", "{p.deny_count}" }
                            " denied"
                        }
                        span { class: "provenance-tally bounce",
                            span { class: "provenance-num", "{p.total_bounces}" }
                            " total bounces"
                        }
                    }
                    if !p.rules_fired.is_empty() {
                        p { class: "provenance-fired",
                            "Rules that bounced a write: {p.rules_fired.join(\", \")}"
                        }
                    }
                    p { class: "provenance-inforce",
                        "Rules in force ({p.rules_in_force.len()}): {p.rules_in_force.join(\", \")}"
                    }
                },
                None => rsx! {
                    p { class: "provenance-empty", "Computing provenance…" }
                },
            }

            // The explicit sign-off action — never automatic.
            div { class: "run-signoff-row",
                if signed() {
                    span { class: "run-signoff-done", "✓ Signed off" }
                } else {
                    button {
                        class: "btn-run",
                        disabled: signing(),
                        onclick: move |_| {
                            let rid = run_id.clone();
                            let toasts = toasts;
                            let mut uow_refresh = uow_refresh;
                            signing.set(true);
                            spawn(async move {
                                let ok = sign_off_run(&rid, "architect", None).await.is_some();
                                signing.set(false);
                                if ok {
                                    signed.set(true);
                                    uow_refresh += 1;
                                    crate::toast::push_toast(
                                        toasts,
                                        crate::toast::ToastKind::Info,
                                        "Run signed off.".to_string(),
                                    );
                                } else {
                                    crate::toast::push_toast(
                                        toasts,
                                        crate::toast::ToastKind::Warning,
                                        "Could not sign off the run.".to_string(),
                                    );
                                }
                            });
                        },
                        if signing() { "Signing off…" } else { "✓ Sign off this run" }
                    }
                }
                span { class: "section-hint", "Camerata never auto-opens a PR or signs off. Review the provenance, then sign off explicitly." }
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
/// A `<select>` of model options, generic over the bound signal. Renders nothing
/// until the model list has loaded. Used by every per-step run control.
#[component]
fn ModelSelect(models: Option<AuditModelsResp>, selected: Signal<String>) -> Element {
    let mut selected = selected;
    rsx! {
        if let Some(m) = models {
            select {
                class: "run-model-select",
                value: "{selected}",
                onchange: move |e| selected.set(e.value()),
                for opt in m.models.iter() {
                    option {
                        value: "{opt.id}",
                        selected: selected() == opt.id,
                        "{opt.label}"
                    }
                }
            }
        }
    }
}

/// Poll a started run to completion, pushing each snapshot to `active_run` and
/// bumping `uow_refresh` once it finishes (so the panel / stage re-fetch). Shared by
/// the investigation and development run controls.
async fn poll_run_to_done(
    run_id: String,
    mut active_run: Signal<Option<RunView>>,
    mut uow_refresh: Signal<u32>,
) {
    loop {
        if let Some(rv) = fetch_run(&run_id).await {
            let done = rv.done;
            active_run.set(Some(rv));
            if done {
                uow_refresh += 1;
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
    }
}

/// AI-assisted "Update branch" control (the GitHub PR "Update branch" pattern, gated).
///
/// Merges a user-selected SOURCE branch (local or origin) INTO this UoW's working
/// branch. A `<select>` is populated from `POST /api/uow/:story_id/branches`, grouped
/// into "Local" and "Origin" `<optgroup>`s (origin values carry an `origin:` prefix so
/// the handler knows the source kind). The "▶ Update branch (AI-assisted)" button POSTs
/// to `POST /api/uow/:story_id/update-branch`, then drives `AgentActivity` on the
/// returned run and refreshes the UoW when the run completes.
///
/// A clean merge commits server-side; a conflict is resolved by ONE gated agent (the
/// gate is preserved end to end). A server 4xx (e.g. no branch yet, repo not resolved
/// locally) raises a toast carrying the server's reason. Owns its OWN active-run signal
/// so it doesn't collide with the lifecycle run control.
#[component]
fn UowUpdateBranchControl(
    story_id: String,
    uow_refresh: Signal<u32>,
    models: Option<AuditModelsResp>,
) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();

    // The mergeable branches, fetched once per UoW (and after a refresh tick).
    let branches_res = {
        let sid = story_id.clone();
        use_resource(move || {
            let sid = sid.clone();
            let _dep = uow_refresh();
            async move { fetch_uow_branches(&sid).await }
        })
    };
    let branches = branches_res.read().clone().unwrap_or_default();

    // The selected source. The select's value carries the source kind: a bare branch
    // name is local; an `origin:`-prefixed value is an origin branch.
    let mut selected = use_signal(String::new);
    // The conflict-resolution agent's model (default = project strongest, editable).
    let model = use_signal(String::new);
    // Its own active run + busy flag (independent of the lifecycle run control).
    let active_run = use_signal(|| Option::<RunView>::None);
    let mut updating = use_signal(|| false);

    let has_branches = !branches.local.is_empty() || !branches.origin.is_empty();

    rsx! {
        div { class: "uow-step-control uow-update-branch",
            p { class: "uow-step-h", "Update branch (AI-assisted)" }
            p { class: "section-hint",
                "Merge a branch INTO this UoW's branch (GitHub's \"Update branch\"). A clean merge commits; conflicts are resolved by a gated agent."
            }
            if has_branches {
                div { class: "run-control-row",
                    select {
                        class: "uow-branch-select",
                        value: "{selected}",
                        onchange: move |e| selected.set(e.value()),
                        option { value: "", disabled: true, selected: selected().is_empty(), "Choose a branch…" }
                        if !branches.local.is_empty() {
                            optgroup { label: "Local",
                                for b in branches.local.iter() {
                                    option { key: "local:{b}", value: "{b}", "{b}" }
                                }
                            }
                        }
                        if !branches.origin.is_empty() {
                            optgroup { label: "Origin",
                                for b in branches.origin.iter() {
                                    option { key: "origin:{b}", value: "origin:{b}", "{b}" }
                                }
                            }
                        }
                    }
                    ModelSelect { models: models.clone(), selected: model }
                    button {
                        class: "btn-run",
                        disabled: updating() || selected().is_empty(),
                        onclick: move |_| {
                            let raw = selected();
                            if raw.is_empty() {
                                return;
                            }
                            // Decode the source kind from the option value's prefix.
                            let (source, branch) = match raw.strip_prefix("origin:") {
                                Some(b) => ("origin".to_string(), b.to_string()),
                                None => ("local".to_string(), raw.clone()),
                            };
                            let sid = story_id.clone();
                            let md = model();
                            updating.set(true);
                            spawn(async move {
                                match start_update_branch_run(&sid, &branch, &source, &md).await {
                                    StartRunOutcome::Started(rid) => {
                                        poll_run_to_done(rid, active_run, uow_refresh).await;
                                    }
                                    StartRunOutcome::Blocked(reason) => crate::toast::push_toast(
                                        toasts,
                                        crate::toast::ToastKind::Warning,
                                        reason,
                                    ),
                                    StartRunOutcome::Failed => crate::toast::push_toast(
                                        toasts,
                                        crate::toast::ToastKind::Warning,
                                        "Could not start the update-branch run.".to_string(),
                                    ),
                                }
                                updating.set(false);
                            });
                        },
                        if updating() { "Updating…" } else { "▶ Update branch (AI-assisted)" }
                    }
                }
                // The gated run's live activity (conflict-resolution agent), when running.
                {
                    let rid = match active_run() {
                        Some(ref r) => r.id.clone(),
                        None => String::new(),
                    };
                    rsx! { crate::agent_activity::AgentActivity { run_id: rid } }
                }
            } else {
                p { class: "section-hint",
                    "No branches available — the repo must be cloned locally (set its path in the Rules view)."
                }
            }
        }
    }
}

/// The lifecycle strip + the run control for the CURRENT phase, rendered inline with
/// the steps (Increment 1). Runs live ON THE STEPS: the control shown is the one for
/// the active stage and it REPLACES the prior phase's control rather than stacking.
///
/// - **Intake** → a single model `<select>` (default = project strongest, editable)
///   beside a **Begin investigation** button that calls `begin_investigation_run` and
///   then drives the live agent activity on the returned run. The server transitions
///   the stage Intake → Investigating.
/// - **Investigating** → the architect's **Approve decisions** transition
///   (Investigating → DecisionsApproved), which the server gates on the story's
///   decision records and 409s (with a precise reason) if not all are approved.
/// - **DecisionsApproved** → three per-tier model `<select>`s (Strongest / Balanced /
///   Fast, defaulted from the project tier map, editable for this run) beside a
///   **Run development (governed)** button that calls `start_dev_run` with the tier
///   map. The strongest tier leads and delegates simpler work to the others.
///
/// Later stages (`Development`, `AwaitingQa`, `SignedOff`) are engine-driven — set by
/// the gated run, its provenance watcher, and the explicit sign-off — so no run
/// control is shown for them here. A blocked transition or run raises a toast carrying
/// the server's reason.
#[component]
fn UowStepRunControls(
    story_id: String,
    stage: UowStage,
    uow_refresh: Signal<u32>,
    active_run: Signal<Option<RunView>>,
    models: Option<AuditModelsResp>,
    invest_model: Signal<String>,
    dev_strongest: Signal<String>,
    dev_balanced: Signal<String>,
    dev_fast: Signal<String>,
) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();

    // The full ordered progression, rendered as a strip with the reached stages lit.
    const STAGES: &[UowStage] = &[
        UowStage::Intake,
        UowStage::Investigating,
        UowStage::DecisionsApproved,
        UowStage::Development,
        UowStage::AwaitingQa,
        UowStage::SignedOff,
    ];

    let sid_begin = story_id.clone();
    let sid_approve = story_id.clone();
    let sid_dev = story_id.clone();

    // One-time BOOTSTRAP toggle (default OFF, per-run, NOT persisted): when on, this dev
    // run skips ONLY the layer-2 post-task lint/test bounce so a brownfield repo can land
    // the linters/checkers layer-2 needs. The security gate (layer 1) + the no-code-first
    // decisions gate still apply. The architect turns it back off after the tooling lands.
    let mut bootstrap_skip_layer2 = use_signal(|| false);

    rsx! {
        div { class: "uow-lifecycle",
            span { class: "uow-field-label", "Lifecycle" }
            div { class: "uow-lifecycle-strip",
                for s in STAGES.iter().copied() {
                    {
                        let reached = s.ordinal() <= stage.ordinal();
                        let current = s == stage;
                        let mut cls = String::from("uow-stage-pip");
                        if reached { cls.push_str(" reached"); }
                        if current { cls.push_str(" current"); }
                        rsx! {
                            span { class: "{cls}", title: "{s.label()}", "{s.label()}" }
                        }
                    }
                }
            }

            // The run control for the CURRENT phase, inline with the steps. Only one
            // shows at a time — it replaces the prior phase's control.
            match stage {
                // INVESTIGATION: model select + Begin investigation (Intake → Investigating).
                UowStage::Intake => rsx! {
                    div { class: "uow-step-control",
                        p { class: "uow-step-h", "Investigation" }
                        div { class: "run-control-row",
                            button {
                                class: "btn-run",
                                onclick: move |_| {
                                    let sid = sid_begin.clone();
                                    let md = invest_model();
                                    spawn(async move {
                                        match begin_investigation_run(&sid, &md).await {
                                            Some(rid) => poll_run_to_done(rid, active_run, uow_refresh).await,
                                            None => crate::toast::push_toast(
                                                toasts,
                                                crate::toast::ToastKind::Warning,
                                                "Could not begin the investigation run.".to_string(),
                                            ),
                                        }
                                    });
                                },
                                "▶ Begin investigation"
                            }
                            ModelSelect { models: models.clone(), selected: invest_model }
                        }
                        p { class: "section-hint", "Runs an investigation pass, then advances the stage to Investigating." }
                    }
                },
                // DECISIONS APPROVED → ready to run development: 3 tier selects + run.
                UowStage::DecisionsApproved => rsx! {
                    div { class: "uow-step-control",
                        p { class: "uow-step-h", "Development" }
                        div { class: "uow-tier-grid",
                            div { class: "uow-tier-field",
                                span { class: "uow-field-label", "Strongest" }
                                ModelSelect { models: models.clone(), selected: dev_strongest }
                            }
                            div { class: "uow-tier-field",
                                span { class: "uow-field-label", "Balanced" }
                                ModelSelect { models: models.clone(), selected: dev_balanced }
                            }
                            div { class: "uow-tier-field",
                                span { class: "uow-field-label", "Fast" }
                                ModelSelect { models: models.clone(), selected: dev_fast }
                            }
                        }
                        p { class: "section-hint", "The strongest tier orchestrates and delegates simpler work to the balanced and fast tiers." }
                        // One-time bootstrap escape hatch (default OFF, per-run). Skips ONLY
                        // layer-2; the security gate (layer 1) still applies.
                        label { class: "uow-bootstrap-toggle",
                            input {
                                r#type: "checkbox",
                                checked: bootstrap_skip_layer2(),
                                onchange: move |e| bootstrap_skip_layer2.set(e.checked()),
                            }
                            span { class: "uow-bootstrap-text",
                                span { class: "uow-bootstrap-label", "Bootstrap run — skip layer-2 checks" }
                                span { class: "uow-bootstrap-hint",
                                    "For the run that installs the linters/checkers layer-2 needs. The security gate (layer 1) still applies. Turn off afterward."
                                }
                            }
                        }
                        div { class: "run-control-row",
                            button {
                                class: "btn-run",
                                onclick: move |_| {
                                    let sid = sid_dev.clone();
                                    let tm = TierMapView {
                                        strongest: dev_strongest(),
                                        balanced: dev_balanced(),
                                        fast: dev_fast(),
                                    };
                                    let skip_l2 = bootstrap_skip_layer2();
                                    spawn(async move {
                                        match start_dev_run(&sid, &tm, skip_l2).await {
                                            StartRunOutcome::Started(rid) => {
                                                poll_run_to_done(rid, active_run, uow_refresh).await
                                            }
                                            StartRunOutcome::Blocked(reason) => crate::toast::push_toast(
                                                toasts,
                                                crate::toast::ToastKind::Warning,
                                                reason,
                                            ),
                                            StartRunOutcome::Failed => crate::toast::push_toast(
                                                toasts,
                                                crate::toast::ToastKind::Warning,
                                                "Could not start the governed development run.".to_string(),
                                            ),
                                        }
                                    });
                                },
                                "▶ Run development (governed)"
                            }
                        }
                    }
                },
                _ => rsx! {},
            }

            // Architect transition: Approve decisions (Investigating → DecisionsApproved).
            // Kept where it was — enabled only at the Investigating stage (the server
            // enforces this too; disabling avoids a guaranteed-409 click).
            div { class: "uow-lifecycle-actions",
                button {
                    // Transition action → the onboarding SECONDARY variant (bordered),
                    // distinct from the accent primary run buttons but on the same system.
                    class: "btn-secondary",
                    disabled: stage != UowStage::Investigating,
                    onclick: move |_| {
                        let sid = sid_approve.clone();
                        let mut uow_refresh = uow_refresh;
                        spawn(async move {
                            match post_uow_transition(&sid, "approve-decisions").await {
                                TransitionOutcome::Ok => { uow_refresh += 1; }
                                TransitionOutcome::Blocked(reason) => crate::toast::push_toast(
                                    toasts, crate::toast::ToastKind::Warning, reason,
                                ),
                                TransitionOutcome::Failed => crate::toast::push_toast(
                                    toasts,
                                    crate::toast::ToastKind::Warning,
                                    "Could not advance the lifecycle stage.".to_string(),
                                ),
                            }
                        });
                    },
                    "Approve decisions"
                }
            }
        }
    }
}

#[component]
fn UowPanel(story_id: String, uow_refresh: Signal<u32>) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let sid = story_id.clone();
    let uow_data = use_resource(move || {
        let sid = sid.clone();
        // Re-fetch when the shared tick bumps (e.g. after a sign-off) so the panel
        // reflects the latest sign-off / history without a manual reload.
        let _dep = uow_refresh();
        async move { fetch_uow(&sid).await }
    });

    let uow = uow_data.read().clone().flatten();
    let dev_status = uow.as_ref().map(|u| u.dev_status).unwrap_or_default();
    let branch = uow.as_ref().and_then(|u| u.branch.clone());
    let history = uow.as_ref().map(|u| u.history.clone()).unwrap_or_default();
    let sign_off = uow.as_ref().and_then(|u| u.sign_off.clone());
    let gate_provenance = uow.as_ref().and_then(|u| u.gate_provenance.clone());

    // The three status options for the segmented control.
    const STATUS_OPTS: &[DevStatus] = &[DevStatus::New, DevStatus::InProgress, DevStatus::Done];

    rsx! {
        div { class: "uow-panel",
            p { class: "uow-panel-h", "UNIT OF WORK" }

            // The governed-development lifecycle strip + per-phase run controls now
            // live with the steps in `UowStepRunControls` (rendered above this panel by
            // `UowDevControls`), so runs sit ON the steps. This panel keeps the
            // post-run read-out: dev status, branch, gate provenance, sign-off, history.

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

            // ── Frozen gate provenance (Pillar 2): the durable QA-review record ─
            // Stamped onto the UoW when a governed run finishes, so the honest gate
            // accounting survives even after the in-memory run is gone.
            if let Some(ref p) = gate_provenance {
                div { class: "uow-provenance",
                    span { class: "uow-field-label", "Gate provenance" }
                    span { class: "uow-provenance-val",
                        "run {p.run_id} ({p.mode}) — {p.allow_count} allowed, {p.deny_count} denied ({p.total_bounces} bounces)"
                    }
                    if !p.rules_fired.is_empty() {
                        span { class: "uow-provenance-rules",
                            "Bounced: {p.rules_fired.join(\", \")}"
                        }
                    }
                }
            }

            // ── Sign-off (issue #21): the architect's explicit approval, if any ─
            div { class: "uow-signoff-row",
                span { class: "uow-field-label", "Sign-off" }
                if let Some(ref so) = sign_off {
                    span { class: "uow-signoff-val", "✓ {so.by} · run {so.run_id} · {so.ts}" }
                } else {
                    span { class: "uow-signoff-none", "not signed off" }
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


// ── Docs view ─────────────────────────────────────────────────────────────────

// ── pw/cockpit-ui product wave ────────────────────────────────────────────────
//
// Features 2–5 of the last product wave. All are purely client-side additions;
// the server endpoints they call are part of the companion pw/server-features wave.
// Endpoints are called optimistically and degrade gracefully (None/empty) when the
// server hasn't shipped the new routes yet.
//
//  Feature 2: App-update banner + applied-rule drift notice + "update this rule".
//  Feature 3: Single-rule editing scoped to project AND repo.
//  Feature 4: Deep-report export button (Markdown surfaced in a modal).
//  Feature 5: Feature-flag awareness (GET /api/feature-flags) gates SOC-2 UI.

// ── Feature 5: Feature flags ──────────────────────────────────────────────────

/// Feature-flag map returned by `GET /api/feature-flags`.
/// Keys are flag names; values are booleans.
/// A missing flag is treated as `false` (conservative default: the feature is off).
#[derive(Clone, PartialEq, serde::Deserialize, Default)]
struct FeatureFlagMap {
    /// SOC-2 gap analysis section in the deep-tier results. When `false`, the
    /// SOC-2 gap table and the SOC-2 portion of the deep-export are hidden.
    #[serde(default)]
    soc2: bool,
    /// Flat catch-all for any flag the UI hasn't explicitly modelled yet.
    #[serde(flatten)]
    extra: std::collections::HashMap<String, serde_json::Value>,
}

/// Fetch the current feature-flag state from the server. Returns the default map
/// (all flags off) when the endpoint is unreachable, so older server versions
/// don't break the UI.
async fn fetch_feature_flags() -> FeatureFlagMap {
    let result: Option<FeatureFlagMap> = async {
        reqwest::get(format!("{}/api/feature-flags", crate::BFF_URL))
            .await
            .ok()?
            .json::<FeatureFlagMap>()
            .await
            .ok()
    }
    .await;
    result.unwrap_or_default()
}

// ── Feature 2: App-update banner + rule-drift notice ─────────────────────────

/// Minimum release info returned by `GET /api/release`.
/// The server checks the latest GitHub release tag and reports whether the running
/// binary is behind.
#[derive(Clone, PartialEq, serde::Deserialize)]
struct AppReleaseView {
    /// Version string of the running binary (e.g. `"0.4.1"`).
    current: String,
    /// Latest published release tag (e.g. `"0.4.2"`). `None` when the check
    /// hasn't run yet or the GitHub API was unreachable.
    #[serde(default)]
    latest: Option<String>,
    /// True when `latest > current` (server-side semver compare).
    #[serde(default)]
    update_available: bool,
    /// HTML / Markdown release notes for `latest` (empty when not available).
    #[serde(default)]
    release_notes: String,
}

async fn fetch_app_release() -> Option<AppReleaseView> {
    reqwest::get(format!("{}/api/release", crate::BFF_URL))
        .await
        .ok()?
        .json::<AppReleaseView>()
        .await
        .ok()
}

/// App-update banner. Shown across the top of every cockpit tab when the server
/// reports a newer release. Dismissible within the session (not persisted).
#[component]
fn AppUpdateBanner() -> Element {
    let release_res = use_resource(fetch_app_release);
    let mut dismissed = use_signal(|| false);

    let Some(rel) = release_res.read().clone().flatten() else {
        return rsx! {};
    };
    if !rel.update_available || dismissed() {
        return rsx! {};
    }
    let latest = rel.latest.clone().unwrap_or_default();
    let current = rel.current.clone();
    rsx! {
        div { class: "app-update-banner",
            span { class: "app-update-icon", "\u{2B06}" }
            span { class: "app-update-text",
                "Camerata {latest} is available (you are running {current}). "
                if !rel.release_notes.is_empty() {
                    span { class: "app-update-notes", "{rel.release_notes}" }
                }
            }
            a {
                class: "app-update-link",
                href: "https://github.com/zernst3/camerata-orchestrator/releases",
                target: "_blank",
                "View release"
            }
            button {
                class: "app-update-dismiss",
                onclick: move |_| dismissed.set(true),
                "\u{00D7}"
            }
        }
    }
}

/// One applied-rule drift entry from `GET /api/projects/:id/rule-drift`.
/// Reports a rule that is applied to the project but whose corpus version has
/// changed since it was adopted (corpus body updated after grounding/verification).
#[derive(Clone, PartialEq, serde::Deserialize)]
struct RuleDriftEntry {
    rule_id: String,
    #[serde(default)]
    title: String,
    /// The text of the directive as it was when the rule was adopted.
    #[serde(default)]
    applied_directive: String,
    /// The current corpus directive (the update the architect is being asked to review).
    #[serde(default)]
    corpus_directive: String,
    /// Repos in the project that currently have the stale directive.
    #[serde(default)]
    repos: Vec<String>,
}

async fn fetch_rule_drift(project_id: &str) -> Option<Vec<RuleDriftEntry>> {
    let v: serde_json::Value = reqwest::get(format!(
        "{}/api/projects/{}/rule-drift",
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
    serde_json::from_value(v.get("drift").cloned()?).ok()
}

/// Apply a corpus-updated directive to a project rule (calls the update endpoint).
/// The server re-emits the governance files for the affected repos.
async fn apply_rule_drift_update(project_id: &str, rule_id: &str) -> bool {
    reqwest::Client::new()
        .post(format!(
            "{}/api/projects/{}/rule-drift/{}/accept",
            crate::BFF_URL,
            project_id,
            rule_id
        ))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// The drift-notice panel: shown in the Rules view whenever the active project has
/// rules whose corpus version has changed. Each entry shows an inline diff and an
/// "Update this rule" action.
#[component]
fn RuleDriftNotice(project_id: String) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let refresh = use_signal(|| 0u32);
    let pid = project_id.clone();
    let drift_res = use_resource(move || {
        let pid = pid.clone();
        let _ = refresh();
        async move { fetch_rule_drift(&pid).await }
    });
    let drift = drift_res.read().clone().flatten().unwrap_or_default();
    if drift.is_empty() {
        return rsx! {};
    }

    // Which entry's diff is currently expanded.
    let expanded: Signal<Option<String>> = use_signal(|| None);

    rsx! {
        div { class: "drift-notice",
            div { class: "drift-notice-header",
                span { class: "drift-notice-icon", "\u{26A0}" }
                span { class: "drift-notice-title",
                    "{drift.len()} applied rule(s) have corpus updates"
                }
                p { class: "drift-notice-hint",
                    "These rules were adopted before their corpus entry was updated. \
                     Review the diff and accept the update to keep your governance in sync."
                }
            }
            for entry in drift.iter() {
                {
                    let rule_id = entry.rule_id.clone();
                    let rule_id_exp = rule_id.clone();
                    let rule_id_update = rule_id.clone();
                    let pid_update = project_id.clone();
                    let is_expanded = expanded.read().as_deref() == Some(&rule_id);
                    let entry = entry.clone();
                    rsx! {
                        div { class: "drift-entry", key: "{rule_id}",
                            div { class: "drift-entry-head",
                                span { class: "drift-entry-id", "{rule_id}" }
                                if !entry.title.is_empty() {
                                    span { class: "drift-entry-title", " — {entry.title}" }
                                }
                                if !entry.repos.is_empty() {
                                    span { class: "drift-entry-repos", "repos: {entry.repos.join(\", \")}" }
                                }
                                button {
                                    class: "btn-edit-sm",
                                    onclick: move |_| {
                                        let mut exp = expanded;
                                        if exp.read().as_deref() == Some(&rule_id_exp) {
                                            exp.set(None);
                                        } else {
                                            exp.set(Some(rule_id_exp.clone()));
                                        }
                                    },
                                    if is_expanded { "Hide diff" } else { "Show diff" }
                                }
                                button {
                                    class: "btn-run drift-update-btn",
                                    onclick: move |_| {
                                        let pid = pid_update.clone();
                                        let rid = rule_id_update.clone();
                                        let mut refresh = refresh;
                                        spawn(async move {
                                            if apply_rule_drift_update(&pid, &rid).await {
                                                crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("{rid}: updated to current corpus version."));
                                                refresh += 1;
                                            } else {
                                                crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, format!("{rid}: update failed — check server logs."));
                                            }
                                        });
                                    },
                                    "Update this rule"
                                }
                            }
                            if is_expanded {
                                div { class: "drift-diff",
                                    div { class: "drift-diff-col drift-diff-old",
                                        p { class: "drift-diff-label", "Applied (current)" }
                                        pre { class: "drift-diff-body", "{entry.applied_directive}" }
                                    }
                                    div { class: "drift-diff-col drift-diff-new",
                                        p { class: "drift-diff-label", "Corpus (update)" }
                                        pre { class: "drift-diff-body", "{entry.corpus_directive}" }
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

// ── Feature 3: Single-rule editor ────────────────────────────────────────────

/// The scope at which a single-rule edit applies. Rules cascade: repo overrides
/// project which overrides the corpus default. The editor lets the architect pick
/// which level to write to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RuleEditScope {
    /// Edit the project-level rule selection (applies to all repos in the project).
    Project,
    /// Edit a repo-specific override (applies to one repo, overrides the project value).
    Repo,
}

/// Fetch the current state of a single rule for a project (project-level option + any
/// repo overrides) from `GET /api/projects/:id/rules/:rule_id`.
/// Retained as the documented server contract; the UI currently opens the editor
/// without pre-fetching (the data is already in `ProjectView`).
#[allow(dead_code)]
async fn fetch_single_rule(project_id: &str, rule_id: &str) -> Option<serde_json::Value> {
    reqwest::get(format!(
        "{}/api/projects/{}/rules/{}",
        crate::BFF_URL,
        project_id,
        rule_id
    ))
    .await
    .ok()?
    .json::<serde_json::Value>()
    .await
    .ok()
}

/// Persist a single-rule edit. Scope determines whether the edit goes to the project
/// level or a specific repo override.
///
/// Body shape: `{ "chosen_option": "opt-id", "scope": "project"|"repo", "repo": "owner/repo" }`
async fn save_single_rule_edit(
    project_id: &str,
    rule_id: &str,
    chosen_option: &str,
    scope: RuleEditScope,
    repo: Option<&str>,
) -> bool {
    let body = serde_json::json!({
        "chosen_option": chosen_option,
        "scope": match scope {
            RuleEditScope::Project => "project",
            RuleEditScope::Repo => "repo",
        },
        "repo": repo,
    });
    reqwest::Client::new()
        .post(format!(
            "{}/api/projects/{}/rules/{}",
            crate::BFF_URL,
            project_id,
            rule_id
        ))
        .json(&body)
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Single-rule editor. Shown in the Rules view when the architect selects a corpus rule
/// from the applied-rules table and clicks "Edit rule". Lets them choose the option at
/// project scope OR override it for one specific repo.
///
/// The scope cascade: repo override > project selection > corpus default.
/// Writing to "Repo" creates a repo-local override that silently wins over the
/// project-level choice for that repo; writing to "Project" updates the project
/// selection which applies to every repo that doesn't have a repo override.
#[component]
fn SingleRuleEditor(
    project: ProjectView,
    rule: ProposedRuleView,
    on_close: EventHandler<()>,
    on_saved: EventHandler<()>,
) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let mut scope = use_signal(|| RuleEditScope::Project);
    let mut repo_target = use_signal(String::new);
    let chosen = use_signal(|| rule.default_option.clone().unwrap_or_default());
    let mut saving = use_signal(|| false);

    let (vbadge_label, vbadge_cls) = verif_badge(&rule.verification);

    rsx! {
        div { class: "single-rule-editor-overlay", onclick: move |_| on_close.call(()),
            div { class: "single-rule-editor", onclick: move |e| e.stop_propagation(),
                div { class: "single-rule-editor-head",
                    div { class: "single-rule-editor-id-row",
                        span { class: "rule-modal-id", "{rule.id}" }
                        span {
                            class: "verif-badge verif-badge-{vbadge_cls}",
                            "{vbadge_label}"
                        }
                        button {
                            class: "rule-modal-close",
                            onclick: move |_| on_close.call(()),
                            "\u{00D7}"
                        }
                    }
                    p { class: "rule-modal-title", "{rule.title}" }
                }

                div { class: "single-rule-editor-body",
                    // Scope picker: Project (apply to all repos) vs Repo (override for one repo).
                    div { class: "single-rule-scope",
                        p { class: "section-label", "Edit scope" }
                        p { class: "section-hint",
                            "Project scope updates the project-level selection (all repos). \
                             Repo scope creates or overwrites a repo-specific override that silently \
                             wins over the project value for that repo."
                        }
                        div { class: "single-rule-scope-btns",
                            button {
                                class: if scope() == RuleEditScope::Project { "scope-btn active" } else { "scope-btn" },
                                onclick: move |_| scope.set(RuleEditScope::Project),
                                "Project (all repos)"
                            }
                            button {
                                class: if scope() == RuleEditScope::Repo { "scope-btn active" } else { "scope-btn" },
                                onclick: move |_| scope.set(RuleEditScope::Repo),
                                "Repo override"
                            }
                        }
                        if scope() == RuleEditScope::Repo {
                            div { class: "single-rule-repo-row",
                                label { class: "single-rule-repo-label", "Target repo" }
                                select {
                                    class: "repo-select-input",
                                    value: "{repo_target}",
                                    onchange: move |e| repo_target.set(e.value()),
                                    option { value: "", "Choose a repo…" }
                                    for r in project.repos.iter() {
                                        option { key: "{r}", value: "{r}", "{r}" }
                                    }
                                }
                            }
                        }
                    }

                    // Option picker (if the rule has alternatives).
                    if rule.options.is_empty() {
                        p { class: "section-hint", "Single-variant rule — no alternatives to choose. Editing the scope or using Apply/Emit re-arms it as-is." }
                    } else {
                        div { class: "single-rule-options",
                            p { class: "section-label", "Choose the alternative" }
                            if rule.default_option.is_none() {
                                p { class: "rule-modal-mustchoose", "No default — you must choose an alternative." }
                            }
                            div { class: "rule-modal-opts",
                                for o in rule.options.iter() {
                                    {
                                        let oid = o.id.clone();
                                        let cur = chosen();
                                        let picked = cur == o.id;
                                        let is_default = rule.default_option.as_deref() == Some(o.id.as_str());
                                        let cls = if picked { "rule-opt on" } else { "rule-opt" };
                                        let mut chosen = chosen;
                                        rsx! {
                                            button {
                                                key: "{o.id}",
                                                class: "{cls}",
                                                onclick: move |_| chosen.set(oid.clone()),
                                                div { class: "rule-opt-head",
                                                    span { class: "rule-opt-label", "{o.label}" }
                                                    if is_default {
                                                        span { class: "rule-opt-default-badge", "default" }
                                                    }
                                                    if picked {
                                                        span { class: "rule-opt-picked-badge", "\u{2713} selected" }
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

                div { class: "single-rule-editor-actions",
                    button {
                        class: "btn-restart",
                        onclick: move |_| on_close.call(()),
                        "Cancel"
                    }
                    button {
                        class: "btn-run",
                        disabled: saving() || (scope() == RuleEditScope::Repo && repo_target().is_empty()),
                        title: if scope() == RuleEditScope::Repo && repo_target().is_empty() {
                            "Choose a target repo for the repo override"
                        } else { "" },
                        onclick: move |_| {
                            let pid = project.id.clone();
                            let rid = rule.id.clone();
                            let opt = chosen();
                            let sc = scope();
                            let repo = if sc == RuleEditScope::Repo { Some(repo_target()) } else { None };
                            saving.set(true);
                            spawn(async move {
                                if save_single_rule_edit(
                                    &pid, &rid, &opt, sc,
                                    repo.as_deref()
                                ).await {
                                    crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("{rid}: saved."));
                                    on_saved.call(());
                                } else {
                                    crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, format!("{rid}: save failed."));
                                }
                                saving.set(false);
                            });
                        },
                        if saving() { "Saving\u{2026}" } else { "Save" }
                    }
                }
            }
        }
    }
}

// ── Feature 4: Deep-report export ────────────────────────────────────────────

/// Fetch and return the Markdown deep report for the active project from
/// `GET /api/projects/:id/deep-report`. Returns the Markdown string on success.
/// The `soc2` parameter controls whether the SOC-2 section is included.
async fn fetch_deep_report(project_id: &str, include_soc2: bool) -> Option<String> {
    let url = format!(
        "{}/api/projects/{}/deep-report?include_soc2={}",
        crate::BFF_URL,
        project_id,
        include_soc2
    );
    let resp = reqwest::get(url).await.ok()?;
    if resp.status().is_success() {
        resp.text().await.ok()
    } else {
        None
    }
}

/// The deep-report export panel: a single button that calls the export endpoint and
/// shows the resulting Markdown in a scrollable modal. The SOC-2 section is only
/// included when the `soc2` feature flag is on (Feature 5 gate).
///
/// Placed in the Onboard view after the audit findings, below the deep-tier results.
#[component]
fn DeepReportExportPanel(project_id: String, soc2_enabled: bool) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let mut loading = use_signal(|| false);
    let mut report_md: Signal<Option<String>> = use_signal(|| None);

    rsx! {
        div { class: "deep-export-panel",
            p { class: "section-label", "Export deep compliance report" }
            p { class: "section-hint",
                "Downloads the full deep-tier analysis as a Markdown document. \
                 Includes deep security findings and threat model."
                if soc2_enabled {
                    " SOC-2 gap analysis is also included."
                }
                if !soc2_enabled {
                    " SOC-2 gap analysis is disabled for this workspace (feature flag off)."
                }
            }
            button {
                class: "btn-run",
                disabled: loading(),
                onclick: move |_| {
                    let pid = project_id.clone();
                    let include_soc2 = soc2_enabled;
                    loading.set(true);
                    spawn(async move {
                        match fetch_deep_report(&pid, include_soc2).await {
                            Some(md) => report_md.set(Some(md)),
                            None => crate::toast::push_toast(
                                toasts,
                                crate::toast::ToastKind::Error,
                                "Deep report export failed — run an audit with deep tier enabled first.",
                            ),
                        }
                        loading.set(false);
                    });
                },
                if loading() { "Exporting\u{2026}" } else { "Export deep report (Markdown)" }
            }
            if let Some(md) = report_md.read().clone() {
                div { class: "deep-export-modal-overlay",
                    onclick: move |_| report_md.set(None),
                    div { class: "deep-export-modal",
                        onclick: move |e| e.stop_propagation(),
                        div { class: "deep-export-modal-head",
                            p { class: "deep-export-modal-title", "Deep compliance report" }
                            button {
                                class: "rule-modal-close",
                                onclick: move |_| report_md.set(None),
                                "\u{00D7}"
                            }
                        }
                        textarea {
                            class: "deep-export-body",
                            readonly: true,
                            value: "{md}",
                        }
                        button {
                            class: "btn-edit-sm",
                            onclick: move |_| {
                                let md_copy = md.clone();
                                spawn(async move {
                                    let _ = save_csv("camerata-deep-report.md", md_copy).await;
                                });
                            },
                            "Save to file"
                        }
                    }
                }
            }
        }
    }
}

// ── In-app documentation viewer ───────────────────────────────────────────────

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

#[cfg(test)]
mod tests {
    use super::{
        dev_run_body, estimate_audit_cost, is_enforced_floor, FindingView, TierMapView,
    };

    /// The development-run body must match the frozen backend contract exactly:
    /// `{ "tier_map": { "strongest", "balanced", "fast" } }`.
    #[test]
    fn dev_run_body_matches_frozen_contract() {
        let tm = TierMapView {
            strongest: "opus-x".to_string(),
            balanced: "sonnet-x".to_string(),
            fast: "haiku-x".to_string(),
        };
        let body = dev_run_body(&tm, false);
        let tier = body.get("tier_map").expect("tier_map key present");
        assert_eq!(tier.get("strongest").unwrap(), "opus-x");
        assert_eq!(tier.get("balanced").unwrap(), "sonnet-x");
        assert_eq!(tier.get("fast").unwrap(), "haiku-x");
        // Exactly the three tier keys, nothing else.
        assert_eq!(tier.as_object().unwrap().len(), 3);
        // Default (skip_layer2 = false): body is exactly today's — just tier_map, no flag.
        assert_eq!(body.as_object().unwrap().len(), 1);
        assert!(body.get("skip_layer2").is_none());
    }

    /// The bootstrap toggle adds `skip_layer2: true` to the body ONLY when on, and never
    /// when off (so a normal run is byte-for-byte the existing contract).
    #[test]
    fn dev_run_body_includes_skip_layer2_only_when_on() {
        let tm = TierMapView {
            strongest: "opus-x".to_string(),
            balanced: "sonnet-x".to_string(),
            fast: "haiku-x".to_string(),
        };
        // OFF: no flag at all.
        let off = dev_run_body(&tm, false);
        assert!(off.get("skip_layer2").is_none(), "off must omit the flag");

        // ON: the flag is present and true; tier_map is unchanged.
        let on = dev_run_body(&tm, true);
        assert_eq!(on.get("skip_layer2").unwrap(), &serde_json::Value::Bool(true));
        let tier = on.get("tier_map").expect("tier_map still present");
        assert_eq!(tier.get("strongest").unwrap(), "opus-x");
        // Exactly tier_map + skip_layer2.
        assert_eq!(on.as_object().unwrap().len(), 2);
    }

    /// The default TierMapView feeds the per-phase model defaults. Investigation defaults
    /// to the strongest tier; the dev-run body carries all three.
    #[test]
    fn default_tier_map_seeds_all_three_tiers() {
        let tm = TierMapView::default();
        assert!(!tm.strongest.is_empty());
        assert!(!tm.balanced.is_empty());
        assert!(!tm.fast.is_empty());
        let body = dev_run_body(&tm, false);
        let tier = body.get("tier_map").unwrap();
        assert_eq!(tier.get("strongest").unwrap(), &tm.strongest);
    }

    /// Sequential mode (1 batch per chunk) has no caching reuse across batches — the
    /// estimate must match the pre-caching math (full digest price every pass).
    #[test]
    fn sequential_mode_no_cache_discount() {
        // Small repo: 100k chars, 0 rules, sequential.
        let (toks, dollars, passes) =
            estimate_audit_cost(100_000, 0, "sequential", 3.0, 15.0, 3.0, 15.0, false, false, false);
        assert_eq!(passes, 1, "0 rules + sequential = one pass");
        assert!(toks > 0, "some tokens");
        assert!(dollars > 0.0, "some cost");
    }

    /// Parallel mode with multiple batches should cost LESS than the naive per-batch full
    /// price because subsequent batches read the digest from cache at ~0.1×.
    #[test]
    fn parallel_multi_batch_cheaper_than_sequential_sum() {
        // 30 rules -> ceil(30/15)=2 batches; 350k chars = 1 chunk.
        let (_, dollars_parallel, passes_parallel) =
            estimate_audit_cost(350_000, 30, "parallel", 3.0, 15.0, 3.0, 15.0, false, false, false);
        assert_eq!(passes_parallel, 2, "2 batches for 30 rules");

        // If we ran sequential with 30 rules we get 1 pass; run twice to simulate
        // the naive "pay full price twice" baseline.
        let (_, dollars_seq_single, _) =
            estimate_audit_cost(350_000, 30, "sequential", 3.0, 15.0, 3.0, 15.0, false, false, false);
        let naive_two_passes = dollars_seq_single * 2.0;

        assert!(
            dollars_parallel < naive_two_passes,
            "caching makes 2 parallel batches cheaper than naive 2× sequential: {dollars_parallel:.4} < {naive_two_passes:.4}"
        );
    }

    /// Single-batch parallel (1 rule, or 0 rules) has nothing to cache — no second batch
    /// to amortise over, so the discount path is not taken.
    #[test]
    fn parallel_single_batch_no_discount() {
        // 1 rule -> 1 batch in parallel mode.
        let (toks1, dollars1, passes1) =
            estimate_audit_cost(350_000, 1, "parallel", 3.0, 15.0, 3.0, 15.0, false, false, false);
        let (toks_seq, dollars_seq, passes_seq) =
            estimate_audit_cost(350_000, 1, "sequential", 3.0, 15.0, 3.0, 15.0, false, false, false);
        assert_eq!(passes1, 1);
        assert_eq!(passes_seq, 1);
        // Token counts should be in the same ballpark (both are 1 pass over the same chunk).
        // The cache-write surcharge on the parallel path makes it *slightly* higher than
        // sequential, but they should be within 30% of each other.
        let ratio = toks1 as f64 / toks_seq as f64;
        assert!(
            ratio < 1.3,
            "single-batch parallel not much more expensive than sequential: ratio={ratio:.2}"
        );
        let _ = (dollars1, dollars_seq); // exercise the values without asserting exact amounts
    }

    /// Thorough mode triples the calibration cost; the estimate should grow accordingly.
    #[test]
    fn thorough_mode_costs_more_than_default() {
        let (_, dollars_default, _) =
            estimate_audit_cost(200_000, 15, "parallel", 3.0, 15.0, 1.0, 5.0, false, false, false);
        let (_, dollars_thorough, _) =
            estimate_audit_cost(200_000, 15, "parallel", 3.0, 15.0, 1.0, 5.0, true, false, false);
        assert!(
            dollars_thorough > dollars_default,
            "thorough costs more: {dollars_thorough:.4} > {dollars_default:.4}"
        );
    }

    /// Batch mode applies a flat 50% discount to the SCAN passes vs. parallel on the same
    /// config. Calibration is NOT discounted (it always runs real-time). The pass count
    /// is identical (same chunking + rule-batching).
    #[test]
    fn batch_mode_cheaper_than_parallel_due_to_scan_discount() {
        // 30 rules, 350k chars = 1 chunk, 2 rule-batches. Calibration = same model.
        let (_, dollars_parallel, passes_parallel) =
            estimate_audit_cost(350_000, 30, "parallel", 3.0, 15.0, 3.0, 15.0, false, false, false);
        let (_, dollars_batch, passes_batch) =
            estimate_audit_cost(350_000, 30, "batch", 3.0, 15.0, 3.0, 15.0, false, false, false);
        assert_eq!(
            passes_parallel, passes_batch,
            "same pass count in parallel and batch (only pricing differs)"
        );
        // Batch must be cheaper than parallel (scan discount applied), but the ratio is
        // not exactly 0.5 because calibration is priced at full rate in both modes.
        assert!(
            dollars_batch < dollars_parallel,
            "batch is cheaper than parallel: {dollars_batch:.4} < {dollars_parallel:.4}"
        );
        // The discount is at least 25% overall (scan dominates in a 2-batch, 1-chunk case).
        let ratio = dollars_batch / dollars_parallel;
        assert!(
            ratio < 0.75,
            "batch should be at least 25% cheaper than parallel: ratio={ratio:.4}"
        );
    }

    /// Batch mode with 0 rules (free-form, 1 pass per chunk): calibration cost is
    /// identical in both modes; scan cost is halved. Total must be cheaper in batch mode.
    #[test]
    fn batch_mode_zero_rules_cheaper_than_parallel() {
        let (_, dollars_parallel, _) =
            estimate_audit_cost(200_000, 0, "parallel", 3.0, 15.0, 3.0, 15.0, false, false, false);
        let (_, dollars_batch, _) =
            estimate_audit_cost(200_000, 0, "batch", 3.0, 15.0, 3.0, 15.0, false, false, false);
        assert!(
            dollars_batch < dollars_parallel,
            "batch cheaper even with 0 rules: {dollars_batch:.4} < {dollars_parallel:.4}"
        );
    }

    /// Deep tier (three extra whole-repo passes) must ADD to the dollar figure, and it must be
    /// the single priciest option vs. thorough or full-vs-incremental on the same config.
    #[test]
    fn deep_tier_costs_more_and_is_the_priciest_option() {
        let base = |deep: bool, thorough: bool| {
            estimate_audit_cost(350_000, 30, "parallel", 3.0, 15.0, 3.0, 15.0, thorough, false, deep).1
        };
        let standard = base(false, false);
        let thorough = base(false, true);
        let deep = base(true, false);
        assert!(deep > standard, "deep adds cost: {deep:.4} > {standard:.4}");
        assert!(
            deep > thorough,
            "deep is the priciest option (more than thorough): {deep:.4} > {thorough:.4}"
        );
    }

    /// The incremental flag is plumbed through but, with no changed-file breakdown available
    /// client-side, prices the same full-scan figure (over-estimate by design). It must not
    /// blow up the estimate and must equal the full-scan number for the same inputs.
    #[test]
    fn incremental_flag_prices_same_as_full_today() {
        let full =
            estimate_audit_cost(350_000, 30, "parallel", 3.0, 15.0, 3.0, 15.0, false, false, false);
        let incremental =
            estimate_audit_cost(350_000, 30, "parallel", 3.0, 15.0, 3.0, 15.0, false, true, false);
        assert_eq!(
            full.1, incremental.1,
            "incremental prices the full set today (no changed-file data): {} vs {}",
            full.1, incremental.1
        );
    }

    // ── selection_key() unit tests ────────────────────────────────────────────

    use super::{selection_key, SINGLE_REPO_SELECTION_KEY};

    #[test]
    fn selection_key_empty_view_repo_uses_sentinel() {
        assert_eq!(selection_key(""), SINGLE_REPO_SELECTION_KEY);
    }

    #[test]
    fn selection_key_named_repo_is_passthrough() {
        assert_eq!(selection_key("owner/repo"), "owner/repo");
    }

    #[test]
    fn selection_key_sentinel_cannot_collide_with_a_real_repo() {
        // A real `owner/repo` never contains a NUL byte, so the sentinel can't be a repo name.
        assert!(SINGLE_REPO_SELECTION_KEY.contains('\u{0}'));
        assert_ne!(selection_key("a/b"), SINGLE_REPO_SELECTION_KEY);
    }

    // ── verif_badge() unit tests ──────────────────────────────────────────────
    //
    // Pure function, no DOM — each test just asserts label + CSS modifier.
    // Coverage: all four canonical values + an unknown value (falls back to draft).

    use super::{verif_badge, verif_sources_tooltip, RuleSourceView};

    #[test]
    fn verif_badge_verified_returns_checkmark_label_and_green_class() {
        let (label, cls) = verif_badge("verified");
        assert!(label.contains("Verified"), "label should mention Verified, got: {label}");
        assert_eq!(cls, "verified");
    }

    #[test]
    fn verif_badge_grounded_returns_grounded_label_and_blue_class() {
        let (label, cls) = verif_badge("grounded");
        assert!(label.contains("Grounded"), "label should mention Grounded, got: {label}");
        // Grounded must carry its own distinct symbol (the circled source-dot), separate from
        // the verified checkmark, so it's a clear table status not a faint tint.
        assert!(label.contains('\u{29bf}'), "grounded label should carry its source-dot symbol");
        assert!(!label.contains('\u{2713}'), "grounded must NOT reuse the verified checkmark");
        assert_eq!(cls, "grounded");
    }

    #[test]
    fn verif_badge_draft_returns_draft_label_and_gray_class() {
        let (label, cls) = verif_badge("draft");
        assert_eq!(label, "Draft");
        assert_eq!(cls, "draft");
    }

    #[test]
    fn verif_badge_needs_recheck_returns_distinct_label_and_class() {
        let (label, cls) = verif_badge("needs_recheck");
        assert!(label.contains("re-check") || label.contains("recheck"), "label should signal re-check, got: {label}");
        assert_eq!(cls, "needs-recheck");
    }

    #[test]
    fn verif_badge_unknown_value_falls_back_to_draft() {
        // An unrecognised value (e.g. a future extension the UI hasn't caught up to)
        // must not panic and must fall back to the `draft` visual.
        let (label, cls) = verif_badge("something_new");
        assert_eq!(label, "Draft");
        assert_eq!(cls, "draft");
    }

    #[test]
    fn verif_sources_tooltip_empty_sources_returns_empty_string() {
        assert_eq!(verif_sources_tooltip(&[]), "");
    }

    #[test]
    fn verif_sources_tooltip_with_linter_includes_bracket_suffix() {
        let sources = vec![RuleSourceView {
            url: "https://example.com".to_string(),
            title: "Example rule".to_string(),
            linter: Some("eslint: no-unused-vars".to_string()),
        }];
        let tip = verif_sources_tooltip(&sources);
        assert!(tip.contains("Example rule"), "title absent: {tip}");
        assert!(tip.contains("eslint: no-unused-vars"), "linter absent: {tip}");
    }

    #[test]
    fn verif_sources_tooltip_without_linter_uses_title_only() {
        let sources = vec![RuleSourceView {
            url: "https://example.com".to_string(),
            title: "Google Style Guide".to_string(),
            linter: None,
        }];
        let tip = verif_sources_tooltip(&sources);
        assert_eq!(tip, "Google Style Guide");
    }

    #[test]
    fn verif_sources_tooltip_multiple_sources_joined_with_separator() {
        let sources = vec![
            RuleSourceView { url: "u1".to_string(), title: "Source A".to_string(), linter: None },
            RuleSourceView { url: "u2".to_string(), title: "Source B".to_string(), linter: Some("tool: rule".to_string()) },
        ];
        let tip = verif_sources_tooltip(&sources);
        // Both titles must appear; the separator is " · " (middle dot).
        assert!(tip.contains("Source A"), "Source A missing: {tip}");
        assert!(tip.contains("Source B"), "Source B missing: {tip}");
        assert!(tip.contains(" · "), "separator missing: {tip}");
    }

    // ── pw/cockpit-ui: Feature 1 — effective_auto_recommended ─────────────────
    // Tests for the `ProposedRuleView::effective_auto_recommended()` helper which
    // is the single truth-gate for pre-checking a proposed rule during onboarding.

    use super::{FeatureFlagMap, ProposedRuleView};

    fn make_proposed_rule(recommended: bool, verification: &str, is_auto_recommended: bool) -> ProposedRuleView {
        ProposedRuleView {
            id: "TEST-1".to_string(),
            title: "Test rule".to_string(),
            kind: "structural".to_string(),
            enforcement: "error".to_string(),
            options: vec![],
            default_option: None,
            decision_question: None,
            decision_why: None,
            scope: "repo".to_string(),
            domain: "test".to_string(),
            repos: vec![],
            placement: "AGENTS.md".to_string(),
            finding_count: 0,
            recommended,
            is_auto_recommended,
            verification: verification.to_string(),
            sources: vec![],
        }
    }

    /// If the server sets `is_auto_recommended = true`, the rule is pre-checked
    /// regardless of the verification level.
    #[test]
    fn effective_auto_recommended_server_flag_wins() {
        let r = make_proposed_rule(false, "draft", true);
        assert!(r.effective_auto_recommended(), "server flag should override draft");
    }

    /// Backward-compat path: when `is_auto_recommended` is false (old server),
    /// the method falls back to `recommended && (grounded | verified)`.
    #[test]
    fn effective_auto_recommended_fallback_grounded() {
        let r = make_proposed_rule(true, "grounded", false);
        assert!(r.effective_auto_recommended(), "grounded + recommended should be pre-checked");
    }

    #[test]
    fn effective_auto_recommended_fallback_verified() {
        let r = make_proposed_rule(true, "verified", false);
        assert!(r.effective_auto_recommended(), "verified + recommended should be pre-checked");
    }

    /// Draft rules that are recommended but not yet grounded should NOT be
    /// pre-checked in backward-compat mode (old server).
    #[test]
    fn effective_auto_recommended_fallback_draft_not_pre_checked() {
        let r = make_proposed_rule(true, "draft", false);
        assert!(!r.effective_auto_recommended(), "draft rules should not be pre-checked");
    }

    /// `needs_recheck` is not pre-checked in the backward-compat path.
    #[test]
    fn effective_auto_recommended_fallback_needs_recheck_not_pre_checked() {
        let r = make_proposed_rule(true, "needs_recheck", false);
        assert!(!r.effective_auto_recommended(), "needs_recheck rules should not be pre-checked");
    }

    /// Not recommended at all: must not be pre-checked even if grounded.
    #[test]
    fn effective_auto_recommended_fallback_grounded_not_recommended() {
        let r = make_proposed_rule(false, "grounded", false);
        assert!(!r.effective_auto_recommended(), "grounded-but-not-recommended must not be pre-checked");
    }

    // ── pw/cockpit-ui: Feature 5 — FeatureFlagMap deserialization ─────────────
    // The feature flag map uses `#[serde(flatten)]` to absorb unknown future flags.
    // These tests confirm the known `soc2` field parses correctly and unknown
    // keys are absorbed without error.

    #[test]
    fn feature_flag_map_soc2_true() {
        let json = r#"{"soc2": true}"#;
        let m: FeatureFlagMap = serde_json::from_str(json).unwrap();
        assert!(m.soc2, "soc2 should be true");
    }

    #[test]
    fn feature_flag_map_soc2_false() {
        let json = r#"{"soc2": false}"#;
        let m: FeatureFlagMap = serde_json::from_str(json).unwrap();
        assert!(!m.soc2, "soc2 should be false");
    }

    #[test]
    fn feature_flag_map_defaults_to_false_when_key_absent() {
        let json = r#"{}"#;
        let m: FeatureFlagMap = serde_json::from_str(json).unwrap();
        assert!(!m.soc2, "soc2 should default to false when absent");
    }

    #[test]
    fn feature_flag_map_extra_keys_do_not_error() {
        let json = r#"{"soc2": true, "future_flag": true, "another": 42}"#;
        let m: FeatureFlagMap = serde_json::from_str(json).unwrap();
        assert!(m.soc2, "soc2 should still parse when extra keys present");
        assert_eq!(m.extra.len(), 2, "extra keys should be absorbed into the extra map");
    }

    #[test]
    fn feature_flag_map_default_impl_all_flags_off() {
        let m = FeatureFlagMap::default();
        assert!(!m.soc2, "default FeatureFlagMap should have all flags off");
        assert!(m.extra.is_empty(), "default extra should be empty");
    }

    // ── Governed Development: pure work-item / UoW helpers ─────────────────────
    use super::{
        active_mention_partial, apply_mention_selection, create_or_open_label, existing_uow_for,
        filter_mention_candidates, labels_summary, work_item_state_badge, UowListEntry, UowStage,
        WorkItem,
    };

    fn wi(id: &str) -> WorkItem {
        WorkItem {
            id: id.to_string(),
            provider: "github".to_string(),
            repo: "acme/web".to_string(),
            number: 1,
            title: "t".to_string(),
            body: String::new(),
            state: "open".to_string(),
            url: String::new(),
            labels: vec![],
        }
    }

    #[test]
    fn state_badge_maps_open_closed_and_unknown() {
        assert_eq!(work_item_state_badge("open"), ("OPEN", "active"));
        // Casing is normalized.
        assert_eq!(work_item_state_badge("OPEN"), ("OPEN", "active"));
        assert_eq!(work_item_state_badge("closed"), ("CLOSED", "done"));
        assert_eq!(work_item_state_badge("weird"), ("UNKNOWN", "neutral"));
    }

    #[test]
    fn labels_summary_joins_and_placeholders() {
        assert_eq!(labels_summary(&[]), "—");
        assert_eq!(labels_summary(&["bug".to_string()]), "bug");
        assert_eq!(
            labels_summary(&["bug".to_string(), "ui".to_string()]),
            "bug, ui"
        );
    }

    #[test]
    fn create_or_open_label_dedup_logic() {
        assert_eq!(create_or_open_label(false), "Create Unit of Work from this issue");
        assert_eq!(create_or_open_label(true), "Open Unit of Work");
    }

    #[test]
    fn existing_uow_for_matches_by_work_item_id() {
        let uows = vec![
            UowListEntry {
                id: "uow-1".to_string(),
                work_item: wi("github:acme/web#10"),
                stage: UowStage::Development,
            },
            UowListEntry {
                id: "uow-2".to_string(),
                work_item: wi("github:acme/web#11"),
                stage: UowStage::Intake,
            },
        ];
        // A match returns the right UoW (drives "Open Unit of Work").
        let found = existing_uow_for(&uows, "github:acme/web#11");
        assert_eq!(found.map(|u| u.id.as_str()), Some("uow-2"));
        // No match -> None (drives "Create Unit of Work").
        assert!(existing_uow_for(&uows, "github:acme/web#99").is_none());
        // The dedup display logic composes: no UoW -> Create label.
        assert_eq!(
            create_or_open_label(existing_uow_for(&uows, "github:acme/web#99").is_some()),
            "Create Unit of Work from this issue"
        );
        assert_eq!(
            create_or_open_label(existing_uow_for(&uows, "github:acme/web#10").is_some()),
            "Open Unit of Work"
        );
    }

    #[test]
    fn active_mention_partial_detects_trailing_at_token() {
        // A bare `@` at the tail is an active token with an empty partial.
        assert_eq!(active_mention_partial("hey @"), Some(""));
        // `@oct` -> partial `oct`.
        assert_eq!(active_mention_partial("hey @oct"), Some("oct"));
        // No trailing token -> None.
        assert_eq!(active_mention_partial("hey there"), None);
        // A completed mention followed by a space is no longer the trailing token.
        assert_eq!(active_mention_partial("hey @octocat "), None);
        // An email-ish token (second `@`) is not a mention.
        assert_eq!(active_mention_partial("ping a@b"), None);
        // Empty input -> None.
        assert_eq!(active_mention_partial(""), None);
    }

    #[test]
    fn apply_mention_selection_replaces_trailing_token() {
        // Completes the active partial with the chosen login + trailing space.
        assert_eq!(apply_mention_selection("hey @oct", "octocat"), "hey @octocat ");
        // A bare `@` completes to the full handle.
        assert_eq!(apply_mention_selection("hey @", "hubot"), "hey @hubot ");
        // No active token -> appends a mention (with a separating space).
        assert_eq!(apply_mention_selection("hello", "octocat"), "hello @octocat ");
        // Appending onto empty input needs no leading space.
        assert_eq!(apply_mention_selection("", "octocat"), "@octocat ");
    }

    #[test]
    fn filter_mention_candidates_is_case_insensitive_and_capped() {
        let users: Vec<String> = vec!["Octocat", "octo-bot", "hubot", "alice"]
            .into_iter()
            .map(String::from)
            .collect();
        // Case-insensitive `contains` match on the partial.
        let m = filter_mention_candidates(&users, "OCT");
        assert_eq!(m, vec!["Octocat", "octo-bot"]);
        // Empty partial returns the leading set (so a bare `@` shows suggestions).
        assert_eq!(filter_mention_candidates(&users, "").len(), 4);
        // A non-match yields an empty list (dropdown hidden).
        assert!(filter_mention_candidates(&users, "zzz").is_empty());
    }

    #[test]
    fn work_item_deserializes_from_contract_shape() {
        let json = r#"{
            "id": "github:acme/web#42",
            "provider": "github",
            "repo": "acme/web",
            "number": 42,
            "title": "Add CSV export",
            "body": "Members CSV",
            "state": "open",
            "url": "https://github.com/acme/web/issues/42",
            "labels": ["enhancement", "ui"]
        }"#;
        let item: WorkItem = serde_json::from_str(json).unwrap();
        assert_eq!(item.id, "github:acme/web#42");
        assert_eq!(item.number, 42);
        assert_eq!(item.labels, vec!["enhancement".to_string(), "ui".to_string()]);
        assert_eq!(labels_summary(&item.labels), "enhancement, ui");
    }

    // ── CiRulesPanel helpers ───────────────────────────────────────────────────

    use super::{
        ci_rule_items_from_proposed, ci_rule_items_from_selections, first_linter, CiRuleItem,
        RuleSelectionView,
    };

    fn make_rule(id: &str, enforcement: &str, linter: Option<&str>) -> ProposedRuleView {
        ProposedRuleView {
            id: id.to_string(),
            title: format!("{id} title"),
            kind: "structural".to_string(),
            enforcement: enforcement.to_string(),
            options: vec![],
            default_option: None,
            decision_question: None,
            decision_why: None,
            scope: "repo".to_string(),
            domain: "test".to_string(),
            repos: vec![],
            placement: "CONVENTIONS.md".to_string(),
            finding_count: 0,
            recommended: false,
            is_auto_recommended: false,
            verification: "draft".to_string(),
            sources: linter
                .map(|l| vec![RuleSourceView {
                    url: "https://example.com".to_string(),
                    title: "Source".to_string(),
                    linter: Some(l.to_string()),
                }])
                .unwrap_or_default(),
        }
    }

    /// Only mechanical and architectural rules pass the CI-tier filter.
    #[test]
    fn ci_rule_items_from_proposed_filters_to_ci_tier_only() {
        let rules = vec![
            make_rule("MECH-1", "mechanical", Some("eslint: rule")),
            make_rule("ARCH-1", "architectural", None),
            make_rule("STRUCT-1", "structured", None),
            make_rule("PROSE-1", "prose", None),
        ];
        let items = ci_rule_items_from_proposed(&rules);
        assert_eq!(items.len(), 2, "only mechanical + architectural should pass");
        assert!(items.iter().any(|i| i.id == "MECH-1"));
        assert!(items.iter().any(|i| i.id == "ARCH-1"));
        assert!(!items.iter().any(|i| i.id == "STRUCT-1"));
        assert!(!items.iter().any(|i| i.id == "PROSE-1"));
    }

    /// The linter is carried through from the first source with one.
    #[test]
    fn ci_rule_items_from_proposed_carries_linter_from_first_source() {
        let rules = vec![make_rule("MECH-1", "mechanical", Some("eslint: no-unused-vars"))];
        let items = ci_rule_items_from_proposed(&rules);
        assert_eq!(items[0].linter.as_deref(), Some("eslint: no-unused-vars"));
    }

    /// Rules with no linter source yield None.
    #[test]
    fn ci_rule_items_from_proposed_none_linter_when_no_source() {
        let rules = vec![make_rule("ARCH-1", "architectural", None)];
        let items = ci_rule_items_from_proposed(&rules);
        assert!(items[0].linter.is_none());
    }

    /// Empty input produces empty output.
    #[test]
    fn ci_rule_items_from_proposed_empty_input() {
        assert!(ci_rule_items_from_proposed(&[]).is_empty());
    }

    /// `ci_rule_items_from_selections` joins by rule_id and filters to CI tier.
    #[test]
    fn ci_rule_items_from_selections_joins_and_filters() {
        let corpus = vec![
            make_rule("MECH-1", "mechanical", Some("eslint: rule")),
            make_rule("ARCH-1", "architectural", None),
            make_rule("PROSE-1", "prose", None),
        ];
        let selections = vec![
            RuleSelectionView { rule_id: "MECH-1".to_string(), chosen_option: None, repos: vec![] },
            RuleSelectionView { rule_id: "ARCH-1".to_string(), chosen_option: None, repos: vec![] },
            RuleSelectionView { rule_id: "PROSE-1".to_string(), chosen_option: None, repos: vec![] },
        ];
        let items = ci_rule_items_from_selections(&selections, &corpus);
        assert_eq!(items.len(), 2, "prose should be excluded");
        assert!(items.iter().any(|i| i.id == "MECH-1"));
        assert!(items.iter().any(|i| i.id == "ARCH-1"));
    }

    /// A selection whose rule_id is not in the corpus is silently dropped.
    #[test]
    fn ci_rule_items_from_selections_drops_unknown_rule_ids() {
        let corpus = vec![make_rule("MECH-1", "mechanical", None)];
        let selections = vec![
            RuleSelectionView { rule_id: "MECH-1".to_string(), chosen_option: None, repos: vec![] },
            RuleSelectionView { rule_id: "GHOST-99".to_string(), chosen_option: None, repos: vec![] },
        ];
        let items = ci_rule_items_from_selections(&selections, &corpus);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "MECH-1");
    }

    /// `first_linter` returns the linter from the first matching source.
    #[test]
    fn first_linter_returns_first_nonempty_linter() {
        let rule = ProposedRuleView {
            sources: vec![
                RuleSourceView { url: "u1".to_string(), title: "S1".to_string(), linter: None },
                RuleSourceView { url: "u2".to_string(), title: "S2".to_string(), linter: Some("mypy".to_string()) },
            ],
            ..make_rule("R-1", "mechanical", None)
        };
        assert_eq!(first_linter(&rule).as_deref(), Some("mypy"));
    }

    /// `first_linter` skips empty-string linters.
    #[test]
    fn first_linter_skips_empty_string() {
        let rule = ProposedRuleView {
            sources: vec![RuleSourceView {
                url: "u".to_string(),
                title: "S".to_string(),
                linter: Some(String::new()),
            }],
            ..make_rule("R-1", "mechanical", None)
        };
        assert!(first_linter(&rule).is_none());
    }

    // ── CI-security Part B: scan-time preview findings in triage ──────────────

    /// The Authority column's classification: a preview finding reads "preview"
    /// (distinct from an enforced floor hit and from an AI-advisory finding). This
    /// mirrors the inline accessor in `finding_columns`.
    fn authority_label(f: &FindingView) -> &'static str {
        if f.preview {
            "preview"
        } else if is_enforced_floor(&f.rule_id) {
            "enforced"
        } else {
            "advisory"
        }
    }

    #[test]
    fn preview_finding_labeled_distinctly() {
        // A scan-time preview finding (deterministic tool, not yet wired): "preview".
        let preview: FindingView = serde_json::from_str(
            r#"{"repo":"me/api","path":"q.py","line":12,"rule_id":"S608",
                "severity":"medium","snippet":"x","detail":"d",
                "preview":true,"preview_tool":"ruff"}"#,
        )
        .unwrap();
        assert!(preview.preview);
        assert_eq!(preview.preview_tool.as_deref(), Some("ruff"));
        assert_eq!(authority_label(&preview), "preview");

        // An enforced floor finding stays "enforced".
        let floor: FindingView = serde_json::from_str(
            r#"{"repo":"me/api","path":"a.rs","line":1,
                "rule_id":"SEC-NO-HARDCODED-SECRETS-1","severity":"high",
                "snippet":"x","detail":"d"}"#,
        )
        .unwrap();
        assert!(!floor.preview, "absent preview field defaults to false (back-compat)");
        assert_eq!(authority_label(&floor), "enforced");

        // An AI-advisory finding (no preview, not a floor id) stays "advisory".
        let ai: FindingView = serde_json::from_str(
            r#"{"repo":"me/api","path":"x.rs","line":2,"rule_id":"AI-FOO",
                "severity":"medium","snippet":"x","detail":"d"}"#,
        )
        .unwrap();
        assert_eq!(authority_label(&ai), "advisory");
    }
}
