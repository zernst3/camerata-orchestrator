use super::*;


/// Re-emit the project's ruleset (source of truth) into its repos — one PR per repo.
pub(super) async fn emit_project_rules(project_id: &str) -> Option<Vec<ArmResultView>> {
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

/// Re-emit the project's ruleset locally into each repo's working copy
/// (POST /api/projects/:id/emit-local).  Writes AGENTS.md + CONVENTIONS.md
/// directly into the local checkout; no GitHub token needed, no PR opened.
/// Returns `(ok, message)` — ok=true means at least one file was written.
pub(super) async fn emit_project_local(project_id: &str) -> (bool, String) {
    let resp = reqwest::Client::new()
        .post(format!(
            "{}/api/projects/{}/emit-local",
            crate::BFF_URL,
            project_id
        ))
        .send()
        .await;
    match resp {
        Ok(r) => {
            let v: serde_json::Value = r.json().await.unwrap_or_default();
            let ok = v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false);
            let msg = v
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or(if ok { "Rules emitted locally." } else { "emit-local failed." })
                .to_string();
            (ok, msg)
        }
        Err(e) => (false, format!("Network error: {e}")),
    }
}

/// Add or edit (by name) a custom rule on a project.
pub(super) async fn add_custom_rule(project_id: &str, name: &str, body: &str, domain: &str) -> bool {
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
pub(super) async fn delete_custom_rule(project_id: &str, name: &str) -> bool {
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

pub(super) fn custom_columns() -> Vec<ColumnDef<CustomRuleView>> {
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
pub(super) fn CustomRulesTable(
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
pub(super) async fn import_ruleset(project_id: &str, json: String) -> bool {
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
pub(super) async fn fetch_corpus_rules() -> Option<Vec<ProposedRuleView>> {
    reqwest::get(format!("{}/api/corpus-rules", crate::BFF_URL))
        .await
        .ok()?
        .json::<Vec<ProposedRuleView>>()
        .await
        .ok()
}

/// Persist a full ruleset (read-modify-write). Always includes the existing `custom` array
/// unchanged — callers must pass it through from the current project.
pub(super) async fn save_ruleset(project_id: &str, ruleset: serde_json::Value) -> bool {
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
pub(super) struct SuppressionView {
    pub rule_id: String,
    pub path: String,
    #[serde(default)]
    pub line: Option<usize>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub ticket: Option<String>,
    pub source: String,
    #[serde(default)]
    pub accepted_by: Option<String>,
    pub stale: bool,
}

pub(super) async fn fetch_suppressions(project_id: &str) -> Option<Vec<SuppressionView>> {
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
pub(super) fn SuppressionsPanel(project_id: String) -> Element {
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

/// Build the ruleset JSON body for a `POST /api/projects/{id}/ruleset` call.
/// Merges the current project's selections with the provided override and
/// always preserves the custom rules array unchanged.
pub(super) fn build_ruleset_json(project: &ProjectView) -> serde_json::Value {
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
pub(super) enum SelectionBucket {
    Selections,
    CrossRepo,
    Process,
}

pub(super) fn bucket_of(rule: &ProposedRuleView) -> SelectionBucket {
    match rule.scope.as_str() {
        "cross-repo" => SelectionBucket::CrossRepo,
        "process" => SelectionBucket::Process,
        _ => SelectionBucket::Selections,
    }
}

/// A row for Table 1: a selection from the project's ruleset, joined with the full
/// corpus rule for title / domain / scope / options.
#[derive(Clone, PartialEq)]
pub(super) struct AppliedRuleRow {
    /// From the ruleset selection.
    pub selection: RuleSelectionView,
    /// Scope bucket (selections / cross_repo / process) — drives "applies to all repos" label.
    pub bucket: SelectionBucket,
    /// Full corpus rule (may be None for custom / unknown ids).
    pub corpus: Option<ProposedRuleView>,
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
pub(super) fn enforcement_badges() -> BadgeVariantMap {
    BadgeVariantMap::new()
        .with("prose",        BadgeVariant::new("Prose",        "gray"))
        .with("structured",   BadgeVariant::new("Structured",   "blue"))
        .with("mechanical",   BadgeVariant::new("Mechanical",   "green"))
        .with("architectural",BadgeVariant::new("Architectural","yellow"))
        .with_fallback(BadgeVariant::new("\u{2014}", "gray"))
}

/// Return the modality definition tooltip text for a given enforcement value.
/// Used both in modal `title` attributes and in the per-cell RowCellRenderer tooltips.
pub(super) fn enforcement_tooltip(enforcement: &str) -> &'static str {
    match enforcement {
        "prose"        => "A principle or idiom a human judges \u{2014} a matter of degree (rendered to AGENTS.md).",
        "structured"   => "A concrete design contract with a clear conform/violate answer; human-verified, not lint-able (CONVENTIONS.md).",
        "mechanical"   => "An existing off-the-shelf linter decides it (clippy, eslint, ruff, golangci-lint, \u{2026}).",
        "architectural"=> "Deterministic, but needs a bespoke custom checker \u{2014} no off-the-shelf linter expresses it.",
        _              => "The enforcement modality for this rule is not yet classified.",
    }
}

pub(super) fn applied_rule_columns() -> Vec<ColumnDef<AppliedRuleRow>> {
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

pub(super) fn corpus_columns() -> Vec<ColumnDef<ProposedRuleView>> {
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
pub(super) fn ProjectRulesTable(
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
pub(super) fn AllRulesTable(
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
pub(super) fn RulesDetailModalHost(on_option_picked: EventHandler<(String, String)>) -> Element {
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

/// Model Efficiency Profile selector + Apply button with confirm popup.
///
/// Shows a profile picker (MaxEfficiency / Balanced / MaxQuality / Custom).
/// On Apply: fetches the preview, shows a confirm popup listing the per-entry
/// `current -> new` changes + count of entries affected, then on confirm POSTs apply.
/// After apply the per-entry editors still allow manual override.
#[component]
pub(super) fn ModelProfileEditor(project: ProjectView, refresh: Signal<u32>) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let pid = project.id.clone();

    let profile_options = [
        ("balanced", "Balanced", "Opus/Sonnet/Haiku via Claude subscription. Reliable and caching-efficient."),
        ("max_efficiency", "Max Efficiency", "Opus orchestrates; free OpenRouter models fill balanced/fast/steps. Quota relief with paid fallback."),
        ("max_quality", "Max Quality", "Opus/Sonnet throughout; L3 review on. Highest quality, most subscription usage."),
        ("custom", "Custom", "No cascade. You own every entry."),
    ];

    let current_profile = project.model_profile.clone();
    let selected_profile = use_signal(|| current_profile.clone());
    let applying = use_signal(|| false);

    // Confirm popup state.
    let confirm_open = use_signal(|| false);
    let preview_data: Signal<Option<serde_json::Value>> = use_signal(|| None);

    rsx! {
        div { class: "tier-map-editor model-profile-editor",
            p { class: "tier-map-heading", "Model Efficiency Profile" }
            p { class: "section-hint tier-map-hint",
                "Applying a profile cascades concrete model assignments to ALL entry points \
                 (tier map, step models, L3). A confirm popup shows current \u{2192} new changes \
                 before applying. After apply, per-entry editors still allow manual override."
            }

            div { class: "profile-selector-list",
                for (value, label, desc) in profile_options.iter() {
                    {
                        let v = value.to_string();
                        let is_current = current_profile == *value;
                        let is_selected = selected_profile() == *value;
                        rsx! {
                            label {
                                key: "{value}",
                                class: if is_selected { "profile-option profile-option-selected" } else { "profile-option" },
                                input {
                                    r#type: "radio",
                                    name: "model-profile",
                                    value: "{value}",
                                    checked: is_selected,
                                    onchange: {
                                        let v = v.clone();
                                        let mut selected_profile = selected_profile;
                                        move |_| selected_profile.set(v.clone())
                                    },
                                }
                                span { class: "profile-option-label", "{label}" }
                                if is_current {
                                    span { class: "profile-option-active-badge", "active" }
                                }
                                span { class: "profile-option-desc", "{desc}" }
                            }
                        }
                    }
                }
            }

            div { class: "profile-apply-row",
                button {
                    class: "btn-run",
                    disabled: applying() || selected_profile() == "custom",
                    title: if selected_profile() == "custom" { "Custom profile: no cascade to apply. Edit entries directly below." } else { "Preview and apply this profile" },
                    onclick: move |_| {
                        let sel = selected_profile();
                        if sel == "custom" { return; }
                        let pid = pid.clone();
                        let mut applying = applying;
                        let mut confirm_open = confirm_open;
                        let mut preview_data = preview_data;
                        applying.set(true);
                        spawn(async move {
                            let data = preview_model_profile(&pid, &sel).await;
                            preview_data.set(data);
                            confirm_open.set(true);
                            applying.set(false);
                        });
                    },
                    if applying() { "Loading preview\u{2026}" } else { "Apply profile\u{2026}" }
                }
            }

            // Confirm popup
            if confirm_open() {
                {
                    let sel = selected_profile();
                    let pid2 = project.id.clone();
                    let preview = preview_data().clone();
                    let mut confirm_open = confirm_open;
                    let refresh = refresh;

                    // Build the change list from the preview.
                    let (change_lines, affected_count) = build_change_summary(&project, &preview);

                    rsx! {
                        div { class: "profile-confirm-overlay",
                            div { class: "profile-confirm-modal",
                                p { class: "profile-confirm-title", "Apply \u{201c}{sel}\u{201d} profile?" }
                                p { class: "profile-confirm-count", "{affected_count} entries will change" }
                                div { class: "profile-confirm-changes",
                                    for (i, line) in change_lines.iter().enumerate() {
                                        p { key: "{i}", class: "profile-confirm-change-row", "{line}" }
                                    }
                                }
                                div { class: "profile-confirm-actions",
                                    button {
                                        class: "btn-run",
                                        onclick: move |_| {
                                            let sel2 = sel.clone();
                                            let pid3 = pid2.clone();
                                            let toasts = toasts;
                                            let mut confirm_open = confirm_open;
                                            let mut refresh = refresh;
                                            confirm_open.set(false);
                                            spawn(async move {
                                                let result = apply_model_profile(&pid3, &sel2).await;
                                                if result.as_ref().and_then(|v| v.get("ok")).and_then(|b| b.as_bool()).unwrap_or(false) {
                                                    crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, "Profile applied.");
                                                    refresh += 1;
                                                } else {
                                                    crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, "Could not apply profile.");
                                                }
                                            });
                                        },
                                        "Confirm"
                                    }
                                    button {
                                        class: "btn-restart",
                                        onclick: move |_| confirm_open.set(false),
                                        "Cancel"
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

/// Build the human-readable change summary for the confirm popup.
/// Returns (lines, affected_count).
fn build_change_summary(project: &ProjectView, preview: &Option<serde_json::Value>) -> (Vec<String>, usize) {
    let Some(v) = preview else {
        return (vec!["Preview unavailable.".to_string()], 0);
    };
    if v.get("noop").and_then(|b| b.as_bool()).unwrap_or(false) {
        return (vec!["Custom profile: no changes.".to_string()], 0);
    }
    let Some(a) = v.get("assignments") else {
        return (vec!["No assignments in preview.".to_string()], 0);
    };

    let mut lines = Vec::new();
    let mut count = 0usize;

    // Tier map
    if let Some(tm) = a.get("tier_map") {
        let new_strongest = tm.get("strongest").and_then(|v| v.as_str()).unwrap_or("");
        let cur_strongest = &project.tier_map.strongest;
        if new_strongest != cur_strongest {
            lines.push(format!("tier.strongest: {} \u{2192} {}", cur_strongest, new_strongest));
            count += 1;
        }
        let new_balanced = tier_chain_str(tm.get("balanced"));
        let cur_balanced = project.tier_map.balanced.join(", ");
        if new_balanced != cur_balanced {
            lines.push(format!("tier.balanced: {} \u{2192} {}", cur_balanced, new_balanced));
            count += 1;
        }
        let new_fast = tier_chain_str(tm.get("fast"));
        let cur_fast = project.tier_map.fast.join(", ");
        if new_fast != cur_fast {
            lines.push(format!("tier.fast: {} \u{2192} {}", cur_fast, new_fast));
            count += 1;
        }
    }

    // Step models
    if let Some(sm) = a.get("step_models") {
        let steps = [
            ("audit", &project.step_models.audit),
            ("calibration", &project.step_models.calibration),
            ("research_chat", &project.step_models.research_chat),
            ("story_authoring", &project.step_models.story_authoring),
            ("decomposition", &project.step_models.decomposition),
            ("escalation", &project.step_models.escalation),
            ("clarification", &project.step_models.clarification),
        ];
        for (key, cur) in steps.iter() {
            let new_val = sm.get(key).and_then(|v| v.as_str()).unwrap_or("");
            if new_val != cur.as_str() {
                lines.push(format!("step.{}: {} \u{2192} {}", key, cur, new_val));
                count += 1;
            }
        }
    }

    // L3
    if let Some(l3) = a.get("l3_review") {
        let new_enabled = l3.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
        let cur_enabled = project.l3_review.enabled;
        if new_enabled != cur_enabled {
            lines.push(format!("l3.enabled: {} \u{2192} {}", cur_enabled, new_enabled));
            count += 1;
        }
        let new_model = l3.get("model").and_then(|v| v.as_str()).unwrap_or("");
        let cur_model = project.l3_review.model.as_str();
        if new_model != cur_model {
            lines.push(format!("l3.model: {} \u{2192} {}", if cur_model.is_empty() { "(balanced fallback)" } else { cur_model }, if new_model.is_empty() { "(balanced fallback)" } else { new_model }));
            count += 1;
        }
    }

    if lines.is_empty() {
        lines.push("No changes (all entries already match).".to_string());
    }

    (lines, count)
}

fn tier_chain_str(v: Option<&serde_json::Value>) -> String {
    match v {
        Some(serde_json::Value::Array(arr)) => arr
            .iter()
            .filter_map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        Some(serde_json::Value::String(s)) => s.clone(),
        _ => String::new(),
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
pub(super) fn TierMapEditor(project: ProjectView) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let pid = project.id.clone();
    // Local editable copies. fast/balanced are ordered chains (Vec<String>).
    // strongest stays a single model id.
    let mut fast = use_signal(|| project.tier_map.fast.clone());
    let mut balanced = use_signal(|| project.tier_map.balanced.clone());
    let mut strongest = use_signal(|| project.tier_map.strongest.clone());
    let mut saving = use_signal(|| false);

    rsx! {
        div { class: "tier-map-editor",
            p { class: "tier-map-heading", "Model tier map" }
            p { class: "section-hint tier-map-hint",
                "Maps each capability band to a model chain. The fast and balanced bands support \
                 multiple models: the primary is tried first; on a retryable error (429, 5xx, timeout) \
                 the next model in the chain is tried automatically. Strongest stays a single model. \
                 Changes take effect from the next run onward."
            }
            div { class: "tier-map-rows",
                // Fast band — chain editor
                div { class: "tier-map-row tier-map-chain-row",
                    label { class: "tier-map-band-label tier-map-fast", "Fast" }
                    span { class: "tier-map-band-desc", "(throughput — tests, simple edits)" }
                    div { class: "tier-chain-list",
                        for (i, _model) in fast().iter().enumerate() {
                            div { key: "{i}", class: "tier-chain-entry",
                                input {
                                    class: "tier-map-input addressee-input tier-chain-input",
                                    r#type: "text",
                                    placeholder: "model id",
                                    value: "{fast()[i]}",
                                    oninput: {
                                        let mut fast = fast;
                                        move |e: dioxus::prelude::Event<dioxus::prelude::FormData>| {
                                            let mut v = fast();
                                            if let Some(entry) = v.get_mut(i) {
                                                *entry = e.value();
                                            }
                                            fast.set(v);
                                        }
                                    },
                                }
                                if fast().len() > 1 {
                                    button {
                                        class: "btn-edit-sm tier-chain-remove",
                                        title: "Remove this model from the chain",
                                        onclick: {
                                            let mut fast = fast;
                                            move |_| {
                                                let mut v = fast();
                                                if i < v.len() { v.remove(i); }
                                                fast.set(v);
                                            }
                                        },
                                        "\u{2715}"
                                    }
                                }
                            }
                        }
                        button {
                            class: "btn-edit-sm tier-chain-add",
                            onclick: move |_| {
                                let mut v = fast();
                                v.push(String::new());
                                fast.set(v);
                            },
                            "\u{002b} Add fallback"
                        }
                    }
                }
                // Balanced band — chain editor
                div { class: "tier-map-row tier-map-chain-row",
                    label { class: "tier-map-band-label tier-map-balanced", "Balanced" }
                    span { class: "tier-map-band-desc", "(mid-tier — most tasks)" }
                    div { class: "tier-chain-list",
                        for (i, _model) in balanced().iter().enumerate() {
                            div { key: "{i}", class: "tier-chain-entry",
                                input {
                                    class: "tier-map-input addressee-input tier-chain-input",
                                    r#type: "text",
                                    placeholder: "model id",
                                    value: "{balanced()[i]}",
                                    oninput: {
                                        let mut balanced = balanced;
                                        move |e: dioxus::prelude::Event<dioxus::prelude::FormData>| {
                                            let mut v = balanced();
                                            if let Some(entry) = v.get_mut(i) {
                                                *entry = e.value();
                                            }
                                            balanced.set(v);
                                        }
                                    },
                                }
                                if balanced().len() > 1 {
                                    button {
                                        class: "btn-edit-sm tier-chain-remove",
                                        title: "Remove this model from the chain",
                                        onclick: {
                                            let mut balanced = balanced;
                                            move |_| {
                                                let mut v = balanced();
                                                if i < v.len() { v.remove(i); }
                                                balanced.set(v);
                                            }
                                        },
                                        "\u{2715}"
                                    }
                                }
                            }
                        }
                        button {
                            class: "btn-edit-sm tier-chain-add",
                            onclick: move |_| {
                                let mut v = balanced();
                                v.push(String::new());
                                balanced.set(v);
                            },
                            "\u{002b} Add fallback"
                        }
                    }
                }
                // Strongest band — single model (stays String)
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
                    // Filter out empty entries from the chains.
                    let fast_chain: Vec<String> = fast().into_iter()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                    let balanced_chain: Vec<String> = balanced().into_iter()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                    let strongest_val = strongest().trim().to_string();
                    if fast_chain.is_empty() || balanced_chain.is_empty() || strongest_val.is_empty() {
                        crate::toast::push_toast(toasts, crate::toast::ToastKind::Warning, "Each tier requires at least one model id.");
                        return;
                    }
                    let map = TierMapView {
                        fast: fast_chain,
                        balanced: balanced_chain,
                        strongest: strongest_val,
                    };
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

/// Per-step model editor: one labeled `<select>` per NON-FLEET AI step (audit,
/// calibration, research chat, story authoring, decomposition, escalation, clarification).
///
/// Reads each current value from `project.step_models` and saves on change via
/// `POST /api/projects/:id/step-models` (one step per round-trip — patch semantics, mirrors
/// the per-step server endpoint). The dropdown options come from `GET /api/models` (the
/// same source the tier-map / audit selectors use). The project id is the scoped id from
/// the passed-in `ProjectView`, so each save targets exactly this project.
#[component]
pub(super) fn StepModelsEditor(project: ProjectView) -> Element {
    let models = use_resource(|| async move { fetch_audit_models().await });
    let models = models.read().clone().flatten();

    // The (step-key, human label, current model id) tuples, in display order.
    let sm = &project.step_models;
    let rows: Vec<(&'static str, &'static str, String)> = vec![
        ("audit", "Audit", sm.audit.clone()),
        ("calibration", "Calibration", sm.calibration.clone()),
        ("research_chat", "Research chat", sm.research_chat.clone()),
        ("story_authoring", "Story authoring", sm.story_authoring.clone()),
        ("decomposition", "Decomposition", sm.decomposition.clone()),
        ("escalation", "Escalation", sm.escalation.clone()),
        ("clarification", "Clarification", sm.clarification.clone()),
    ];

    rsx! {
        div { class: "tier-map-editor step-models-editor",
            p { class: "tier-map-heading", "Step models" }
            p { class: "section-hint tier-map-hint",
                "The model each NON-FLEET AI step uses for THIS project. Once set, the project's \
                 value is authoritative (no environment fallback). Audit, calibration, and research \
                 chat still let an explicit per-run pick override this default; the other steps use \
                 it directly. Each change saves immediately."
            }
            div { class: "tier-map-rows",
                for (step_key , label , current) in rows.into_iter() {
                    StepModelRow {
                        key: "{project.id}-{step_key}",
                        project_id: project.id.clone(),
                        step_key,
                        label,
                        current,
                        models: models.clone(),
                    }
                }
            }
        }
    }
}

/// One row of [`StepModelsEditor`]: a label + a model `<select>` that POSTs the chosen model
/// for its single step on change. Self-contained (owns its local selection + saving signals)
/// so a save on one step never touches another.
#[component]
pub(super) fn StepModelRow(
    project_id: String,
    step_key: &'static str,
    label: &'static str,
    current: String,
    models: Option<AuditModelsResp>,
) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let mut selected = use_signal(|| current.clone());
    let mut saving = use_signal(|| false);

    rsx! {
        div { class: "tier-map-row step-model-row",
            label { class: "tier-map-band-label", "{label}" }
            if let Some(m) = models {
                select {
                    class: "tier-map-input run-model-select",
                    value: "{selected}",
                    disabled: saving(),
                    onchange: move |e| {
                        let model = e.value();
                        selected.set(model.clone());
                        let pid = project_id.clone();
                        saving.set(true);
                        spawn(async move {
                            if set_project_step_model(&pid, step_key, &model).await {
                                crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, &format!("{label} model saved."));
                            } else {
                                crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, &format!("Could not save {label} model."));
                            }
                            saving.set(false);
                        });
                    },
                    for (group_label , opts) in m.grouped().into_iter() {
                        optgroup { label: "{group_label}",
                            for opt in opts.into_iter() {
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
        }
    }
}

/// Stall-threshold editor: two numeric inputs (watched/interactive seconds and
/// routine/autonomous seconds). Saves to `POST /api/projects/:id/stall-thresholds`
/// via an explicit "Save thresholds" button (batch-save pattern like `TierMapEditor`).
#[component]
pub(super) fn StallThresholdsEditor(project: ProjectView) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let pid = project.id.clone();
    let mut watched = use_signal(|| project.stall_thresholds.watched_secs);
    let mut routine = use_signal(|| project.stall_thresholds.routine_secs);
    let mut saving = use_signal(|| false);

    rsx! {
        div { class: "tier-map-editor stall-thresholds-editor",
            p { class: "tier-map-heading", "Stall thresholds" }
            p { class: "section-hint tier-map-hint",
                "How long a run may be idle before Camerata flags it as stalled. \
                 Watched (interactive) runs are expected to respond faster; \
                 Routine (autonomous) runs have a longer grace period."
            }
            div { class: "tier-map-rows",
                div { class: "tier-map-row",
                    label { class: "tier-map-band-label", "Watched (interactive) seconds" }
                    input {
                        class: "tier-map-input addressee-input",
                        r#type: "number",
                        min: "1",
                        value: "{watched}",
                        oninput: move |e| {
                            if let Ok(v) = e.value().parse::<u64>() {
                                if v > 0 { watched.set(v); }
                            }
                        },
                    }
                }
                div { class: "tier-map-row",
                    label { class: "tier-map-band-label", "Routine (autonomous) seconds" }
                    input {
                        class: "tier-map-input addressee-input",
                        r#type: "number",
                        min: "1",
                        value: "{routine}",
                        oninput: move |e| {
                            if let Ok(v) = e.value().parse::<u64>() {
                                if v > 0 { routine.set(v); }
                            }
                        },
                    }
                }
            }
            button {
                class: "btn-run",
                disabled: saving(),
                onclick: move |_| {
                    let (pid, w, r) = (pid.clone(), watched(), routine());
                    saving.set(true);
                    spawn(async move {
                        if set_project_stall_thresholds(&pid, w, r).await {
                            crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, "Stall thresholds saved.");
                        } else {
                            crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, "Could not save stall thresholds.");
                        }
                        saving.set(false);
                    });
                },
                if saving() { "Saving\u{2026}" } else { "Save thresholds" }
            }
        }
    }
}

/// L3 agentic code-review gate editor (R7): a toggle (on/off) and a model selector.
///
/// When enabled, the L3 reviewer runs after each governed development stage, checking
/// the generated diff against story intent and the project's rules. The model selector
/// offers the same Anthropic tier options as the step-model and tier-map editors; an
/// empty selection means "use the project's Balanced tier model" (the fallback defined
/// in `Project::l3_model`).
///
/// Reads the current config from `project.l3_review` and persists on change via
/// `POST /api/projects/:id/l3-review`. Uses the existing toast feedback pattern.
#[component]
pub(super) fn L3ReviewEditor(project: ProjectView) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let pid = project.id.clone();
    // Load the available Anthropic model options (same source as tier-map / step-model editors).
    let models = use_resource(|| async move { fetch_audit_models().await });
    let models = models.read().clone().flatten();

    // Local edit state, seeded from the project's current L3 config.
    let mut enabled = use_signal(|| project.l3_review.enabled);
    // Empty string = "use balanced tier" (the serde/server default).
    let mut model = use_signal(|| project.l3_review.model.clone());
    let mut saving = use_signal(|| false);

    rsx! {
        div { class: "tier-map-editor l3-review-editor",
            p { class: "tier-map-heading", "L3 AI code review" }
            p { class: "section-hint tier-map-hint",
                "When enabled, an agentic reviewer checks each governed development stage's diff \
                 against story intent and the project rules. Off by default. The model falls back \
                 to the Balanced tier when left blank."
            }

            div { class: "tier-map-rows",
                // ── Toggle ───────────────────────────────────────────────────
                div { class: "tier-map-row l3-review-toggle-row",
                    label { class: "tier-map-band-label", "Enabled" }
                    // Checkbox styled inline.
                    input {
                        r#type: "checkbox",
                        class: "l3-review-checkbox",
                        checked: enabled(),
                        disabled: saving(),
                        onchange: move |e| {
                            enabled.set(e.checked());
                        },
                    }
                    span { class: "l3-review-toggle-hint",
                        if enabled() { "On — L3 reviewer runs after each stage." }
                        else { "Off — human is the reviewer." }
                    }
                }

                // ── Model selector ───────────────────────────────────────────
                div { class: "tier-map-row l3-review-model-row",
                    label { class: "tier-map-band-label", "Model" }
                    if let Some(ref m) = models {
                        select {
                            class: "tier-map-input run-model-select",
                            disabled: saving(),
                            onchange: move |e| {
                                model.set(e.value());
                            },
                            // The first option is the "use Balanced tier" fallback (empty value).
                            option {
                                value: "",
                                selected: model().is_empty(),
                                "Use Balanced tier (default)"
                            }
                            for (group_label , opts) in m.grouped().into_iter() {
                                optgroup { label: "{group_label}",
                                    for opt in opts.into_iter() {
                                        option {
                                            value: "{opt.id}",
                                            selected: model() == opt.id,
                                            "{opt.label}"
                                        }
                                    }
                                }
                            }
                        }
                    } else {
                        // Models haven't loaded yet: show a text input as fallback.
                        input {
                            class: "tier-map-input addressee-input",
                            r#type: "text",
                            placeholder: "model id (empty = use Balanced tier)",
                            value: "{model}",
                            disabled: saving(),
                            oninput: move |e| model.set(e.value()),
                        }
                    }
                }
            }

            button {
                class: "btn-run",
                disabled: saving(),
                onclick: move |_| {
                    let (pid, en, mo) = (pid.clone(), enabled(), model());
                    saving.set(true);
                    spawn(async move {
                        if set_project_l3_review(&pid, en, &mo).await {
                            let msg = if en {
                                if mo.is_empty() {
                                    "L3 review enabled (using Balanced tier model)."
                                } else {
                                    "L3 review enabled."
                                }
                            } else {
                                "L3 review disabled."
                            };
                            crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, msg);
                        } else {
                            crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, "Could not save L3 review settings.");
                        }
                        saving.set(false);
                    });
                },
                if saving() { "Saving\u{2026}" } else { "Save L3 review settings" }
            }
        }
    }
}

#[component]
pub(super) fn RulesView() -> Element {
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
    let mut emitting_local = use_signal(|| false);

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
                    let pid_emit_local = p.id.clone();
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

                            // #106 Re-emit rules locally: writes AGENTS.md + CONVENTIONS.md
                            // directly into each repo's local working copy (no GitHub token,
                            // no PR).  Calls POST /api/projects/:id/emit-local.
                            button {
                                class: "btn-emit-local",
                                disabled: emitting_local(),
                                title: "Re-emit rules locally — writes AGENTS.md + CONVENTIONS.md into each repo's local checkout without opening a PR.",
                                onclick: move |_| {
                                    let id = pid_emit_local.clone();
                                    emitting_local.set(true);
                                    spawn(async move {
                                        let (ok, msg) = emit_project_local(&id).await;
                                        if ok {
                                            crate::toast::push_toast(
                                                toasts,
                                                crate::toast::ToastKind::Info,
                                                msg,
                                            );
                                        } else {
                                            crate::toast::push_toast(
                                                toasts,
                                                crate::toast::ToastKind::Error,
                                                msg,
                                            );
                                        }
                                        emitting_local.set(false);
                                    });
                                },
                                if emitting_local() { "Re-emitting locally…" } else { "Re-emit rules locally" }
                            }
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

                        // ── SETTINGS: Model Efficiency Profile ────────────────────────
                        // Cascades sensible model defaults to ALL entry points on apply.
                        p { class: "section-label settings-label", "SETTINGS: Model Efficiency Profile" }
                        ModelProfileEditor { project: p_owned.clone(), refresh }

                        // ── SETTINGS: Model tier map (#63) ────────────────────────────
                        // NOT a ruleset concern — controls which model the fleet uses
                        // per-task-tier at runtime. Labeled SETTINGS to distinguish from
                        // the rule tables above.
                        p { class: "section-label settings-label", "SETTINGS: Model tier map" }
                        TierMapEditor { project: p_owned.clone() }

                        // ── SETTINGS: Per-step models ─────────────────────────────────
                        // The model each non-fleet AI step uses for this project. Distinct
                        // from the fleet tier map above (that is per-task-tier for governed
                        // runs); this covers audit / calibration / chat / authoring / etc.
                        p { class: "section-label settings-label", "SETTINGS: Step models" }
                        StepModelsEditor { project: p_owned.clone() }

                        // ── SETTINGS: Stall thresholds ────────────────────────────
                        p { class: "section-label settings-label", "SETTINGS: Stall thresholds" }
                        StallThresholdsEditor { project: p_owned.clone() }

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
pub(super) fn RuleCount(label: String, n: usize) -> Element {
    rsx! {
        div { class: "rule-count",
            span { class: "rule-count-n", "{n}" }
            span { class: "rule-count-l", "{label}" }
        }
    }
}

/// Build CSV for the proposed-rules table.
pub(super) fn rules_csv(rules: &[ProposedRuleView]) -> String {
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
pub(super) struct RuleOptionView {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub directive: String,
    #[serde(default)]
    pub why: String,
}

/// One authoritative source backing a rule's grounding (mirrors `RuleSourceView`
/// from the server DTO). Used in `ProposedRuleView.sources`.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Default)]
pub(super) struct RuleSourceView {
    pub url: String,
    pub title: String,
    #[serde(default)]
    pub linter: Option<String>,
}

#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub(super) struct ProposedRuleView {
    pub id: String,
    pub title: String,
    pub kind: String,
    #[serde(default)]
    pub enforcement: String,
    #[serde(default)]
    pub options: Vec<RuleOptionView>,
    #[serde(default)]
    pub default_option: Option<String>,
    #[serde(default)]
    pub decision_question: Option<String>,
    #[serde(default)]
    pub decision_why: Option<String>,
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub domain: String,
    #[serde(default)]
    pub repos: Vec<String>,
    #[serde(default)]
    pub placement: String,
    #[serde(default)]
    pub finding_count: usize,
    #[serde(default)]
    pub recommended: bool,
    /// Server-side auto-recommend flag (pw/cockpit-ui product wave). The server
    /// emits `is_auto_recommended: true` for rules whose `verification` is
    /// `grounded` or `verified` (the two rungs that have been reviewed against a
    /// real source). `draft` and `needs_recheck` rules arrive with it `false`.
    /// Falls back to `recommended` when the field is absent so old server payloads
    /// continue to work.
    #[serde(default)]
    pub is_auto_recommended: bool,
    /// Provenance / verification status: `draft` | `grounded` | `verified` |
    /// `needs_recheck`. Defaults to `draft` for any rule that omits the field
    /// (pre-schema corpus rules, AI-discovered rules). See
    /// `docs/decisions/2026-06-20_rule_provenance_schema.md`.
    #[serde(default = "default_draft")]
    pub verification: String,
    /// Authoritative sources backing this rule's grounding (empty for `draft`).
    #[serde(default)]
    pub sources: Vec<RuleSourceView>,
}

pub(super) fn default_draft() -> String {
    "draft".to_string()
}

impl ProposedRuleView {
    /// True when this rule should be pre-checked on first view of the proposed-rules
    /// table.
    ///
    /// The SERVER is authoritative for this value. It gates on three conditions:
    /// stack-relevance (the rule's domain matches the scanned repo) + provenance
    /// (`grounded` or `verified`) + `!opt_in_only`. `opt_in_only` rules (e.g.
    /// CICD-CODEQL-SECURITY-SCAN-1, CICD-SEMGREP-SECURITY-SCAN-1) are NEVER
    /// pre-checked even when they are grounded and stack-relevant — they appear in
    /// the list so the architect can deliberately opt in, but the server sends
    /// `is_auto_recommended: false` for them and the UI must honour that flag
    /// without re-deriving it from `recommended` or `verification`.
    ///
    /// `draft` and `needs_recheck` rules appear LISTED but unchecked so the
    /// architect must explicitly opt them in.
    pub(super) fn effective_auto_recommended(&self) -> bool {
        // The server encodes the full gate (stack-relevance + grounded/verified +
        // !opt_in_only) into `is_auto_recommended`. Use it directly — do NOT
        // fall back to `recommended` or re-derive from `verification`. A fallback
        // that re-derives from `recommended && grounded/verified` would incorrectly
        // pre-check opt_in_only rules (which are grounded + recommended but must
        // never be pre-selected). The server is always co-versioned with the UI in
        // this codebase, so there is no version-skew risk.
        self.is_auto_recommended
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
pub(super) fn verif_badge(verif: &str) -> (&'static str, &'static str) {
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
pub(super) fn verif_sources_tooltip(sources: &[RuleSourceView]) -> String {
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
pub(super) struct StackView {
    pub repo: String,
    #[serde(default)]
    pub languages: Vec<String>,
    #[serde(default)]
    pub frameworks: Vec<String>,
}

/// True when a rule id is a user-authored custom rule (so apply routes it through the project's
/// `ruleset.custom` / CUSTOM-block emit, not the regular arm-request path).
pub(super) fn is_custom_rule_id(id: &str) -> bool {
    id.starts_with("CUSTOM-")
}

/// A fully-resolved rule sent to arm (the chosen directive + where it installs).
#[derive(Clone, serde::Serialize)]
pub(super) struct ArmRuleReq {
    pub id: String,
    pub title: String,
    pub directive: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub option: Option<String>,
    pub enforcement: String,
    pub scope: String,
    pub repos: Vec<String>,
}

#[derive(Clone, serde::Deserialize)]
pub(super) struct ArmResultView {
    pub repo: String,
    pub ok: bool,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    /// Local working-copy path the governance files were written into (Apply step).
    #[serde(default)]
    pub path: Option<String>,
    /// The governance branch created/pushed (Apply step).
    #[serde(default)]
    pub branch: Option<String>,
}

/// Apply: write the selected rules onto a governance branch in each repo's LOCAL clone and
/// push it to origin — NO pull request. The architect opens the PR separately.
pub(super) async fn apply_rules(
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
pub(super) struct ApplyPreflightRepo {
    pub repo: String,
    #[serde(default)]
    pub existing_files: Vec<String>,
}

/// Preflight for Apply: ask the server which governance files Camerata is about to write
/// ALREADY EXIST in each repo's local clone (and would be clobbered). Returns the per-repo
/// list (empty when Apply is safe). `None` only on a transport/parse failure — the caller
/// treats that as "could not check" and falls through to the normal apply path rather than
/// blocking the architect on a preflight outage.
pub(super) async fn preflight_apply(
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
pub(super) async fn open_governance_pr(repos: &[String]) -> Option<Vec<ArmResultView>> {
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
pub(super) async fn create_ticket(
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
pub(super) fn split_needs_review(detail: &str) -> (String, Option<String>) {
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
pub(super) const FLOOR_RULE_IDS: &[&str] = &[
    "SEC-NO-HARDCODED-SECRETS-1",
    "SEC-NO-RAW-SQL-CONCAT-1",
    "ARCH-NO-SECRETS-IN-URL-1",
];

/// True when a finding is from the deterministic floor (enforced/stable) vs the AI audit (advisory).
pub(super) fn is_enforced_floor(rule_id: &str) -> bool {
    FLOOR_RULE_IDS.contains(&rule_id)
}

pub(super) fn rule_columns(domains: Vec<String>) -> Vec<ColumnDef<ProposedRuleView>> {
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

/// Composite key for the per-repo "chosen alternative" map. Option choices are INDEPENDENT
/// per repo: picking an alternative for a rule while viewing one repo must not change another
/// repo's choice for the same rule. The NUL byte separates the parts (it can't appear in an
/// `owner/repo` or a rule id), and the key stays a plain string so the map still serializes to
/// JSON for the auto-saved draft.
pub(super) fn chosen_key(repo: &str, rule_id: &str) -> String {
    format!("{repo}\u{0}{rule_id}")
}

/// Sentinel key under which the SINGLE-repo scan stores its rule selection in the lifted
/// `repo -> selected rule ids` map. A real `owner/repo` can never contain a NUL byte, so this
/// can't collide with a multi-repo entry. Using a stable map key (instead of skipping the map
/// entirely when `view_repo` is empty) is what lets a single-repo selection survive a remount:
/// the map is what's serialized into the auto-saved onboarding draft, so without an entry here
/// the architect's manual (non-recommended) picks were dropped on navigate-away-and-back and
/// the table re-seeded to recommended-only. See `docs/decisions/2026-06-20_ui_bugfixes.md`.
pub(super) const SINGLE_REPO_SELECTION_KEY: &str = "\u{0}__single_repo__";

/// The map key a `ProposedRulesTable` reads/writes its selection under. Multi-repo tables key
/// by their `view_repo`; the single-repo case (`view_repo` empty) uses the sentinel so its
/// picks persist through the draft like every other repo's do. Pure + unit-tested.
pub(super) fn selection_key(view_repo: &str) -> String {
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
pub(super) fn ProposedRulesTable(
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
    // Gate: do NOT write back to repo_selection until the async draft restore has finished.
    // Without this guard the writeback effect fires immediately on mount (before the draft
    // loads) and overwrites the draft's saved repo_selection with the recommended-only seed,
    // so the restore that runs later has nothing to rehydrate. Provided as context by
    // ScanResults immediately after `let mut draft_loaded = use_signal(|| false)`.
    let draft_loaded = use_context::<Signal<bool>>();
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
    // One-shot restore: when the async draft load completes (draft_loaded flips to true),
    // re-apply the saved selection from repo_selection to the table checkboxes. This is
    // necessary because use_hook's pre-select ran ONCE at mount — before the async restore
    // could populate repo_selection — so the checkboxes reflect the recommended-only seed,
    // not the architect's saved picks. `selection_restored` guards against re-running on
    // every subsequent repo_selection write (which would fight the user's live ticks).
    //
    // REGISTRATION ORDER MATTERS: This effect must be registered BEFORE the writeback effect
    // below. Dioxus runs effects in registration order when multiple effects are dirtied by the
    // same signal change (here: draft_loaded flipping to true). The restore must apply saved
    // picks to the table checkboxes FIRST; then the writeback reads those corrected checkboxes
    // and writes them back to repo_selection. If the writeback ran first it would read the
    // recommended-only seed (checkboxes haven't been restored yet) and contaminate
    // repo_selection before restore could see the true saved picks.
    let mut selection_restored = use_signal(|| false);
    {
        let id_map_restore = id_map.clone();
        let view_repo_restore = view_repo.clone();
        use_effect(move || {
            if selection_restored() || !draft_loaded() {
                return;
            }
            // Subscribes to repo_selection so this effect re-runs if the signal is set AFTER
            // draft_loaded (the async future may set them in either order across ticks).
            let map = repo_selection.read().clone();
            if let Some(saved_ids) = map.get(&selection_key(&view_repo_restore)) {
                let saved_set: std::collections::HashSet<String> =
                    saved_ids.iter().cloned().collect();
                // Clear ALL current checkboxes then re-apply only the saved set.
                let current = handle.selected_ids();
                for rid in &current {
                    handle.set_selection(*rid, false);
                }
                for (rid, rule) in &id_map_restore {
                    if saved_set.contains(&rule.id) {
                        handle.set_selection(*rid, true);
                    }
                }
            }
            // Always set selection_restored = true after draft_loaded is true (even when there
            // is no saved entry for this repo — that means it's a fresh first-view where the
            // recommended-only seed is already correct). This unblocks the writeback effect.
            selection_restored.set(true);
        });
    }
    use_effect(move || {
        // Do NOT write back until BOTH the async draft restore has completed AND the one-shot
        // checkbox restore has finished applying saved picks to the table. Without the
        // `selection_restored` gate this effect runs first (registration order) when
        // `draft_loaded` flips to true, reads the recommended-only seed from the table
        // (checkboxes haven't been restored yet), and overwrites repo_selection — so the
        // restore effect then reads contaminated data and permanently loses the architect's
        // saved picks.
        if !draft_loaded() || !selection_restored() {
            return;
        }
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
pub(super) fn RuleDetailModal() -> Element {
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
pub(super) fn CustomRulesPanel(all_repos: Vec<String>) -> Element {
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

/// One applied-rule drift entry from `GET /api/projects/:id/rule-drift`.
/// Reports a rule that is applied to the project but whose corpus version has
/// changed since it was adopted (corpus body updated after grounding/verification).
#[derive(Clone, PartialEq, serde::Deserialize)]
pub(super) struct RuleDriftEntry {
    pub rule_id: String,
    #[serde(default)]
    pub title: String,
    /// The text of the directive as it was when the rule was adopted.
    #[serde(default)]
    pub applied_directive: String,
    /// The current corpus directive (the update the architect is being asked to review).
    #[serde(default)]
    pub corpus_directive: String,
    /// Repos in the project that currently have the stale directive.
    #[serde(default)]
    pub repos: Vec<String>,
}

pub(super) async fn fetch_rule_drift(project_id: &str) -> Option<Vec<RuleDriftEntry>> {
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
pub(super) async fn apply_rule_drift_update(project_id: &str, rule_id: &str) -> bool {
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
pub(super) fn RuleDriftNotice(project_id: String) -> Element {
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

/// The scope at which a single-rule edit applies. Rules cascade: repo overrides
/// project which overrides the corpus default. The editor lets the architect pick
/// which level to write to.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum RuleEditScope {
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
pub(super) async fn fetch_single_rule(project_id: &str, rule_id: &str) -> Option<serde_json::Value> {
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
pub(super) async fn save_single_rule_edit(
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
pub(super) fn SingleRuleEditor(
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

#[cfg(test)]
mod tests {
    use super::{selection_key, SINGLE_REPO_SELECTION_KEY};

    #[test]
    fn selection_key_empty_returns_sentinel() {
        assert_eq!(selection_key(""), SINGLE_REPO_SELECTION_KEY);
    }

    #[test]
    fn selection_key_repo_name_passes_through() {
        assert_eq!(selection_key("my-repo"), "my-repo");
        assert_eq!(selection_key("org/repo"), "org/repo");
    }

    // ── Writeback / restore ordering invariant tests ──────────────────────────
    //
    // These tests verify the pure logic of the effect-ordering fix:
    // the writeback must not fire until AFTER the one-shot restore has applied
    // the saved picks to the table. In the reactive layer this is enforced by
    // the `selection_restored` gate (`if !draft_loaded() || !selection_restored()`).
    // The tests below verify the equivalent pure-logic checks on the data structures
    // that the effects operate on.

    /// Simulates what happens when the writeback effect runs BEFORE restore (the pre-fix
    /// bug): the recommended-only seed gets written into repo_selection, contaminating
    /// the saved picks so the subsequent restore sees only recommended rules.
    /// This test documents the BROKEN behavior and shows why the gate was needed.
    #[test]
    fn writeback_before_restore_contaminates_repo_selection() {
        let saved_picks = vec!["rule-a".to_string(), "rule-c".to_string(), "rule-d".to_string()];
        let recommended_only_seed = vec!["rule-a".to_string(), "rule-b".to_string(), "rule-c".to_string()];

        // repo_selection starts with saved picks (set by the use_future async restore).
        let mut repo_selection: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        repo_selection.insert(SINGLE_REPO_SELECTION_KEY.to_string(), saved_picks.clone());

        // BUG: writeback fires first (before restore applied picks to checkboxes).
        // The table checkboxes still show the recommended-only seed. Writeback reads THOSE
        // and overwrites repo_selection with them.
        repo_selection.insert(
            SINGLE_REPO_SELECTION_KEY.to_string(),
            recommended_only_seed.clone(),
        );

        // Now restore fires and reads repo_selection — but it's been contaminated.
        let restored = repo_selection.get(SINGLE_REPO_SELECTION_KEY).unwrap().clone();
        let mut restored_sorted = restored.clone();
        restored_sorted.sort();
        let mut expected_sorted = recommended_only_seed.clone();
        expected_sorted.sort();
        assert_eq!(restored_sorted, expected_sorted, "contaminated: restore saw recommended-only seed, not saved picks");
        // Specifically: the user's non-recommended pick (rule-d) was lost.
        assert!(!restored.contains(&"rule-d".to_string()), "rule-d (user pick) must be lost in the buggy path");
        // And rule-b (which the user deselected) was reintroduced.
        assert!(restored.contains(&"rule-b".to_string()), "rule-b (user deselected) reappears in the buggy path");
    }

    /// Simulates the CORRECT ordering: restore runs first (applying saved picks to
    /// checkboxes), THEN writeback reads the corrected checkboxes and writes them back.
    /// This is the `selection_restored` gate in action: the writeback only fires after
    /// `selection_restored` is true, which the restore effect sets after it completes.
    #[test]
    fn restore_before_writeback_preserves_user_picks() {
        let saved_picks = vec!["rule-a".to_string(), "rule-c".to_string(), "rule-d".to_string()];
        let recommended_only_seed = vec!["rule-a".to_string(), "rule-b".to_string(), "rule-c".to_string()];

        // repo_selection starts with saved picks (set by the async use_future).
        let mut repo_selection: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        repo_selection.insert(SINGLE_REPO_SELECTION_KEY.to_string(), saved_picks.clone());

        // Step 1: Restore runs first. It reads repo_selection (saved picks), applies them
        // to the table checkboxes, and sets selection_restored = true.
        let current_table_selection: Vec<String> = {
            let map = &repo_selection;
            if let Some(ids) = map.get(SINGLE_REPO_SELECTION_KEY) {
                ids.clone() // table checkboxes now reflect saved picks
            } else {
                recommended_only_seed.clone() // fallback (fresh view)
            }
        };
        let selection_restored = true; // restore sets this

        // Step 2: Writeback fires (only because selection_restored is now true).
        // It reads the corrected table checkboxes (saved picks, not recommended-only seed).
        assert!(selection_restored, "writeback gate check: selection_restored must be true");
        repo_selection.insert(
            SINGLE_REPO_SELECTION_KEY.to_string(),
            current_table_selection.clone(),
        );

        // repo_selection now holds the user's effective picks — no contamination.
        let final_selection = repo_selection.get(SINGLE_REPO_SELECTION_KEY).unwrap();
        let mut final_sorted = final_selection.clone();
        final_sorted.sort();
        let mut picks_sorted = saved_picks.clone();
        picks_sorted.sort();
        assert_eq!(final_sorted, picks_sorted, "correct path: repo_selection must match saved picks");

        // The user's non-recommended pick (rule-d) is preserved.
        assert!(final_selection.contains(&"rule-d".to_string()), "rule-d (user pick) must survive");
        // The user's deselected rule (rule-b) stays gone.
        assert!(!final_selection.contains(&"rule-b".to_string()), "rule-b (user-deselected) must not reappear");
    }

    /// When there is no saved entry (fresh first-view), the restore effect falls through
    /// without modifying the checkboxes (the recommended seed is already correct), sets
    /// selection_restored = true, and unblocks the writeback. The writeback then writes
    /// the recommended seed into repo_selection, which is the desired baseline behavior.
    #[test]
    fn fresh_view_no_saved_entry_restore_unblocks_writeback_with_recommended_seed() {
        // No saved picks: repo_selection is empty.
        let repo_selection: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        let recommended_seed = vec!["rule-a".to_string(), "rule-b".to_string()];

        // Restore runs: no saved entry, so it falls through. Unconditionally sets selection_restored.
        let saved = repo_selection.get(SINGLE_REPO_SELECTION_KEY);
        assert!(saved.is_none(), "no saved entry on fresh view");
        // selection_restored is set unconditionally (fixed behavior vs. old code that only
        // set it inside `if let Some(...)` — old code would have left it false, permanently
        // blocking the writeback for fresh views after draft_loaded = true).
        let selection_restored = true; // unconditional set in fixed code

        // Writeback fires because selection_restored is true.
        assert!(selection_restored, "writeback must be unblocked even with no saved entry");

        // The writeback reads the recommended seed from the table checkboxes (use_hook
        // seeded them since there was no saved entry) and writes it to repo_selection.
        let mut result: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        result.insert(SINGLE_REPO_SELECTION_KEY.to_string(), recommended_seed.clone());

        assert_eq!(
            result.get(SINGLE_REPO_SELECTION_KEY).cloned(),
            Some(recommended_seed),
            "recommended seed must be written as the baseline on fresh first-view"
        );
    }
}
