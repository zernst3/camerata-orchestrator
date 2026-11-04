//! The active-project context endpoint and the chat-facing scan renderers.

use crate::*;

/// The pre-onboard phase: the active project has a saved onboarding draft but has not yet
/// completed the Apply step for any repo.
///
/// The draft is a UI-owned blob; we surface its raw JSON as the grounding text so the
/// project-aware chat can reference what the architect was doing. Callers should treat the
/// draft as informational context, not canonical state.
#[derive(serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectPhase {
    /// Project has no saved draft and no onboarded repos — truly blank.
    Blank,
    /// An onboarding draft exists but Apply has not yet been completed for any repo.
    PreOnboard,
    /// At least one repo is onboarded (Apply completed).
    PostOnboard,
}

/// The grounding payload the project-aware chat mode uses to build its system prompt.
///
/// The UI fetches this once when the Project tab is opened and injects its fields into
/// the system prompt it sends to `POST /api/chat`. This keeps the LLM call on the
/// existing `chat` handler — no separate AI endpoint is needed here.
#[derive(serde::Serialize)]
pub struct ProjectContextResponse {
    /// Whether the project exists (false when there is no active project).
    pub ok: bool,
    /// Onboarding phase determines which grounding fields are populated.
    pub phase: ProjectPhase,
    /// The active project's stable id (present when `ok=true`). Lets the client confirm
    /// which project this grounding belongs to, closing the cross-project leak surface.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// The active project's name (present when `ok=true`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_name: Option<String>,
    /// The repos in scope for the active project.
    #[serde(default)]
    pub repos: Vec<String>,
    /// The repos that have completed onboarding.
    #[serde(default)]
    pub onboarded: Vec<String>,
    /// A compact, human-readable summary of the project's selected ruleset (post-onboard).
    /// Each line is `<rule_id>: <scope>` for easy citation in the chat.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ruleset_summary: Option<String>,
    /// The number of active findings from the onboarding audit. Populated post-onboard
    /// when the project has findings recorded in the draft (the draft carries the last audit).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finding_count: Option<usize>,
    /// A compact listing of the most recent findings (up to 50), suitable for injection
    /// into a system prompt. Each entry is a compact one-line summary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub findings_summary: Option<String>,
    /// The raw onboarding draft (pre-onboard: the architect's in-progress session).
    /// This is the UI-owned JSON blob; it is surfaced as-is for the chat to reference.
    /// Only populated in the PreOnboard phase so the Post-onboard prompt is not cluttered.
    ///
    /// INTERNAL REVIEW TIMING NEVER GOES TO A MODEL: the draft carries the reviewer's timer and
    /// per-row reviewed record (`review_log`, internal only), so this value is built by
    /// [`without_internal_review_data`], which removes it. Keep it that way for any new use.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub draft_json: Option<serde_json::Value>,
    /// A compact, queryable summary of the active scan results — severity/status totals,
    /// by-rule breakdown, and a capped finding list. Populated from the onboarding draft
    /// when a scan has been run. The snippet field is NEVER included to avoid leaking
    /// credential-shaped values into the context. Present when a scan report is found in
    /// the draft; absent when no scan has been run yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scan_results_section: Option<String>,
    /// A compact, queryable summary of the rules the user has selected in the onboarding
    /// draft (repo_selection + chosen options), formatted for injection into the chat
    /// system prompt as Layer 3d. Populated whenever repo_selection is non-empty — works
    /// pre-scan and pre-onboard as soon as the user makes a selection. Absent when no
    /// rules are selected yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_rules_section: Option<String>,
    /// A human-readable message explaining why the context is limited (e.g., no active
    /// project, or the project has not been onboarded).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Build a compact one-line ruleset summary: `<rule_id>: <scope>` per selected rule.
/// Cross-repo and process rules are tagged accordingly so the chat can explain the
/// distinction.
pub(crate) fn build_ruleset_summary(ruleset: &crate::project::ProjectRuleset) -> String {
    let mut lines = Vec::new();
    for sel in &ruleset.selections {
        let note = if sel.repos.is_empty() {
            "(all repos)".to_string()
        } else {
            format!("({})", sel.repos.join(", "))
        };
        lines.push(format!("{}: repo-local {}", sel.rule_id, note));
    }
    for sel in &ruleset.cross_repo {
        lines.push(format!("{}: cross-repo", sel.rule_id));
    }
    for sel in &ruleset.process {
        lines.push(format!("{}: process (VCS workflow)", sel.rule_id));
    }
    for c in &ruleset.custom {
        let dom = if c.domain.is_empty() || c.domain == "*" {
            "all repos".to_string()
        } else {
            c.domain.clone()
        };
        lines.push(format!("CUSTOM-{}: custom rule ({})", c.name, dom));
    }
    lines.join("\n")
}

/// Build a compact, queryable scan-results section for the chat system prompt.
///
/// Includes:
/// - Totals: total findings, counts by severity (critical/high/medium/low), counts by
///   status (active / suppressed-* / preview vs floor).
/// - By-rule breakdown: top rules by finding count.
/// - A capped list of the most severe findings (up to `CAP`), each as a single line:
///   `severity · rule_id · repo/path:line · status`.
/// - Coverage notes (tools skipped / not available during the scan).
///
/// The `snippet` field is NEVER included — snippets may contain credential-shaped values
/// and must not be injected into the LLM context. Only rule id, location, severity, status,
/// and the gate `detail` (which is gate-authored prose, not the raw secret) are surfaced.
/// The `detail` is capped at 120 chars per finding.
///
/// Returns an empty string when `findings` is empty (no scan yet — caller should treat as
/// absent and skip the section rather than injecting an empty/misleading block).
pub(crate) fn render_scan_results_for_chat(
    findings: &[crate::onboard::Finding],
    coverage_notes: &[crate::onboard::CoverageNote],
) -> String {
    const CAP: usize = 40;

    if findings.is_empty() && coverage_notes.is_empty() {
        return String::new();
    }

    let mut out = String::new();

    // ── Totals ────────────────────────────────────────────────────────────────
    let total = findings.len();
    let mut by_severity: std::collections::BTreeMap<&str, usize> =
        std::collections::BTreeMap::new();
    let mut active_count: usize = 0;
    let mut suppressed_count: usize = 0;
    let mut preview_count: usize = 0;

    for f in findings {
        *by_severity.entry(f.severity.as_str()).or_insert(0) += 1;
        if f.status == "active" {
            active_count += 1;
        } else if f.status.starts_with("suppressed") {
            suppressed_count += 1;
        }
        if f.preview {
            preview_count += 1;
        }
    }
    let floor_count = total - preview_count;

    out.push_str(&format!("Total findings: {total}\n"));
    // Severity breakdown — emit in fixed order (critical first)
    for sev in &["critical", "high", "medium", "low"] {
        if let Some(&n) = by_severity.get(sev) {
            out.push_str(&format!("  {sev}: {n}\n"));
        }
    }
    // Any unexpected severity values
    for (sev, &n) in &by_severity {
        if !matches!(*sev, "critical" | "high" | "medium" | "low") {
            out.push_str(&format!("  {sev}: {n}\n"));
        }
    }
    out.push_str(&format!(
        "Status: {active_count} active (enforced), {suppressed_count} suppressed\n"
    ));
    out.push_str(&format!(
        "Preview (not yet wired into CI gate): {preview_count} | Floor (enforced): {floor_count}\n"
    ));

    // ── By-rule breakdown ─────────────────────────────────────────────────────
    let mut by_rule: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for f in findings {
        *by_rule.entry(f.rule_id.as_str()).or_insert(0) += 1;
    }
    let mut rule_counts: Vec<(&str, usize)> = by_rule.into_iter().collect();
    // Sort by count descending, then rule_id ascending for determinism
    rule_counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    out.push_str("By rule (top hits):\n");
    for (rule_id, count) in rule_counts.iter().take(20) {
        out.push_str(&format!("  {rule_id}: {count}\n"));
    }

    // ── Capped finding list (severity-ordered, no snippet) ────────────────────
    // Sort: active before suppressed, severity (critical→high→medium→low), then rule_id.
    let sev_rank = |s: &str| match s {
        "critical" => 0u8,
        "high" => 1,
        "medium" => 2,
        "low" => 3,
        _ => 4,
    };
    let mut sorted: Vec<&crate::onboard::Finding> = findings.iter().collect();
    sorted.sort_by(|a, b| {
        // Active before suppressed
        let a_active = a.status == "active";
        let b_active = b.status == "active";
        b_active
            .cmp(&a_active)
            .then_with(|| sev_rank(&a.severity).cmp(&sev_rank(&b.severity)))
            .then_with(|| a.rule_id.cmp(&b.rule_id))
            .then_with(|| a.repo.cmp(&b.repo))
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.line.cmp(&b.line))
    });

