//! Repo health panel and the code context and recommended fix blocks of the finding modal.

use super::*;

/// One repo's local-path resolution status (issue #33), from `/api/projects/:id/repo-health`.
#[derive(Clone, PartialEq, serde::Deserialize)]
pub(in crate::cockpit) struct RepoResolutionView {
    pub repo: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub resolved: bool,
    #[serde(default)]
    pub reason: String,
    /// The source was deleted from this machine on purpose; there is nothing to resolve.
    #[serde(default)]
    pub source_deleted: bool,
    #[serde(default)]
    pub deleted_at: Option<String>,
}

pub(in crate::cockpit) async fn fetch_repo_health(
    project_id: &str,
) -> Option<Vec<RepoResolutionView>> {
    let v: serde_json::Value = reqwest::get(format!(
        "{}/api/projects/{}/repo-health",
        crate::bff_base(),
        project_id
    ))
    .await
    .ok()?
    .json()
    .await
    .ok()?;
    serde_json::from_value(v.get("repos")?.clone()).ok()
}

pub(in crate::cockpit) async fn set_repo_path(repo: &str, path: &str) -> bool {
    reqwest::Client::new()
        .post(format!("{}/api/repo-path", crate::bff_base()))
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
pub(in crate::cockpit) fn RepoHealthPanel(project_id: String) -> Element {
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
    // A repo whose source was deleted on purpose has nothing to resolve: it is neither "broken"
    // nor "fine", and the way back is Re-fetch source, not a folder picker.
    let broken = repos
        .iter()
        .filter(|r| !r.resolved && !r.source_deleted)
        .count();
    let deleted = repos.iter().filter(|r| r.source_deleted).count();
    rsx! {
        div { class: "repo-health",
            if broken == 0 && deleted == 0 {
                p { class: "repo-health-ok", "✓ All {repos.len()} repo path(s) resolve to a local checkout." }
            } else if broken > 0 {
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
                    let source_deleted = r.source_deleted;
                    let deleted_block = camerata_ui_core::readiness::ScanBlock {
                        repos: vec![(r.repo.clone(), r.deleted_at.clone().unwrap_or_default())],
                    };
                    rsx! {
                        div { class: "repo-health-row", key: "{r.repo}",
                            span { class: if resolved { "repo-health-icon ok" } else if source_deleted { "repo-health-icon deleted" } else { "repo-health-icon bad" },
                                if resolved { "✓" } else if source_deleted { "–" } else { "⚠" }
                            }
                            span { class: "repo-health-repo", "{r.repo}" }
                            if source_deleted {
                                crate::source_ui::ScanBlockNotice { block: deleted_block }
                            } else if resolved {
                                span { class: "repo-health-path", "{path}" }
                            } else {
                                span { class: "repo-health-reason", "{reason}" }
                                button {
                                    class: "btn-edit-sm",
                                    onclick: move |_| {
                                        let repo = repo.clone();
                                        spawn(async move {
                                            if let Some(folder) = camerata_ui::platform::current()
                                                .pick_folder("Choose this repo's local folder")
                                                .await
                                            {
                                                let p = folder.to_string_lossy().to_string();
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

// ── Finding-code-context (docs/design/2026-07-27_finding-code-context.md) ─────────
//
// Shows the WHOLE enclosing code block a finding's violation line lives in — the whole
// function / RLS policy / SQL statement — inside the finding-detail modal, so the reviewer
// sees context without opening the file. ON-DEMAND (no scan-time cache): each finding modal
// open triggers exactly one small local-file read + parse on the server, always reflecting
// the file as it exists right now. Zero LLM tokens — this is a local read + deterministic
// display feature, fully decoupled from the AI audit.

/// One `GET /api/onboard/finding-context` response, deserialized straight off the wire. Every
/// field defaults so a response the UI doesn't recognize (or an older/newer server) degrades
/// to an empty/false shape rather than failing to parse.
#[derive(Clone, PartialEq, serde::Deserialize)]
pub(super) struct FindingContextView {
    #[serde(default)]
    status: String,
    #[serde(default)]
    lines: Vec<String>,
    #[serde(default)]
    start_line: usize,
    #[serde(default)]
    violation_line: usize,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    matches_snippet: bool,
    #[serde(default)]
    reason: String,
    /// Where the block was read from: `git` (the inspected commit) or `working_tree`.
    #[serde(default)]
    source: String,
    /// The commit a `git` read was taken at.
    #[serde(default)]
    commit: String,
    /// The checkout has moved on from `commit`.
    #[serde(default)]
    head_differs: bool,
    /// When the source was deleted (`source_deleted` status).
    #[serde(default)]
    deleted_at: Option<String>,
}

impl FindingContextView {
    /// The banner or note that explains where this block came from, if any (pure mapping in
    /// `camerata_ui_core::source::context_note`).
    fn note(&self) -> Option<camerata_ui_core::source::ContextNote> {
        camerata_ui_core::source::context_note(
            &self.status,
            &self.source,
            &self.commit,
            self.head_differs,
            self.deleted_at.as_deref(),
        )
    }
}

/// The note under (or instead of) a finding's code block.
#[component]
pub(super) fn ContextNoteView(note: camerata_ui_core::source::ContextNote) -> Element {
    use camerata_ui_core::source::ContextNote;
    let nav = try_consume_context::<Signal<CockpitView>>();
    match note {
        ContextNote::SourceDeleted(text) => rsx! {
            p { class: "finding-ctx-note finding-ctx-deleted",
                span { "{text}" }
                button {
                    class: "btn-edit-sm finding-ctx-link",
                    onclick: move |_| {
                        if let Some(mut nav) = nav {
                            nav.set(CockpitView::Workspace);
                        }
                    },
                    "Open Workspace"
                }
            }
        },
        ContextNote::NewerSource(text) => rsx! {
            p { class: "finding-ctx-note finding-ctx-newer", "{text}" }
        },
        ContextNote::CommitUnavailable(text) => rsx! {
            p { class: "finding-ctx-banner", "{text}" }
        },
    }
}

pub(super) async fn fetch_finding_context(
    repo: &str,
    path: &str,
    line: usize,
    expect: &str,
) -> Option<FindingContextView> {
    let mut params = vec![
        ("repo", repo.to_string()),
        ("path", path.to_string()),
        ("line", line.to_string()),
    ];
    if !expect.trim().is_empty() {
        params.push(("expect", expect.trim().to_string()));
    }
    let resp = reqwest::Client::new()
        .get(format!("{}/api/onboard/finding-context", crate::bff_base()))
        .query(&params)
        .send()
        .await
        .ok()?;
    let v: serde_json::Value = resp.json().await.ok()?;
    serde_json::from_value(v).ok()
}

/// A one-line human reason for a degraded (non-`ok`) status — mirrors the endpoint's own
/// `reason` field as a client-side fallback in case an older server sends the status alone.
pub(super) fn degraded_context_reason(status: &str, reason: &str) -> String {
    if !reason.is_empty() {
        return reason.to_string();
    }
    match status {
        "file_missing" => "file not found — changed since scan?",
        "line_gone" => "file changed since scan",
        "not_utf8" => "file is not valid UTF-8 (binary?)",
        "too_large" => "file too large to load",
        "path_traversal" => "couldn't resolve this file path",
        _ => "couldn't load code context",
    }
    .to_string()
}

pub(super) fn context_kind_label(kind: &str) -> &'static str {
    match kind {
        "function" => "enclosing function",
        "sql_statement" => "SQL statement",
        _ => "surrounding lines",
    }
}

/// Big-block cap (design §4): a block over this many lines initially shows only ±40 lines
/// around the violation, with a "Show full block" toggle to reveal the rest. The resolver
/// itself never truncates — this is a pure display concern, applied client-side over the
/// already-fetched `lines` so expanding never needs a second round trip.
pub(super) const FINDING_CTX_CAP: usize = 80;
pub(super) const FINDING_CTX_HALF: usize = 40;

/// The commit each repo was inspected at, from the scan's and the audit's `audited_refs`, and the
/// repos where the two differ (see `camerata_ui_core::viewer::merge_audited_shas`). Repos with no
/// recorded commit are left out: the server then reads the commit the project's latest inspection
/// recorded.
pub(in crate::cockpit) fn audited_shas(
    report: &ScanReportView,
    audited: Option<&ScanReportView>,
) -> camerata_ui_core::viewer::ShaChoice {
    let pairs = |r: &ScanReportView| -> Vec<(String, Option<String>)> {
        r.provenance
            .audited_refs
            .iter()
            .map(|a| (a.repo.clone(), a.sha.clone()))
            .collect()
    };
    camerata_ui_core::viewer::merge_audited_shas(
        &pairs(report),
        &audited.map(pairs).unwrap_or_default(),
    )
}

/// The origin remote each repo was scanned from, from the scan's and the audit's `audited_refs`
/// (the audit's wins where both recorded one). Repos with no recorded origin are left out.
pub(in crate::cockpit) fn audited_origins(
    report: &ScanReportView,
    audited: Option<&ScanReportView>,
) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    for r in [Some(report), audited].into_iter().flatten() {
        for a in &r.provenance.audited_refs {
            if let Some(url) = a.origin_url.as_ref().filter(|u| !u.is_empty()) {
                out.insert(a.repo.clone(), url.clone());
            }
        }
    }
    out
}

/// The "Code context" section of the finding-detail modal. Fetches ON-DEMAND every time it
/// mounts; the caller keys this component by `repo`/`path`/`line` (see the finding-detail
/// modal) so opening a DIFFERENT finding remounts it fresh rather than showing stale content
/// left over from the previous one.
///
/// Degradation floor (design §5): the stored `finding.snippet` is already rendered above this
/// section by the caller. ANY failure here — file moved/deleted, line gone, non-UTF8,
/// oversized, unresolved repo, a path-traversal refusal, or the request simply failing —
/// renders a single reason line and nothing else. This section can only ADD context on top of
/// the snippet the modal already shows; it never replaces or hides it, and never panics.
#[component]
pub(super) fn FindingCodeContext(
    finding: FindingView,
    /// Open the whole file in the code viewer. `None` when the viewer is not reachable from here
    /// (the Queue is not the surface on screen).
    #[props(default)]
    on_open_viewer: Option<Callback<()>>,
) -> Element {
    let expect_line = finding
        .snippet
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    let f = finding.clone();
    let ctx_res = use_resource(move || {
        let f = f.clone();
        let expect_line = expect_line.clone();
        async move { fetch_finding_context(&f.repo, &f.path, f.line, &expect_line).await }
    });

    let ctx = ctx_res.read().clone();
    let door = on_open_viewer.map(|cb| {
        rsx! {
            button {
                class: "btn-edit-sm finding-ctx-viewer",
                title: "Read the whole file in the code viewer, where you can also decide this finding.",
                onclick: move |_| cb.call(()),
                "Open in viewer"
            }
        }
    });
    let body = match ctx {
        None => rsx! {
            div { class: "finding-ctx finding-ctx-loading",
                p { class: "rule-modal-label", "Code context" }
                span { class: "finding-ctx-note", "loading…" }
            }
        },
        // The request itself failed outright (server unreachable, bad JSON, etc.) — the
        // stored snippet above already covers the finding, so say nothing further here
        // rather than showing a confusing empty error box.
        Some(None) => rsx! {},
        Some(Some(v)) => rsx! { FindingContextBlock { view: v } },
    };
    rsx! {
        if let Some(door) = door {
            div { class: "finding-ctx-door", {door} }
        }
        {body}
    }
}

/// The "Code context" block for one answered `finding-context` request: the code (from the
/// inspected commit, or the working tree when that is all there is), the note about where it
/// came from, or the reason there is nothing to show. Props-only so it renders in tests.
#[component]
pub(super) fn FindingContextBlock(view: FindingContextView) -> Element {
    let v = view;
    let mut expanded = use_signal(|| false);
    let note = v.note();
    // The code block renders for a normal read and for the commit-unavailable fallback (the
    // working tree rides along); every other status degrades to one reason line.
    let has_block = v.status == "ok" || (v.status == "commit_unavailable" && !v.lines.is_empty());
    if !has_block {
        return rsx! {
            div { class: "finding-ctx finding-ctx-degraded",
                p { class: "rule-modal-label", "Code context" }
                if let Some(note) = note.clone().filter(|n| matches!(n, camerata_ui_core::source::ContextNote::SourceDeleted(_))) {
                    ContextNoteView { note }
                } else {
                    span { class: "finding-ctx-note", "{degraded_context_reason(&v.status, &v.reason)}" }
                }
            }
        };
    }
    let total = v.lines.len();
    let violation_idx = v.violation_line.saturating_sub(v.start_line);
    let capped = total > FINDING_CTX_CAP;
    let (show_from, show_to) = if capped && !expanded() {
        let from = violation_idx.saturating_sub(FINDING_CTX_HALF);
        let to = (violation_idx + FINDING_CTX_HALF).min(total.saturating_sub(1));
        (from, to)
    } else {
        (0, total.saturating_sub(1))
    };
    let above = show_from;
    let below = total.saturating_sub(1).saturating_sub(show_to);
    rsx! {
        div { class: "finding-ctx",
            // The amber banner (commit unavailable) goes above the code it qualifies.
            if let Some(camerata_ui_core::source::ContextNote::CommitUnavailable(text)) = note.clone() {
                ContextNoteView { note: camerata_ui_core::source::ContextNote::CommitUnavailable(text) }
            }
            div { class: "finding-ctx-head",
                p { class: "rule-modal-label", "Code context" }
                span { class: "rule-modal-tag", "{context_kind_label(&v.kind)}" }
                if !v.matches_snippet {
                    span { class: "rule-modal-tag finding-ctx-stale", "file changed since this scan" }
                }
            }
            if capped && above > 0 {
                p { class: "finding-ctx-fold", "… {above} more lines above …" }
            }
            pre { class: "finding-ctx-pre",
                for (i, line) in v.lines.iter().enumerate() {
                    if i >= show_from && i <= show_to {
                        {
                            let lineno = v.start_line + i;
                            let hit = lineno == v.violation_line;
                            let row_class = if hit { "finding-ctx-line finding-ctx-line-hit" } else { "finding-ctx-line" };
                            rsx! {
                                div { class: "{row_class}", key: "{lineno}",
                                    span { class: "finding-ctx-gutter", "{lineno}" }
                                    span { class: "finding-ctx-code", "{line}" }
                                }
                            }
                        }
                    }
                }
            }
            if capped && below > 0 {
                p { class: "finding-ctx-fold", "… {below} more lines below …" }
            }
            if capped {
                button {
                    class: "finding-ctx-toggle",
                    onclick: move |_| expanded.set(!expanded()),
                    if expanded() { "Show less" } else { "Show full block ({total} lines)" }
                }
            }
            // "References commit <sha>; source on disk is newer" sits under the code it describes.
            if let Some(camerata_ui_core::source::ContextNote::NewerSource(text)) = note {
                ContextNoteView { note: camerata_ui_core::source::ContextNote::NewerSource(text) }
            }
        }
    }
}

// ── Recommended fix (docs/design/2026-07-26_audit-report-refinements.md) ──────────
//
// Surfaces the finding's recommended fix directly in the finding-detail modal, during
// triage — rather than the architect only discovering it later in an exported report.
// MUST be byte-identical to the report's own "Fix:" line (PDF) / "Recommended Fix" column
// (xlsx): both of those call `report_export::resolve_fix(rule_id, corpus)` server-side, and
// this component fetches that SAME function's output over `GET /api/onboard/finding-fix`
// rather than re-deriving the corpus join client-side — one function, three renderers, never
// a second divergent computation. See `crates/server/src/lib.rs::onboard_finding_fix`.

/// One `GET /api/onboard/finding-fix` response. `fix` defaults to empty so an unrecognized/
/// older/newer server response degrades to "no fix" rather than failing to parse.
#[derive(Clone, PartialEq, serde::Deserialize)]
pub(super) struct FindingFixView {
    #[serde(default)]
    pub(super) fix: String,
}

pub(super) async fn fetch_finding_fix(rule_id: &str) -> Option<FindingFixView> {
    let resp = reqwest::Client::new()
        .get(format!("{}/api/onboard/finding-fix", crate::bff_base()))
        .query(&[("rule_id", rule_id)])
        .send()
        .await
        .ok()?;
    let v: serde_json::Value = resp.json().await.ok()?;
    serde_json::from_value(v).ok()
}

/// The "Recommended fix" block of the finding-detail modal. Fetches ON-DEMAND every time it
/// mounts; the caller keys this component by `rule_id` (see the finding-detail modal) so
/// switching to a different finding remounts it fresh.
///
/// Degradation floor (matches `FindingCodeContext`'s discipline): no corpus entry, no
/// adopted default option, a blank directive, a request that fails outright, or a response
/// this client doesn't recognize — ALL of these render nothing rather than an empty box or a
/// fabricated sentence. This block can only ADD information on top of the modal's existing
/// "Rule violated" / "Explanation" sections; it never blocks or replaces them, and never
/// panics.
#[component]
pub(super) fn FindingRecommendedFix(rule_id: String) -> Element {
    let rid = rule_id.clone();
    let fix_res = use_resource(move || {
        let rid = rid.clone();
        async move { fetch_finding_fix(&rid).await }
    });

    let fix = fix_res.read().clone();
    match fix {
        // Loading, request failed outright, or an unparseable response — the modal's
        // existing "Rule violated"/"Explanation" text already covers the finding, so this
        // section says nothing rather than showing a loading flicker or an error box.
        None | Some(None) => rsx! {},
        Some(Some(v)) if !v.fix.trim().is_empty() => rsx! {
            div { class: "finding-ctx",
                p { class: "rule-modal-label", "Recommended fix" }
                p { class: "rule-modal-detail", "{v.fix}" }
            }
        },
        // No corpus directive for this rule — omit the block entirely, never an empty one.
        Some(Some(_)) => rsx! {},
    }
}