    let shown = sorted.len().min(CAP);
    out.push_str(&format!(
        "Top {shown} findings (of {total}, severity-ordered; snippet omitted):\n"
    ));
    for f in sorted.iter().take(CAP) {
        // NEVER include f.snippet — it may contain a credential.
        let detail_preview: String = f.detail.chars().take(120).collect();
        let preview_tag = if f.preview { " [preview]" } else { "" };
        out.push_str(&format!(
            "  {} · {} · {}/{}:{} · {}{}\n",
            f.severity, f.rule_id, f.repo, f.path, f.line, f.status, preview_tag
        ));
        if !detail_preview.is_empty() {
            out.push_str(&format!("    {detail_preview}\n"));
        }
    }

    // ── Coverage notes ────────────────────────────────────────────────────────
    if !coverage_notes.is_empty() {
        out.push_str("Coverage notes (tools skipped / unavailable):\n");
        for note in coverage_notes {
            out.push_str(&format!("  [{}] {}\n", note.tool, note.message));
        }
    }

    out
}

/// Extract a compact, queryable scan-results section from the UI-owned onboarding draft blob.
///
/// Looks for findings in `draft["scan"]["findings"]` or `draft["audit"]["findings"]`,
/// deserializes them as `Vec<Finding>` (the struct that owns the canonical field names),
/// and delegates to `render_scan_results_for_chat`. Returns `None` when no findings section
/// is found, when deserialization fails, or when the findings list is empty.
/// The draft as the project chat may see it: without the review timer's log, which is internal
/// only (`camerata_api_types::review_timing`) and must never reach a model prompt.
pub(crate) fn without_internal_review_data(mut draft: serde_json::Value) -> serde_json::Value {
    if let Some(object) = draft.as_object_mut() {
        object.remove(camerata_api_types::review_timing::REVIEW_LOG_KEY);
    }
    draft
}

pub(crate) fn extract_scan_results_from_draft(draft: &serde_json::Value) -> Option<String> {
    // Prefer the scan section (which contains the deterministic floor + Phase-1 findings)
    // but fall back to the audit section (Phase-2 AI findings live there in some draft shapes).
    let findings_val = draft
        .get("scan")
        .and_then(|s| s.get("findings"))
        .or_else(|| draft.get("audit").and_then(|a| a.get("findings")))?;

    let findings: Vec<crate::onboard::Finding> =
        serde_json::from_value(findings_val.clone()).ok()?;

    if findings.is_empty() {
        return None;
    }

    // Coverage notes are at scan.coverage_notes; absent in audit-only drafts.
    let coverage_notes: Vec<crate::onboard::CoverageNote> = draft
        .get("scan")
        .and_then(|s| s.get("coverage_notes"))
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    let rendered = render_scan_results_for_chat(&findings, &coverage_notes);
    if rendered.is_empty() {
        None
    } else {
        Some(rendered)
    }
}

/// Extract a compact findings summary from a draft JSON blob (the UI-owned onboarding draft).
/// Looks for the `findings` array inside the audit section; returns a one-line-per-finding
/// compact listing, capped at 50 findings to keep the prompt manageable.
pub(crate) fn extract_findings_from_draft(draft: &serde_json::Value) -> Option<(usize, String)> {
    // The draft shape is UI-owned; we look for `audit.findings` or `scan.findings`
    // (the audit section, which contains the Phase-2 AI audit findings).
    let findings = draft
        .get("audit")
        .and_then(|a| a.get("findings"))
        .or_else(|| draft.get("scan").and_then(|s| s.get("findings")))
        .and_then(|f| f.as_array())?;

    if findings.is_empty() {
        return None;
    }
    let total = findings.len();
    let cap = total.min(50);
    let mut lines = Vec::with_capacity(cap);
    for f in findings.iter().take(cap) {
        let repo = f.get("repo").and_then(|v| v.as_str()).unwrap_or("?");
        let path = f.get("path").and_then(|v| v.as_str()).unwrap_or("?");
        let line = f.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
        let rule_id = f.get("rule_id").and_then(|v| v.as_str()).unwrap_or("?");
        let severity = f.get("severity").and_then(|v| v.as_str()).unwrap_or("?");
        let detail = f
            .get("detail")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .chars()
            .take(120)
            .collect::<String>();
        lines.push(format!(
            "[{severity}] {rule_id} in {repo}/{path}:{line} — {detail}"
        ));
    }
    Some((total, lines.join("\n")))
}

/// Build the selected-rules section for the chat system prompt from the onboarding draft.
///
/// Reads `repo_selection` (repo → [rule_id]) and `chosen` (rule_id → option_id) from the
/// draft, enriches with titles and option labels from `scan.proposed_rules`, and renders a
/// compact, queryable block:
///
/// ```text
/// Total selected: 4 rule(s) across 2 repo(s)
/// Rules:
///   SEC-NO-HARDCODED-SECRETS-1 · "No hardcoded secrets" (all repos) [default option]
///   PERF-FK-INDEX-1 · "FK index required" (owner/api) → chosen: strict-mode
/// ```
///
/// Returns `None` when no rules are selected (empty `repo_selection`), so callers can
/// skip the section rather than injecting an empty or misleading block. Works pre-scan
/// and pre-onboard: the draft carries `repo_selection`/`chosen` as soon as the user
/// makes a selection, even before any findings exist.
///
/// This is a pure function: it takes the draft JSON value and returns a formatted string
/// or None. It does not touch any async I/O or server state.
pub(crate) fn render_selected_rules_for_chat(draft: &serde_json::Value) -> Option<String> {
    // repo_selection: { repo_key: [rule_id, ...] }
    // A sentinel key ("__single__" or similar) means the project is single-repo.
    let repo_selection_val = draft.get("repo_selection")?;
    let repo_selection: std::collections::HashMap<String, Vec<String>> =
        serde_json::from_value(repo_selection_val.clone()).ok()?;

    // Collect all selected rule ids (union across repos) and which repos selected each.
    let mut by_rule: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for (repo_key, rule_ids) in &repo_selection {
        for rid in rule_ids {
            by_rule
                .entry(rid.clone())
                .or_default()
                .push(repo_key.clone());
        }
    }

    if by_rule.is_empty() {
        return None;
    }

    // chosen: { rule_id: option_id } — non-default option overrides.
    let chosen: std::collections::HashMap<String, String> = draft
        .get("chosen")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    // Build a lookup: rule_id → (title, options map {id → label}, default_option_id).
    #[derive(serde::Deserialize)]
    struct DraftRuleLight {
        id: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        options: Vec<DraftOptionLight>,
        #[serde(default)]
        default_option: Option<String>,
    }
    #[derive(serde::Deserialize)]
    struct DraftOptionLight {
        id: String,
        label: String,
    }

    let rule_meta: std::collections::HashMap<String, DraftRuleLight> = draft
        .get("scan")
        .and_then(|s| s.get("proposed_rules"))
        .and_then(|v| serde_json::from_value::<Vec<DraftRuleLight>>(v.clone()).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|r| (r.id.clone(), r))
        .collect();

    // Count distinct repos (excluding sentinel single-repo key if present).
    let distinct_repos: std::collections::HashSet<&str> = repo_selection
        .keys()
        .map(String::as_str)
        .filter(|k| !k.starts_with("__"))
        .collect();
    let repo_display_count = distinct_repos.len().max(1);

    let total = by_rule.len();
    let mut out =
        format!("Total selected: {total} rule(s) across {repo_display_count} repo(s)\nRules:\n");

    for (rule_id, mut repos) in by_rule {
        repos.sort();
        repos.dedup();

        // Scope annotation — hide sentinel keys from human-readable display.
        let visible_repos: Vec<&str> = repos
            .iter()
            .map(String::as_str)
            .filter(|k| !k.starts_with("__"))
            .collect();
        let scope_note = if visible_repos.is_empty() || visible_repos.len() == repo_display_count {
            "all repos".to_string()
        } else {
            visible_repos.join(", ")
        };

        // Title from proposed_rules, falling back to the id itself.
        let title = rule_meta
            .get(&rule_id)
            .and_then(|m| m.title.as_deref())
            .unwrap_or("");

        // Chosen-option annotation.
        let option_note = if let Some(chosen_id) = chosen.get(&rule_id) {
            // Try to resolve the label.
            let label = rule_meta
                .get(&rule_id)
                .and_then(|m| m.options.iter().find(|o| &o.id == chosen_id))
                .map(|o| o.label.as_str())
                .unwrap_or(chosen_id.as_str());
            format!(" → chosen: {label}")
        } else {
            // Check if there IS a default so we can annotate honestly.
            if rule_meta
                .get(&rule_id)
                .and_then(|m| m.default_option.as_deref())
                .is_some()
            {
                " [default option]".to_string()
            } else {
                String::new()
            }
        };

        if title.is_empty() {
            out.push_str(&format!("  {rule_id} ({scope_note}){option_note}\n"));
        } else {
            out.push_str(&format!(
                "  {rule_id} · \"{title}\" ({scope_note}){option_note}\n"
            ));
        }
    }

    Some(out)
}

/// `GET /api/projects/active/context` — the grounding payload for the project-aware chat
/// mode. No AI call; purely a read of the active project's current state (draft + ruleset).
///
/// The UI injects the returned fields into the system prompt it sends to `POST /api/chat`.
/// This keeps the AI call on the existing chat handler and this endpoint purely informational.
pub(crate) async fn active_project_context(
    State(state): State<AppState>,
) -> Json<ProjectContextResponse> {
    let Some(project) = state.projects.active() else {
        return Json(ProjectContextResponse {
            ok: false,
            phase: ProjectPhase::Blank,
            project_id: None,
            project_name: None,
            repos: Vec::new(),
            onboarded: Vec::new(),
            ruleset_summary: None,
            finding_count: None,
            findings_summary: None,
            scan_results_section: None,
            selected_rules_section: None,
            draft_json: None,
            message: Some("No active project — create one to use project-aware chat.".to_string()),
        });
    };

    let draft = state.draft.load(&project.id);

    if !project.onboarded.is_empty() {
        // Post-onboard: at least one repo has been fully onboarded. Surface the live ruleset
        // + any findings captured in the draft (the last audit the architect ran).
        let ruleset_summary = build_ruleset_summary(&project.ruleset);
        let ruleset_summary = if ruleset_summary.is_empty() {
            None
        } else {
            Some(ruleset_summary)
        };
        let (finding_count, findings_summary) = draft
            .as_ref()
            .and_then(extract_findings_from_draft)
            .map(|(n, s)| (Some(n), Some(s)))
            .unwrap_or((None, None));
        let scan_results_section = draft
            .as_ref()
            .and_then(extract_scan_results_from_draft)
            .or_else(|| {
                state
                    .get_last_scan(&project.id)
                    .map(|r| render_scan_results_for_chat(&r.findings, &r.coverage_notes))
                    .filter(|s| !s.is_empty())
            });
        let selected_rules_section = draft.as_ref().and_then(render_selected_rules_for_chat);
        Json(ProjectContextResponse {
            ok: true,
            phase: ProjectPhase::PostOnboard,
            project_id: Some(project.id.clone()),
            project_name: Some(project.name.clone()),
            repos: project.repos.clone(),
            onboarded: project.onboarded.clone(),
            ruleset_summary,
            finding_count,
            findings_summary,
            scan_results_section,
            selected_rules_section,
            draft_json: None, // Don't inject the full draft post-onboard (noisy).
            message: None,
        })
    } else if draft.is_some() {
        // Pre-onboard with a saved draft: the architect is mid-onboarding. Surface the draft
        // as-is so the chat can help interpret the in-progress onboarding state.
        let (finding_count, findings_summary) = draft
            .as_ref()
            .and_then(extract_findings_from_draft)
            .map(|(n, s)| (Some(n), Some(s)))
            .unwrap_or((None, None));
        let scan_results_section = draft
            .as_ref()
            .and_then(extract_scan_results_from_draft)
            .or_else(|| {
                state
                    .get_last_scan(&project.id)
                    .map(|r| render_scan_results_for_chat(&r.findings, &r.coverage_notes))
                    .filter(|s| !s.is_empty())
            });
        let selected_rules_section = draft.as_ref().and_then(render_selected_rules_for_chat);
        Json(ProjectContextResponse {
            ok: true,
            phase: ProjectPhase::PreOnboard,
            project_id: Some(project.id.clone()),
            project_name: Some(project.name.clone()),
            repos: project.repos.clone(),
            onboarded: Vec::new(),
            ruleset_summary: None, // No committed ruleset yet.
            finding_count,
            findings_summary,
            scan_results_section,
            selected_rules_section,
            draft_json: draft.map(without_internal_review_data),
            message: Some(format!(
                "Project '{}' has an in-progress onboarding (scan/audit) that has not been applied yet.",
                project.name
            )),
        })
    } else {
        // Blank: project exists but no draft and no onboarded repos.
        // Still check last_scan — a scan may have completed without a draft being
        // round-tripped back (timing race: this is the exact bug being fixed).
        let scan_results_section = state
            .get_last_scan(&project.id)
            .map(|r| render_scan_results_for_chat(&r.findings, &r.coverage_notes))
            .filter(|s| !s.is_empty());
        Json(ProjectContextResponse {
            ok: true,
            phase: ProjectPhase::Blank,
            project_id: Some(project.id.clone()),
            project_name: Some(project.name.clone()),
            repos: project.repos.clone(),
            onboarded: Vec::new(),
            ruleset_summary: None,
            finding_count: None,
            findings_summary: None,
            scan_results_section,
            selected_rules_section: None, // Blank: no draft, so no selections yet.
            draft_json: None,
            message: Some(format!(
                "Project '{}' has no scan or onboarding data yet — start an onboarding scan to populate the project context.",
                project.name
            )),
        })
    }
}
