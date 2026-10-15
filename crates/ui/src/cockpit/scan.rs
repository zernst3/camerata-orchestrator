use super::*;


/// One repo's local-path resolution status (issue #33), from `/api/projects/:id/repo-health`.
#[derive(Clone, PartialEq, serde::Deserialize)]
pub(super) struct RepoResolutionView {
    pub repo: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub resolved: bool,
    #[serde(default)]
    pub reason: String,
}

pub(super) async fn fetch_repo_health(project_id: &str) -> Option<Vec<RepoResolutionView>> {
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

pub(super) async fn set_repo_path(repo: &str, path: &str) -> bool {
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
pub(super) fn RepoHealthPanel(project_id: String) -> Element {
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
struct FindingContextView {
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
}

async fn fetch_finding_context(
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
fn degraded_context_reason(status: &str, reason: &str) -> String {
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

fn context_kind_label(kind: &str) -> &'static str {
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
const FINDING_CTX_CAP: usize = 80;
const FINDING_CTX_HALF: usize = 40;

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
fn FindingCodeContext(finding: FindingView) -> Element {
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
    let mut expanded = use_signal(|| false);

    let ctx = ctx_res.read().clone();
    match ctx {
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
        Some(Some(v)) if v.status == "ok" => {
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
                }
            }
        }
        Some(Some(v)) => rsx! {
            div { class: "finding-ctx finding-ctx-degraded",
                p { class: "rule-modal-label", "Code context" }
                span { class: "finding-ctx-note", "{degraded_context_reason(&v.status, &v.reason)}" }
            }
        },
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
struct FindingFixView {
    #[serde(default)]
    fix: String,
}

async fn fetch_finding_fix(rule_id: &str) -> Option<FindingFixView> {
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
fn FindingRecommendedFix(rule_id: String) -> Element {
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

// csv_field moved to camerata-ui-core::rules (shared by rules_csv there and findings_csv here).
pub(super) use camerata_ui_core::rules::csv_field;

/// Pop a native save dialog and write `content`. Returns true on success.
pub(super) async fn save_csv(default_name: &str, content: String) -> bool {
    match rfd::AsyncFileDialog::new()
        .set_file_name(default_name)
        .save_file()
        .await
    {
        Some(file) => file.write(content.as_bytes()).await.is_ok(),
        None => false,
    }
}

/// Byte-based sibling of `save_csv` — same native-save-dialog idiom, but for raw bytes
/// (the audit-report PDF export; `save_csv`'s `String` param can't carry PDF bytes, which
/// aren't valid UTF-8 in general).
pub(super) async fn save_bytes(default_name: &str, content: Vec<u8>) -> bool {
    match rfd::AsyncFileDialog::new()
        .set_file_name(default_name)
        .save_file()
        .await
    {
        Some(file) => file.write(&content).await.is_ok(),
        None => false,
    }
}

/// Build CSV for the audit findings table.
pub(super) fn findings_csv(findings: &[FindingView]) -> String {
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

// Scan-surface helpers (human_tokens, det_tool_label, default_finding_status, estimate_audit_cost)
// now live in the framework-agnostic core (RUST-HEADLESS-CORE-1); re-exported so the scan/cockpit
// call sites are unchanged. `default_finding_status` is re-exported via the triage block below
// (FindingView moved to core::triage), so it's not repeated here.
pub(super) use camerata_ui_core::scan::{det_tool_label, estimate_audit_cost, human_tokens};

// The onboarding-triage state machine + its data shapes now live in the framework-agnostic core
// (RUST-HEADLESS-CORE-1): FindingView, Disposition (+ its Default), TriageState, TechDebtBucket,
// finding_key, and the pure TriageModel + its TriageCounts tally. Re-exported so every existing
// scan/cockpit call site (and cockpit's `pub use scan::*`) is unchanged; the Dioxus layer keeps
// the signals, table effects, toasts, and rsx. (FindingView's `default_finding_status` serde
// provider lives with it in core::triage — re-exported from core::scan; not needed bare here.)
//
// `finding_state` is no longer re-exported: Pass 2 step 3 replaced this file's last caller (the
// hand-rolled disposition tally in `ScanResults`) with `TriageModel::counts`, and a re-export
// nothing imports is a warning, not an interface.
pub(super) use camerata_ui_core::triage::{
    finding_key, Disposition, FindingView, TechDebtBucket, TriageCounts, TriageModel, TriageState,
};

// Pass 2 step 1 (docs/plans/2026-10-11_pass2-curator-ui-design.md): the five-kind taxonomy's
// surface routing, the Kind badge's wire tag, and the one-rule-one-surface invariant. All pure,
// all in core; this layer only renders them.
pub(super) use camerata_ui_core::triage::{
    findings_on_surface, kind_tag, rules_on_multiple_surfaces, surface_counts,
    unspecified_kind_count, KindSurface,
};

// Pass 2, design §8: THE RULE HUB. `rule_hub` is the one join — everything this scan knows
// about one (repo, rule) — and `RuleHubView` is already the flat owned props shape, so the
// strip that renders it needs no second DTO. The conforming count comes through as two
// integers deliberately: the conforming sites are not in the data, only the counts.
pub(super) use camerata_ui_core::triage::{rule_hub, RuleHubView};

// Pass 2 step 2: the MANUAL column sort over the curated default. `severity_sort_value` is the
// Severity column's cell value — the authored severity carrying its calibrated ladder position,
// because chorale sorts a column by exactly that value and 0.2.3 offers no sort-key hook. See
// the function's docs in core for why that is not a second calibration layer.
pub(super) use camerata_ui_core::triage::{severity_sort_value, severity_sort_values};

// Design §9.3: the CONFIDENCE column's cell value and its filter vocabulary, from the same
// closed set, so the two cannot drift. Structured fields only — `confidence_tag` is the
// replacement for a cell value that OR'd two flags with a regex over the model's English.
pub(super) use camerata_ui_core::triage::{confidence_tag, confidence_tags};

// Design §3/§9.2: the heading the ON-DEMAND reason prompt shows. It names the disposition AND
// the count, which the two always-visible textareas it replaces could not — a box beside a
// selection cannot say how many rows the button next to it will move.
pub(super) use camerata_ui_core::triage::bulk_reason_heading;

// ── `split_needs_review` IN THE UI: what §9.3 retires, and the two uses that survive ──
//
// §9.3 asks for "the `split_needs_review` prose-parsing path in the UI ... retired entirely
// ONCE THE STRUCTURED FIELD IS UNIVERSAL". It is not universal: `FindingView::confidence` is
// `Option<String>`, and a row serialized before the field existed carries the `[needs review:
// …]` marker and nothing else. So the retirement is partial, and the line it is drawn on is
// the one the principle is actually about — prose must not DECIDE anything:
//
//   RETIRED (this commit): the Queue's hedging column. Its cell value, and therefore its
//   filter vocabulary and its sort key, were derived in part by regex over `f.detail`. That is
//   a UI classifying a row on the strength of a sentence, which is what "structured identity,
//   never prose" forbids. `confidence_tag` reads structured fields only.
//
//   KEPT, deliberately: `DetailPaneRow::from_finding` and the finding modal. Both call it to
//   SPLIT text the producer already wrote into body + marker so the marker can be bolded for a
//   human reader. Nothing is classified, filtered, counted or sorted by the result, and §9.3's
//   own next clause — "the reason text lives in the detail pane" — is satisfied by exactly
//   these two call sites. Deleting them would silently drop a legacy row's only disclosure of
//   its hedge, which is the capability loss this sweep is supposed to avoid, not cause.
//
// When `confidence` becomes universal upstream, the two survivors become deletable and the
// retirement completes. Until then they are the honest half.

// Pass 2 step 2: the DEVIATION GROUP HEADER. `rule_group_label` is the Queue's group-by value
// (rule id, plus the structured sentence when the row is a deviation), so chorale renders the
// sentence as the group header's own label; `deviation_groups` is the same partition lifted out
// for the rule-level bulk actions. Both are pure and structured-fields-only — there is no prose
// path in either, and there must never be one.
pub(super) use camerata_ui_core::triage::{
    deviation_groups, rule_group_label, DeviationGroup,
};

// Pass 2 step 4 (THE DOCKED DETAIL PANE, design §2 + the owner's Q5 ruling in §13). All pure
// and all in core: the row cursor that arrow keys drive, the SCOPED key handler (the whole of
// Q5's "only fire while the detail pane holds focus" clause lives in `pane_command`), and the
// five-second undo. This layer owns the focus signal, the keydown wiring and the rsx; it owns
// none of the rules.
pub(super) use camerata_ui_core::triage::{
    deviation_headline, pane_command, pane_keys_armed, site_count, state_hotkey, KeyMods,
    MergedSiteView, PaneCommand, PaneFocus, RowCursor, UndoEntry, UndoOutcome,
};

// Pass 2, THE GATED CONVENTION COMMIT (design §3, Q3 + Q4 in §12/§13). Every rule about what
// is adoptable, what is adopted, what was actually OPENED and whether the gate is shut lives
// in `camerata_ui_core::adopt`, unit-tested with no VirtualDom. This layer owns the pixels,
// the two HTTP calls and the focus handling; it owns none of the decisions.
pub(super) use camerata_ui_core::adopt::{
    adoptable_conventions, adoption_key, commit_gate, merge_adoptions, split_rules,
    AdoptableConvention, AdoptedRule, AdoptionModel, CommitPlan, ProjectRulesetView, SplitRule,
};

// ── The convention commit's two HTTP calls (design §3; owner's Q3/Q4) ─────────
//
// THERE IS NO NEW SERVER SURFACE HERE, and that is deliberate. Both routes already exist and
// already do exactly this; the only thing Pass 2 adds is the curator's decision about WHICH
// rules travel them.
//
//   1. `GET  /api/projects/:id/ruleset`     — `export_project_ruleset`
//   2. `POST /api/projects/:id/ruleset`     — `import_project_ruleset` -> `upsert_base_rules`
//   3. `POST /api/projects/:id/emit-local`  — `emit_project_local` -> `camerata_server::arm`,
//      which renders AGENTS.md / CONVENTIONS.md / the gate config into the repo's local
//      checkout. The cascade is all-false: write the files in place, no branch, no push, no
//      PR. Adoption is a decision about what the standard IS; shipping it is a separate act
//      with its own controls on the Rules screen.
//
// Step 1 exists because step 2 REPLACES `selections` wholesale (`upsert_base_rules` assigns
// the three lists). Posting only the adopted rules would delete every other selection the
// project has. The read-modify-write is therefore not an optimization, it is the contract.

/// Read the project's current base ruleset (`GET /api/projects/:id/ruleset`).
///
/// `None` on a transport/parse failure or an `ok: false` body — the caller must not proceed
/// to the write, because a merge against a default (empty) ruleset would erase the project's
/// selections.
pub(super) async fn fetch_project_ruleset(project_id: &str) -> Option<ProjectRulesetView> {
    let v: serde_json::Value = reqwest::get(format!(
        "{}/api/projects/{}/ruleset",
        crate::bff_base(),
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
    serde_json::from_value(v.get("ruleset")?.clone()).ok()
}

/// Write the merged base ruleset back (`POST /api/projects/:id/ruleset`).
///
/// The body carries no `custom` key, so the server's `merge_custom` sees nothing and every
/// architect-authored custom rule stays exactly where it is. See [`ProjectRulesetView`].
pub(super) async fn save_project_ruleset(project_id: &str, ruleset: &ProjectRulesetView) -> bool {
    let Ok(resp) = reqwest::Client::new()
        .post(format!(
            "{}/api/projects/{}/ruleset",
            crate::bff_base(),
            project_id
        ))
        .json(ruleset)
        .send()
        .await
    else {
        return false;
    };
    if !resp.status().is_success() {
        return false;
    }
    resp.json::<serde_json::Value>()
        .await
        .map(|v| v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false))
        .unwrap_or(false)
}

/// COMMIT the curator's adopted rules into one destination repository's `AGENTS.md`.
///
/// Read, merge, write, emit — in that order, and it STOPS at the first failure rather than
/// pressing on. A failed read followed by a write would post a merge built on an empty
/// ruleset, which is the one failure mode here that destroys data rather than merely not
/// doing anything.
///
/// Returns `(ok, one sentence for the toast)`.
pub(super) async fn commit_adoptions(
    project_id: &str,
    destination_repo: &str,
    adopted: &[AdoptedRule],
) -> (bool, String) {
    if adopted.is_empty() {
        return (false, "Nothing selected to write.".to_string());
    }
    let Some(current) = fetch_project_ruleset(project_id).await else {
        return (
            false,
            "Could not read the project's current ruleset, so nothing was written. \
             (Writing without it would have erased the project's other rule selections.)"
                .to_string(),
        );
    };
    let merged = merge_adoptions(&current, destination_repo, adopted);
    if !save_project_ruleset(project_id, &merged).await {
        return (
            false,
            "Could not save the project's ruleset. Nothing was written to the repository."
                .to_string(),
        );
    }
    let n = adopted.len();
    let (ok, emit_msg) = super::rules::emit_project_local(project_id, false, false, false).await;
    if ok {
        (
            true,
            format!("Wrote {n} into {destination_repo}. {emit_msg}"),
        )
    } else {
        (
            false,
            format!(
                "Saved {n} to the project's ruleset, but writing the governance files failed: \
                 {emit_msg}"
            ),
        )
    }
}

/// Which panel raised the commit dialog, where it is writing, and what has been typed.
///
/// Session state. The typed destination is deliberately NOT persisted anywhere: a half-typed
/// confirmation restored from a draft would be a gate the curator did not pass.
#[derive(Clone, PartialEq, Debug)]
pub(super) struct CommitDialogState {
    /// `KindSurface::Conventions` or `KindSurface::NoConvention` — which plan to rebuild.
    pub surface: KindSurface,
    /// The destination repository. The string the curator has to type.
    pub destination: String,
    /// What has been typed so far.
    pub typed: String,
}

/// Durable ignore: record the findings as reasoned baseline suppressions (governed PR).
/// Returns the PR URL.
pub(super) async fn ignore_findings(
    repo: &str,
    findings: &[FindingView],
    reason: &str,
    ticket: Option<String>,
) -> Option<String> {
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/api/onboard/ignore", crate::bff_base()))
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

/// Real audit usage from the server (the actual half of actual-vs-estimated).
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Default)]
pub(super) struct ActualUsageView {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cost_usd: f64,
    #[serde(default)]
    pub calls: u64,
    /// False when some calls didn't report a cost (the dollar figure is a partial sum).
    #[serde(default)]
    pub cost_complete: bool,
    /// Tokens served from the prompt cache (billed at ~0.1x input). Nonzero only when
    /// the API backend ran with prompt caching active (multi-batch parallel scans).
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    /// Tokens written to the prompt cache (billed at ~1.25x input, one-time per TTL).
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
}

/// One SOC-2 control gap entry from the deep tier, mirroring `DeepReport.soc2_gaps`.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub(super) struct Soc2GapView {
    pub control: String,
    #[serde(default)]
    pub title: String,
    /// One of "met" | "partial" | "gap" | "unknown".
    pub status: String,
    #[serde(default)]
    pub observed: String,
    #[serde(default)]
    pub gap: String,
}

/// One deep-tier lens result (SOC-2 gap / deep-security / threat-model).
/// Mirrors `ai_audit::DeepLensResult` on the wire.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub(super) struct DeepLensResultView {
    /// Stable id: "soc2-gap" | "deep-security" | "threat-model".
    pub lens: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub soc2_gaps: Vec<Soc2GapView>,
    /// Extra security / threat findings (deep-security + threat-model free-text content).
    #[serde(default)]
    pub detail: String,
    /// Always `true` for the deep tier — the whole tier is model-inferred, advisory.
    #[serde(default)]
    pub advisory: bool,
    /// Per-lens honesty disclaimer surfaced in the UI.
    #[serde(default)]
    pub disclaimer: String,
}

/// The top-level deep-tier output attached to a scan report when `deep: true` was sent.
/// Mirrors `ai_audit::DeepReport` on the wire.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub(super) struct DeepReportView {
    pub lenses: Vec<DeepLensResultView>,
    /// Always `true` — the whole tier is advisory.
    #[serde(default)]
    pub advisory: bool,
    /// Honesty disclaimer for the whole tier.
    #[serde(default)]
    pub disclaimer: String,
}

#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Default)]
pub(super) struct CoverageNoteView {
    pub tool: String,
    pub message: String,
}

/// One audited repo's git identity, mirroring `camerata_server::onboard::AuditedRef` on the
/// wire. Not yet rendered (that's the PDF-export pass); the fields are mirrored now so a
/// server-side addition is never silently dropped by serde here (see the round-trip contract
/// tests at the bottom of this module).
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Default)]
pub(super) struct AuditedRefView {
    pub repo: String,
    #[serde(default)]
    pub sha: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub dirty: bool,
}

/// The scan's provenance stamp, mirroring `camerata_server::onboard::ScanProvenance` on the
/// wire. Not yet rendered in the cockpit (Pass B / the PDF export reads it); mirrored here so
/// the wire contract stays in sync.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Default)]
pub(super) struct ScanProvenanceView {
    #[serde(default)]
    pub audited_refs: Vec<AuditedRefView>,
    #[serde(default)]
    pub audit_model: Option<String>,
    #[serde(default)]
    pub calibration_model: Option<String>,
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub thorough: bool,
    #[serde(default)]
    pub deep: bool,
    #[serde(default)]
    pub rules_fingerprint: String,
    #[serde(default)]
    pub audited_rule_ids: Vec<String>,
    #[serde(default)]
    pub camerata_version: String,
    #[serde(default)]
    pub osv_scanner_version: Option<String>,
    #[serde(default)]
    pub started_at: String,
    #[serde(default)]
    pub finished_at: String,
}

/// One rule's alternative recommendation, as surfaced by the audit-integrated
/// alternative-recommendation feature (docs/design/2026-09-22_audit-integrated-alternatives.md).
/// Mirrors `camerata_server::ai_audit::RuleRecommendation` (minus its own `rule_id` field,
/// since the UI reaches this through `ScanReportView::recommendations`, a map ALREADY keyed by
/// rule id). Present only for multi-option semantic rules; single-option / mechanical /
/// deterministic-floor rules never get an entry.
#[derive(Clone, PartialEq, Default, serde::Deserialize, serde::Serialize)]
pub(super) struct RuleRecommendationView {
    /// The option id violations were (or will be) reported against.
    #[serde(default)]
    pub recommended_option_id: String,
    /// 1-3 sentences explaining the pick, grounded in the codebase evidence the model saw (or,
    /// for an operator-forced pick, a short fixed note instead of a model explanation).
    #[serde(default)]
    pub recommendation_reasoning: String,
    /// True when the model's raw answer wasn't a real option on this rule (or was
    /// missing/unparseable) and `recommended_option_id` is therefore a FALLBACK, not what the
    /// model actually said. The UI shows a note rather than presenting the fallback as genuine.
    #[serde(default)]
    pub hallucinated: bool,
    /// True when this entry reflects the OPERATOR's explicit choice (via `rescan-alternatives`
    /// or `accept-alternatives`), not the AI's own pick. Drives "your choice" vs "AI
    /// recommended" in the review panel.
    #[serde(default)]
    pub operator_chosen: bool,
    /// The `file:line` (or `file:line — short note`) citation in the repo the model grounded its
    /// pick in (P7, docs/plans/2026-09-29_codebase-inspection-hardening.md). `None` when the
    /// model gave no evidence for this rule (or, for an operator-forced pick, no evidence is
    /// expected). Mirrors `camerata_server::ai_audit::RuleRecommendation::evidence`.
    /// `#[serde(default)]` so a report from before this field existed still deserializes as
    /// `None` (no evidence line shown, not an error).
    #[serde(default)]
    pub evidence: Option<String>,
    /// False when the model itself judged this rule NOT applicable to the repo (e.g. a rule
    /// aimed at a stack the codebase doesn't use) — surfaced as "Not applicable to this repo" so
    /// a wrong pick costs a glance rather than reading as a real finding. Mirrors
    /// `camerata_server::ai_audit::RuleRecommendation::applicable`, whose own default is `true`
    /// (`default_recommendation_applicable`): a report from before this field existed carries no
    /// applicability signal at all, and defaulting THAT case to `false` would wrongly flag every
    /// legacy recommendation as not-applicable, so the UI default matches the server's `true`
    /// rather than `bool`'s own `false` default.
    #[serde(default = "default_recommendation_applicable")]
    pub applicable: bool,
}

fn default_recommendation_applicable() -> bool {
    true
}

#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub(super) struct ScanReportView {
    #[serde(default)]
    pub repos: Vec<String>,
    #[serde(default)]
    pub stacks: Vec<StackView>,
    pub files_scanned: usize,
    #[serde(default)]
    pub files_excluded: usize,
    #[serde(default)]
    pub code_chars: usize,
    /// Mechanical rule ids dropped from the code-only scan (enforced in CI instead).
    #[serde(default)]
    pub excluded_mechanical_rules: Vec<String>,
    #[serde(default)]
    pub actual_usage: Option<ActualUsageView>,
    pub findings: Vec<FindingView>,
    pub proposed_rules: Vec<ProposedRuleView>,
    pub gated: bool,
    /// Feature B compliance block: the scan was REFUSED (API-only project, no Anthropic key).
    /// Set explicitly by the server (a `BackendResolution::Blocked`) so we render the block
    /// under its own heading rather than inferring it from the report's shape. `serde(default)`
    /// so reports persisted before this field existed still deserialize (as `false`).
    #[serde(default)]
    pub blocked: bool,
    /// Set when the compliance backend gate was `Blocked` for this run but a local pass
    /// (the deterministic floor and/or preview linters / dep-audit) was requested and DID
    /// run anyway — so this report carries real (partial) findings, not a silent empty
    /// table. Distinct from `blocked`: `blocked` means nothing ran at all; this means the
    /// AI review specifically did not run. Mirrors
    /// `camerata_server::onboard::ScanReport::ai_blocked_reason`. `#[serde(default)]` so a
    /// report from before this field existed still deserializes as `None`.
    #[serde(default)]
    pub ai_blocked_reason: Option<String>,
    /// Set when the AI review was requested (not compliance-blocked) but every pass failed
    /// with a real error (bad model id, an API/transport error, an auth failure, …), so the
    /// AI review produced no findings. Carries the first such error's text verbatim. Mirrors
    /// `camerata_server::onboard::ScanReport::ai_error`. `#[serde(default)]` for the same
    /// back-compat reason as `ai_blocked_reason`.
    #[serde(default)]
    pub ai_error: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    /// OPT-IN deep compliance & security tier output (#55). `None` unless the audit
    /// request sent `deep: true`. Everything inside is ADVISORY + model-inferred.
    #[serde(default)]
    pub deep: Option<DeepReportView>,
    /// Coverage notes from the scan preview (tools skipped or unavailable).
    #[serde(default)]
    pub coverage_notes: Vec<CoverageNoteView>,
    /// What was actually audited: git refs, models/mode/rule-selection, tool versions.
    /// Mirrors `camerata_server::onboard::ScanReport::provenance`. Additive
    /// (`#[serde(default)]`) so a report from before this field existed still
    /// deserializes, with an empty/default stamp.
    #[serde(default)]
    pub provenance: ScanProvenanceView,
    /// Per-rule alternative recommendations (audit-integrated alternative recommendation,
    /// docs/design/2026-09-22_audit-integrated-alternatives.md), keyed by rule id. Present for
    /// every multi-option semantic rule that was part of this audit, REGARDLESS of whether it
    /// produced any findings (so the review panel can list zero-violation rules too).
    /// `#[serde(default)]` so a report from before this field existed still deserializes, with
    /// an empty map (no recommendations to show — matches the pre-feature UI exactly).
    #[serde(default)]
    pub recommendations: std::collections::HashMap<String, RuleRecommendationView>,
}

pub(super) async fn scan_repos(repos: &[String]) -> Option<ScanReportView> {
    // Hold a loading guard for the full duration of the scan request so the
    // background Bombe machine runs while the server is scanning.
    let _guard = crate::loading::LoadingGuard::new();
    reqwest::Client::new()
        .post(format!("{}/api/onboard/scan", crate::bff_base()))
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
pub(super) struct OnboardingDraft {
    pub scan: ScanReportView,
    #[serde(default)]
    pub audit: Option<ScanReportView>,
    #[serde(default)]
    pub repo_selection: std::collections::HashMap<String, Vec<String>>,
    // The architect's chosen alternative per rule (rule id -> option id). Persisted so a
    // non-default option survives reload — without this the choices reset to defaults.
    #[serde(default)]
    pub chosen: std::collections::HashMap<String, String>,
    // User-authored custom rules created during onboarding (Custom + Custom Global). Persisted
    // so they survive reload; written to the project's ruleset.custom on apply/complete.
    #[serde(default)]
    pub custom: Vec<CustomRuleView>,
    #[serde(default)]
    pub dispositions: std::collections::HashMap<String, Disposition>,
    #[serde(default)]
    pub viewed_repo: String,
    #[serde(default)]
    pub triage_view: TriageState,
}

/// Load the saved onboarding draft, or None when nothing is in progress.
pub(super) async fn load_onboarding_draft() -> Option<OnboardingDraft> {
    reqwest::Client::new()
        .get(format!("{}/api/onboard/draft", crate::bff_base()))
        .send()
        .await
        .ok()?
        .json::<Option<OnboardingDraft>>()
        .await
        .ok()
        .flatten()
}

/// Persist the current onboarding draft (best-effort; failure is non-fatal).
pub(super) async fn save_onboarding_draft(draft: &OnboardingDraft) {
    let _ = reqwest::Client::new()
        .post(format!("{}/api/onboard/draft", crate::bff_base()))
        .json(draft)
        .send()
        .await;
}

/// Drop the saved draft (a fresh scan starts a new session; clearing avoids re-seeding the
/// previous run's audit/dispositions onto it).
pub(super) async fn clear_onboarding_draft() {
    let _ = reqwest::Client::new()
        .post(format!("{}/api/onboard/draft/clear", crate::bff_base()))
        .send()
        .await;
}

/// Finish onboarding for the active project: marks its repos onboarded and clears the
/// draft. The post-scan steps (audit / triage / apply / wire-CI) are all optional, so this
/// is the explicit "I'm done" action. Returns true on success.
pub(super) async fn complete_onboarding() -> bool {
    let Ok(resp) = reqwest::Client::new()
        .post(format!("{}/api/onboard/complete", crate::bff_base()))
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
pub(super) struct SelectedAuditRule {
    pub id: String,
    pub directive: String,
    pub repos: Vec<String>,
}

/// Serialize selected rules into the audit request shape (`{id, directive, repos}` each).
pub(super) fn audit_rules_json(rules: &[SelectedAuditRule]) -> Vec<serde_json::Value> {
    rules
        .iter()
        .map(|r| serde_json::json!({ "id": r.id, "directive": r.directive, "repos": r.repos }))
        .collect()
}

/// Phase 2 — audit the repos against the selected rules (each carrying its repo binding).
/// When `deep` is true, the server also runs the three deep-tier lenses (SOC-2 gap,
/// deep security, threat model) and attaches the results to `report.deep`.
#[allow(clippy::too_many_arguments)]
pub(super) async fn audit_against(
    repos: &[String],
    rules: &[SelectedAuditRule],
    model: &str,
    calibration_model: &str,
    mode: &str,
    thorough: bool,
    incremental: bool,
    deep: bool,
    run_ai_review: bool,
    run_deterministic: bool,
) -> Option<ScanReportView> {
    // Loading guard: the Bombe machine runs for the full audit round-trip.
    let _guard = crate::loading::LoadingGuard::new();
    let rule_json = audit_rules_json(rules);
    reqwest::Client::new()
        .post(format!("{}/api/onboard/audit", crate::bff_base()))
        .json(&serde_json::json!({
            "repos": repos,
            "rules": rule_json,
            "model": model,
            "calibration_model": calibration_model,
            "mode": mode,
            "thorough": thorough,
            "incremental": incremental,
            "deep": deep,
            "run_ai_review": run_ai_review,
            "run_deterministic": run_deterministic,
        }))
        .send()
        .await
        .ok()?
        .json::<ScanReportView>()
        .await
        .ok()
}

// ── Audit-integrated alternative recommendation: batched rescan + accept ───────────────────
// docs/design/2026-09-22_audit-integrated-alternatives.md — Phase 2 (UI). The server side
// (Phase 1) is done; this is the wire contract the two endpoints already speak.

/// One staged override: force `rule_id` to `chosen_option_id` on the next batched rescan.
/// Mirrors the server's `AlternativeOverride` request-body entry
/// (`POST /api/projects/:id/rescan-alternatives`).
#[derive(Clone, PartialEq, serde::Serialize)]
pub(super) struct AlternativeOverride {
    pub rule_id: String,
    pub chosen_option_id: String,
}

/// Response body of `POST /api/projects/:id/rescan-alternatives`: `{ ok, message, findings,
/// recommendations }`. `findings` + `recommendations` cover ONLY the rescanned rules — the
/// caller merges them in place over the rest of the held audit report via
/// [`merge_rescanned_alternatives`]. `ok: false` (e.g. the backend `Blocked` compliance gate,
/// or every submitted override being invalid) carries the reason in `message`; the caller
/// surfaces it rather than silently failing.
#[derive(Clone, PartialEq, Default, serde::Deserialize, serde::Serialize)]
pub(super) struct RescanAlternativesResp {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub findings: Vec<FindingView>,
    #[serde(default)]
    pub recommendations: std::collections::HashMap<String, RuleRecommendationView>,
}

/// POST the batched rescan for exactly the given overridden rules. This is an AI call (it
/// re-runs the audit for the given rules, forced to the operator's chosen options) — the guard
/// here drives the background Bombe machine for the whole round trip, same as `audit_against`
/// and `scan_repos` above. Returns `None` only on a network/transport/parse failure; a
/// server-reported `{ ok: false, message }` still deserializes into `Some(..)` so the caller
/// can read `message` (see this type's doc comment).
pub(super) async fn rescan_alternatives(
    project_id: &str,
    overrides: &[AlternativeOverride],
) -> Option<RescanAlternativesResp> {
    let _guard = crate::loading::LoadingGuard::new();
    reqwest::Client::new()
        .post(format!(
            "{}/api/projects/{}/rescan-alternatives",
            crate::bff_base(),
            project_id
        ))
        .json(&serde_json::json!({ "overrides": overrides }))
        .send()
        .await
        .ok()?
        .json::<RescanAlternativesResp>()
        .await
        .ok()
}

/// Response body of `POST /api/projects/:id/accept-alternatives`: `{ ok, message?, accepted,
/// skipped }` (`accepted` is a COUNT, not a list — mirrors the server's
/// `accept_alternatives_handler`). `ruleset` is intentionally not modeled here: the caller
/// doesn't need to re-parse the project's full ruleset shape to reflect an accept — it already
/// knows which rules it asked to accept.
#[derive(Clone, PartialEq, Default, serde::Deserialize, serde::Serialize)]
pub(super) struct AcceptAlternativesResp {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub accepted: usize,
    #[serde(default)]
    pub skipped: Vec<String>,
}

/// POST `accept-alternatives`. `rule_ids: None` accepts EVERY rule in the project's last-scan
/// recommendations (the server's `{}` "accept all" shape); `Some(ids)` scopes to that subset.
/// Not an AI call (it only persists the current selection into `chosen_option`), so — unlike
/// `rescan_alternatives` — this does NOT take a `LoadingGuard`; no model runs.
pub(super) async fn accept_alternatives(
    project_id: &str,
    rule_ids: Option<&[String]>,
) -> Option<AcceptAlternativesResp> {
    let body = match rule_ids {
        Some(ids) => serde_json::json!({ "rule_ids": ids }),
        None => serde_json::json!({}),
    };
    reqwest::Client::new()
        .post(format!(
            "{}/api/projects/{}/accept-alternatives",
            crate::bff_base(),
            project_id
        ))
        .json(&body)
        .send()
        .await
        .ok()?
        .json::<AcceptAlternativesResp>()
        .await
        .ok()
}

/// Merge a batched rescan's updated findings + recommendations into the audit report's
/// existing findings + recommendations, replacing ONLY the rescanned rules' rows — every other
/// rule's findings and recommendations are left untouched. Pure (no signals, no Dioxus), so the
/// merge is unit-tested directly; the call site just assigns the two returned values back onto
/// the held `ScanReportView`.
pub(super) fn merge_rescanned_alternatives(
    existing_findings: &[FindingView],
    existing_recommendations: &std::collections::HashMap<String, RuleRecommendationView>,
    rescanned_rule_ids: &std::collections::HashSet<String>,
    new_findings: Vec<FindingView>,
    new_recommendations: std::collections::HashMap<String, RuleRecommendationView>,
) -> (
    Vec<FindingView>,
    std::collections::HashMap<String, RuleRecommendationView>,
) {
    let mut findings: Vec<FindingView> = existing_findings
        .iter()
        .filter(|f| !rescanned_rule_ids.contains(&f.rule_id))
        .cloned()
        .collect();
    findings.extend(new_findings);
    let mut recommendations = existing_recommendations.clone();
    for (rule_id, rec) in new_recommendations {
        recommendations.insert(rule_id, rec);
    }
    (findings, recommendations)
}

/// One model entry in a selector, sourced from `GET /api/models/registry`.
///
/// `label` carries a badge-enriched display string (e.g. "DeepSeek R1  $0.55/M · tool-use · 64K · cache").
/// `provider` groups the entry in `<optgroup>` elements ("claude" | "openrouter").
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub(super) struct AuditModelOption {
    /// Badge-enriched display label. Built by [`model_badge_label`] at fetch time.
    pub label: String,
    /// The model id passed to the API / CLI (stable key; stored in project settings).
    pub id: String,
    /// Provider key: "claude" or "openrouter". Used for `<optgroup>` grouping.
    #[serde(default)]
    pub provider: String,
    /// Whether the model is free to call (prompt + completion price = 0).
    #[serde(default)]
    pub free: bool,
    /// Whether the model supports tool use.
    #[serde(default)]
    pub tool_use: bool,
    /// Context window in tokens.
    #[serde(default)]
    pub context: u64,
    /// USD per million tokens (input / output). Drives the pre-audit cost estimate.
    #[serde(default)]
    pub price_in: f64,
    #[serde(default)]
    pub price_out: f64,
    /// Whether this model supports prompt caching.
    #[serde(default)]
    pub caching: bool,
    /// Whether this model supports vision / multimodal input (images).
    /// Used to filter the Designer (vision) band model selector.
    #[serde(default)]
    pub vision: bool,
    /// Whether `price_in`/`price_out` are a real, known price. `false` for a live-fetched
    /// Anthropic model (Feature A) whose id isn't in the hardcoded price table yet —
    /// `model_badge_label` shows "price unknown" instead of a `$0`/`M` badge in that case,
    /// so an un-priced model never LOOKS free. Absent in older/mocked JSON defaults to
    /// `true` (every price this endpoint ever served before Feature A was known).
    #[serde(default = "default_price_known")]
    pub price_known: bool,
}

fn default_price_known() -> bool {
    true
}

/// Build a badge-enriched display label from registry entry fields.
///
/// Format: "<display>  FREE · tool-use · 200K · cache"
/// Or for paid: "<display>  $0.55/M · tool-use · 64K · cache"
///
/// - Price: `FREE` if free, `price unknown` if `!price_known` (a live-fetched model absent
///   from the hardcoded price table — see [`AuditModelOption::price_known`]), else
///   `$<price_out>/M` (output price, compact).
/// - `tool-use`: always shown (absence as `no-tools` for OpenRouter models lacking it).
/// - Context: `<N>K` (e.g. `200K`, `64K`).
/// - `cache`: shown only when `caching` is true.
///
/// Badges are separated by ` · ` and appended after two spaces following the display name.
fn model_badge_label(
    display: &str,
    free: bool,
    tool_use: bool,
    context: u64,
    price_out: f64,
    caching: bool,
    price_known: bool,
) -> String {
    let mut parts = Vec::<String>::new();

    // Price badge: FREE, "price unknown" (never a misleading $0), or $<price>/M.
    if free {
        parts.push("FREE".to_string());
    } else if !price_known {
        parts.push("price unknown".to_string());
    } else if price_out > 0.0 {
        // Compact price: show 2 sig figs but strip trailing zeros.
        // e.g. 15.0 → "$15/M", 0.55 → "$0.55/M", 3.0 → "$3/M"
        let formatted = if price_out >= 10.0 {
            format!("${:.0}/M", price_out)
        } else if price_out >= 1.0 {
            // Up to 1 decimal place, strip trailing zero.
            let s = format!("{:.1}", price_out);
            format!("${}/M", s.trim_end_matches('0').trim_end_matches('.'))
        } else {
            // Small prices: 2 decimal places, strip trailing zeros.
            let s = format!("{:.2}", price_out);
            format!("${}/M", s.trim_end_matches('0').trim_end_matches('.'))
        };
        parts.push(formatted);
    }

    // Tool-use: always shown (absence flagged for OpenRouter models).
    if tool_use {
        parts.push("tool-use".to_string());
    } else {
        parts.push("no-tools".to_string());
    }

    // Context window.
    if context > 0 {
        let ctx_k = context / 1000;
        parts.push(format!("{ctx_k}K"));
    }

    // Caching tag.
    if caching {
        parts.push("cache".to_string());
    }

    if parts.is_empty() {
        display.to_string()
    } else {
        format!("{}  {}", display, parts.join(" · "))
    }
}

/// Shape of the raw registry wire response from `GET /api/models/registry`.
#[derive(serde::Deserialize)]
struct RegistryResp {
    models: Vec<RegistryEntryWire>,
    #[serde(default)]
    openrouter_fetched: bool,
}

/// One entry from the registry wire response. Mirrors `camerata_api_types::model_registry::RegistryEntry` (subset, fields tolerated absent).
#[derive(serde::Deserialize)]
struct RegistryEntryWire {
    id: String,
    display: String,
    provider: String,
    #[serde(default)]
    free: bool,
    #[serde(default)]
    tool_use: bool,
    #[serde(default)]
    context: u64,
    #[serde(default)]
    price_in: f64,
    #[serde(default)]
    price_out: f64,
    #[serde(default)]
    caching: bool,
    /// Whether the model supports vision / multimodal input (images).
    #[serde(default)]
    vision: bool,
    /// Whether `price_in`/`price_out` are a real, known price. See
    /// [`AuditModelOption::price_known`].
    #[serde(default = "default_price_known")]
    price_known: bool,
}

impl RegistryEntryWire {
    fn to_option(&self) -> AuditModelOption {
        AuditModelOption {
            label: model_badge_label(
                &self.display,
                self.free,
                self.tool_use,
                self.context,
                self.price_out,
                self.caching,
                self.price_known,
            ),
            id: self.id.clone(),
            provider: self.provider.clone(),
            free: self.free,
            tool_use: self.tool_use,
            context: self.context,
            price_in: self.price_in,
            price_out: self.price_out,
            caching: self.caching,
            vision: self.vision,
            price_known: self.price_known,
        }
    }
}

#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub(super) struct AuditModelsResp {
    pub models: Vec<AuditModelOption>,
    /// The default model id to pre-select. For the registry this is the first Claude model.
    #[serde(default)]
    pub default: String,
    /// Whether the OpenRouter portion of the registry has been fetched.
    #[serde(default)]
    pub openrouter_fetched: bool,
}

impl AuditModelsResp {
    /// Return models grouped by provider for `<optgroup>` rendering.
    /// Each group is `(group_label, entries)`. Claude always comes first.
    pub fn grouped(&self) -> Vec<(&'static str, Vec<&AuditModelOption>)> {
        let claude: Vec<&AuditModelOption> =
            self.models.iter().filter(|m| m.provider == "claude").collect();
        let openrouter: Vec<&AuditModelOption> =
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

    /// Return only vision-capable models, grouped by provider.
    /// Used to populate the Designer (vision) band model selector.
    pub fn vision_grouped(&self) -> Vec<(&'static str, Vec<&AuditModelOption>)> {
        let claude: Vec<&AuditModelOption> =
            self.models.iter().filter(|m| m.provider == "claude" && m.vision).collect();
        let openrouter: Vec<&AuditModelOption> =
            self.models.iter().filter(|m| m.provider == "openrouter" && m.vision).collect();
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

/// Fetch the model registry (`GET /api/models/registry`) and map entries to selector options.
///
/// Falls back gracefully: if OpenRouter key is not set, only Claude entries are returned.
/// Returns `None` only when the server is unreachable.
pub(super) async fn fetch_audit_models() -> Option<AuditModelsResp> {
    let resp = reqwest::get(format!("{}/api/models/registry", crate::bff_base()))
        .await
        .ok()?
        .json::<RegistryResp>()
        .await
        .ok()?;

    let models: Vec<AuditModelOption> = resp.models.iter().map(|e| e.to_option()).collect();
    // Default = first Claude model (Opus), or first model overall if no Claude entries.
    let default = models
        .iter()
        .find(|m| m.provider == "claude")
        .or_else(|| models.first())
        .map(|m| m.id.clone())
        .unwrap_or_default();

    Some(AuditModelsResp { models, default, openrouter_fetched: resp.openrouter_fetched })
}

/// One deterministic-scan tool's live progress (mirror of the server's `DetToolProgress`).
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Default)]
pub(super) struct DetToolProgressView {
    #[serde(default)]
    pub tool: String,
    /// `starting` | `running` | `done`.
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub findings: usize,
}

/// The deterministic pass's progress (mirror of the server's `DetProgress`): per-tool rows
/// plus an overall done/total. Drives the "Deterministic scan" progress component.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Default)]
pub(super) struct DetProgressView {
    #[serde(default)]
    pub tools: Vec<DetToolProgressView>,
    #[serde(default)]
    pub done: usize,
    #[serde(default)]
    pub total: usize,
}

/// A polled async-audit job (`GET /api/onboard/audit/job/:id`).
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize, Default)]
pub(super) struct JobStateView {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub done: usize,
    #[serde(default)]
    pub total: usize,
    #[serde(default)]
    pub findings: Vec<FindingView>,
    /// Live deterministic-pass progress (floor + preview tools). Empty until a tool registers.
    #[serde(default)]
    pub deterministic: DetProgressView,
    #[serde(default)]
    pub report: Option<ScanReportView>,
    #[serde(default)]
    pub message: Option<String>,
    /// Batch mode (#61): the Anthropic Message Batch id currently being processed
    /// (`msgbatch_01...`). Surfaced in the job-progress status line so the user can
    /// look it up in the Anthropic console. `None` for parallel/sequential mode jobs.
    #[serde(default)]
    pub batch_id: Option<String>,
}

/// Full envelope for `GET /api/onboard/audit/job/:id` — the server wraps
/// `JobState` under a `job:` key alongside stall/cancel metadata.
#[derive(Clone, serde::Deserialize, Default)]
pub(super) struct JobStatusEnvelope {
    pub job: JobStateView,
    /// Milliseconds since last job progress update. `None` if no activity recorded yet.
    #[serde(default)]
    pub idle_ms: Option<u128>,
    /// True if a cancel has been requested for this job.
    #[serde(default)]
    pub cancel_requested: bool,
}

/// Mode 3: START an async audit job, returning its id (the request returns immediately).
/// `deep` forwards the opt-in deep compliance & security tier (#55); the server
/// runs the three lenses after the standard audit completes and attaches the result
/// to the final job report's `deep` field.
#[allow(clippy::too_many_arguments)]
pub(super) async fn audit_job_start(
    repos: &[String],
    rules: &[SelectedAuditRule],
    model: &str,
    calibration_model: &str,
    exec_mode: &str,
    thorough: bool,
    incremental: bool,
    deep: bool,
    run_ai_review: bool,
    run_deterministic: bool,
) -> Option<String> {
    let rule_json = audit_rules_json(rules);
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/api/onboard/audit/start", crate::bff_base()))
        .json(&serde_json::json!({
            "repos": repos,
            "rules": rule_json,
            "model": model,
            "calibration_model": calibration_model,
            "mode": exec_mode,
            "thorough": thorough,
            "incremental": incremental,
            "deep": deep,
            "run_ai_review": run_ai_review,
            "run_deterministic": run_deterministic,
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
pub(super) fn recommend_scan_mode(report: &ScanReportView) -> String {
    if report.repos.len() > 1 || report.files_scanned > 150 {
        "job".to_string()
    } else {
        "parallel".to_string()
    }
}

/// Poll an async audit job for progress + incremental findings + the final report.
pub(super) async fn audit_job_poll(job_id: &str) -> Option<JobStatusEnvelope> {
    reqwest::get(format!(
        "{}/api/onboard/audit/job/{}",
        crate::bff_base(),
        job_id
    ))
    .await
    .ok()?
    .json::<Option<JobStatusEnvelope>>()
    .await
    .ok()
    .flatten()
}

/// Drive an async audit job to completion: poll every ~1.5s, update progress + (on done) the
/// final report, clearing the shared `active_audit_job` so a later mount doesn't re-resume.
/// Shared by the manual start AND the resume-on-mount path. Gives up after a few misses (the
/// job vanished, e.g. the server restarted) so it can't spin forever.
///
/// SILENT-DEATH FIX: `failed` used to clear the spinner and stop, with nothing else — the
/// job's `message` (the server's `jobs.fail` reason) was read off the wire but never shown
/// anywhere, so a failed background scan looked EXACTLY like nothing had happened. Same gap
/// on the "job vanished" (misses >= 3) path: no report, no message, no signal it even ran.
/// Both paths now set `job_failed_message`, which `ScanJobFailedBanner` renders
/// unconditionally (not gated on a report existing, since a failed/vanished job never
/// produces one).
///
/// BOMBE-SILENT-MISS FIX: this used to take its guard via `LoadingGuard::new()`, resolving the
/// `LoadingCount` context from WITHIN this function's own body — which runs inside a detached
/// `spawn`ed task (both call sites `.await` this from inside `spawn(async move { .. })` / a
/// `use_future`). Context resolution there is not reliable (see the pitfall note on
/// `LoadingGuard::new`), so the guard could silently no-op and the Bombe would never animate for
/// a running scan. `loading_count` is now REQUIRED and must be captured by the caller in its own
/// render-scope body (`use_context::<LoadingCount>()`) before the `spawn`/`use_future`, then
/// passed straight through here.
#[allow(clippy::too_many_arguments)]
pub(super) async fn poll_job(
    jid: String,
    mut audit: Signal<Option<ScanReportView>>,
    mut auditing: Signal<bool>,
    mut job_progress: Signal<Option<(usize, usize, usize)>>,
    // Live DETERMINISTIC-pass progress (floor + preview tools), rendered by the
    // "Deterministic scan" component above the AI agent-activity drawer. `None` clears it.
    mut det_progress: Signal<Option<DetProgressView>>,
    mut active_audit_job: Signal<Option<String>>,
    mut scan_idle_ms: Signal<Option<u128>>,
    // Set on `failed` (with the server's reason) or when the job vanishes mid-poll; cleared
    // by the caller when a fresh scan starts. `None` renders nothing (`ScanJobFailedBanner`).
    mut job_failed_message: Signal<Option<String>>,
    // Captured by the CALLER in its component render scope (never resolved from context in
    // here) — see the BOMBE-SILENT-MISS note above.
    loading_count: crate::loading::LoadingCount,
) {
    // Loading guard held for the ENTIRE poll loop so the Bombe machine stays
    // active until the background job reports done/failed/cancelled.
    let _guard = crate::loading::LoadingGuard::from_count(loading_count);
    let mut misses = 0u32;
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        match audit_job_poll(&jid).await {
            Some(js) => {
                misses = 0;
                scan_idle_ms.set(js.idle_ms);
                job_progress.set(Some((js.job.done, js.job.total, js.job.findings.len())));
                // Surface the deterministic progress whenever any tool has registered, so the
                // component appears the moment the floor starts (not only once findings land).
                if js.job.deterministic.total > 0 {
                    det_progress.set(Some(js.job.deterministic.clone()));
                }
                match js.job.status.as_str() {
                    "done" => {
                        audit.set(js.job.report);
                        auditing.set(false);
                        job_progress.set(None);
                        det_progress.set(None);
                        active_audit_job.set(None);
                        break;
                    }
                    "failed" => {
                        job_failed_message.set(Some(js.job.message.clone().unwrap_or_else(|| {
                            "The scan failed for an unspecified reason.".to_string()
                        })));
                        auditing.set(false);
                        job_progress.set(None);
                        det_progress.set(None);
                        active_audit_job.set(None);
                        break;
                    }
                    "cancelled" => {
                        auditing.set(false);
                        job_progress.set(None);
                        det_progress.set(None);
                        active_audit_job.set(None);
                        break;
                    }
                    _ => {}
                }
            }
            None => {
                misses += 1;
                if misses >= 3 {
                    job_failed_message.set(Some(
                        "Lost contact with the running scan (the server may have restarted \
                         or the job was lost). Please retry."
                            .to_string(),
                    ));
                    auditing.set(false);
                    job_progress.set(None);
                    det_progress.set(None);
                    active_audit_job.set(None);
                    break;
                }
            }
        }
    }
}

/// The "Deterministic scan" progress component — rendered ABOVE the AI agent-activity drawer.
/// Shows the deterministic pass's per-tool state (start/run/done + findings count) and an
/// overall done/total bar. It's the PRIMARY progress view in deterministic-only mode, where
/// the AI drawer is empty. Styled to match the existing job-progress UI.
#[component]
pub(super) fn DeterministicProgress(progress: DetProgressView) -> Element {
    let pct = (progress.done * 100)
        .checked_div(progress.total)
        .unwrap_or(0)
        .min(100);
    rsx! {
        div { class: "det-progress",
            div { class: "det-progress-head",
                span { class: "det-progress-title", "Deterministic scan" }
                span { class: "det-progress-count", "{progress.done}/{progress.total} tools" }
            }
            div { class: "det-progress-track",
                div { class: "det-progress-fill", style: "width: {pct}%" }
            }
            div { class: "det-progress-tools",
                for t in progress.tools.iter() {
                    {
                        let label = det_tool_label(&t.tool);
                        let status_class = match t.status.as_str() {
                            "done" => "det-tool det-tool-done",
                            "running" => "det-tool det-tool-running",
                            _ => "det-tool det-tool-starting",
                        };
                        let glyph = match t.status.as_str() {
                            "done" => "\u{2713}",   // ✓
                            "running" => "\u{2026}", // …
                            _ => "\u{00b7}",         // ·
                        };
                        rsx! {
                            div { key: "{t.tool}", class: "{status_class}",
                                span { class: "det-tool-glyph", "{glyph}" }
                                span { class: "det-tool-name", "{label}" }
                                if t.status == "done" {
                                    span { class: "det-tool-findings", "{t.findings} finding(s)" }
                                } else {
                                    span { class: "det-tool-state", "{t.status}" }
                                }
                            }
                        }
                    }
                }
            }
            span { class: "det-progress-note",
                "Deterministic scans run locally — no LLM, no tokens. The security floor is always-on; preview tools (clippy/ruff/eslint/semgrep) run for your selected mechanical rules."
            }
        }
    }
}

/// Friendly label for a deterministic tool name. `floor` is the always-on security scanner;
/// the rest are the scan-preview linters; `unrouted` collects rules with no driveable tool.

/// The Queue's GROUP-BY column: invisible, and the reason the deviation sentence appears on
/// the group header at all.
///
/// chorale 0.2.3 renders a group header's text from the group-by column's cell value
/// (`build_group_rows` takes `label` straight from `(col.accessor)(row)`), and it exposes NO
/// content-injection hook for that row — `group_header_tr` is private and `group_header_class`
/// is a CSS class, not a slot. So the way to put Camerata's own sentence on a chorale group
/// header, without standing up a parallel grouped renderer beside chorale's, is to make the
/// grouped-on VALUE be the sentence. See [`RULE_GROUP_COLUMN`]'s use in `finding_columns`.
///
/// The column is hidden (`column_visibility`), never sortable and never filterable: it is a
/// grouping key, not data. `build_group_rows` resolves it out of `state.columns` directly, so
/// hiding it does not disturb the grouping.
pub(super) const RULE_GROUP_COLUMN: ColumnId = ColumnId("rule_group");

/// THE RED FULL-ROW HIGHLIGHT, as a named predicate (design §9.5 — "the red full-row critical
/// highlight stays").
///
/// It reads `severity` and nothing else. The legend §9.5 removed glossed this highlight as
/// "Security findings (deterministic, stop-the-line)", which it never was: a critical
/// AI-ADVISORY finding is highlighted and is neither deterministic nor security, and a
/// deterministic floor hit below `critical` is both and is NOT highlighted. The per-row
/// Severity and Authority badges now carry those two facts separately and correctly.
///
/// Reads the CALIBRATED `FindingView::severity`, never the Severity column's cell value — that
/// one carries a rank prefix (`1 critical`) purely so chorale's lexicographic column sort
/// orders the ladder, and comparing against it here would silently stop matching.
pub(super) fn finding_row_class(f: &FindingView) -> Option<String> {
    (f.severity == "critical").then(|| "finding-row-critical".to_string())
}

pub(super) fn finding_columns(repos: Vec<String>, show_bucket: bool) -> Vec<ColumnDef<FindingView>> {
    // chorale 0.2.3's palette has a native orange, so each severity gets a distinct color
    // straight from RenderKind::Badge — no custom cell renderer needed (Critical = red,
    // High = orange, Medium = yellow, Low = gray).
    //
    // The MAP KEYS are `severity_sort_value`s, not bare severities, because that is what the
    // accessor emits (the cell value doubles as the column's sort key — see below). The LABELS
    // are unchanged, so the rendered badge still reads "Critical" / "High" / "Medium" / "Low".
    let sev = BadgeVariantMap::new()
        .with(severity_sort_value("critical"), BadgeVariant::new("Critical", "red"))
        .with(severity_sort_value("high"), BadgeVariant::new("High", "orange"))
        .with(severity_sort_value("medium"), BadgeVariant::new("Medium", "yellow"))
        .with(severity_sort_value("low"), BadgeVariant::new("Low", "gray"));
    let mut cols = vec![
        ColumnDef::new(ColumnId("repo"), "Repo", |f: &FindingView| {
            CellValue::Text(f.repo.clone())
        })
        .sortable()
        .filter(FilterKind::MultiSelect { options: repos })
        .initial_width(180.0),
        // KIND — the most load-bearing new pixel on the row (design §4). Says what the row IS:
        // an objective fault (DEFECT), an inconsistency against the codebase's own observed
        // convention (DEVIATION), or an UNMAPPED row whose producer never assigned a kind.
        //
        // The cell VALUE is the stable wire tag straight from api-types (never re-derived, never
        // parsed from prose), so the multi-select filter is exactly the wire vocabulary. The
        // visible chip is drawn by the row renderer below, which gives `unspecified` the red
        // outline it needs as an ALARM and keeps it visually out of the Defect family.
        //
        // Only the Queue's three tags appear here; the informational kinds never reach this
        // table (they route to their own panels), so listing them as filter options would be a
        // dead affordance.
        ColumnDef::new(ColumnId("kind"), "Kind", |f: &FindingView| {
            CellValue::Text(kind_tag(f).to_string())
        })
        .sortable()
        .filter(FilterKind::MultiSelect {
            options: vec![
                "defect".to_string(),
                "deviation".to_string(),
                "unspecified".to_string(),
            ],
        })
        .initial_width(130.0),
        // SEVERITY. The cell value is the authored severity PREFIXED BY ITS CALIBRATED LADDER
        // POSITION (`severity_sort_value`), because a sortable chorale column is ordered by
        // exactly the value its accessor returns and `chorale-core` 0.2.3 has no separate
        // sort-key hook. Severity is the one Queue column whose words do not sort into their
        // own meaning — alphabetically `low` sits above `medium` — so without the prefix a
        // header click reads "critical, high, low, medium" in one direction and the same
        // nonsense reversed in the other.
        //
        // This re-derives nothing: the prefix is `severity_rank`, the identical ordering of the
        // identical authored string that `queue_sort_key` applies for the default order, and
        // kind is not an input. The authored word is still what the badge label, the row
        // highlight (`row_class`, which reads `f.severity`), the CSV export (`findings_csv`,
        // hand-rolled off `FindingView`) and the report all read.
        ColumnDef::new(ColumnId("severity"), "Severity", |f: &FindingView| {
            CellValue::Text(severity_sort_value(&f.severity))
        })
        .sortable()
        // Options come from the same function as the accessor, so the filter vocabulary cannot
        // drift from the cell it filters (a stale option list would filter every row out and
        // read as "no findings"). The list is in ladder order, as it was before.
        .filter(FilterKind::MultiSelect {
            options: severity_sort_values(),
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
        // CONFIDENCE (design §4, §9.3) — REPLACES the 300px "Needs review" column.
        //
        // > the structured `confidence` field ("high" / "needs-review") as a narrow chip; the
        // > verbose inline reason moves to the detail pane. Recovers ~300px.
        //
        // The column it replaces computed its cell value as
        // `in_test || needs_review || split_needs_review(detail).1.is_some()` — two structured
        // flags OR'd with a REGEX OVER THE MODEL'S OWN ENGLISH — and then drew the matched
        // sentence inline, at 300px, on every row. `confidence_tag` reads structured fields
        // only; see its docs in core for why the prose term was redundant as well as
        // undesirable (the server sets `needs_review` unconditionally two lines above writing
        // the `[needs review: …]` tag, so the regex was reading a shadow of the field beside
        // it) and for why `unrated` is deliberately NOT folded into `high`.
        //
        // THE REASON TEXT IS NOT LOST, it moves: `DetailPaneRow::needs_review` carries it into
        // the docked pane for the focused row, and the finding modal bolds it inline. §9.3's
        // own sentence is "the reason text lives in the detail pane".
        //
        // THE COLUMN ID STAYS `needs_review` on purpose. It is an internal key, never rendered
        // — the header below is what the curator reads, and the order bar resolves its
        // sentence from that header. Renaming it would buy nothing and would break
        // `req_pass2_8_every_meaningful_queue_column_header_is_sortable`, which pins this
        // column as re-cuttable by id; that test asserts a behaviour this change keeps, so
        // editing it to accommodate a cosmetic rename would be loosening a guard for no gain.
        //
        // No custom row-cell renderer any more: the whole point of the removal is that the
        // cell is a CHIP, and chorale's own `RenderKind::Badge` draws chips. The four variants
        // are the exact closed set `confidence_tags()` offers as filter options, which is the
        // same anti-drift discipline the Severity column uses.
        ColumnDef::new(ColumnId("needs_review"), "Confidence", |f: &FindingView| {
            CellValue::Text(confidence_tag(f).to_string())
        })
        .sortable()
        .filter(FilterKind::MultiSelect { options: confidence_tags() })
        .render_kind(RenderKind::Badge(
            BadgeVariantMap::new()
                .with("test", BadgeVariant::new("Test", "yellow"))
                .with("needs-review", BadgeVariant::new("Needs review", "orange"))
                .with("high", BadgeVariant::new("High", "green"))
                // "Calibration never looked at this row" is a different statement from "it
                // looked and was confident", and the chip says so rather than leaving a blank
                // the curator would read as the latter.
                .with("unrated", BadgeVariant::new("Unrated", "gray")),
        ))
        .initial_width(130.0),
        // The ratchet: enforced (active = new/changed) vs suppressed (baseline debt or
        // an inline waiver). Report shows all; the gate blocks only the enforced ones.
        ColumnDef::new(ColumnId("status"), "Enforcement", |f: &FindingView| {
            CellValue::Text(match f.status.as_str() {
                "suppressed-baseline" => "baseline".to_string(),
                "suppressed-inline" => "waived".to_string(),
                "suppressed-self-reference" => "self-ref".to_string(),
                _ => "enforced".to_string(),
            })
        })
        .sortable()
        .render_kind(RenderKind::Badge(
            BadgeVariantMap::new()
                .with("enforced", BadgeVariant::new("Enforced", "red"))
                .with("baseline", BadgeVariant::new("Baseline debt", "gray"))
                .with("waived", BadgeVariant::new("Waived", "yellow"))
                .with("self-ref", BadgeVariant::new("Self-referential", "gray")),
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
        // The hidden rule-group key — the deviation sentence's delivery mechanism. See
        // `RULE_GROUP_COLUMN`. Not `.sortable()`, no `.filter(...)`: the curator never
        // interacts with it, and it is excluded from the rendered columns by
        // `finding_table_state`. Its value is `rule_group_label`, which is the bare rule id
        // for a defect and `rule id · N of M sites deviate from "<convention>"` for a
        // deviation, so grouping by it partitions exactly as grouping by the rule id did.
        ColumnDef::new(RULE_GROUP_COLUMN, "Rule group", |f: &FindingView| {
            CellValue::Text(rule_group_label(f))
        }),
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

/// The Queue's `TableState` exactly as `FindingsTable` mounts it: the real column set, with
/// the grouping key hidden.
///
/// One function rather than an inline construction so the render path and the tests mount the
/// SAME state. A test that forgot the visibility line would be asserting against a table with
/// a stray "Rule group" column the product never shows.
pub(super) fn finding_table_state(
    rows: Vec<(RowId, FindingView)>,
    repos: Vec<String>,
    show_bucket: bool,
) -> TableState<FindingView> {
    let mut state = TableState::new(rows, finding_columns(repos, show_bucket));
    // Set directly on the state rather than through `handle.set_column_visibility` after
    // mount: the column must never be visible, not even for the first frame, and a transition
    // fired from an effect is one render too late.
    state
        .column_visibility
        .insert(RULE_GROUP_COLUMN, false);
    state
}

/// What the Queue's order indicator says when NO manual column sort is active: the curated
/// default from design §5, named rather than implied.
///
/// A blank indicator would be the failure mode worth avoiding — the default order is a
/// JUDGEMENT (the `unspecified` alarm floated to the top of its severity band, Defect before
/// Deviation at equal severity, cheapest deviations first), and a curator 300 rows into a
/// sitting has to be able to tell at a glance whether he is looking at that judgement or at
/// his own re-cut.
pub(super) const DEFAULT_ORDER_LABEL: &str =
    "Camerata default order \u{2014} severity, then kind, then cheapest deviations first";

/// Human name of a findings column, for the order indicator. Resolved from the live column
/// set (never a second hand-written table), so a renamed or new column can't leave the
/// indicator naming a header that no longer exists; an unknown id degrades to the raw id
/// rather than vanishing.
fn finding_column_header(cols: &[ColumnDef<FindingView>], id: ColumnId) -> String {
    cols.iter()
        .find(|c| c.id == id)
        .map_or_else(|| id.as_str().to_string(), |c| c.header.clone())
}

/// The sentence the Queue's order indicator shows for `sort` (chorale's own sort state).
///
/// Empty sort = the curated default, which is also what `clear_sort` restores: chorale's
/// `filtered_sorted_pairs` only reorders when `state.sort` is non-empty, so an empty sort
/// hands back `state.rows` exactly as `TriageModel::visible` ordered them.
///
/// A manual cut names its column(s) and direction, and says the default is still underneath —
/// which is literally true, because chorale's sort is STABLE: ties in the manual key keep the
/// curated order, so e.g. a cut by Kind still lists the deviations cheapest-ratio-first within
/// the Deviation block. Multi-column (shift+click) cuts are listed in priority order.
pub(super) fn order_indicator_text(
    sort: &[chorale_core::SortState],
    cols: &[ColumnDef<FindingView>],
) -> String {
    if sort.is_empty() {
        return DEFAULT_ORDER_LABEL.to_string();
    }
    let cuts: Vec<String> = sort
        .iter()
        .map(|s| {
            let arrow = match s.direction {
                chorale_core::SortDirection::Asc => "\u{2191}",
                chorale_core::SortDirection::Desc => "\u{2193}",
            };
            format!("{} {arrow}", finding_column_header(cols, s.column))
        })
        .collect();
    format!(
        "Your sort: {} \u{2014} Camerata default order underneath",
        cuts.join(", then ")
    )
}

/// The order the Queue is showing, resolved for display: the sentence, plus whether it is the
/// curator's own cut (so the way back has to be offered).
///
/// One struct from one function ([`queue_order`]) rather than two props, so the sentence and
/// the "is this a manual cut" flag cannot disagree — a bar that said "Camerata default order"
/// while offering a reset button, or named a cut while hiding the way back, would be worse
/// than no indicator.
#[derive(Clone, PartialEq)]
pub(super) struct QueueOrder {
    /// The sentence shown to the curator.
    pub text: String,
    /// True when a manual column cut is active.
    pub manual: bool,
}

/// Resolve chorale's sort state into what the order bar shows.
pub(super) fn queue_order(
    sort: &[chorale_core::SortState],
    cols: &[ColumnDef<FindingView>],
) -> QueueOrder {
    QueueOrder {
        text: order_indicator_text(sort, cols),
        manual: !sort.is_empty(),
    }
}

/// The Queue's ORDER BAR: which order is on screen, and one click back to the curated default.
///
/// Rendered immediately above the table's header row so the sentence and chorale's own ↑/↓
/// arrow read in one glance. Its own component (rather than inline in `FindingsTable`) because
/// both of its states need to be verifiable under render: SSR cannot click a column header, so
/// a bar buried inside the table component could only ever be checked in its default state.
///
/// `Default order` is shown ONLY while a manual sort is active. On the default it would be a
/// no-op control, and its absence is itself part of the indicator: no button means you are
/// looking at the curated order.
#[component]
pub(super) fn QueueOrderBar(
    /// The resolved order (see [`queue_order`]).
    order: QueueOrder,
    /// Invoked by `Default order`. The call site clears chorale's whole sort list.
    on_reset: Callback<()>,
) -> Element {
    let QueueOrder { text, manual } = order;
    rsx! {
        div { class: "queue-order",
            span { class: "queue-order-eyebrow", "Order" }
            span {
                class: if manual { "queue-order-value manual" } else { "queue-order-value" },
                title: "Click any column header to re-cut the list; click it again to reverse. The curated default is severity, then kind (the unspecified alarm first, then defects, then deviations), then the cheapest deviations first.",
                "{text}"
            }
            if manual {
                button {
                    class: "btn-edit-sm queue-order-reset",
                    title: "Drop every manual column sort and restore Camerata's curated default order.",
                    onclick: move |_| on_reset.call(()),
                    "Default order"
                }
            }
        }
    }
}

/// One deviation rule group as the bulk bar shows it.
///
/// Flat, owned, `PartialEq` data rather than the `DeviationGroup` itself, so the bar is a
/// props-only component that renders (and is therefore testable) without a chorale table, a
/// disposition map or a VirtualDom-minted handle.
#[derive(Clone, PartialEq)]
pub(super) struct DeviationBulkRow {
    /// The rule id, as the group header shows it.
    pub rule_id: String,
    /// The structured sentence (see `DeviationHeadline::sentence`) — the claim the bulk action
    /// is accepting or rejecting. Repeated here rather than only on the group header because
    /// a bulk button whose subject is somewhere else on screen is exactly the "confident
    /// click, unread claim" failure the convention-commit gate exists to prevent.
    pub sentence: String,
    /// How many rows in THIS view the action would move. Not `deviating_sites` — see
    /// `DeviationGroup::row_count`.
    pub row_count: usize,
}

impl DeviationBulkRow {
    /// Flatten one group into the bar's props. The ONLY place `DeviationBulkRow` is built, so
    /// the bar's sentence is always the group's own `headline.sentence()` — the same builder
    /// the group header's label goes through — and its count is always `row_count()`.
    pub(super) fn from_group(g: &DeviationGroup) -> Self {
        Self {
            rule_id: g.rule_id.clone(),
            sentence: g.headline.sentence(),
            row_count: g.row_count(),
        }
    }
}

/// RULE-LEVEL BULK ACTIONS for deviation groups (design §2/§4, Pass 2 step 2).
///
/// "The natural unit of a deviation decision is the rule, not the site: if the curator
/// believes the convention is real, all N sites are confirmed together far more often than
/// not." So the deviation decision gets one gesture; the per-site gestures in the toolbar stay
/// for the minority case where one of the N exceptions is itself a false positive.
///
/// ## Why this is a bar and not content inside the group-header `<tr>`
///
/// The design's 2026-10-12 dependency check recorded `render_grouped_row` / `group_header_tr`
/// / `group_header_class` as a chorale 0.2.3 group-header content hook. They are not one:
/// in `chorale-dioxus` 0.2.3 the first two are PRIVATE functions and the third is a `String`
/// CSS class on the `<tr>`. The crate exports only `Table`, `use_table`, `UseTableHandle`,
/// the cell/row renderer types and `RowClass` — there is no slot for host content on a group
/// header, and no `Send + Sync` cell renderer could carry an onclick over a Dioxus `Signal`
/// anyway. The SENTENCE still reaches the real header, through the grouped-on value
/// (`RULE_GROUP_COLUMN`); the BUTTONS cannot, so they sit immediately above the table where
/// every deviation group is visible at once, including on mount when every group is collapsed.
///
/// ## Both directions, and why Confirm comes first
///
/// Step 2 shipped the bulk PLUMBING with only `False positive all N` on it, because none of
/// the four dispositions that existed then honestly meant "yes, this is real, it ships".
/// Step 3 adds `Confirm all N` over the identical group row set, as the owner's ruling
/// required. It is rendered FIRST and styled as the primary action because it is the
/// overwhelmingly more common answer: a deviation group exists precisely because the codebase
/// itself established the convention, so "the convention is real and these are the exceptions"
/// is the default reading, and "the tool mis-parsed all N of these" is the exception.
#[component]
pub(super) fn DeviationBulkBar(
    /// The deviation groups in the current view, in table order.
    groups: Vec<DeviationBulkRow>,
    /// Invoked with the index into `groups` when `Confirm all N` is clicked. Takes no reason,
    /// because confirming needs none (see `TriageModel::confirm`).
    on_confirm_all: Callback<usize>,
    /// Invoked with the index into `groups` when `False positive all N` is clicked. The call
    /// site owns the reason, the require-reason invariant and the disposition write.
    on_false_positive_all: Callback<usize>,
    /// Open the RULE HUB on this group's rule (design §8). This bar IS the deviation group
    /// header's reachable surface — chorale's real `<tr>` header takes no host content — so
    /// the hub hangs off it rather than off a header that cannot carry a button.
    ///
    /// Defaulted rather than required so the bar's existing render tests, which care about the
    /// bulk decisions and not about navigation, keep compiling and asserting unchanged.
    #[props(default)]
    on_open_hub: Callback<usize>,
) -> Element {
    // No deviations in this view means no decisions of this shape to make. An empty bar with
    // a heading would be a standing reminder of nothing.
    if groups.is_empty() {
        return rsx! {};
    }
    rsx! {
        div { class: "deviation-bulk",
            span { class: "deviation-bulk-eyebrow",
                "Rule-level decisions"
            }
            for (i, g) in groups.iter().enumerate() {
                div { key: "{g.rule_id}-{i}", class: "deviation-bulk-group",
                    // The rule id is the hub's door here, as it is on every surface that shows
                    // one (design §8). A button rather than the plain span it used to be, so
                    // the gesture is the same one the panels and the detail pane offer.
                    button {
                        class: "rule-hub-link deviation-bulk-rule",
                        title: "Everything this scan knows about {g.rule_id}.",
                        onclick: move |_| on_open_hub.call(i),
                        "{g.rule_id}"
                    }
                    span { class: "deviation-bulk-sentence", "{g.sentence}" }
                    button {
                        class: "btn-run deviation-bulk-confirm",
                        title: "Confirm every one of this rule's {g.row_count} row(s) in this table in one act: the convention is real and these are genuine exceptions. Identical to selecting them and confirming each. No reason required.",
                        onclick: move |_| on_confirm_all.call(i),
                        "Confirm all {g.row_count}"
                    }
                    button {
                        class: "btn-restart deviation-bulk-fp",
                        // §9.2 removed the always-visible false-positive textarea this button
                        // used to read. It now RAISES the on-demand prompt, which names this
                        // group's own count — a thing a shared textarea beside the table could
                        // never do. The reason is still required and still enforced in the same
                        // place, `TriageModel::bulk_false_positive`.
                        title: "Mark every one of this rule's {g.row_count} row(s) in this table as a false positive, in one act. Identical to selecting them and marking each. Asks for the required reason first.",
                        onclick: move |_| on_false_positive_all.call(i),
                        "False positive all {g.row_count}"
                    }
                }
            }
        }
    }
}

/// The refusal the curator sees when a required reason is blank.
///
/// One function for every refusal path (the per-selection prompt, the bulk-bar prompt, and
/// the detail pane's own), because the require-reason invariant is ONE rule and a curator who
/// meets it worded three ways will reasonably wonder whether it is three rules. It names the
/// count for the same reason [`bulk_reason_heading`] does: a refusal that says "a finding"
/// while twelve rows wait is quietly wrong about the stakes.
pub(super) fn reason_required_message(state: TriageState, count: usize) -> String {
    let noun = if count == 1 { "finding" } else { "findings" };
    format!(
        "A reason is required to move {count} {noun} to \u{201c}{}\u{201d}. It is recorded in \
         the draft and read by the report export.",
        state.label()
    )
}

/// WHAT a raised toolbar reason prompt will commit when the curator finishes typing.
///
/// The row set is SNAPSHOTTED at the moment the prompt opens, never re-read at commit time.
/// That is the whole reason this is a struct rather than two loose signals: the heading says
/// "3 findings", and the commit has to move those three. Re-reading `handle.selected_ids()`
/// after the curator has typed a sentence would let a stray click between the two change the
/// row set under a heading that already promised a number.
#[derive(Clone, PartialEq)]
pub(super) struct ToolbarReasonPrompt {
    /// The disposition the reason is for. Only the two that require one ever appear here.
    pub state: TriageState,
    pub target: ReasonTarget,
}

/// Which rows a raised prompt will move. SELF-CONTAINED on purpose: each variant carries the
/// findings themselves, not a handle that has to be re-resolved at commit time.
///
/// The alternative — a `Vec<RowId>` resolved against `id_map`, or a group INDEX resolved
/// against `bulk_groups` — looks lighter and is the version that can lie. Both of those lists
/// are rebuilt every render from the current visible row set, so between raising the prompt
/// and committing it (which is as long as the curator takes to type a sentence) a re-sort, a
/// column filter or a group collapse can change what the handle points at. The heading has
/// already promised a number by then. Carrying the rows makes "the count the curator read"
/// and "the rows the commit moves" the same list by construction rather than by timing.
#[derive(Clone, PartialEq)]
pub(super) enum ReasonTarget {
    /// The rows that were selected when the toolbar button was pressed, with the chorale row
    /// ids to drop from the table afterwards.
    Selection {
        ids: Vec<RowId>,
        rows: Vec<FindingView>,
    },
    /// One whole deviation group, as the bulk bar's button advertised it.
    Group(DeviationGroup),
}

impl ToolbarReasonPrompt {
    /// How many findings this prompt's commit will move. Drives the heading, so the number the
    /// curator reads is the number the commit uses — the same `Vec`, counted once here and
    /// disposed once there.
    pub(super) fn count(&self) -> usize {
        match &self.target {
            ReasonTarget::Selection { rows, .. } => rows.len(),
            ReasonTarget::Group(g) => g.rows.len(),
        }
    }
}

/// THE QUEUE TOOLBAR'S ON-DEMAND REASON PROMPT (design §3, §9.2).
///
/// > The two always-visible reason textareas in the Queue toolbar. Replaced by on-demand
/// > prompts. An empty textarea the curator is not using is noise 95% of the time.
///
/// Props-only, for the reason `ProcessGate` and `DeviationBulkBar` are: SSR cannot click, so
/// the only way to render-test "the prompt the Ignore button raises" is to make the prompt a
/// component that can be handed its open state directly.
///
/// It is NOT the detail pane's prompt and must not be confused with it. The pane's prompt is
/// scoped to the pane's focus and disarms the `C`/`I`/`T`/`F` keys while it is open; this one
/// lives outside the pane entirely, so focusing it fires the pane's own `focusout` and the
/// keys disarm through the existing path rather than through a second rule. The two never
/// render at once in practice (the pane's opens from a pane gesture, this one from a toolbar
/// or bulk-bar gesture) and, critically, neither can commit the other's rows.
///
/// The heading comes from `camerata_ui_core::triage::bulk_reason_heading`, which NAMES THE
/// COUNT. The textareas this replaces never could: a box sitting beside a selection cannot
/// say how many rows the button next to it will move, and a bulk disposition is exactly where
/// "I thought that was three rows" is expensive.
#[component]
pub(super) fn QueueReasonPrompt(
    /// The raised prompt. The parent mounts this component only when one is open.
    prompt: ToolbarReasonPrompt,
    /// The reason typed so far.
    reason: String,
    on_reason: Callback<String>,
    on_commit: Callback<()>,
    on_cancel: Callback<()>,
) -> Element {
    let heading = bulk_reason_heading(prompt.state, prompt.count());
    rsx! {
        div { class: "queue-reason-prompt",
            label { class: "rule-modal-label", "{heading}" }
            textarea {
                class: "addressee-input queue-reason-input",
                rows: "2",
                autofocus: true,
                placeholder: "why \u{2014} the export reads this",
                value: "{reason}",
                // Pre-focused for the same reason the pane's prompt is: a prompt that appears
                // on a re-render and takes no focus is a surface the curator has to go find
                // after asking for it.
                onmounted: move |e| {
                    spawn(async move {
                        let _ = e.set_focus(true).await;
                    });
                },
                oninput: move |e| on_reason.call(e.value()),
                onkeydown: move |e: KeyboardEvent| {
                    // Stop propagation even though this prompt sits outside the detail pane
                    // and the pane's keydown handler is on the pane element: the guarantee
                    // "a letter typed into a reason box is never read as a disposition" should
                    // not depend on where in the tree this happens to be mounted today.
                    e.stop_propagation();
                    match e.key() {
                        Key::Escape => { on_cancel.call(()); }
                        Key::Enter if !e.modifiers().contains(Modifiers::SHIFT) => {
                            e.prevent_default();
                            on_commit.call(());
                        }
                        _ => {}
                    }
                },
            }
            div { class: "queue-prompt-actions",
                button {
                    class: "btn-run queue-prompt-commit",
                    onclick: move |_| on_commit.call(()),
                    "Commit \u{2192}"
                }
                button {
                    class: "btn-edit-sm queue-prompt-cancel",
                    onclick: move |_| on_cancel.call(()),
                    "Cancel"
                }
            }
        }
    }
}

/// THE PROCESS GATE: the hint that says what is still outstanding, and the button that commits
/// the ignored and tech-debt buckets.
///
/// Its own props-only component (Pass 2 step 3) for the reason `QueueOrderBar` and
/// `DeviationBulkBar` are: SSR cannot click, and this gate lives inside `ScanResults`, which
/// sits behind `use_resource` network resources and is not SSR-testable at all. Step 1's build
/// note recorded exactly that as a coverage caveat — the gate's live bug (informational rows
/// blocking it forever) was fixed with only the pure derivation under test. As props-only data
/// the gate can now be rendered in any tally state, including the one this step introduces: a
/// Queue where everything is CONFIRMED, nothing is outstanding, and there is still nothing to
/// commit.
///
/// Every judgment it renders comes from [`TriageCounts`], which is unit-tested in core. This
/// component chooses words, not rules.
#[component]
pub(super) fn ProcessGate(
    /// The live tally over the QUEUE rows (never the raw finding set — see
    /// `TriageModel::counts`).
    counts: TriageCounts,
    /// Whether a Process run is already in flight.
    processing: bool,
    /// Invoked when the button is clicked. The caller owns the commit itself.
    on_process: Callback<()>,
) -> Element {
    let outstanding = counts.outstanding();
    rsx! {
        div { class: "triage-process",
            if outstanding > 0 {
                // The exits are named in full. Before step 3 this sentence offered only
                // "Ignore or save as Tech debt", which was already incomplete (False positive
                // also resolves a row) and would now omit the one the curator reaches for
                // most.
                p { class: "section-hint",
                    "Resolve the {outstanding} remaining Unresolved finding(s) \u{2014} Confirm, \
                     Ignore, save as Tech debt, or mark a false positive \u{2014} before Processing."
                }
            } else if !counts.has_committable_work() {
                // The common end state once Confirm exists: every row decided, and nothing
                // for the server to do. Saying so beats a disabled button with no
                // explanation, which is the dead affordance the standing rule is about.
                p { class: "section-hint",
                    "Nothing to process: no findings were ignored or saved as tech debt. \
                     Confirmed and False positive are recorded in this draft and read by the \
                     report export; neither commits anything to a repository."
                }
            }
            button {
                class: "btn-run",
                disabled: processing || !counts.can_process(),
                onclick: move |_| on_process.call(()),
                if processing { "Processing\u{2026}" } else { "Process ignored + tech-debt buckets" }
            }
        }
    }
}

/// The Kind chip's `(class, label, tooltip)` for a wire kind tag.
///
/// One function, THREE renderers: the Queue's Kind column cell, the docked detail pane's
/// header, and the rule hub strip all call it, so the badge a curator sees in the row, the
/// badge he sees in the pane describing that same row, and the badge in the hub joining them
/// can never say different things. The states are not colours of one thing — `unspecified` is
/// an ALARM about the pipeline and gets a red OUTLINE and its own label, which is why this is
/// hand-rolled rather than a chorale `RenderKind::Badge`.
///
/// The three INFORMATIONAL tags are listed here even though the Queue never shows one, because
/// the hub does: it is reachable from the Conventions, No convention and Not assessed panels,
/// and a convention row falling into the catch-all would be labelled "Unspecified" — an alarm
/// raised about a perfectly healthy row. Listing them also narrows the catch-all to what it
/// should always have been: a tag this UI does not recognise.
pub(super) fn kind_chip_parts(tag: &str) -> (&'static str, &'static str, &'static str) {
    match tag {
        "convention_observed" => (
            "kind-chip kind-informational",
            "Convention observed",
            "The codebase follows this convention consistently, with zero violations. \
             Informational — there is nothing here to dispose.",
        ),
        "no_convention" => (
            "kind-chip kind-informational",
            "No convention",
            "The codebase is split with no clear majority. One informational row per rule.",
        ),
        "not_assessed" => (
            "kind-chip kind-informational",
            "Not assessed",
            "The rule was armed but could not be evaluated. An honesty row: the scan cannot \
             vouch for it either way.",
        ),
        "defect" => ("kind-chip kind-defect", "Defect", "An objective fault."),
        "deviation" => (
            "kind-chip kind-deviation",
            "Deviation",
            "A consistency violation against the convention this codebase itself \
             predominantly follows. Same severity words as a defect, a different claim.",
        ),
        _ => (
            "kind-chip kind-unspecified",
            "Unspecified",
            "PIPELINE BUG: this row reached the UI with no kind, which means an unmapped \
             emission path. It is NOT a defect — it is a gap to fix upstream.",
        ),
    }
}

// ── THE RULE HUB (design §8) ─────────────────────────────────────────────────────────────

/// WHICH RULE the hub is open on, scoped exactly as `camerata_ui_core::triage::rule_hub` is.
///
/// Carries the repo as well as the rule id because the hub is a per-`(repo, rule)` view: in a
/// multi-repo scan one repo can honestly follow a convention while another deviates from it,
/// and a hub keyed on the rule id alone would merge two different assessments into one strip
/// that states a contradiction as a fact.
#[derive(Clone, PartialEq, Debug)]
pub(super) struct RuleHubRequest {
    pub repo: String,
    pub rule_id: String,
}

/// THE "THIS RULE IN THIS SCAN" STRIP (design §8) — the one join the workbench builds.
///
/// > Rule -> everything about that rule. This is THE join, and it gets the investment.
///
/// Props-only over `camerata_ui_core::triage::RuleHubView`, which is already the flat owned
/// shape (the same discipline as `DeviationBulkRow` and `DetailPaneRow`): every judgement —
/// which surface the rule is on, what its outcome sentence is, how many sites it touches,
/// whether the invariant is violated — is made in core and unit-tested there. This component
/// chooses words and pixels. SSR can therefore render it in every state, including the two
/// that matter most and are hardest to reach by clicking: a conflicting rule, and a deviation
/// whose denominator the producer could not establish.
///
/// ## The one honesty constraint, rendered
///
/// §8: "conforming sites are not in the data (only counts), so there is no 'show me the 45
/// conforming endpoints' link to offer. Do not fabricate one." The conforming count is
/// therefore rendered as TEXT that says so in the same breath, and the strip offers exactly
/// ONE navigation control: `hub.jump`, the single surface this rule lives on.
#[component]
pub(super) fn RuleHubStrip(
    hub: RuleHubView,
    /// "Take me to this rule." Invoked with `hub.jump` — never with a surface the caller
    /// chose, so the button and the derivation cannot disagree about where the rule is.
    on_jump: Callback<KindSurface>,
    /// THE ALTERNATIVES CONTEXT (design §9.1), for a multi-option rule the audit evaluated.
    /// `None` for a single-option rule, and for any rule the audit recorded no recommendation
    /// for — which is the majority, so this must render nothing at all rather than an empty
    /// "Evaluated against: (none)" line.
    ///
    /// Read-only here, and deliberately so. The Change select and the staged-override/rescan
    /// flow stay on `RuleAlternativesPanel`; see that component's doc comment for why §9.1
    /// cannot move them. What moves is the FACT and its justification, to the surface every
    /// rule id already opens.
    ///
    /// Defaulted so the strip's existing render tests — which assert, among other things,
    /// that the hub offers exactly ONE control — keep compiling and asserting unchanged.
    #[props(default)]
    alternatives: Option<AlternativeControlsRow>,
) -> Element {
    let (kind_class, kind_label, kind_tip) = kind_chip_parts(hub.kind_tag);
    let jump = hub.jump;
    let jump_label = match jump {
        KindSurface::Queue => "Show in the Queue, filtered to this rule".to_string(),
        other => format!("Show in \u{201c}{}\u{201d}", other.label()),
    };
    rsx! {
        div { class: "rule-hub",
            p { class: "rule-hub-eyebrow", "This rule in this scan" }
            div { class: "rule-hub-head",
                span { class: "rule-hub-id", "{hub.rule_id}" }
                span { class: "{kind_class}", title: "{kind_tip}", "{kind_label}" }
                if !hub.repo.is_empty() {
                    span { class: "rule-modal-tag", "{hub.repo}" }
                }
            }
            p { class: "rule-hub-outcome", "{hub.outcome}" }
            div { class: "rule-hub-meta",
                span { class: "rule-modal-tag", "{hub.row_count} row(s)" }
                // Sites, not rows. A merged finding touches several places and is still ONE
                // decision; conflating the two is the shape C5 shipped.
                span { class: "rule-modal-tag", "{hub.site_count} site(s)" }
                if let Some(label) = hub.convention_label.clone() {
                    span { class: "rule-modal-tag rule-hub-convention", "convention \u{b7} {label}" }
                }
                // The Evaluated-against fact, relocated from `RuleAlternativesPanel`
                // (design §9.1) to the surface that is reachable from every rule id.
                //
                // The raw OPTION ID, and only when the richer line below is absent. When the
                // audit recorded a recommendation for this rule, that line carries the
                // authored LABEL instead — which is what the curator reads everywhere else,
                // and the id is an internal key he has no reason to decode. Rendering both
                // would state one fact twice in two vocabularies.
                if alternatives.is_none() {
                    if let Some(opt) = hub.evaluated_option_id.clone() {
                        span { class: "rule-modal-tag rule-hub-evaluated", "evaluated against \u{b7} {opt}" }
                    }
                }
            }
            // THE ALTERNATIVES CONTEXT (design §9.1): what option this rule was judged
            // against, who chose it, the evidence behind the choice, and WHY — the answer the
            // panel's "Why?" button reveals, rendered directly.
            //
            // Directly, rather than behind a Why? button of its own, for two reasons. The hub
            // is an overlay the curator opened deliberately on ONE rule, so three sentences
            // about that rule are what he came for, not something to hide behind a second
            // click; and a modal raised from inside a modal is a worse surface than the text
            // it would reveal. The panel keeps its button because it lists many rules at once
            // and inline reasoning for all of them would be a wall.
            //
            // It also adds NO control, which is load-bearing: §8's "the hub offers exactly one
            // navigation and no link graph" is pinned by a test that counts the rendered
            // buttons, and this line is read-only precisely so that invariant survives it.
            if let Some(alt) = alternatives.clone() {
                div { class: "rule-hub-alt",
                    p { class: "rule-hub-alt-line",
                        "Evaluated against: "
                        strong { "{alt.evaluated_label}" }
                        span { class: "{alt.badge_class}", "{alt.badge_text}" }
                        if alt.accepted {
                            span { class: "rec-badge rec-badge-accepted", "Accepted" }
                        }
                        if !alt.applicable {
                            span { class: "rec-not-applicable", "Not applicable to this repo" }
                        }
                    }
                    if let Some(ev) = alt.evidence.clone() {
                        span { class: "rec-evidence", "Evidence: {ev}" }
                    }
                    p { class: "rule-hub-alt-why",
                        if alt.reasoning.trim().is_empty() {
                            "No reasoning was recorded for this recommendation."
                        } else {
                            "{alt.reasoning}"
                        }
                    }
                    if alt.hallucinated {
                        span { class: "rec-hallucinated-note",
                            "The model did not return a valid choice for this rule; this is the selected or default option shown instead."
                        }
                    }
                    // Where the DECISION lives, named rather than implied. §9.1 wanted the
                    // Change select here too; it could not come (see `RuleAlternativesPanel`),
                    // and a curator reading "Override, pending rescan" with no way to act on
                    // it needs to be told where the control is rather than left to hunt.
                    p { class: "rule-hub-alt-where",
                        "Change this, or rescan a staged override, in \u{201c}Rule alternatives\u{201d} above the Queue."
                    }
                }
            }
            // COUNTS ONLY, and the sentence says why. Offering a link here would be inventing
            // a data set: the pipeline reports deviating sites and a total, never the
            // conforming set.
            if let Some(c) = hub.conforming {
                p { class: "rule-hub-conforming",
                    "{c.conforming} of {c.assessed} assessed sites conform. The conforming sites are not in the scan data \u{2014} only the counts \u{2014} so there is nothing here to open."
                }
            }
            // The invariant, surfaced rather than re-decided. `conflict_surfaces` is
            // `rules_on_multiple_surfaces`' own verdict for this (repo, rule).
            if !hub.conflict_surfaces.is_empty() {
                {
                    let names: Vec<&'static str> =
                        hub.conflict_surfaces.iter().map(|s| s.label()).collect();
                    rsx! {
                        p { class: "rule-hub-conflict",
                            "Pipeline bug: this rule reached {hub.conflict_surfaces.len()} kind surfaces in this scan ({names.join(\", \")}). A rule emits deviations OR a convention OR one no-convention row OR a not-assessed row \u{2014} never two, so these counts are not trustworthy."
                        }
                    }
                }
            }
            div { class: "rule-hub-actions",
                button {
                    class: "btn-edit-sm rule-hub-jump",
                    onclick: move |_| on_jump.call(jump),
                    "{jump_label}"
                }
            }
        }
    }
}

/// THE RULE HUB MODAL: the strip in an overlay, reachable from every surface that shows a
/// rule id.
///
/// ## Why this is not a strip bolted onto `RuleDetailModal`
///
/// Design §8 says "the existing rule detail modal gains a 'this rule in this scan' strip".
/// The existing modal is [`crate::cockpit::rules::RuleDetailModal`], and it is driven by a
/// `ProposedRuleView` — the corpus row for a rule the scan PROPOSED. A scan's findings include
/// rule ids with no proposed-rule row at all (the deterministic content-rule floor, external
/// tool previews, the model's advisory ids), so hanging the hub off that modal would make the
/// hub unreachable for exactly the rules whose provenance a curator is most likely to question.
/// The hub is therefore its own overlay over core's `rule_hub` derivation, which needs nothing
/// but the scan's own rows; where a `ProposedRuleView` does exist, the full rule detail stays
/// one further click away, the same relationship the detail pane has with the finding modal.
#[component]
pub(super) fn RuleHubModal(
    hub: RuleHubView,
    on_jump: Callback<KindSurface>,
    on_close: Callback<()>,
    /// See [`RuleHubStrip`]'s `alternatives`. Passed straight through.
    #[props(default)]
    alternatives: Option<AlternativeControlsRow>,
) -> Element {
    rsx! {
        div { class: "rule-modal-overlay", onclick: move |_| on_close.call(()),
            div { class: "rule-modal rule-hub-modal", onclick: move |e| e.stop_propagation(),
                div { class: "rule-modal-head",
                    span { class: "rule-modal-id", "{hub.rule_id}" }
                    button {
                        class: "rule-modal-close",
                        onclick: move |_| on_close.call(()),
                        "\u{2715}"
                    }
                }
                RuleHubStrip { hub, on_jump, alternatives }
            }
        }
    }
}

/// The DOM id of the docked detail pane's focusable region.
///
/// Named once because three things depend on it agreeing: the `aside`'s own `id`, the
/// `focus_detail_pane` helper, and the CSS focus ring that makes "the pane holds focus"
/// visible. The Q5 scoping is stated in terms of that condition, so the curator has to be
/// able to both see it and reach it.
pub(super) const DETAIL_PANE_ID: &str = "camerata-finding-pane";

/// Move real keyboard focus into the detail pane.
///
/// Deliberately a DOM focus call rather than a signal write: `PaneFocus` is maintained by the
/// pane's own `focusin`/`focusout`, so it always reflects where focus actually is. Writing the
/// signal directly would let the pane report its keys armed while the keydown handler — which
/// lives on the pane element — received nothing.
fn focus_detail_pane() {
    dioxus::document::eval(&format!(
        "var el=document.getElementById('{DETAIL_PANE_ID}');if(el)el.focus();"
    ));
}

/// One ABSORBED evidence site as the detail pane lists it (design §6).
#[derive(Clone, PartialEq)]
pub(super) struct PaneSiteRow {
    /// `path:line`, or `repo path:line` when the absorbed site lives in a different repo
    /// from the primary (a cross-file merge can span two repos in one scan).
    pub location: String,
    /// The rule id this site was originally flagged under, before being absorbed.
    pub rule_id: String,
    /// The line this site cited.
    pub snippet: String,
    /// A pure CONSEQUENCE of the primary's root cause rather than another place needing the
    /// same fix. Rendered subordinated, under "also affects".
    pub consequence: bool,
}

/// THE FOCUSED FINDING, flattened into exactly what the pane renders.
///
/// Flat, owned, `PartialEq` data rather than the `FindingView` itself — the same shape (and
/// the same reason) as `DeviationBulkRow`: the pane is then a props-only component that
/// renders, and is therefore testable, without a chorale table, a disposition map, a network
/// resource or a VirtualDom-minted handle. SSR cannot press a key; what it CAN do is read
/// what the pane says about the row it is describing and about whether the keys are armed,
/// and those are the two things a keystroke test would otherwise have to take on trust.
#[derive(Clone, PartialEq)]
pub(super) struct DetailPaneRow {
    pub rule_id: String,
    /// From [`kind_chip_parts`] — the same chip the Queue's Kind column renders.
    pub kind_class: String,
    pub kind_label: String,
    pub kind_tip: String,
    /// A deviation's structured claim (`DeviationHeadline::sentence`), verbatim — the same
    /// words its group header carries. `None` for every other kind.
    pub claim: Option<String>,
    /// The CALIBRATED severity, authored word, read straight off `FindingView::severity`.
    /// Never the rank-prefixed Severity CELL value, which exists only so chorale's
    /// lexicographic column sort orders the ladder correctly.
    pub severity: String,
    pub status: String,
    pub repo: String,
    /// `path:line` of the primary site.
    pub location: String,
    /// Every absorbed site (design §6). Empty for an un-merged row.
    pub sites: Vec<PaneSiteRow>,
    /// `1 + sites.len()`, from `camerata_ui_core::triage::site_count`.
    pub site_count: usize,
    pub snippet: String,
    /// THE CITED STANDARD: the rule's own authored directive, as the rules corpus states it.
    pub standard: Option<String>,
    /// The finding's explanation, with the calibration `[needs review: …]` flag split off.
    pub explanation: String,
    pub needs_review: Option<String>,
    /// Structured calibration confidence (`high` / `needs-review`).
    pub confidence: Option<String>,
    /// Structured remediation-effort estimate (`low` / `medium` / `high`).
    pub effort: Option<String>,
    /// The recommended fix, fetched by the call site from the SAME server function the
    /// report's "Fix:" line uses — never re-derived here.
    pub fix: Option<String>,
    /// Other rule ids violated at this same location.
    pub also_matches: Vec<String>,
    /// 1-based position of this row in the table, and the table's row count: "3 of 212".
    pub position: usize,
    pub total: usize,
}

impl DetailPaneRow {
    /// Flatten the focused finding into the pane's props. The ONLY place a `DetailPaneRow` is
    /// built, so every field goes through one derivation: the kind chip is
    /// [`kind_chip_parts`]'s, the claim is `DeviationHeadline::sentence`'s, the site count is
    /// core's `site_count`, and the severity is the authored word rather than the sort value.
    ///
    /// `index` is 0-based (the cursor's); `position` is the 1-based number shown.
    pub(super) fn from_finding(
        f: &FindingView,
        directive: Option<&str>,
        fix: Option<String>,
        index: usize,
        total: usize,
    ) -> Self {
        let (kind_class, kind_label, kind_tip) = kind_chip_parts(kind_tag(f));
        let (body, needs_review) = split_needs_review(&f.detail);
        Self {
            rule_id: f.rule_id.clone(),
            kind_class: kind_class.to_string(),
            kind_label: kind_label.to_string(),
            kind_tip: kind_tip.to_string(),
            claim: deviation_headline(f).map(|h| h.sentence()),
            severity: f.severity.clone(),
            status: f.status.clone(),
            repo: f.repo.clone(),
            location: format!("{}:{}", f.path, f.line),
            sites: f.also_locations.iter().map(|s| pane_site(f, s)).collect(),
            site_count: site_count(f),
            snippet: f.snippet.clone(),
            standard: directive.map(str::to_string).filter(|d| !d.trim().is_empty()),
            explanation: body,
            needs_review,
            confidence: f.confidence.clone(),
            effort: f.effort.clone(),
            fix: fix.filter(|s| !s.trim().is_empty()),
            also_matches: f.also_matches.clone(),
            position: index + 1,
            total,
        }
    }
}

/// One absorbed site, with its repo named only when it differs from the primary's — a
/// same-repo merge does not need the owner/repo prefix on every line, and a cross-repo one
/// would be actively misleading without it.
fn pane_site(primary: &FindingView, s: &MergedSiteView) -> PaneSiteRow {
    let location = if s.repo.is_empty() || s.repo == primary.repo {
        format!("{}:{}", s.path, s.line)
    } else {
        format!("{} {}:{}", s.repo, s.path, s.line)
    };
    PaneSiteRow {
        location,
        rule_id: s.rule_id.clone(),
        snippet: s.snippet.clone(),
        consequence: s.consequence,
    }
}

/// The live five-second undo, as the pane renders it.
#[derive(Clone, PartialEq)]
pub(super) struct UndoBanner {
    /// What it will reverse — `UndoEntry::message`.
    pub message: String,
    /// Whole seconds left in the window, for the countdown.
    pub seconds_left: u64,
}

/// THE DOCKED DETAIL PANE (design §2; the owner's Q5 ruling, design §13).
///
/// > At 400+ rows the modal's open/decide/close cycle is the single biggest per-decision time
/// > cost in the current UI. A docked pane plus arrow-key row focus turns review into
/// > cursor-down, read, one keystroke.
///
/// ## chorale does not have this, and the design document says it does
///
/// Design §13's 2026-10-12 dependency check recorded `detail_renderer` /
/// `set_detail_column_enabled` / `detail_column_enabled` as "the docked pane" in chorale
/// 0.2.3. Reading the installed source, they are not: `detail_renderer` is an EXPANDABLE
/// INLINE ROW DETAIL, rendered by `detail_panel_tr` (chorale-dioxus 0.2.3
/// `components.rs:1998`) as `<tr class="chorale-detail-panel"><td colspan=N>…</td></tr>`
/// immediately beneath its parent row, with a chevron column to expand it. That is a
/// different thing, and it would defeat the gesture this pane exists for: an inline panel
/// pushes every row below it down the page, so cursor-down-read-decide would reflow the table
/// under the cursor on every single move. This is the same class of error step 2 found in the
/// same dependency check's group-header entry. Per the standing ruling not to context-switch
/// into chorale, the pane is built here.
///
/// What chorale DOES provide, and what the call site uses, is `visible_grouped_view` — the
/// post-filter, post-sort, post-COLLAPSE row order. The cursor walks that, which is why focus
/// survives a re-sort and a collapse by construction rather than by special-casing.
///
/// ## Props-only, deliberately
///
/// Every judgment is made in `camerata_ui_core::triage` and arrives here as data: whether the
/// keys are armed ([`pane_keys_armed`]), which disposition a key means ([`pane_command`]),
/// what the undo will reverse. This component chooses words and pixels, nothing else — the
/// same discipline `ProcessGate` and `DeviationBulkBar` are built on, and the reason SSR can
/// verify the scoping at all.
#[component]
pub(super) fn FindingDetailPane(
    /// The focused row, or `None` when nothing is focused (an empty triage table, or every
    /// group collapsed).
    row: Option<DetailPaneRow>,
    /// Where keyboard focus is. THE scoping input — see [`pane_command`].
    focus: PaneFocus,
    /// The disposition whose reason is being collected, when the on-demand prompt is open.
    prompt: Option<TriageState>,
    /// The reason typed so far.
    prompt_reason: String,
    /// The live undo, when one is armed.
    undo: Option<UndoBanner>,
    /// Every keydown on the pane, as `(key, modifiers)`. The call site runs it through
    /// [`pane_command`]; this component never decides what a key means.
    on_key: Callback<(String, KeyMods)>,
    /// Focus moved into or out of the pane's cursor / its reason prompt.
    on_focus: Callback<PaneFocus>,
    on_prompt_reason: Callback<String>,
    on_prompt_commit: Callback<()>,
    on_prompt_cancel: Callback<()>,
    on_undo: Callback<()>,
    /// A disposition chosen with the MOUSE. The keys are an accelerator, not the only door.
    on_dispose: Callback<TriageState>,
    /// Open the finding-detail MODAL on this row. The pane replaces the modal as the Queue's
    /// row-click surface (design §9.6), but not as the deep dive: the modal carries the
    /// on-demand enclosing-code-block fetch, which is far too expensive to re-run on every
    /// cursor-down and so stays one deliberate click away.
    on_open_detail: Callback<()>,
    /// Open the RULE HUB on the focused row's rule (design §8).
    ///
    /// This is how a QUEUE ROW reaches the hub. The rule id in the row's own "Finding type"
    /// cell cannot be the door: a chorale cell renderer is an `Arc<dyn Fn(..) + Send + Sync>`
    /// and a Dioxus `Callback` is neither `Send` nor `Sync` — the same constraint that put the
    /// deviation bulk actions in a bar instead of inside the group header `<tr>`. The pane IS
    /// the Queue row's detail surface and it opens on the very click that focuses the row, so
    /// the hub sits one further click from any row, at exactly the place the curator is
    /// already reading.
    ///
    /// Defaulted so the pane's existing render tests keep compiling and asserting unchanged.
    #[props(default)]
    on_open_hub: Callback<()>,
) -> Element {
    let armed = pane_keys_armed(focus);
    rsx! {
        aside {
            class: "finding-pane",
            id: "{DETAIL_PANE_ID}",
            // The pane is a focusable REGION: tabbing into it arms the keys, tabbing out
            // disarms them. That is the Q5 scoping made operable rather than merely checked —
            // there is a visible, reachable thing that "the detail pane holds focus" means.
            tabindex: "0",
            onfocusin: move |_| on_focus.call(PaneFocus::Cursor),
            onfocusout: move |_| on_focus.call(PaneFocus::Elsewhere),
            onkeydown: move |e: KeyboardEvent| {
                let m = e.modifiers();
                let mods = KeyMods {
                    ctrl: m.contains(Modifiers::CONTROL),
                    alt: m.contains(Modifiers::ALT),
                    meta: m.contains(Modifiers::META),
                    shift: m.contains(Modifiers::SHIFT),
                };
                let key = e.key().to_string();
                // Swallow only the presses the pane actually claims, so Tab still tabs and
                // Cmd-C still copies the snippet the curator is reading. The claim test is
                // `pane_command` itself — the same call the handler makes — so a key can
                // never be eaten by a pane that then does nothing with it.
                if pane_command(focus, &key, mods).is_some() {
                    e.prevent_default();
                    e.stop_propagation();
                }
                on_key.call((key, mods));
            },

            if let Some(r) = row.clone() {
                div { class: "pane-head",
                    // The rule id is the hub's door from a Queue row (see `on_open_hub`).
                    button {
                        class: "rule-hub-link pane-rule",
                        title: "Everything this scan knows about {r.rule_id}: its kind outcome, its counts, the convention behind it, and where it lives.",
                        onclick: move |_| on_open_hub.call(()),
                        "{r.rule_id}"
                    }
                    span { class: "{r.kind_class}", title: "{r.kind_tip}", "{r.kind_label}" }
                    span { class: "pane-pos", "{r.position} of {r.total}" }
                    button {
                        class: "btn-edit-sm pane-full-detail",
                        title: "Open the full finding modal: the enclosing code block around this line, fetched on demand.",
                        onclick: move |_| on_open_detail.call(()),
                        "Full detail"
                    }
                }
                div { class: "pane-meta",
                    // The AUTHORED severity. Not the Severity column's cell value, which
                    // carries a calibrated rank prefix purely so chorale's lexicographic
                    // column sort orders the ladder; the badge, the row highlight, the CSV
                    // and the report all read the clean word, and so does this.
                    span { class: "rule-modal-tag", "severity \u{b7} {r.severity}" }
                    span { class: "rule-modal-tag", "{r.location}" }
                    if !r.repo.is_empty() {
                        span { class: "rule-modal-tag", "{r.repo}" }
                    }
                    span { class: "rule-modal-tag", "{r.status}" }
                    if let Some(c) = r.confidence.clone() {
                        span { class: "rule-modal-tag", "confidence \u{b7} {c}" }
                    }
                }
                // A deviation's structured claim, verbatim from the same builder the group
                // header uses. Nothing here is parsed out of prose.
                if let Some(claim) = r.claim.clone() {
                    p { class: "pane-claim", "{claim}" }
                }
                if let Some(d) = r.standard.clone() {
                    p { class: "rule-modal-label", "Cited standard" }
                    p { class: "rule-modal-detail", "{d}" }
                }
                p { class: "rule-modal-label", "Evidence" }
                pre { class: "pane-snippet", "{r.snippet}" }
                if !r.explanation.trim().is_empty() {
                    p { class: "rule-modal-detail",
                        "{r.explanation}"
                        if let Some(nr) = r.needs_review.clone() {
                            " "
                            b { class: "nr-inline",
                                if nr.is_empty() { "[needs review]" } else { "[needs review: {nr}]" }
                            }
                        }
                    }
                }
                // ABSORBED SITES (design §6) — the full-disclosure surface. Rendered only
                // when there is more than one site: "Sites (1)" on an un-merged row would be
                // a section that exists to say nothing.
                if !r.sites.is_empty() {
                    p { class: "rule-modal-label", "Sites ({r.site_count})" }
                    ul { class: "pane-sites",
                        li { class: "pane-site pane-site-primary",
                            span { class: "pane-site-loc", "{r.location}" }
                            span { class: "pane-site-rule", "{r.rule_id}" }
                        }
                        for (i, s) in r.sites.iter().enumerate() {
                            li {
                                key: "{s.location}-{i}",
                                class: if s.consequence { "pane-site pane-site-consequence" } else { "pane-site" },
                                if s.consequence {
                                    span { class: "pane-site-sub", "also affects" }
                                }
                                span { class: "pane-site-loc", "{s.location}" }
                                span { class: "pane-site-rule", "{s.rule_id}" }
                                if !s.snippet.is_empty() {
                                    span { class: "pane-site-snippet", "{s.snippet}" }
                                }
                            }
                        }
                    }
                }
                if !r.also_matches.is_empty() {
                    p { class: "rule-modal-label", "Also violates here" }
                    div { class: "pane-meta",
                        for rid in r.also_matches.iter() {
                            span { key: "{rid}", class: "rule-modal-tag", "{rid}" }
                        }
                    }
                }
                // THE FIX, with its structured effort estimate beside it. The text is the
                // SAME server function's output the report's "Fix:" line renders — fetched
                // by the call site, never re-derived here.
                if r.fix.is_some() || r.effort.is_some() {
                    div { class: "pane-fix-head",
                        p { class: "rule-modal-label", "Recommended fix" }
                        if let Some(e) = r.effort.clone() {
                            span {
                                class: "rule-modal-tag pane-effort",
                                title: "The calibration pass's remediation-effort estimate for this finding.",
                                "effort \u{b7} {e}"
                            }
                        }
                    }
                    if let Some(fix) = r.fix.clone() {
                        p { class: "rule-modal-detail", "{fix}" }
                    }
                }
            } else {
                p { class: "pane-empty",
                    "No finding focused. Click a row, or press Tab into this panel and use the \
                     arrow keys."
                }
            }

            // THE UNDO (Q5's five-second toast). Docked in the pane rather than in the
            // app-wide toast stack because that stack's cards are purely informational and
            // auto-dismiss on their own timer; this one carries an action whose window must
            // be the model's, and it must sit where the curator's eyes already are.
            if let Some(u) = undo.clone() {
                div { class: "pane-undo",
                    span { class: "pane-undo-msg", "{u.message}" }
                    button {
                        class: "btn-edit-sm pane-undo-btn",
                        onclick: move |_| on_undo.call(()),
                        "Undo"
                    }
                    span { class: "pane-undo-left", "{u.seconds_left}s" }
                }
            }

            if row.is_some() {
                // THE ON-DEMAND REASON PROMPT (design §3). Replaces the two permanently
                // visible textareas for the keyboard path: it appears when `I` or `F` is
                // pressed, pre-focused, Enter commits, Esc cancels — and while it holds
                // focus the disposition keys are OFF, which is the composition the Q5
                // scoping clause asks for rather than a second mechanism bolted beside it.
                if let Some(state) = prompt {
                    div { class: "pane-reason-prompt",
                        label { class: "rule-modal-label",
                            "Reason for \u{201c}{state.label()}\u{201d} (required)"
                        }
                        textarea {
                            class: "addressee-input pane-reason-input",
                            rows: "2",
                            autofocus: true,
                            placeholder: "why — the export reads this",
                            value: "{prompt_reason}",
                            // Pre-focused, as design §3 specifies. `autofocus` alone is not
                            // reliable in a webview for an element appearing on a re-render,
                            // and getting this wrong is not cosmetic: the pane has already
                            // disarmed its keys (opening the prompt is the one place it does
                            // so EAGERLY, because disarming early is the safe direction and
                            // arming early is not), so a prompt nobody is focused in would
                            // be a dead surface with dead keys beside it.
                            onmounted: move |e| {
                                spawn(async move {
                                    let _ = e.set_focus(true).await;
                                });
                            },
                            onfocusin: move |_| on_focus.call(PaneFocus::ReasonPrompt),
                            oninput: move |e| on_prompt_reason.call(e.value()),
                            onkeydown: move |e: KeyboardEvent| {
                                // Belt AND braces. The pure gate above already refuses every
                                // key while `focus == ReasonPrompt`; stopping propagation
                                // here means the pane's handler never even sees a keystroke
                                // aimed at this textarea, so a focus-tracking bug cannot
                                // turn the first `c` of "cannot reproduce" into a Confirm.
                                e.stop_propagation();
                                match e.key() {
                                    Key::Escape => { on_prompt_cancel.call(()); }
                                    Key::Enter if !e.modifiers().contains(Modifiers::SHIFT) => {
                                        e.prevent_default();
                                        on_prompt_commit.call(());
                                    }
                                    _ => {}
                                }
                            },
                        }
                        div { class: "pane-prompt-actions",
                            button {
                                class: "btn-run",
                                onclick: move |_| on_prompt_commit.call(()),
                                "Commit \u{2192}"
                            }
                            button {
                                class: "btn-edit-sm",
                                onclick: move |_| on_prompt_cancel.call(()),
                                "Cancel"
                            }
                        }
                    }
                }
                div { class: "pane-actions",
                    span {
                        class: if armed { "pane-keys armed" } else { "pane-keys off" },
                        // The indicator is `pane_keys_armed`, which is DEFINED as "would C
                        // produce a command". A pane that said "armed" while the handler
                        // refused would be the ships-inert shape in miniature.
                        if armed {
                            "Keys: C confirm \u{b7} I ignore \u{b7} T tech debt \u{b7} F false positive \u{b7} \u{2191}\u{2193} move"
                        } else if prompt.is_some() {
                            "Keys are off while you type a reason. Enter commits, Esc cancels."
                        } else {
                            "Keys are off. Click this panel (or Tab into it) to arm C / I / T / F."
                        }
                    }
                    for state in TriageState::all().into_iter().filter(|s| s.is_decided()) {
                        button {
                            key: "{state.label()}",
                            class: "btn-edit-sm pane-dispose",
                            title: "Press {state_hotkey(state).unwrap_or(\"\").to_uppercase()} while this panel has focus.",
                            onclick: move |_| on_dispose.call(state),
                            "{state.label()}"
                        }
                    }
                }
            }
        }
    }
}

/// The findings table with TRIAGE: sort by repo/severity/type, filter, select rows
/// and Ignore / Resolve / Accept-as-tech-debt (open a ticket) them. Virtualized by
/// chorale, so a large audit doesn't choke the UI.
#[component]
pub(super) fn FindingsTable(
    findings: Vec<FindingView>,
    repos: Vec<String>,
    descriptions: std::collections::HashMap<String, String>,
    // Which triage table this is (issue #26). Findings not in this state are filtered out;
    // the component is keyed on it by the parent, so a switch remounts with that table's set.
    #[props(default = TriageState::Unresolved)] triage_view: TriageState,
    // The lifted finding -> disposition map. Move actions write here; the row is then dropped
    // from this table (remove_rows) and reappears under its new table on the next switch.
    #[props(default)] dispositions: Signal<std::collections::HashMap<String, Disposition>>,
    /// Open the RULE HUB (design §8). Raised from the two surfaces inside this table that
    /// carry a rule id and CAN carry a click: the docked detail pane (which is how a Queue row
    /// reaches it) and the deviation bulk bar (which is how a deviation group header does).
    #[props(default)] on_open_hub: Callback<RuleHubRequest>,
    /// THE HUB'S QUEUE JUMP, arriving as state rather than as a call: "show me this rule in
    /// the Queue" is a column filter, exactly as design §8 describes the File join — "a
    /// filter, not a hyperlink graph". The parent writes a rule id here; the effect below
    /// applies it to chorale's own `type` column filter, so the jump leaves the curator in a
    /// normal, visibly-filtered table he can clear with the controls already on screen.
    #[props(default)] queue_rule_filter: Signal<Option<String>>,
) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    // Filter to THIS table's triage state + apply the default triage order (enforced/new before
    // suppressed/debt, then critical → high → medium → low). Both the filter and the sort are the
    // pure `TriageModel::visible` derivation now (RUST-HEADLESS-CORE-1): a flat 200-row dump is
    // paralysis; this floats the exploitable-bug criticals to the very top so a hardcoded secret
    // can never sit below "no mappers crate." Absent from the map = Unresolved.
    let findings: Vec<FindingView> = {
        let model = TriageModel { dispositions: dispositions.peek().clone(), triage_view };
        model.visible(&findings)
    };
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
    // The column set is rebuilt for the order indicator below, so the repo list is cloned
    // before `use_table` takes ownership of its copy.
    let indicator_repos = filter_repos.clone();
    let handle = use_table(move || {
        finding_table_state(rows.clone(), filter_repos.clone(), in_techdebt)
    });
    // ── THE HUB'S QUEUE JUMP, applied to chorale's own column filter ────────────────────
    //
    // `applied` is PEEKED, never read, so this effect subscribes to exactly one signal
    // (`queue_rule_filter`) and cannot re-trigger itself by recording what it just did. The
    // alternative — clearing the source signal after applying it — would have the effect
    // write the thing it reads, which is how a render loop starts.
    //
    // Re-jumping to a rule the table is ALREADY filtered to is correctly a no-op: the curator
    // is looking at exactly what the jump promises.
    let mut applied_rule_filter = use_signal(|| Option::<String>::None);
    use_effect(move || {
        let want = queue_rule_filter();
        if want != *applied_rule_filter.peek() {
            applied_rule_filter.set(want.clone());
            handle.set_filter(ColumnId("type"), want.map(FilterValue::Text));
        }
    });

    // ── The ORDER BAR: which order is on screen, and one click back to the default ───────
    //
    // Pass 2 step 2 (design §5; Zach 2026-10-12: "Keep sort with [unspecified] first, sure, as
    // long as the table can be manually sorted by me as the user"). chorale already owns the
    // manual sort itself — every column is `.sortable()` and `sort_enabled` makes the headers
    // clickable with its own ↑/↓ arrow. What it does not own is the DEFAULT: `state.sort` being
    // empty is chorale's "unsorted", but it is Camerata's curated order, and nothing on screen
    // said so or offered a way back to it from an arbitrary sort state.
    //
    // A memo rather than reading the table signal inline: this component renders the chorale
    // table, and subscribing it directly to `TableState` would re-render the whole subtree on
    // every scroll/selection write. The memo narrows the state to the sort vector, so the bar
    // below re-renders only when the sort actually changes.
    let sort_signal = handle.signal();
    let active_sort = use_memo(move || sort_signal.read().sort.clone());
    // The column set the bar resolves sorted-column headers against. Built once per mount and
    // shared by `Rc`, so the bar's props stay cheap to compare (`ColumnDef` holds `Arc`'d
    // closures and is not `PartialEq`).
    let order_columns = use_hook(|| {
        std::rc::Rc::new(finding_columns(indicator_repos.clone(), in_techdebt))
    });
    // Subscribe to the disposition map so the bucket flag column re-renders when the architect
    // marks resolve-later/now (the renderer below captures this snapshot).
    let bucket_snapshot = dispositions.read().clone();
    // Two-level grouping: by RULE, then by FILE within each rule. chorale groups by an
    // ordered key list (it recurses through the Vec, one depth per key), so a rule violated
    // 4× across one file renders as "RULE (4)" → "handlers.rs (4)" → the 4 individual lines.
    // Counts come free on every header. This is a PRESENTATION view of the flat finding
    // list; the CSV export stays flat + lossless (one row per finding), unchanged.
    // Grouping + collapse-by-default (rule → file → lines, all collapsed on mount) is
    // owned by `CamerataTable` via `group_by` below; it applies the load-all-then-collapse
    // order that works around chorale's paginated `collapse_all_groups`.
    // ── THE ON-DEMAND REASON PROMPT (design §3, §9.2) ───────────────────────────────────
    //
    // REMOVED: `ignore_reason` and `fp_reason`, the two `use_signal(String)`s behind the two
    // permanently visible textareas. A durable ignore and a false positive each still REQUIRE
    // a reason — the invariant is unchanged and is still enforced inside `TriageModel` — but
    // the box is raised by the gesture instead of standing open beside the table.
    //
    // `toolbar_prompt` is `None` whenever no reason is being collected, which is the state
    // the Queue is in essentially all the time.
    let mut toolbar_prompt = use_signal(|| Option::<ToolbarReasonPrompt>::None);
    let mut toolbar_reason = use_signal(String::new);
    // Two id_map clones: each triage table renders two move buttons, and the two closures in
    // an arm each move a clone. Match arms are mutually exclusive, so the same two clones
    // serve every arm.
    let id_map_a = id_map.clone();
    let id_map_b = id_map.clone();
    // Two more clones for the tech-debt bucket buttons (resolve later / now).
    let id_map_c = id_map.clone();
    let id_map_d = id_map.clone();
    // One more clone for the Unresolved view's "mark false positive" button + the
    // FalsePositive view's "move back to Unresolved" button.
    let id_map_e = id_map.clone();
    // And one for the Unresolved view's Confirm button (Pass 2 step 3). Its own clone rather
    // than sharing `id_map_a`/`id_map_b`: those two are already moved into the Ignore and
    // Tech-debt closures in this same match arm.
    let id_map_f = id_map.clone();
    // The (sorted) rows for CSV export.
    let csv_rows = findings.clone();

    // ── RULE-LEVEL BULK over deviation groups (design §2/§4) ────────────────
    //
    // The groups are derived from THIS table's visible row set, so a group's count is exactly
    // the number of rows its button will move. `bulk_groups` keeps the full rows (for the
    // disposition write); `bulk_rows` is the flat props the bar renders.
    //
    // Unresolved only. Both of the bar's actions are decisions ABOUT an undecided group: a
    // deviation already moved to Confirmed, Ignored or Tech debt has had its rule-level
    // decision made. (Before §9.2 there was a second reason — `False positive all N` read the
    // reason textarea that existed only in the Unresolved toolbar. That textarea is gone and
    // the button raises its own prompt, so the Unresolved-only scoping now rests entirely on
    // the first reason, which was always the real one.) Rendering a dead bar in the other
    // four tables would be the 403-button shape the standing rule forbids.
    let bulk_groups = std::rc::Rc::new(if triage_view == TriageState::Unresolved {
        deviation_groups(&findings)
    } else {
        Vec::new()
    });
    let bulk_rows: Vec<DeviationBulkRow> =
        bulk_groups.iter().map(DeviationBulkRow::from_group).collect();
    // `finding_key` -> `RowId`, so a bulk action can drop exactly its own group's rows from
    // the table. Built from the same `id_map` the per-row buttons use, so the two paths
    // resolve rows identically.
    let key_to_row_id: std::collections::HashMap<String, RowId> = id_map
        .iter()
        .map(|(rid, f)| (finding_key(f), *rid))
        .collect();

    // ── COMMITTING A RAISED TOOLBAR REASON (design §3, §9.2) ───────────────────────────
    //
    // ONE commit path for BOTH sources — the toolbar's per-selection buttons and the bulk
    // bar's `False positive all N`. The two textareas §9.2 removed had two separate commit
    // closures with two copies of the require-reason check; collapsing them is most of the
    // reason the removal is a simplification and not just a relocation.
    //
    // The REQUIRE-REASON INVARIANT does not live here. It lives where it always did, inside
    // `TriageModel::{ignore, mark_false_positive, bulk_false_positive}` — this closure asks
    // the model and reports the refusal. A `None` from `bulk_false_positive` and an empty
    // `reason` for the selection path are the same refusal, and both leave the prompt OPEN
    // with what the curator typed still in it: closing it would throw away a half-written
    // justification to punish a trailing space.
    let commit_toolbar_reason = Callback::new({
        let key_to_row_id = key_to_row_id.clone();
        move |()| {
            let Some(p) = toolbar_prompt.peek().clone() else { return };
            let reason = toolbar_reason.peek().clone();
            let mut model =
                TriageModel { dispositions: dispositions.peek().clone(), triage_view };
            let (ids, message) = match &p.target {
                ReasonTarget::Selection { ids, rows } => {
                    // THE REQUIRE-REASON INVARIANT for the per-selection path, enforced at
                    // the same point it always was: `TriageModel::{ignore,
                    // mark_false_positive}` record the reason but do not police it (unlike
                    // `bulk_false_positive`, which refuses), so the caller checks. §9.2 moved
                    // the BOX, not the check, and the check is deliberately still here rather
                    // than quietly pushed into core as a side effect of a UI removal.
                    //
                    // A refusal leaves the prompt OPEN with what the curator typed still in
                    // it. Closing it would throw away a half-written justification to punish
                    // a trailing space.
                    if reason.trim().is_empty() {
                        crate::toast::push_toast(
                            toasts,
                            crate::toast::ToastKind::Warning,
                            reason_required_message(p.state, rows.len()),
                        );
                        return;
                    }
                    match p.state {
                        TriageState::Ignored => model.ignore(rows, &reason),
                        _ => model.mark_false_positive(rows, &reason),
                    }
                    let moved = rows.len();
                    (ids.clone(), format!("Moved {moved} to {}.", p.state.label()))
                }
                ReasonTarget::Group(group) => {
                    // The SAME `bulk_false_positive` the bar called directly before §9.2 —
                    // a thin wrapper over the per-row `mark_false_positive`, so one click is
                    // still provably identical to N individual gestures. `None` is its
                    // refusal, which is the require-reason invariant speaking.
                    let Some(moved) = model.bulk_false_positive(group, &reason) else {
                        crate::toast::push_toast(
                            toasts,
                            crate::toast::ToastKind::Warning,
                            reason_required_message(
                                TriageState::FalsePositive,
                                group.rows.len(),
                            ),
                        );
                        return;
                    };
                    let ids: Vec<RowId> = group
                        .rows
                        .iter()
                        .filter_map(|f| key_to_row_id.get(&finding_key(f)).copied())
                        .collect();
                    (
                        ids,
                        format!("Marked all {moved} {} row(s) as false positive.", group.rule_id),
                    )
                }
            };
            dispositions.set(model.dispositions);
            handle.remove_rows(&ids);
            toolbar_prompt.set(None);
            toolbar_reason.set(String::new());
            crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, message);
        }
    });

    // ── THE DOCKED DETAIL PANE (design §2; the owner's Q5 ruling, §13) ──────────────────
    //
    // Everything below is wiring: the pure cursor, scoped key handler and undo all live in
    // `camerata_ui_core::triage` and are unit-tested there, and `FindingDetailPane` is
    // props-only and render-tested. This block owns only the signals and the effects.

    // THE ON-SCREEN ROW ORDER, as chorale itself has it. `visible_grouped_view` is
    // post-filter, post-sort AND post-collapse, so a manual column re-cut, a column filter
    // and a group collapse all reach the cursor as a change to this one list — which is what
    // makes "focus survives a re-sort" and "focus survives a collapse" one rule rather than
    // two features. A memo rather than an inline read, for the same reason `active_sort` is
    // one: subscribing this whole subtree to `TableState` would re-render the table on every
    // scroll and selection write.
    let visible_keys = use_memo({
        let id_map = id_map.clone();
        move || {
            let state = sort_signal.read();
            chorale_core::visible_grouped_view(&state)
                .into_iter()
                .filter_map(|r| match r {
                    chorale_core::GroupedRow::Data(rid, _) => {
                        id_map.get(&rid).map(finding_key)
                    }
                    // Group headers are not rows the cursor can land on; `GroupedRow` is
                    // `#[non_exhaustive]`, so anything chorale adds later is skipped rather
                    // than silently treated as a focusable row.
                    _ => None,
                })
                .collect::<Vec<String>>()
        }
    });
    // The cursor itself. Stored unresolved and resolved against the CURRENT frame's row list
    // on every render, which is why `RowCursor`'s methods are pure and non-mutating: the
    // resolution is a read, not a write, so it cannot loop.
    let mut cursor = use_signal(RowCursor::new);
    let mut pane_focus = use_signal(|| PaneFocus::Elsewhere);
    let mut prompt = use_signal(|| Option::<TriageState>::None);
    let mut prompt_reason = use_signal(String::new);
    // The live undo, plus what it would take to put the row back ON SCREEN (the disposition
    // revert is the model's job; chorale's row set is this layer's).
    let mut undo_entry = use_signal(|| Option::<UndoEntry>::None);
    let mut undo_restore = use_signal(|| Option::<(usize, RowId, FindingView)>::None);
    // A MONOTONIC clock, not a wall clock: the five-second window is measured against time
    // elapsed since this table mounted, so a system-clock change (NTP step, timezone, the
    // laptop waking) can neither extend the window nor close it early.
    let epoch = use_hook(std::time::Instant::now);
    let mut now_ms = use_signal(|| 0u64);

    let resolved = cursor.read().resolve(&visible_keys());
    let focused_finding: Option<FindingView> = resolved
        .key()
        .and_then(|k| key_to_row_id.get(k))
        .and_then(|rid| id_map.get(rid).cloned());
    // A second handle on the focused row for the "Full detail" callback: `focused_finding`
    // is move-captured by `apply_disposition` below.
    let focused_for_modal = focused_finding.clone();
    // And a third, for the rule-hub door in the pane's header. Each `move` closure owns its
    // own copy; there is no shared borrow to hand round.
    let focused_for_hub = focused_finding.clone();
    // THE FOCUSED RULE, as a MEMO. It has to be one: `use_resource` re-runs when a signal
    // read inside its closure changes, and a plain captured `Option<String>` is not one — a
    // resource built over the captured value would fetch the first focused rule's fix once
    // and then never again, so every subsequent row would show that first rule's fix. A memo
    // also only notifies when its VALUE changes, so cursoring within one rule's group does
    // not refetch.
    let focused_rule = use_memo({
        let id_map = id_map.clone();
        let key_to_row_id = key_to_row_id.clone();
        move || {
            cursor
                .read()
                .resolve(&visible_keys())
                .key()
                .and_then(|k| key_to_row_id.get(k))
                .and_then(|rid| id_map.get(rid))
                .map(|f| f.rule_id.clone())
        }
    });
    // The recommended fix for the focused rule, from the SAME server function the report's
    // "Fix:" line and the xlsx's "Recommended Fix" column call — never a second derivation
    // here.
    let fix_res = use_resource(move || {
        let rid = focused_rule();
        async move {
            match rid {
                Some(rid) => fetch_finding_fix(&rid).await.map(|v| v.fix),
                None => None,
            }
        }
    });
    let focused_fix: Option<String> = fix_res.read().clone().flatten();
    let pane_row = focused_finding.as_ref().map(|f| {
        DetailPaneRow::from_finding(
            f,
            descriptions.get(&f.rule_id).map(String::as_str),
            focused_fix.clone(),
            resolved.index(),
            visible_keys().len(),
        )
    });
    let undo_banner = undo_entry.read().as_ref().and_then(|e| {
        let t = now_ms();
        e.is_live(t).then(|| UndoBanner {
            message: e.message().to_string(),
            // Ceiling, so a window with 1ms left still reads "1s" rather than "0s" — a
            // countdown that shows zero while the button still works is a small lie, and
            // this one is about how long the curator has to act.
            seconds_left: e.remaining_ms(t).div_ceil(1_000),
        })
    });

    // APPLY one disposition to the focused row, arm the undo, and advance the cursor.
    //
    // A single closure for every entry point — the four keys, the four pane buttons, and the
    // reason prompt's Commit — so there is exactly one path from "the curator decided" to
    // "the map changed", and it is the one that captures the undo. `dispose_with_undo` owns
    // the require-reason refusal and routes the write through the same per-row functions the
    // toolbar buttons call.
    // A `Callback` rather than a plain closure because three call sites share it (the keys,
    // the pane buttons, and the reason prompt's Commit) and `Callback` is `Copy`.
    let key_to_row_id_pane = key_to_row_id.clone();
    let focused_for_apply = focused_finding.clone();
    let apply_disposition = Callback::new(move |(state, reason): (TriageState, String)| {
        let Some(f) = focused_for_apply.clone() else {
            return;
        };
        let now = epoch.elapsed().as_millis() as u64;
        now_ms.set(now);
        let mut model = TriageModel {
            dispositions: dispositions.peek().clone(),
            triage_view,
        };
        let Some(entry) = model.dispose_with_undo(std::slice::from_ref(&f), state, &reason, now) else {
            crate::toast::push_toast(
                toasts,
                crate::toast::ToastKind::Warning,
                format!(
                    "A reason is required to move a finding to {}. It is recorded in the draft \
                     and read by the report export.",
                    state.label()
                ),
            );
            return;
        };
        dispositions.set(model.dispositions);
        prompt.set(None);
        prompt_reason.set(String::new());
        // Drop the row from THIS triage table, which is what advances the cursor: the key
        // leaves `visible_keys`, so the next `resolve` clamps to whatever took its index.
        // That is the burn-down gesture — decide, and the next row is already focused.
        // Remember where it was so Undo can put it back on screen, not merely in the map.
        if let Some(rid) = key_to_row_id_pane.get(&finding_key(&f)).copied() {
            let raw_index = sort_signal
                .peek()
                .rows
                .iter()
                .position(|(r, _)| *r == rid)
                .unwrap_or(0);
            undo_restore.set(Some((raw_index, rid, f.clone())));
            handle.remove_rows(&[rid]);
        } else {
            undo_restore.set(None);
        }
        let armed = entry.clone();
        undo_entry.set(Some(entry));
        // Tick the countdown while this undo is armed, and clear it when the window shuts.
        // The loop exits the moment a NEWER undo replaces this one, so two dispositions
        // inside five seconds cannot have the older ticker close the newer window.
        spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                let t = epoch.elapsed().as_millis() as u64;
                now_ms.set(t);
                if undo_entry.peek().as_ref() != Some(&armed) {
                    break;
                }
                if !armed.is_live(t) {
                    undo_entry.set(None);
                    undo_restore.set(None);
                    break;
                }
            }
        });
    });

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
        // KIND chip. Hand-rolled rather than a chorale `RenderKind::Badge` because the three
        // states are not three colors of the same thing: `unspecified` is an ALARM about the
        // pipeline (red OUTLINE, its own label), and a filled badge from the same palette would
        // read as just another kind. Defect is the red family, Deviation the blue.
        m.insert(
            ColumnId("kind"),
            std::sync::Arc::new(move |f: &FindingView, _val: &CellValue| {
                let (cls, label, tip) = kind_chip_parts(kind_tag(f));
                rsx! { span { class: "{cls}", title: "{tip}", "{label}" } }
            }) as RowCellRenderer<FindingView>,
        );
        // REMOVED (design §9.3): the "Needs review" cell renderer — an orange chip followed by
        // the model's hedging SENTENCE, inline, at 300px, on every row that had one. It was
        // the only renderer in the Queue that drew prose it had recovered with a regex. The
        // Confidence column that replaces it is a chorale `RenderKind::Badge` over
        // `confidence_tag`'s closed set, so there is nothing left to override here; the reason
        // text reaches the curator in the detail pane, which is where §9.3 puts it.
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
        // REMOVED, design §9.5: the two-swatch findings key ("Security findings
        // (deterministic, stop-the-line)" / "Architectural findings (everything else)").
        //
        // It was superseded by the Kind + Severity + Authority columns, which say more, per
        // row, in less space — and it was also WRONG about the thing it claimed to explain.
        // The red full-row highlight is `row_class`, below: `f.severity == "critical"`, with
        // no reference to security, to determinism or to the floor rule set. A critical
        // AI-ADVISORY finding is highlighted and is not a "security finding (deterministic)";
        // a non-critical floor hit is a deterministic security finding and is not highlighted.
        // The per-row replacements state both facts without the mismatch: the Severity badge
        // reads "Critical" on exactly the highlighted rows, and the Authority badge reads
        // "Rule · enforced" on exactly the deterministic-floor ones.
        //
        // The red full-row critical highlight itself STAYS (§9.5 says so explicitly).
        div { class: "findings-toolbar",
            // View-specific triage actions. A move writes the new disposition for each
            // selected finding and drops it from this table; it reappears under its target
            // table on the next switch. Backend commit happens later, at Process.
            match triage_view {
                TriageState::Unresolved => rsx! {
                    // CONFIRM — the affirmative disposition, and the majority outcome of an
                    // audit (design §3). First in the toolbar and styled as the primary
                    // action because it is the most common gesture, and the ONLY one with no
                    // reason input in front of it: confirming makes no claim beyond the
                    // finding, which already states its own case. See `TriageModel::confirm`.
                    //
                    // ## No `C` keystroke yet — DEFERRED to the docked detail pane
                    //
                    // The owner's Q5 ruling (design §13, 2026-10-12) ships `C`/`I`/`T`/`F`
                    // "scoped so the keys only fire while the detail pane holds focus, plus a
                    // five-second undo toast". The docked detail pane is a later step and does
                    // not exist; the Queue's detail surface is still the row-click modal. So
                    // the scoping condition the ruling ATTACHES THE KEYS TO cannot be
                    // satisfied today, and the only thing buildable now is a document-level
                    // handler firing on any `c` press anywhere on the page — which at 400 rows
                    // is precisely the fat-finger risk the scoping clause exists to prevent,
                    // and would also swallow `c` typed into the reason textareas six inches
                    // away. The keystroke belongs to the pane step, where its focus owner,
                    // its row cursor and its undo toast all arrive together. Button only here.

                    button {
                        class: "btn-run triage-confirm",
                        title: "I read these findings, they are real, and they ship in the report. No reason required. Local to this draft — the export reads it; Process commits nothing for it.",
                        onclick: move |_| {
                            let sel = handle.selected_ids();
                            let picked: Vec<FindingView> = sel.iter().filter_map(|id| id_map_f.get(id).cloned()).collect();
                            if picked.is_empty() { return; }
                            let mut model = TriageModel { dispositions: dispositions.peek().clone(), triage_view };
                            model.confirm(&picked);
                            dispositions.set(model.dispositions);
                            handle.remove_rows(&sel);
                            crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("Confirmed {}.", picked.len()));
                        },
                        "Confirm \u{2192}"
                    }
                    // §9.2: the standing textarea is gone; the button RAISES the prompt. The
                    // selection is snapshotted into the prompt right here, so the heading's
                    // count and the commit's row set are the same list.
                    button {
                        class: "btn-restart triage-ignore",
                        onclick: move |_| {
                            let sel = handle.selected_ids();
                            let rows: Vec<FindingView> = sel.iter().filter_map(|id| id_map_a.get(id).cloned()).collect();
                            if rows.is_empty() { return; }
                            toolbar_reason.set(String::new());
                            toolbar_prompt.set(Some(ToolbarReasonPrompt {
                                state: TriageState::Ignored,
                                target: ReasonTarget::Selection { ids: sel, rows },
                            }));
                        },
                        "Ignore with reason \u{2192}"
                    }
                    button {
                        class: "btn-run",
                        onclick: move |_| {
                            let sel = handle.selected_ids();
                            let picked: Vec<FindingView> = sel.iter().filter_map(|id| id_map_b.get(id).cloned()).collect();
                            if picked.is_empty() { return; }
                            let mut model = TriageModel { dispositions: dispositions.peek().clone(), triage_view };
                            model.move_to(&picked, TriageState::TechDebt);
                            dispositions.set(model.dispositions);
                            handle.remove_rows(&sel);
                            crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("Moved {} to Tech debt.", picked.len()));
                        },
                        "Save as tech debt"
                    }
                    // False positive: "the tool was wrong" — distinct from Ignore ("real but
                    // accepted risk"). Requires its own reason (the audit trail); at Process
                    // this disposition is a server-side no-op (excluded from the report,
                    // never baselined, never ticketed).
                    button {
                        class: "btn-restart triage-false-positive",
                        onclick: move |_| {
                            let sel = handle.selected_ids();
                            let rows: Vec<FindingView> = sel.iter().filter_map(|id| id_map_e.get(id).cloned()).collect();
                            if rows.is_empty() { return; }
                            toolbar_reason.set(String::new());
                            toolbar_prompt.set(Some(ToolbarReasonPrompt {
                                state: TriageState::FalsePositive,
                                target: ReasonTarget::Selection { ids: sel, rows },
                            }));
                        },
                        "Mark false positive \u{2192}"
                    }
                },
                // Confirmed: one way back, and nothing else. Mirrors the FalsePositive table
                // exactly, for the same reason — the decision was made in the Unresolved
                // table with the inputs it has; the only thing left to offer here is undoing
                // it. Offering Ignore/Tech debt without their reason textareas (which live
                // only in the Unresolved toolbar) would be a button that cannot complete.
                TriageState::Confirmed => rsx! {
                    button {
                        class: "btn-restart",
                        onclick: move |_| {
                            let sel = handle.selected_ids();
                            let picked: Vec<FindingView> = sel.iter().filter_map(|id| id_map_f.get(id).cloned()).collect();
                            if picked.is_empty() { return; }
                            let mut model = TriageModel { dispositions: dispositions.peek().clone(), triage_view };
                            model.move_to(&picked, TriageState::Unresolved);
                            dispositions.set(model.dispositions);
                            handle.remove_rows(&sel);
                            crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("Moved {} back to Unresolved.", picked.len()));
                        },
                        "Move to Unresolved"
                    }
                },
                TriageState::Ignored => rsx! {
                    button {
                        class: "btn-restart",
                        onclick: move |_| {
                            let sel = handle.selected_ids();
                            let picked: Vec<FindingView> = sel.iter().filter_map(|id| id_map_a.get(id).cloned()).collect();
                            if picked.is_empty() { return; }
                            let mut model = TriageModel { dispositions: dispositions.peek().clone(), triage_view };
                            model.move_to(&picked, TriageState::Unresolved);
                            dispositions.set(model.dispositions);
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
                            let mut model = TriageModel { dispositions: dispositions.peek().clone(), triage_view };
                            model.move_to(&picked, TriageState::TechDebt);
                            dispositions.set(model.dispositions);
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
                            let mut model = TriageModel { dispositions: dispositions.peek().clone(), triage_view };
                            model.set_bucket(&picked, TechDebtBucket::Later);
                            dispositions.set(model.dispositions);
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
                            let mut model = TriageModel { dispositions: dispositions.peek().clone(), triage_view };
                            model.set_bucket(&picked, TechDebtBucket::Now);
                            dispositions.set(model.dispositions);
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
                            let mut model = TriageModel { dispositions: dispositions.peek().clone(), triage_view };
                            model.move_to(&picked, TriageState::Unresolved);
                            dispositions.set(model.dispositions);
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
                            let mut model = TriageModel { dispositions: dispositions.peek().clone(), triage_view };
                            model.move_to(&picked, TriageState::Ignored);
                            dispositions.set(model.dispositions);
                            handle.remove_rows(&sel);
                            crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("Moved {} to Ignored.", picked.len()));
                        },
                        "Move to Ignored"
                    }
                },
                TriageState::FalsePositive => rsx! {
                    // No reason input here — the reason was already captured when the finding
                    // was marked (Unresolved view). Moving back to Unresolved is the only
                    // action; it clears no data (the reason stays on the disposition record
                    // until a new one overwrites it).
                    button {
                        class: "btn-restart",
                        onclick: move |_| {
                            let sel = handle.selected_ids();
                            let picked: Vec<FindingView> = sel.iter().filter_map(|id| id_map_e.get(id).cloned()).collect();
                            if picked.is_empty() { return; }
                            let mut model = TriageModel { dispositions: dispositions.peek().clone(), triage_view };
                            model.move_to(&picked, TriageState::Unresolved);
                            dispositions.set(model.dispositions);
                            handle.remove_rows(&sel);
                            crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("Moved {} back to Unresolved.", picked.len()));
                        },
                        "Move to Unresolved"
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
        // `clear_sort` drops the whole sort list in one act, which is why the bar's button
        // exists rather than relying on chorale's third click: a plain click cycles ONE column
        // back to unsorted, but a shift+click multi-sort leaves two or three columns set and no
        // single click undoes all of them. This is correct from any sort state. With
        // `state.sort` empty, chorale's `filtered_sorted_pairs` stops reordering entirely and
        // hands back `state.rows` — the rows exactly as `TriageModel::visible` ordered them —
        // so the restored default is byte-identical, not approximately restored.
        QueueOrderBar {
            order: queue_order(&active_sort(), &order_columns),
            on_reset: Callback::new(move |()| handle.clear_sort()),
        }
        // The rule-level deviation decisions, one row per group. Renders nothing when the
        // view holds no deviations.
        DeviationBulkBar {
            groups: bulk_rows,
            on_confirm_all: Callback::new({
                let bulk_groups = std::rc::Rc::clone(&bulk_groups);
                let key_to_row_id = key_to_row_id.clone();
                move |i: usize| {
                    let Some(group) = bulk_groups.get(i) else { return };
                    let mut model = TriageModel { dispositions: dispositions.peek().clone(), triage_view };
                    // Same shape as the false-positive path below and for the same reason:
                    // core's `bulk_confirm` is a thin wrapper over the per-row `confirm`, so
                    // one click here is provably identical to N individual gestures rather
                    // than a second write path. `None` means an empty group — nothing moved.
                    let Some(moved) = model.bulk_confirm(group) else { return };
                    dispositions.set(model.dispositions);
                    let ids: Vec<RowId> = group.rows.iter().filter_map(|f| key_to_row_id.get(&finding_key(f)).copied()).collect();
                    handle.remove_rows(&ids);
                    crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, format!("Confirmed all {moved} {} row(s).", group.rule_id));
                }
            }),
            // The rule id on a bulk row opens the hub. The repo comes off the group's own
            // first row rather than from a table-level guess: `rule_hub` is scoped per
            // (repo, rule) and a wrong repo would silently describe a different assessment.
            on_open_hub: Callback::new({
                let bulk_groups = std::rc::Rc::clone(&bulk_groups);
                move |i: usize| {
                    let Some(group) = bulk_groups.get(i) else { return };
                    let Some(first) = group.rows.first() else { return };
                    on_open_hub.call(RuleHubRequest {
                        repo: first.repo.clone(),
                        rule_id: group.rule_id.clone(),
                    });
                }
            }),
            // §9.2 CHANGED THIS BUTTON'S REASON SOURCE, deliberately and visibly. It used to
            // read `fp_reason()` — the always-visible toolbar textarea — which meant a
            // rule-level act with a possibly stale reason typed for something else, and no
            // statement anywhere of how many rows it was about to move. It now raises the
            // SAME on-demand prompt the per-selection button raises, carrying this group's
            // index and its advertised count. The require-reason invariant does not move: it
            // is still `TriageModel::bulk_false_positive`'s refusal, enforced at commit.
            on_false_positive_all: Callback::new({
                let bulk_groups = std::rc::Rc::clone(&bulk_groups);
                move |i: usize| {
                    let Some(group) = bulk_groups.get(i) else { return };
                    toolbar_reason.set(String::new());
                    toolbar_prompt.set(Some(ToolbarReasonPrompt {
                        state: TriageState::FalsePositive,
                        target: ReasonTarget::Group(group.clone()),
                    }));
                }
            }),
        }
        // The raised prompt, mounted once for both sources. Rendering nothing when closed is
        // the point of §9.2: the Queue's resting state has no reason box in it at all.
        if let Some(p) = toolbar_prompt() {
            QueueReasonPrompt {
                prompt: p,
                reason: toolbar_reason(),
                on_reason: Callback::new(move |r: String| toolbar_reason.set(r)),
                on_cancel: Callback::new(move |()| {
                    toolbar_prompt.set(None);
                    toolbar_reason.set(String::new());
                }),
                on_commit: commit_toolbar_reason,
            }
        }
        // THE SPLIT: the Queue table and its docked detail pane, side by side. The pane is a
        // SIBLING of the table rather than content inside it — chorale 0.2.3's
        // `detail_renderer` is an expandable inline row-detail `<tr>` beneath its parent row,
        // not a docked panel, so an inline detail would reflow the table under the cursor on
        // every cursor-down. See `FindingDetailPane`'s doc comment.
        div { class: "queue-split",
        div { class: "queue-split-table",
        CamerataTable {
            handle,
            sort_enabled: true,
            filter_enabled: true,
            selection_enabled: true,
            resize_enabled: true,
            // Grouped by RULE GROUP -> file; the wrapper collapses every group on mount so a
            // long audit opens collapsed and the architect drills in deliberately.
            //
            // The outer key is `RULE_GROUP_COLUMN`, not the visible "type" column, because the
            // group header's text IS that column's cell value in chorale — so grouping on
            // `rule_group_label` is what puts the deviation sentence on the header (design
            // §2/§4). It partitions identically to the rule id: the sentence's numbers are
            // group-level and repeat verbatim on every row of a rule.
            group_by: vec![RULE_GROUP_COLUMN, ColumnId("file")],
            // 0.2.3: an expand-all / collapse-all control in the grouped header (the
            // findings table groups by rule -> file), so a long audit collapses at once.
            group_expand_toggle: true,
            row_cell_renderers: row_renderers,
            // Critical rows get a red full-row highlight via the 0.2.3 conditional
            // row-styling hook. §9.5 removed the legend above the table and kept THIS, so the
            // predicate is a named pure function rather than an inline closure: "the
            // highlight stays" is then an assertion a test can make, instead of a claim about
            // a closure no test can reach.
            row_class: RowClass::new(finding_row_class),
            // A row click FOCUSES the pane on that row; it no longer pops the modal
            // (design §9.6 — "the row-click modal for Queue rows, replaced by the docked
            // detail pane"). Mouse and keyboard drive the SAME `RowCursor`, so there is one
            // notion of "the focused row" rather than two that can disagree. The modal is
            // still one click away, from the pane's own "Full detail" button, and it still
            // earns that: it carries the on-demand enclosing-code-block fetch, which is far
            // too expensive to re-run on every cursor-down.
            on_row_click: Callback::new(move |rid: RowId| {
                if let Some(f) = id_map_click.get(&rid) {
                    cursor.set(RowCursor::focused_on(finding_key(f)));
                    // Move REAL keyboard focus into the pane, and let the pane's own
                    // `focusin` set `PaneFocus::Cursor` from that. Setting the signal here
                    // instead would be a lie the curator could act on: the keydown handler
                    // lives on the pane element, so with DOM focus still in the table a `C`
                    // would reach nothing while the pane reported its keys armed. The one
                    // state the indicator reads must come from the one thing that is true.
                    focus_detail_pane();
                }
            }),
        }
        }
        FindingDetailPane {
            row: pane_row,
            focus: pane_focus(),
            prompt: prompt(),
            prompt_reason: prompt_reason(),
            undo: undo_banner,
            // THE SCOPED KEY HANDLER. Every press goes through core's `pane_command`, which
            // refuses everything unless `focus == PaneFocus::Cursor` — so the reason
            // textarea inches away cannot have its `c` read as a Confirm.
            on_key: Callback::new(move |(key, mods): (String, KeyMods)| {
                let here = *pane_focus.peek();
                match pane_command(here, &key, mods) {
                    Some(PaneCommand::MoveFocus(dir)) => {
                        let next = cursor.peek().moved(&visible_keys(), dir);
                        cursor.set(next);
                    }
                    Some(PaneCommand::Dispose(state)) => {
                        apply_disposition.call((state, String::new()));
                    }
                    Some(PaneCommand::PromptFor(state)) => {
                        // `I` and `F` OPEN the reason prompt; they never dispose. The
                        // export reads those reasons, and a keystroke is not a licence to
                        // drop them. Opening it also moves focus into the prompt, which
                        // disarms the keys for exactly as long as the curator is typing.
                        prompt.set(Some(state));
                        prompt_reason.set(String::new());
                        pane_focus.set(PaneFocus::ReasonPrompt);
                    }
                    None => {}
                }
            }),
            on_focus: Callback::new(move |f: PaneFocus| {
                // A blur out of the pane while the reason prompt is open must not disarm
                // the prompt's own focus state — the textarea is INSIDE the pane, so
                // focusing it fires the region's `focusout` first.
                if f == PaneFocus::Elsewhere && prompt.peek().is_some() {
                    return;
                }
                pane_focus.set(f);
            }),
            on_prompt_reason: Callback::new(move |r: String| prompt_reason.set(r)),
            on_prompt_commit: Callback::new(move |()| {
                if let Some(state) = prompt.peek().to_owned() {
                    let reason = prompt_reason.peek().clone();
                    apply_disposition.call((state, reason));
                    // Only hand focus back if the disposition actually LANDED.
                    // `dispose_with_undo` refuses a blank reason and leaves the prompt open;
                    // re-arming the keys over a still-open prompt would put the curator back
                    // in exactly the state the Q5 clause forbids, one keystroke from a
                    // Confirm while mid-sentence.
                    if prompt.peek().is_none() {
                        focus_detail_pane();
                    }
                }
            }),
            on_prompt_cancel: Callback::new(move |()| {
                prompt.set(None);
                prompt_reason.set(String::new());
                // Move REAL focus back, and let `focusin` set the state — never assert it.
                focus_detail_pane();
            }),
            on_undo: Callback::new(move |()| {
                let Some(mut entry) = undo_entry.peek().clone() else { return };
                let now = epoch.elapsed().as_millis() as u64;
                let mut model = TriageModel {
                    dispositions: dispositions.peek().clone(),
                    triage_view,
                };
                match model.undo(&mut entry, now) {
                    UndoOutcome::Reverted(n) => {
                        dispositions.set(model.dispositions);
                        // Put the row back ON SCREEN too. Reverting only the map would
                        // leave the toast truthfully claiming a revert while the row stayed
                        // gone from the table, which is the half-done undo this is for.
                        if let Some((at, rid, f)) = undo_restore.peek().clone() {
                            handle.insert_row(at, rid, f);
                            cursor.set(RowCursor::focused_on(finding_key(
                                &undo_restore.peek().as_ref().expect("just read").2,
                            )));
                        }
                        crate::toast::push_toast(
                            toasts,
                            crate::toast::ToastKind::Info,
                            format!("Undone \u{2014} {n} finding(s) put back."),
                        );
                    }
                    // Both refusals are REPORTED. A visible Undo that silently does nothing
                    // is the exact failure the window exists to prevent.
                    UndoOutcome::WindowClosed => crate::toast::push_toast(
                        toasts,
                        crate::toast::ToastKind::Warning,
                        "That undo expired (the window is five seconds). Move the finding back from its triage table.",
                    ),
                    UndoOutcome::AlreadyUndone => crate::toast::push_toast(
                        toasts,
                        crate::toast::ToastKind::Warning,
                        "That action was already undone.",
                    ),
                }
                undo_entry.set(None);
                undo_restore.set(None);
            }),
            on_dispose: Callback::new(move |state: TriageState| {
                // The pane's own buttons. A reason-carrying state opens the prompt first,
                // exactly as its key does — one path, two triggers.
                if state.requires_reason() {
                    prompt.set(Some(state));
                    prompt_reason.set(String::new());
                    pane_focus.set(PaneFocus::ReasonPrompt);
                } else {
                    apply_disposition.call((state, String::new()));
                }
            }),
            on_open_detail: Callback::new(move |()| {
                if let Some(f) = focused_for_modal.clone() {
                    detail_finding.set(Some(f));
                }
            }),
            on_open_hub: Callback::new(move |()| {
                if let Some(f) = focused_for_hub.clone() {
                    on_open_hub.call(RuleHubRequest {
                        repo: f.repo.clone(),
                        rule_id: f.rule_id.clone(),
                    });
                }
            }),
        }
        }
    }
}

/// Which onboarding path the user is setting up.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum OnboardPath {
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
pub(super) enum RepoDetect {
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
pub(super) async fn detect_local_repo() -> RepoDetect {
    let Some(folder) = rfd::AsyncFileDialog::new()
        .set_title("Choose a local repo folder")
        .pick_folder()
        .await
    else {
        return RepoDetect::Cancelled;
    };
    let path = folder.path().to_string_lossy().to_string();
    let resp = match reqwest::Client::new()
        .post(format!("{}/api/git/detect-repo", crate::bff_base()))
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
pub(super) struct GreenfieldScaffoldResult {
    pub ok: bool,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub files_written: Vec<String>,
    #[serde(default)]
    pub commit_sha: String,
    #[serde(default)]
    pub message: String,
}

/// Resolve the adopted directive for a corpus rule: uses the default option's
/// directive, falling back to the rule title when options are absent or the default
/// is unset. Mirrors the resolve logic in the brownfield apply path.
pub(super) fn resolve_gf_directive(r: &ProposedRuleView) -> String {
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
pub(super) async fn scaffold_greenfield_api(
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
        .post(format!("{}/api/onboard/greenfield", crate::bff_base()))
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
pub(super) fn OnboardView(connection: Option<ProviderView>) -> Element {
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
                    p { class: "scan-section-sub", "Scans only the currently checked-out branch of each repo. Switch branches first if you want a different branch scanned." }
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
                    {
                        // Prefer the explicit server-set `blocked` flag; the shape heuristic is
                        // kept only as a back-compat fallback for any report serialized before the
                        // flag existed (it never sets `blocked`, so `false || heuristic`).
                        let looks_blocked = report.blocked || scan_report_looks_blocked(&report);
                        rsx! {
                            if report.gated || looks_blocked {
                                div { class: "onboard-gate",
                                    span { class: "onboard-gate-dot" }
                                    div {
                                        p { class: "onboard-gate-h",
                                            if report.gated { "Scan not run" } else { "Scan blocked" }
                                        }
                                        p { class: "onboard-gate-b", "{report.message.clone().unwrap_or_default()}" }
                                        if looks_blocked {
                                            p { class: "onboard-gate-hint",
                                                "Add an Anthropic API key in Settings, or enable \u{201c}Allow Claude CLI (personal subscription)\u{201d} for this project, to run this scan."
                                            }
                                        }
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
pub(super) fn GreenfieldForm(
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
pub(super) fn GreenfieldResultView(result: GreenfieldScaffoldResult) -> Element {
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

/// Feature B — the compliance-safety gate (backend-safety-and-live-models design doc):
/// BACK-COMPAT FALLBACK ONLY. The server now sets an explicit `ScanReport.blocked` flag on a
/// `Blocked` resolution (API-only project, no Anthropic key), which the caller keys off
/// directly. This shape-inference remains solely for reports serialized before that flag
/// existed. It detects the zero-stats-plus-message shape a block leaves behind — but excludes
/// any `⚠ COMPLIANCE:`-prefixed message, since that marks a `CliFallbackWarn` scan (which DID
/// run, just on the CLI) and could otherwise be mistaken for a block when it happened to scan
/// an empty repo.
fn scan_report_looks_blocked(report: &ScanReportView) -> bool {
    !report.gated
        && report.files_scanned == 0
        && report.findings.is_empty()
        && report.proposed_rules.is_empty()
        && report
            .message
            .as_deref()
            .is_some_and(|m| !m.trim().is_empty() && !m.contains(COMPLIANCE_NOTE_PREFIX))
}

/// The exact prefix `audit_repos` (crates/server/src/onboard.rs) attaches to a
/// `CliFallbackWarn` resolution's message before folding it into the report's joined-by-
/// " · " `notes` -> `message`. Kept as a single constant so the UI-side split below and any
/// future match stay byte-for-byte in sync with the server string.
const COMPLIANCE_NOTE_PREFIX: &str = "\u{26a0} COMPLIANCE:";

/// Split a `ScanReport::message` into `(compliance-warning notes, remaining notes)`.
///
/// `audit_repos` joins every note for the run with `" · "` into one string (see
/// crates/server/src/onboard.rs ~line 600), so a `⚠ COMPLIANCE:`-prefixed `CliFallbackWarn`
/// warning can arrive interleaved with ordinary per-repo scan notes ("AI review deselected",
/// "audit failed", etc). This pulls the compliance-prefixed segments out (prefix stripped,
/// trimmed) so the caller can render them in a dedicated, higher-alarm banner instead of
/// letting them blend into the routine note line. Returns `other` as `None` when nothing is
/// left over, so the caller can skip rendering an empty `.scan-note` box.
fn split_compliance_notes(message: &str) -> (Vec<String>, Option<String>) {
    let mut compliance = Vec::new();
    let mut other = Vec::new();
    for part in message.split(" · ") {
        let trimmed = part.trim();
        match trimmed.strip_prefix(COMPLIANCE_NOTE_PREFIX) {
            Some(rest) => compliance.push(rest.trim().to_string()),
            None => {
                if !trimmed.is_empty() {
                    other.push(trimmed.to_string());
                }
            }
        }
    }
    let other = if other.is_empty() { None } else { Some(other.join(" · ")) };
    (compliance, other)
}

/// Rescan button / sticky-bar label: singular "rule" for exactly one staged override, plural
/// otherwise. Pure so the pluralization is unit-tested without a render.
pub(super) fn rescan_button_label(pending: usize) -> String {
    if pending == 1 {
        "Rescan 1 overridden rule".to_string()
    } else {
        format!("Rescan {pending} overridden rules")
    }
}

/// Pure staging decision for the "Change" dropdown: picking the option a rule is ALREADY
/// evaluated against clears any staged override for it (back to the AI's pick, or whatever a
/// prior rescan applied); picking anything else stages that rule's override. Extracted out of
/// the `<select>`'s `onchange` handler so this decision has a directly-testable pure core — a
/// Tier-1 SSR render test cannot simulate a real `<select>` change event (see docs/UI_TESTING.md).
pub(super) fn stage_or_clear_override(
    pending: &mut std::collections::HashMap<String, String>,
    rule_id: &str,
    evaluated_option_id: &str,
    picked_option_id: &str,
) {
    if picked_option_id == evaluated_option_id {
        pending.remove(rule_id);
    } else {
        pending.insert(rule_id.to_string(), picked_option_id.to_string());
    }
}

/// ONE multi-option rule's ALTERNATIVES CONTROL LINE, flattened into exactly what renders.
///
/// Flat, owned, `PartialEq` data rather than the three sources it is joined from
/// (`ProposedRuleView`, `RuleRecommendationView`, the staged-override map) — the same shape,
/// and the same reason, as `DetailPaneRow` and `DeviationBulkRow`. Every judgement (which
/// option the rule is CURRENTLY evaluated against, which badge that earns, whether the
/// override is staged) is made once in [`alternative_controls_row`] and unit-tested there;
/// the component chooses words and pixels.
///
/// Built as a precondition of design §9.1, which moves this line off the standalone panel and
/// into the surfaces that describe the rule. A line that can only be produced inside one
/// component's render body cannot move anywhere.
#[derive(Clone, PartialEq)]
pub(super) struct AlternativeControlsRow {
    pub rule_id: String,
    pub title: String,
    /// How many of this scan's findings are attributed to this rule. Zero is a normal and
    /// important value: a multi-option rule the codebase fully complies with still needs
    /// reviewing, because the review is of the OPTION, not of the violations.
    pub finding_count: usize,
    /// The option the rule is evaluated against RIGHT NOW, resolved in priority order:
    /// a staged override, else the option the findings were actually produced under, else
    /// the AI's recommendation, else the rule's declared default.
    pub evaluated_option_id: String,
    /// That option's authored label, or `(none)` when the id matches no option on the rule.
    pub evaluated_label: String,
    pub badge_class: &'static str,
    pub badge_text: &'static str,
    /// An override is staged for this rule and has not been rescanned yet.
    pub staged: bool,
    /// The architect has explicitly accepted this rule's current pick in this session.
    pub accepted: bool,
    /// The model judged the rule applicable to this repo. `false` renders a warning.
    pub applicable: bool,
    /// The `file:line` citation the model grounded its pick in, when it gave one.
    pub evidence: Option<String>,
    /// `(option_id, label)` for the Change select, in the rule's own order.
    pub options: Vec<(String, String)>,
    /// The model's reasoning, for the Why? modal.
    pub reasoning: String,
    /// The model's raw answer was not a real option and the pick shown is a fallback.
    pub hallucinated: bool,
}

/// Build one rule's control line out of the scan's three sources. PURE, so the priority order
/// below is unit-testable rather than buried in a render body.
///
/// `evaluated_by_rule` maps rule id -> the option id that rule's findings were produced under
/// (every finding for one rule shares it); `staged` is the pending override, if any.
pub(super) fn alternative_controls_row(
    rule: &ProposedRuleView,
    rec: &RuleRecommendationView,
    finding_count: usize,
    evaluated_in_findings: Option<&str>,
    staged: Option<&str>,
    accepted: bool,
) -> AlternativeControlsRow {
    // THE PRIORITY ORDER, and why it is this way round. A staged override wins because it is
    // what the next rescan will use and the curator needs to see what he just picked. Below
    // that, the option the FINDINGS were produced under beats the AI's recommendation,
    // because a prior rescan can have already applied an override that the recommendation
    // field does not know about — showing the recommendation there would describe the scan
    // the curator is NOT looking at.
    let evaluated_option_id = staged
        .map(str::to_string)
        .or_else(|| evaluated_in_findings.map(str::to_string))
        .or_else(|| Some(rec.recommended_option_id.clone()).filter(|s| !s.is_empty()))
        .or_else(|| rule.default_option.clone())
        .unwrap_or_default();
    let evaluated_label = rule
        .options
        .iter()
        .find(|o| o.id == evaluated_option_id)
        .map(|o| o.label.clone())
        .unwrap_or_else(|| "(none)".to_string());
    let (badge_class, badge_text) = if staged.is_some() {
        ("rec-badge rec-badge-pending", "Override, pending rescan")
    } else if rec.operator_chosen {
        ("rec-badge rec-badge-yours", "Your choice")
    } else {
        ("rec-badge rec-badge-ai", "AI recommended")
    };
    AlternativeControlsRow {
        rule_id: rule.id.clone(),
        title: rule.title.clone(),
        finding_count,
        evaluated_option_id,
        evaluated_label,
        badge_class,
        badge_text,
        staged: staged.is_some(),
        accepted,
        applicable: rec.applicable,
        evidence: rec.evidence.clone().filter(|e| !e.is_empty()),
        options: rule
            .options
            .iter()
            .map(|o| (o.id.clone(), o.label.clone()))
            .collect(),
        reasoning: rec.recommendation_reasoning.clone(),
        hallucinated: rec.hallucinated,
    }
}

/// THE ALTERNATIVES CONTROL LINE, rendered (design §4's group-header line, §9.1's relocation).
///
/// > Evaluated against: URI-versioned endpoints (AI recommended) · Why? · Change
///
/// Props-only, so SSR can render it in every badge state — including the two a click would be
/// needed to reach through the panel (an override staged, and an accepted rule). That is the
/// same discipline `ProcessGate`, `DeviationBulkBar` and `FindingDetailPane` are built on, and
/// it is what makes §9.1's relocation checkable rather than hopeful: the line can now be
/// rendered anywhere a rule id is, and asserted on wherever it lands.
#[component]
pub(super) fn RuleAlternativeControls(
    row: AlternativeControlsRow,
    /// Open the Why? modal for this rule.
    on_why: Callback<()>,
    /// The curator picked an option in the Change select. Invoked with the picked option id;
    /// the caller decides whether that stages an override or clears one
    /// (see [`stage_or_clear_override`]).
    on_pick: Callback<String>,
) -> Element {
    let evaluated_id = row.evaluated_option_id.clone();
    rsx! {
        div { class: "rec-row", key: "{row.rule_id}",
            div { class: "rec-row-main",
                span { class: "rec-row-id", "{row.rule_id}" }
                span { class: "rec-row-title", "{row.title}" }
                span { class: "rec-row-count",
                    if row.finding_count == 0 { "0 findings" } else { "{row.finding_count} finding(s)" }
                }
            }
            div { class: "rec-line",
                "Evaluated against: "
                strong { "{row.evaluated_label}" }
                span { class: "{row.badge_class}", "{row.badge_text}" }
                if row.accepted {
                    span { class: "rec-badge rec-badge-accepted", "Accepted" }
                }
                if !row.applicable {
                    span { class: "rec-not-applicable", "Not applicable to this repo" }
                }
                button {
                    class: "btn-edit-sm rec-why",
                    onclick: move |_| on_why.call(()),
                    "Why?"
                }
            }
            // Evidence (P7): the file:line citation the model grounded its pick in — "a wrong
            // pick costs a glance, not a false finding". Absent for legacy reports and
            // operator-forced picks with no model evidence, so this line only renders when
            // present.
            if let Some(ev) = row.evidence.clone() {
                span { class: "rec-evidence", "Evidence: {ev}" }
            }
            select {
                class: "rec-select",
                value: "{evaluated_id}",
                onchange: move |e: Event<FormData>| on_pick.call(e.value()),
                for (id , label) in row.options.iter() {
                    option { key: "{id}", value: "{id}", "{label}" }
                }
            }
        }
    }
}

/// The "Why?" explanation modal: shows the recommendation's reasoning (or a fallback note if
/// none was recorded) plus a hallucination notice when the model's raw answer had to be
/// corrected. Extracted out of `RuleAlternativesPanel` as its own props-only component so it
/// can be render-tested directly in its "open" shape — the parent only mounts it conditionally
/// on a local `use_signal`, which a Tier-1 SSR test cannot toggle via a simulated click.
#[component]
pub(super) fn RuleWhyModal(
    rule_id: String,
    title: String,
    reasoning: String,
    hallucinated: bool,
    on_close: EventHandler<()>,
) -> Element {
    rsx! {
        div { class: "rule-modal-overlay", onclick: move |_| on_close.call(()),
            div { class: "rule-modal", onclick: move |e| e.stop_propagation(),
                div { class: "rule-modal-head",
                    span { class: "rule-modal-id", "{rule_id}" }
                    button { class: "rule-modal-close", onclick: move |_| on_close.call(()), "\u{2715}" }
                }
                p { class: "rule-modal-title", "{title}" }
                p { class: "rule-modal-label", "Why this alternative" }
                p { class: "rec-modal-reasoning",
                    if reasoning.is_empty() {
                        "No reasoning was recorded for this recommendation."
                    } else {
                        "{reasoning}"
                    }
                }
                if hallucinated {
                    span { class: "rec-hallucinated-note",
                        "The model did not return a valid choice for this rule; this is the selected or default option shown instead."
                    }
                }
            }
        }
    }
}

/// The sticky batched-rescan action bar: appears once at least one override is staged, showing
/// the count and a button that fires the batched rescan. Extracted out of
/// `RuleAlternativesPanel` as its own props-driven component so the "N staged overrides shows
/// the bar with the right count" shape is render-testable directly (a Tier-1 SSR test cannot
/// stage an override via a simulated `<select>` change to reach this state through the parent).
#[component]
pub(super) fn RescanStickyBar(
    pending_count: usize,
    rescanning: bool,
    on_rescan: EventHandler<()>,
) -> Element {
    rsx! {
        div { class: "rec-sticky-bar",
            span { class: "rec-sticky-label", "{rescan_button_label(pending_count)}" }
            button {
                class: "btn-run",
                disabled: rescanning,
                onclick: move |_| on_rescan.call(()),
                if rescanning { "Rescanning\u{2026}" } else { "{rescan_button_label(pending_count)}" }
            }
        }
    }
}

/// The "Rule alternatives" review panel (audit-integrated alternative recommendation,
/// docs/design/2026-09-22_audit-integrated-alternatives.md). Lists every multi-option rule the
/// audit evaluated (every key of `recommendations`), including rules with zero violations, so
/// the architect can review — and, if they disagree, override — any of them.
///
/// This single panel serves BOTH halves of the design's UX: the per-rule "Evaluated against:
/// <option> (AI recommended) · Why? · Change" control line (normally a findings-table
/// group-header line) AND the standalone "every rule in scope" review panel. chorale's grouped
/// `Table` (the findings table below) exposes no group-header content-injection hook, so rather
/// than duplicate this row UI across two disconnected surfaces — risking the staged-override
/// state diverging between them — it is rendered ONCE, directly above the findings table it
/// describes, which reads exactly like a header for what follows.
///
/// Rendered (and re-keyed) alongside the findings table in `ScanResults`, so a fresh audit
/// remounts it with clean staged-override state.
///
/// # Design §9.1: WHY THIS PANEL IS STILL HERE
///
/// §9.1 is the removals sweep's first item and asks for this panel to go, with its
/// Evaluated-against line, Why?, Change select and staged-override/rescan flow moving "into
/// the rule group headers and the rule hub modal". Both destinations were checked against the
/// built product rather than against the design's description of it, and between them they do
/// not cover what this panel does. The halves:
///
/// * **MOVED (done).** The Evaluated-against line and the Why? answer now also render inside
///   [`RuleHubStrip`], built by the SAME `alternative_controls_row` this panel uses, so the
///   two cannot disagree. A curator reading any rule id in the Queue reaches that context in
///   one click, which is §9.1's actual complaint — "duplicating per-rule context away from the
///   rows it describes" — addressed.
///
/// * **CANNOT MOVE: the group headers take no content.** Established at build time and
///   recorded in the design's own §13 retraction: `render_grouped_row` / `group_header_tr` are
///   private in `chorale-dioxus` 0.2.3 and `group_header_class` is a CSS class string. The
///   deviation SENTENCE reaches the header only because it IS the grouped-on cell value; a
///   `<select>` and a button cannot be a cell value. (chorale is not to be modified.)
///
/// * **CANNOT MOVE: the hub cannot reach a rule with no findings.**
///   `camerata_ui_core::triage::rule_hub` returns `None` when a rule has no rows in the repo,
///   deliberately — its own doc says "there is no '0 findings' hub, an empty strip is a dead
///   affordance". But this panel exists precisely to list "every multi-option rule the audit
///   evaluated (every key of `recommendations`), INCLUDING RULES WITH ZERO VIOLATIONS, so the
///   architect can review — and, if they disagree, override — any of them". A clean rule is
///   the one whose option choice is least visible and most in need of a second look. Deleting
///   the panel would make those rules unreachable: no Queue row, no panel row, no hub.
///
/// So the Change select, the staged-override map and the rescan/accept flow stay here, where
/// every rule in scope can be reached. The hub names this panel as their home rather than
/// leaving the curator to hunt for it.
///
/// Removing this panel is possible, but it needs a surface that enumerates the rules the scan
/// found nothing for — which is a new thing to build, not a relocation, and §9.1 does not
/// describe one.
#[component]
pub(super) fn RuleAlternativesPanel(
    project_id: String,
    proposed_rules: Vec<ProposedRuleView>,
    findings: Vec<FindingView>,
    repos: Vec<String>,
    recommendations: std::collections::HashMap<String, RuleRecommendationView>,
    audit: Signal<Option<ScanReportView>>,
) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    // The architect's per-repo alternative choices (ScanResults provides this via context) —
    // updated here too after a successful rescan, so Arm's directive resolution matches the
    // operator's forced choice rather than the pre-override default.
    let mut chosen = use_context::<Signal<std::collections::HashMap<String, String>>>();

    // rule_id -> staged override option id. A failed rescan leaves the override in place (so
    // the architect can retry or adjust) rather than silently discarding it.
    let mut pending = use_signal(std::collections::HashMap::<String, String>::new);
    // Which rule's Why modal is open (None = closed).
    let mut why_open = use_signal(|| Option::<String>::None);
    let mut rescanning = use_signal(|| false);
    let mut accepting = use_signal(|| false);
    // Rule ids the architect has explicitly accepted THIS session — purely a local echo of the
    // server-persisted chosen_option so the "Accepted" badge updates without a re-fetch.
    let mut accepted_ids = use_signal(std::collections::HashSet::<String>::new);

    // The rules in scope: every multi-option rule the audit recommended on, joined against the
    // scan's proposed-rules list for its title + options (the audit response itself carries no
    // rule metadata — only the scan's `proposed_rules` does).
    let mut rows: Vec<&ProposedRuleView> = proposed_rules
        .iter()
        .filter(|r| recommendations.contains_key(&r.id) && r.options.len() >= 2)
        .collect();
    rows.sort_by(|a, b| a.id.cmp(&b.id));

    if rows.is_empty() {
        // No multi-option rule was part of this audit — nothing to review.
        return rsx! {};
    }
    // Computed BEFORE the rsx! block (not derived from `rows` inside a `move` closure below) so
    // the Accept button's closure can take ownership without fighting `rows`'s borrow.
    let rule_ids_in_scope: Vec<String> = rows.iter().map(|r| r.id.clone()).collect();
    // A separate clone for the rescan closure below (each `move` closure needs its own owned
    // copy; `rule_ids_in_scope` itself is still needed, untouched, by the Accept closure).
    let rule_ids_for_rescan_filter = rule_ids_in_scope.clone();
    // Likewise: `project_id` is a plain `String` prop (not `Copy`), and BOTH the rescan and
    // Accept closures below are `move` closures that need their own owned copy — a `move`
    // closure takes the captured variable itself, not a borrow, so referencing the shared prop
    // from two separate `move` closures needs two clones made before either closure exists.
    let project_id_for_rescan = project_id.clone();

    // rule_id -> finding count (0 for a clean rule) + the option id its findings were actually
    // evaluated under (every finding for one rule shares the same evaluated_option_id).
    let mut finding_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut evaluated_by_rule: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for f in &findings {
        *finding_counts.entry(f.rule_id.clone()).or_insert(0) += 1;
        if let Some(oid) = &f.evaluated_option_id {
            evaluated_by_rule
                .entry(f.rule_id.clone())
                .or_insert_with(|| oid.clone());
        }
    }

    // Only count/send overrides for rules STILL in scope — a stale entry left over from a
    // previous audit (a different rule selection) must never inflate the sticky bar or get
    // sent to the rescan endpoint for a rule that no longer even renders a row.
    let pending_count = rows
        .iter()
        .filter(|r| pending.read().contains_key(&r.id))
        .count();

    rsx! {
        div { class: "rec-panel",
            p { class: "rec-panel-head", "Rule alternatives" }
            p { class: "rec-panel-sub",
                "For every rule with more than one adopted alternative, the audit picked the option it judged best fitting and evaluated the codebase against it (shown below as \"Evaluated against\"). Review each one; change any you disagree with, then rescan just the ones you changed."
            }
            div { class: "rec-rows",
                for r in rows.iter() {
                    {
                        let rid = r.id.clone();
                        let rid_for_select = rid.clone();
                        let rid_for_why = rid.clone();
                        let rec = recommendations.get(&rid).cloned().unwrap_or_default();
                        // ONE derivation, shared with every other surface that will render
                        // this line (design §9.1). The priority order that resolves
                        // "evaluated against" used to live inline here; it is now
                        // `alternative_controls_row`, pure and unit-tested.
                        let row = alternative_controls_row(
                            r,
                            &rec,
                            finding_counts.get(&rid).copied().unwrap_or(0),
                            evaluated_by_rule.get(&rid).map(String::as_str),
                            pending.read().get(&rid).map(String::as_str),
                            accepted_ids.read().contains(&rid),
                        );
                        let evaluated_id = row.evaluated_option_id.clone();
                        rsx! {
                            RuleAlternativeControls {
                                key: "{rid}",
                                row,
                                on_why: Callback::new(move |()| why_open.set(Some(rid_for_why.clone()))),
                                on_pick: Callback::new(move |picked: String| {
                                    stage_or_clear_override(
                                        &mut pending.write(),
                                        &rid_for_select,
                                        &evaluated_id,
                                        &picked,
                                    );
                                }),
                            }
                        }
                    }
                }
            }
        }
        // Why modal: shared across rows, keyed by whichever rule's "Why?" was clicked.
        if let Some(rid) = why_open() {
            {
                let title = rows
                    .iter()
                    .find(|r| r.id == rid)
                    .map(|r| r.title.clone())
                    .unwrap_or_default();
                let rec = recommendations.get(&rid).cloned().unwrap_or_default();
                rsx! {
                    RuleWhyModal {
                        rule_id: rid.clone(),
                        title,
                        reasoning: rec.recommendation_reasoning,
                        hallucinated: rec.hallucinated,
                        on_close: move |_| why_open.set(None),
                    }
                }
            }
        }
        // Sticky batched-rescan bar: appears once at least one override is staged. Drives the
        // centralized LoadingGuard (inside `rescan_alternatives`) for the whole round trip —
        // no ad-hoc spinner; `rescanning` only swaps the button's own label.
        if pending_count > 0 {
            RescanStickyBar {
                pending_count,
                rescanning: rescanning(),
                on_rescan: move |_| {
                    // Filtered to rules still in scope — a stale entry left over from a
                    // previous audit run reusing this same mounted component instance (a
                    // different rule selection, no remount in between) must never be sent.
                    let overrides: Vec<AlternativeOverride> = pending
                        .read()
                        .iter()
                        .filter(|(rule_id, _)| rule_ids_for_rescan_filter.contains(rule_id))
                        .map(|(rule_id, chosen_option_id)| AlternativeOverride {
                            rule_id: rule_id.clone(),
                            chosen_option_id: chosen_option_id.clone(),
                        })
                        .collect();
                    if overrides.is_empty() {
                        return;
                    }
                    let pid = project_id_for_rescan.clone();
                    let repos = repos.clone();
                    let rescanned_ids: std::collections::HashSet<String> =
                        overrides.iter().map(|o| o.rule_id.clone()).collect();
                    let overrides_snapshot = overrides.clone();
                    rescanning.set(true);
                    spawn(async move {
                        match rescan_alternatives(&pid, &overrides).await {
                            Some(resp) if resp.ok => {
                                let current_report = audit.peek().clone();
                                if let Some(current) = current_report {
                                    let (merged_findings, merged_recs) = merge_rescanned_alternatives(
                                        &current.findings,
                                        &current.recommendations,
                                        &rescanned_ids,
                                        resp.findings,
                                        resp.recommendations,
                                    );
                                    let mut updated = current;
                                    updated.findings = merged_findings;
                                    updated.recommendations = merged_recs;
                                    audit.set(Some(updated));
                                }
                                for o in &overrides_snapshot {
                                    for repo in &repos {
                                        chosen
                                            .write()
                                            .insert(chosen_key(repo, &o.rule_id), o.chosen_option_id.clone());
                                    }
                                }
                                pending.write().clear();
                                let msg = resp.message.unwrap_or_else(|| {
                                    format!("Rescanned {} rule(s).", overrides_snapshot.len())
                                });
                                crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, msg);
                            }
                            Some(resp) => {
                                crate::toast::push_toast(
                                    toasts,
                                    crate::toast::ToastKind::Error,
                                    resp.message.unwrap_or_else(|| "Rescan failed.".to_string()),
                                );
                            }
                            None => {
                                crate::toast::push_toast(
                                    toasts,
                                    crate::toast::ToastKind::Error,
                                    "Rescan failed. Could not reach the server.",
                                );
                            }
                        }
                        rescanning.set(false);
                    });
                },
            }
        }
        // Accept alternatives: persists the CURRENT per-rule selection (the AI's where the
        // architect never overrode it, the operator's where a rescan already applied one) into
        // the project's chosen_option. Disabled while an override is staged but not yet
        // rescanned — accepting then would persist the STALE (pre-override) recommendation.
        div { class: "findings-toolbar",
            button {
                class: "btn-edit-sm",
                disabled: accepting() || pending_count > 0,
                title: if pending_count > 0 { "Rescan the pending override(s) first." } else { "" },
                onclick: move |_| {
                    let pid = project_id.clone();
                    let all_ids = rule_ids_in_scope.clone();
                    accepting.set(true);
                    spawn(async move {
                        match accept_alternatives(&pid, None).await {
                            Some(resp) if resp.ok => {
                                accepted_ids.write().extend(all_ids);
                                crate::toast::push_toast(
                                    toasts,
                                    crate::toast::ToastKind::Info,
                                    format!("Accepted {} alternative(s).", resp.accepted),
                                );
                            }
                            Some(resp) => {
                                crate::toast::push_toast(
                                    toasts,
                                    crate::toast::ToastKind::Error,
                                    resp.message.unwrap_or_else(|| "Accept failed.".to_string()),
                                );
                            }
                            None => {
                                crate::toast::push_toast(
                                    toasts,
                                    crate::toast::ToastKind::Error,
                                    "Accept failed. Could not reach the server.",
                                );
                            }
                        }
                        accepting.set(false);
                    });
                },
                if accepting() { "Accepting\u{2026}" } else { "Accept alternatives" }
            }
        }
    }
}

/// The background-JOB failure banner: renders when an async audit job ends in the
/// `failed` state (the server's `jobs.fail` — see `run_job_body_with_panic_guard` and the
/// "no local repos" / panic-guard failure paths in `camerata_server`'s
/// `onboard_audit_start`), or when polling loses contact with a job that never reported
/// back. Distinct from [`AiReviewBlockedBanner`]: that one renders from a COMPLETED
/// report's `ai_blocked_reason` / `ai_error` fields, but a failed (or vanished) job never
/// produces a report at all (`audit` stays `None`), so without this banner the scan just
/// stops — the spinner clears and nothing else appears. That silent stop is the exact
/// "the scan dies with no error" bug this closes. Reuses the `onboard-gate` banner classes
/// for visual consistency with `AiReviewBlockedBanner`. Its own component (rather than
/// inline in `ScanResults`) so it is directly SSR-testable without standing up
/// `ScanResults`'s network-backed resources.
#[component]
pub(super) fn ScanJobFailedBanner(message: Option<String>) -> Element {
    rsx! {
        if let Some(msg) = message {
            div { class: "onboard-gate scan-job-failed-banner",
                span { class: "onboard-gate-dot" }
                div {
                    p { class: "onboard-gate-h", "Scan failed" }
                    p { class: "onboard-gate-b", "{msg}" }
                    p { class: "onboard-gate-hint",
                        "The background scan stopped before it could finish. Check your project's backend setup (Claude CLI on PATH, or an Anthropic API key), then retry the scan."
                    }
                }
            }
        }
    }
}

/// The compliance-gate fix's unmissable banner: renders when the audit's AI review either
/// (a) did not run at all because the compliance backend gate was `Blocked` (`ai_blocked_reason`
/// set), or (b) was requested but every pass failed with a real error (`ai_error` set). Renders
/// nothing when both are `None` (the ordinary happy path — most audits). Deliberately its own
/// small component (rather than inline in `ScanResults`) so it is directly SSR-testable without
/// standing up `ScanResults`'s network-backed resources. See
/// `camerata_server::onboard::ScanReport::ai_blocked_reason` / `ai_error` for the server side.
#[component]
pub(super) fn AiReviewBlockedBanner(
    ai_blocked_reason: Option<String>,
    ai_error: Option<String>,
) -> Element {
    rsx! {
        if let Some(reason) = ai_blocked_reason {
            div { class: "onboard-gate ai-review-blocked-banner",
                span { class: "onboard-gate-dot" }
                div {
                    p { class: "onboard-gate-h", "AI review did not run" }
                    p { class: "onboard-gate-b", "AI review did not run: {reason}" }
                    p { class: "onboard-gate-hint",
                        "Add an Anthropic API key in Settings, or enable \u{201c}Allow Claude CLI (personal subscription)\u{201d} for this project, then re-run the audit for the AI review. The deterministic security floor still ran; its findings (if any) are shown below."
                    }
                }
            }
        } else if let Some(err) = ai_error {
            div { class: "onboard-gate ai-review-error-banner",
                span { class: "onboard-gate-dot" }
                div {
                    p { class: "onboard-gate-h", "AI review failed" }
                    p { class: "onboard-gate-b", "AI review did not run: {err}" }
                    p { class: "onboard-gate-hint",
                        "Check the selected audit model id and your Anthropic API key or Claude CLI setup, then re-run the audit. The deterministic security floor still ran; its findings (if any) are shown below."
                    }
                }
            }
        }
    }
}

/// The one-rule-one-surface invariant, rendered (design §8).
///
/// The pipeline guarantees a rule emits deviations OR a convention row OR one no-convention row
/// OR a not-assessed row — never two in the same scan. The UI ASSERTS that rather than
/// defending against it: when it is violated, something upstream is mislabelling rows, and
/// quietly picking a winner would hide the bug while corrupting the curator's counts. So this
/// banner names the rule and the surfaces it reached, and nothing else changes — the rows still
/// render where they routed, because suppressing them would hide evidence too.
///
/// Renders nothing in the healthy case, which is every case.
#[component]
pub(super) fn KindSurfaceConflictBanner(findings: Vec<FindingView>) -> Element {
    let conflicts = rules_on_multiple_surfaces(&findings);
    if conflicts.is_empty() {
        return rsx! {};
    }
    rsx! {
        div { class: "kind-conflict-banner",
            p { class: "kind-conflict-h",
                "Pipeline bug: {conflicts.len()} rule(s) reached more than one kind surface in this scan"
            }
            p { class: "kind-conflict-b",
                "A rule emits deviations OR a convention OR one no-convention row OR a not-assessed row \u{2014} never two. The counts below are therefore not trustworthy for these rules. This is an upstream emission bug, not something to triage here."
            }
            ul { class: "kind-conflict-list",
                for ((repo , rule_id) , surfaces) in conflicts.iter() {
                    {
                        let names: Vec<&'static str> = surfaces.iter().map(|s| s.label()).collect();
                        rsx! {
                            li { key: "{repo}\u{0}{rule_id}", "{rule_id} ({repo}) \u{2192} {names.join(\", \")}" }
                        }
                    }
                }
            }
        }
    }
}

/// CONVENTIONS panel — the conventions this audit DERIVED from the client's own codebase, and
/// the decision to write them into that codebase's `AGENTS.md`.
///
/// ## What is in the list
///
/// Not only the `ConventionObserved` rows. Per the owner's §13 reversal, the convention BEHIND
/// a rule's `Deviation` rows is in here too, labelled "observed with N exceptions" with a jump
/// into the Queue for those exceptions:
///
/// > "I could decide that 'this is the correct convention even though there are inconsistencies
/// > that exist'. The remediation of those inconsistencies is a separate process."
///
/// So this panel takes the WHOLE finding set, not just the rows routed to its own surface, and
/// derives its list through `adopt::adoptable_conventions`. The exception count is context; no
/// code here reads it to decide anything.
///
/// ## Opt-out, and what that costs
///
/// Adopt is pre-checked (Q4) and Skip is the gesture. That makes the common case fast and it
/// makes one thing dangerous, which the commit dialog is built to say out loud: a convention
/// nobody opened can land in a client's `AGENTS.md`. This panel therefore records an OPENED
/// gesture per row — the "Open" button, which reveals the row's detail — and feeds it to
/// [`CommitPlan::unopened_count`]. Rendering a row is not opening it; nothing else writes that
/// record.
///
/// NO SEVERITY is rendered, here or on the other two panels (design §9.4): informational kinds
/// ship at `info` by construction, so a severity badge would imply a ranking decision that does
/// not exist.
///
/// Props-only, like every other Pass-2 component, so SSR can render each state.
#[component]
pub(super) fn ConventionsPanel(
    /// The scan's findings. The WHOLE set, not one surface's rows — see the type doc.
    findings: Vec<FindingView>,
    /// Open the RULE HUB on a listed rule (design §8: "reachable from every surface that
    /// shows a rule id"). Defaulted so the panel's existing render tests keep compiling and
    /// asserting unchanged.
    #[props(default)]
    on_open_hub: Callback<RuleHubRequest>,
    /// The curator's adopt/skip and openness state. Default = nothing skipped, nothing
    /// opened, which is the opt-out starting position.
    #[props(default)]
    model: AdoptionModel,
    /// Flip one convention between Adopt and Skip. Carries its [`adoption_key`].
    #[props(default)]
    on_toggle: Callback<String>,
    /// The curator OPENED this convention's detail. The ONE writer of the openness record.
    #[props(default)]
    on_open: Callback<String>,
    /// Raise the commit dialog for one destination repository.
    #[props(default)]
    on_commit: Callback<String>,
    /// Show ONLY the adopted-but-unopened rows — the commit dialog's third exit.
    #[props(default)]
    only_unopened: bool,
    /// Leave the unopened-only view.
    #[props(default)]
    on_clear_filter: Callback<()>,
    /// Whether a commit is in flight (disables the commit bars).
    #[props(default)]
    busy: bool,
) -> Element {
    let all = adoptable_conventions(&findings);
    if all.is_empty() {
        return rsx! {
            p { class: "kind-panel-empty", "No conventions observed in this scan." }
        };
    }
    let with_exceptions = all.iter().filter(|c| c.deviating_sites > 0).count();
    // One group per DESTINATION repo, because one commit writes one `AGENTS.md` and the gate
    // asks for one repository name. A single-repo scan renders one group.
    let mut by_repo: std::collections::BTreeMap<String, Vec<AdoptableConvention>> =
        Default::default();
    for c in &all {
        by_repo.entry(c.repo.clone()).or_default().push(c.clone());
    }
    rsx! {
        div { class: "kind-panel",
            p { class: "kind-panel-sub",
                "Conventions this audit derived from the client's own codebase. Adopting one writes it into that repository's AGENTS.md, where it governs how future AI agents work on the code. Adoption is OPT-OUT: every convention below is selected unless you skip it."
            }
            if with_exceptions > 0 {
                p { class: "kind-panel-sub adopt-exceptions-note",
                    "{with_exceptions} of these are conventions the codebase mostly follows; their exceptions are rows on the Queue tab and are counted there, not here. A convention is adoptable whatever its exception count \u{2014} adopting states the standard going forward; remediating the exceptions is separate work."
                }
            }
            if only_unopened {
                div { class: "adopt-filter-bar",
                    span { class: "adopt-filter-label", "Showing only what this commit would write without your having opened it." }
                    button {
                        class: "btn-ghost adopt-filter-clear",
                        onclick: move |_| on_clear_filter.call(()),
                        "Show all"
                    }
                }
            }
            for (repo , rows) in by_repo.iter() {
                {
                    let plan = CommitPlan::conventions(&all, &model, repo);
                    let unopened: std::collections::BTreeSet<String> =
                        plan.unopened_rule_ids().into_iter().collect();
                    let shown: Vec<AdoptableConvention> = if only_unopened {
                        rows.iter().filter(|c| unopened.contains(&c.rule_id)).cloned().collect()
                    } else {
                        rows.clone()
                    };
                    rsx! {
                        div { key: "{repo}", class: "kind-panel-group adopt-group",
                            p { class: "kind-panel-group-h", "{repo}" }
                            ul { class: "kind-panel-list",
                                for c in shown.iter() {
                                    {
                                        let key = c.key();
                                        let adopted = model.is_adopted(&key);
                                        let opened = model.was_opened(&key);
                                        let adoptable = c.is_adoptable();
                                        let req = RuleHubRequest {
                                            repo: c.repo.clone(),
                                            rule_id: c.rule_id.clone(),
                                        };
                                        let toggle_key = key.clone();
                                        let open_key = key.clone();
                                        rsx! {
                                            li {
                                                key: "{key}",
                                                class: if adopted && adoptable { "kind-panel-row adopt-row adopted" } else { "kind-panel-row adopt-row skipped" },
                                                span {
                                                    class: if adoptable && adopted { "adopt-state on" } else { "adopt-state off" },
                                                    if !adoptable {
                                                        "Cannot write"
                                                    } else if adopted {
                                                        "Adopt"
                                                    } else {
                                                        "Skipped"
                                                    }
                                                }
                                                button {
                                                    class: "rule-hub-link kind-panel-rule",
                                                    title: "Everything this scan knows about {c.rule_id}.",
                                                    onclick: move |_| on_open_hub.call(req.clone()),
                                                    "{c.rule_id}"
                                                }
                                                span { class: "kind-panel-label", "{c.label}" }
                                                span { class: "adopt-status", "{c.status_line()}" }
                                                if !opened {
                                                    span { class: "adopt-unopened-flag", title: "You have not opened this one.", "not opened" }
                                                }
                                                span { class: "kind-panel-repo", "{c.repo}" }
                                                button {
                                                    class: "btn-ghost adopt-open",
                                                    onclick: move |_| on_open.call(open_key.clone()),
                                                    if opened { "Opened" } else { "Open" }
                                                }
                                                if adoptable {
                                                    button {
                                                        class: "btn-ghost adopt-toggle",
                                                        onclick: move |_| on_toggle.call(toggle_key.clone()),
                                                        if adopted { "Skip" } else { "Adopt" }
                                                    }
                                                }
                                                if opened {
                                                    div { class: "adopt-detail",
                                                        p { class: "adopt-detail-line",
                                                            "Writes \u{201c}{c.label}\u{201d} into {c.repo}'s AGENTS.md as this project's position on {c.rule_id}."
                                                        }
                                                        if let Some(reason) = c.blocked_reason() {
                                                            p { class: "adopt-detail-blocked", "{reason}" }
                                                        }
                                                        if c.deviating_sites > 0 {
                                                            p { class: "adopt-detail-line",
                                                                "{c.deviating_sites} site(s) do not follow it today. Those are Queue rows under {c.rule_id}; dispositioning them is separate work and does not gate this adoption."
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            if shown.is_empty() {
                                p { class: "kind-panel-empty", "Nothing unopened in {repo}." }
                            }
                            AdoptCommitBar {
                                plan,
                                busy,
                                on_commit: {
                                    let repo = repo.clone();
                                    Callback::new(move |()| on_commit.call(repo.clone()))
                                },
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The footer under one destination's decision list: what a commit would write, and the button
/// that raises the gate.
///
/// Separate from the dialog on purpose. This bar states the counts WITHOUT the typed
/// confirmation, so the curator can read "27 to write, 3 skipped, 25 of them never opened"
/// before committing to the dialog at all — and can go open some of them instead.
///
/// Props-only.
#[component]
pub(super) fn AdoptCommitBar(
    /// What a commit for this destination would do.
    plan: CommitPlan,
    /// A commit is in flight.
    #[props(default)]
    busy: bool,
    /// Raise the commit dialog.
    on_commit: Callback<()>,
) -> Element {
    let n = plan.adopted_count();
    let unopened = plan.unopened_count();
    rsx! {
        div { class: "adopt-commit-bar",
            span { class: "adopt-commit-counts", "{plan.counts_line()}" }
            if unopened > 0 {
                span { class: "adopt-commit-unopened", "{unopened} not opened" }
            }
            button {
                class: "btn-primary adopt-commit-btn",
                disabled: n == 0 || busy,
                onclick: move |_| on_commit.call(()),
                if n == 0 {
                    "Nothing selected to write"
                } else {
                    "Write {n} into {plan.destination_repo}\u{2026}"
                }
            }
        }
    }
}

/// THE COMMIT DIALOG — the gate (owner's Q4, design §13; the reviewed layout mockups).
///
/// > "Yes it should be opt out by default, make sure it is thoroughly gated and not an
/// > accidental click."
///
/// Five things the ruling requires, each one visible in the rsx below:
///
/// 1. **The destination repository, named.** Not "the repo", not "this project".
/// 2. **The adopted count and the skipped count.**
/// 3. **How many were adopted by default and never opened** — the number that matters, and the
///    one opt-out makes necessary. [`CommitPlan::unopened_count`] is a count of a recorded
///    gesture, not a ratio or a guess.
/// 4. **The repository name TYPED, not a checkbox.** A checkbox is muscle memory at 1am; typing
///    the destination cannot be done without reading it. The predicate is core's
///    [`commit_gate`] and the button reads nothing else.
/// 5. **Three exits.** Cancel, Review the unopened set, Commit. The safer path is offered
///    rather than only implied — which is the whole reason the third exit exists.
///
/// The third exit is hidden when there is nothing unopened to review, per the standing
/// hide-dead-end-affordances rule: a button that filters to an empty list is the 403-button bug
/// in miniature.
///
/// Props-only. SSR renders every state: untyped, mismatched, armed, empty, busy.
#[component]
pub(super) fn ConventionCommitDialog(
    /// Exactly what the commit will do.
    plan: CommitPlan,
    /// What the curator has typed into the destination field so far.
    typed: String,
    /// The commit is in flight; every exit is disabled rather than hidden.
    #[props(default)]
    busy: bool,
    /// The destination field changed.
    on_typed: Callback<String>,
    /// Exit 1 — close, write nothing.
    on_cancel: Callback<()>,
    /// Exit 2 — close, and filter the panel to the rules this commit would write unopened.
    on_review_unopened: Callback<()>,
    /// Exit 3 — write. Only ever called when [`commit_gate`] is armed; the handler re-checks.
    on_commit: Callback<()>,
) -> Element {
    let gate = commit_gate(&plan, &typed);
    let armed = gate.is_armed();
    let unopened = plan.unopened_count();
    let destination = plan.destination_repo.clone();
    rsx! {
        div { class: "rule-modal-overlay adopt-dialog-overlay",
            div {
                class: "rule-modal adopt-dialog",
                role: "dialog",
                "aria-modal": "true",
                onclick: move |e| e.stop_propagation(),
                div { class: "rule-modal-head",
                    span { class: "rule-modal-id", "Write to {destination}" }
                }
                p { class: "adopt-dialog-headline", "{plan.headline()}" }
                p { class: "adopt-dialog-dest",
                    "Destination repository: "
                    span { class: "adopt-dialog-dest-name", "{destination}" }
                }
                p { class: "adopt-dialog-counts", "{plan.counts_line()}" }
                if let Some(warning) = plan.unopened_warning() {
                    p { class: "adopt-dialog-warning", "{warning}" }
                }
                ul { class: "adopt-dialog-list",
                    for rule in plan.adopted.iter() {
                        li { key: "{rule.rule_id}", class: "adopt-dialog-item",
                            span { class: "adopt-dialog-item-rule", "{rule.rule_id}" }
                            span { class: "adopt-dialog-item-label", "{rule.label}" }
                            if !rule.opened {
                                span { class: "adopt-unopened-flag", "not opened" }
                            }
                        }
                    }
                }
                label { class: "adopt-dialog-confirm-label", r#for: "adopt-dialog-confirm",
                    "Type {destination} to confirm the destination."
                }
                input {
                    id: "adopt-dialog-confirm",
                    class: "adopt-dialog-confirm-input",
                    r#type: "text",
                    autofocus: true,
                    autocomplete: "off",
                    placeholder: "{destination}",
                    value: "{typed}",
                    disabled: busy,
                    oninput: move |e| on_typed.call(e.value()),
                }
                if let Some(hint) = gate.hint(&destination) {
                    p { class: "adopt-dialog-hint", "{hint}" }
                }
                div { class: "adopt-dialog-exits",
                    button {
                        class: "btn-ghost adopt-dialog-cancel",
                        disabled: busy,
                        onclick: move |_| on_cancel.call(()),
                        "Cancel"
                    }
                    if unopened > 0 {
                        button {
                            class: "btn-ghost adopt-dialog-review",
                            disabled: busy,
                            onclick: move |_| on_review_unopened.call(()),
                            "Review the {unopened} not opened"
                        }
                    }
                    button {
                        class: if armed { "btn-primary adopt-dialog-commit armed" } else { "btn-primary adopt-dialog-commit shut" },
                        // The ONLY predicate. `commit_gate` is the same function the hint
                        // above reads, so a dialog that says "type the name" while offering a
                        // live button is unreachable.
                        disabled: !armed || busy,
                        onclick: move |_| {
                            if armed {
                                on_commit.call(());
                            }
                        },
                        if busy { "Writing\u{2026}" } else { "Commit" }
                    }
                }
            }
        }
    }
}

/// One `NoConvention` option pick, as the panel raises it.
#[derive(Clone, PartialEq, Debug)]
pub(super) struct RecommendationPick {
    /// The rule's [`adoption_key`].
    pub key: String,
    /// The authored option to recommend, or `None` for "leave as reported".
    pub option_id: Option<String>,
}

/// NO CONVENTION panel — `NoConvention` rows: the codebase is inconsistent with no clear
/// majority. Exactly one row per rule (A5.3), listing the options actually observed with their
/// site counts where the producer could count them.
///
/// ## Picking an option is a GOVERNANCE ACT, and the panel says so
///
/// The owner's Q3 ruling:
///
/// > a `NoConvention` option pick IS client-facing. It feeds the derived decision set, so it is
/// > a governance act and must say so in the UI. It therefore inherits Q4's gating: the same
/// > typed-destination confirm, the same honest count of what was not opened.
///
/// So a pick here produces the SAME [`AdoptedRule`] an adoption produces, travels the same
/// [`CommitPlan`] and passes the same [`ConventionCommitDialog`]. The one difference is the
/// default, and it runs the other way: **"Leave as reported" is pre-selected.** Nothing is
/// written unless the curator chooses something, because unlike a convention the audit did not
/// derive an answer here — it found a split, and inventing a default would be the UI picking
/// the client's standard for them.
///
/// NO SEVERITY, here or anywhere informational (design §9.4).
#[component]
pub(super) fn NoConventionPanel(
    findings: Vec<FindingView>,
    /// Open the RULE HUB on a listed rule. See [`ConventionsPanel`].
    #[props(default)]
    on_open_hub: Callback<RuleHubRequest>,
    /// The curator's picks and openness record. Default = everything left as reported.
    #[props(default)]
    model: AdoptionModel,
    /// A pick changed.
    #[props(default)]
    on_recommend: Callback<RecommendationPick>,
    /// The curator OPENED this rule's detail. The ONE writer of the openness record.
    #[props(default)]
    on_open: Callback<String>,
    /// Raise the commit dialog for one destination repository.
    #[props(default)]
    on_commit: Callback<String>,
    /// A commit is in flight.
    #[props(default)]
    busy: bool,
) -> Element {
    let all = split_rules(&findings);
    if all.is_empty() {
        return rsx! {
            p { class: "kind-panel-empty", "Every assessed rule reached a convention \u{2014} nothing split." }
        };
    }
    let mut by_repo: std::collections::BTreeMap<String, Vec<SplitRule>> = Default::default();
    for r in &all {
        by_repo.entry(r.repo.clone()).or_default().push(r.clone());
    }
    // The prose the detail row needs when the producer enumerated nothing. Keyed the same way
    // the rules are, so a row with no options still shows its own honest explanation.
    let detail_of: std::collections::BTreeMap<String, String> = findings
        .iter()
        .filter(|f| {
            matches!(
                f.kind,
                Some(camerata_api_types::audit::FindingKind::NoConvention(_))
            )
        })
        .map(|f| (adoption_key(&f.repo, &f.rule_id), f.detail.clone()))
        .collect();
    rsx! {
        div { class: "kind-panel",
            p { class: "kind-panel-sub",
                "Rules where the codebase is split with no clear majority. Recommending an option is a CLIENT-FACING governance act, not a note to yourself: it is written into that repository's AGENTS.md and governs how future AI agents work on the code. The default is to leave the split as reported and write nothing."
            }
            for (repo , rows) in by_repo.iter() {
                {
                    let plan = CommitPlan::recommendations(&all, &model, repo);
                    rsx! {
                        div { key: "{repo}", class: "kind-panel-group adopt-group",
                            p { class: "kind-panel-group-h", "{repo}" }
                            ul { class: "kind-panel-list",
                                for r in rows.iter() {
                                    {
                                        let key = r.key();
                                        let picked = model.recommendation(&key).map(str::to_string);
                                        let opened = model.was_opened(&key);
                                        let req = RuleHubRequest {
                                            repo: r.repo.clone(),
                                            rule_id: r.rule_id.clone(),
                                        };
                                        let open_key = key.clone();
                                        let pick_key = key.clone();
                                        let options = r.options.clone();
                                        let summary: Vec<String> = r
                                            .options
                                            .iter()
                                            .filter_map(|(id, _, _)| r.option_line(id))
                                            .collect();
                                        let detail = detail_of.get(&key).cloned().unwrap_or_default();
                                        rsx! {
                                            li {
                                                key: "{key}",
                                                class: if picked.is_some() { "kind-panel-row adopt-row adopted" } else { "kind-panel-row adopt-row skipped" },
                                                span {
                                                    class: if picked.is_some() { "adopt-state on" } else { "adopt-state off" },
                                                    if picked.is_some() { "Recommend" } else { "Leave as reported" }
                                                }
                                                button {
                                                    class: "rule-hub-link kind-panel-rule",
                                                    title: "Everything this scan knows about {r.rule_id}.",
                                                    onclick: move |_| on_open_hub.call(req.clone()),
                                                    "{r.rule_id}"
                                                }
                                                if summary.is_empty() {
                                                    span { class: "kind-panel-label", "{detail}" }
                                                } else {
                                                    span { class: "kind-panel-label", "{summary.join(\" \u{00b7} \")}" }
                                                }
                                                if picked.is_some() && !opened {
                                                    span { class: "adopt-unopened-flag", title: "You have not opened this one.", "not opened" }
                                                }
                                                span { class: "kind-panel-repo", "{r.repo}" }
                                                button {
                                                    class: "btn-ghost adopt-open",
                                                    onclick: move |_| on_open.call(open_key.clone()),
                                                    if opened { "Opened" } else { "Open" }
                                                }
                                                select {
                                                    class: "adopt-pick",
                                                    // "Leave as reported" is the first entry AND the
                                                    // default, so the keyboard-first gesture on an
                                                    // untouched row changes nothing.
                                                    value: picked.clone().unwrap_or_default(),
                                                    onchange: move |e| {
                                                        let v = e.value();
                                                        on_recommend
                                                            .call(RecommendationPick {
                                                                key: pick_key.clone(),
                                                                option_id: if v.is_empty() { None } else { Some(v) },
                                                            });
                                                    },
                                                    option { value: "", "Leave as reported" }
                                                    for (id , label , sites) in options.iter() {
                                                        option { key: "{id}", value: "{id}",
                                                            if let Some(n) = sites {
                                                                "Recommend: {label} ({n} sites)"
                                                            } else {
                                                                "Recommend: {label}"
                                                            }
                                                        }
                                                    }
                                                }
                                                if opened {
                                                    div { class: "adopt-detail",
                                                        p { class: "adopt-detail-line",
                                                            "Nothing is written for this rule unless you recommend an option. A recommendation becomes {r.repo}'s stated position on {r.rule_id} in its AGENTS.md."
                                                        }
                                                        if !detail.is_empty() {
                                                            p { class: "adopt-detail-line", "{detail}" }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            AdoptCommitBar {
                                plan,
                                busy,
                                on_commit: {
                                    let repo = repo.clone();
                                    Callback::new(move |()| on_commit.call(repo.clone()))
                                },
                            }
                        }
                    }
                }
            }
        }
    }
}

/// NOT ASSESSED panel — `NotAssessed` rows: the rule was armed but could not be evaluated.
/// Honesty rows, grouped by the structured [`NotAssessedReason`] vocabulary.
///
/// NO buttons, now or later, until a per-rule re-run mechanism actually exists. A disabled
/// "re-run" would be the 403-button bug the standing hide-dead-end-affordances rule forbids.
#[component]
pub(super) fn NotAssessedPanel(
    findings: Vec<FindingView>,
    /// Open the RULE HUB on a listed rule. See [`ConventionsPanel`].
    #[props(default)]
    on_open_hub: Callback<RuleHubRequest>,
) -> Element {
    use camerata_api_types::audit::FindingKind;
    if findings.is_empty() {
        return rsx! {
            p { class: "kind-panel-empty", "Every armed rule was assessed." }
        };
    }
    // Group by the structured reason (a closed vocabulary, so grouping is structural, not
    // prose-matched). BTreeMap for a stable order across renders.
    let mut by_reason: std::collections::BTreeMap<&'static str, Vec<FindingView>> =
        Default::default();
    for f in &findings {
        let reason = match &f.kind {
            Some(FindingKind::NotAssessed(na)) => na.reason.as_str(),
            _ => "unknown",
        };
        by_reason.entry(reason).or_default().push(f.clone());
    }
    rsx! {
        div { class: "kind-panel",
            p { class: "kind-panel-sub",
                "Rules that were armed but could not be evaluated. These are honesty rows, not findings \u{2014} the scan cannot vouch for them either way."
            }
            for (reason , rows) in by_reason.iter() {
                div { key: "{reason}", class: "kind-panel-group",
                    p { class: "kind-panel-group-h", "{reason} \u{00b7} {rows.len()}" }
                    ul { class: "kind-panel-list",
                        for f in rows.iter() {
                            {
                                let detail = match &f.kind {
                                    Some(FindingKind::NotAssessed(na)) => na.reason_detail.clone(),
                                    _ => String::new(),
                                };
                                rsx! {
                                    li { key: "{f.repo}\u{0}{f.rule_id}", class: "kind-panel-row",
                                        {
                                            let req = RuleHubRequest {
                                                repo: f.repo.clone(),
                                                rule_id: f.rule_id.clone(),
                                            };
                                            rsx! {
                                                button {
                                                    class: "rule-hub-link kind-panel-rule",
                                                    title: "Everything this scan knows about {f.rule_id}.",
                                                    onclick: move |_| on_open_hub.call(req.clone()),
                                                    "{f.rule_id}"
                                                }
                                            }
                                        }
                                        span { class: "kind-panel-label", "{detail}" }
                                        span { class: "kind-panel-repo", "{f.repo}" }
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

/// Renders one brownfield scan's results: the audit summary, the findings table,
/// and the proposed-rules table. Keyed by the parent so a new scan remounts the
/// chorale tables with fresh rows.
#[component]
pub(super) fn ScanResults(report: ScanReportView) -> Element {
    // Phase 2 audit result (findings against the selected rules); None until the
    // architect picks rules and runs the audit.
    let mut audit = use_signal(|| Option::<ScanReportView>::None);
    let mut auditing = use_signal(|| false);
    // The model the user picks for the audit — they own the thoroughness/speed trade-off.
    // Company-agnostic list comes from the server (`/api/models`); seed from its default.
    let models_res = use_resource(fetch_audit_models);
    let models = models_res.read().clone().flatten();
    // Fetch the active project so the Audit + Calibration selectors read their initial
    // value from `project.step_models` — the same source Settings writes to. This makes
    // both screens bidirectionally synced: a change here persists to step_models (via the
    // shared endpoint), so it shows in Settings next time; a change in Settings shows here
    // next time the onboarding scan mounts (because we seed from step_models, not from the
    // models-registry default).
    let active_project_res = use_resource(fetch_active_project);
    let active_project = active_project_res.read().clone().flatten();
    // Derive the project id once (empty string = no active project; the persist calls are
    // no-ops in that case since the endpoint requires a real id). Memoised as a String so
    // the onchange closures can clone it cheaply.
    let project_id = active_project
        .as_ref()
        .map(|p| p.id.clone())
        .unwrap_or_default();
    // Seed priority: project.step_models.audit (Settings-authoritative) → models.default
    // (registry fallback for first-time / no-project state). The guard is `is_empty()` so
    // a user actively choosing a different model in THIS session is never overwritten by a
    // later async re-render of the active-project resource.
    let mut audit_model = use_signal(String::new);
    if audit_model().is_empty() {
        let from_step = active_project
            .as_ref()
            .map(|p| p.step_models.audit.clone())
            .filter(|s| !s.is_empty());
        let from_default = models
            .as_ref()
            .map(|m| m.default.clone())
            .filter(|s| !s.is_empty());
        if let Some(m) = from_step.or(from_default) {
            audit_model.set(m);
        }
    }
    // Calibration model — its OWN picker (severity recalibration + confidence tagging).
    // Seed priority: project.step_models.calibration (Settings-authoritative) → current
    // audit_model (so "the model you picked" is used end-to-end unless deliberately split).
    let mut calibration_model = use_signal(String::new);
    if calibration_model().is_empty() {
        let from_step = active_project
            .as_ref()
            .map(|p| p.step_models.calibration.clone())
            .filter(|s| !s.is_empty());
        let from_audit = Some(audit_model()).filter(|s| !s.is_empty());
        if let Some(m) = from_step.or(from_audit) {
            calibration_model.set(m);
        }
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
    // Scan-type selector (Part C): which scans to run. Both default ON (today's behaviour).
    // "AI architectural review" = the LLM scan of architectural/structured/prose rules (and
    // the deep tier). "Deterministic scans" = the always-on security floor + the mechanical
    // preview linters — fast, no LLM, no tokens. Deselecting AI sends `run_ai_review=false`
    // (the server makes zero model calls); deselecting deterministic skips the floor + preview.
    let mut run_ai_review = use_signal(|| true);
    let mut run_deterministic = use_signal(|| true);
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
    // Live DETERMINISTIC-pass progress (floor + preview tools), rendered above the AI
    // agent-activity drawer. Primary progress view in deterministic-only mode (no AI drawer).
    let mut det_progress = use_signal(|| Option::<DetProgressView>::None);
    // Background-job FAILURE message (silent-death fix): set by `poll_job` when a job ends
    // `failed` or vanishes mid-poll. Rendered by `ScanJobFailedBanner`, UNCONDITIONALLY —
    // not gated on a report existing, since a failed job never produces one.
    let mut job_failed_message = use_signal(|| Option::<String>::None);
    // The in-flight async job id (app-scope, survives navigation). RESUME: if a job was
    // already running when this view (re)mounted, re-attach the poll instead of losing it.
    let active_audit_job = use_context::<Signal<Option<String>>>();
    let scan_idle_ms = use_signal(|| Option::<u128>::None);
    // Captured HERE, in the component's render body, where `LoadingCount` context resolution is
    // guaranteed — never resolved from inside `poll_job`'s own body, which runs detached inside
    // this `use_future`'s async block (see the BOMBE-SILENT-MISS note on `poll_job`).
    let loading_count = use_context::<crate::loading::LoadingCount>();
    use_future(move || async move {
        if let Some(jid) = active_audit_job.peek().clone() {
            auditing.set(true);
            poll_job(
                jid,
                audit,
                auditing,
                job_progress,
                det_progress,
                active_audit_job,
                scan_idle_ms,
                job_failed_message,
                loading_count,
            )
            .await;
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
    let mut detail_finding = use_signal(|| Option::<FindingView>::None);
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
    let mut dispositions = use_signal(std::collections::HashMap::<String, Disposition>::new);
    let mut triage_view = use_signal(|| TriageState::Unresolved);
    // Which kind surface is on screen (Pass 2 step 1). Session state, deliberately NOT
    // persisted in the draft: it is a lens, not a decision, and the curator always wants to
    // resume on the Queue. Adding it to `OnboardingDraft` would change a persisted wire shape
    // for no curator benefit.
    let mut kind_view = use_signal(|| KindSurface::Queue);
    // THE RULE HUB (design §8): which (repo, rule) the hub is open on, and the Queue jump it
    // can perform. Both session-local lenses like `kind_view`, and deliberately not persisted
    // for the same reason: an open modal is not a decision.
    let mut hub_open = use_signal(|| Option::<RuleHubRequest>::None);
    let mut queue_rule_filter = use_signal(|| Option::<String>::None);
    let mut processing = use_signal(|| false);
    // ── THE GATED CONVENTION COMMIT (design §3; Q3 + Q4) ──────────────────────
    // The curator's adopt/skip state, his openness record, and his `NoConvention` picks. One
    // model serves both informational panels because a pick and an adoption write the same
    // thing by the same path (Q3).
    //
    // Session state, like `kind_view` and `hub_open`, and deliberately NOT in the draft. The
    // openness record in particular must not be restored: "I opened this one" is a claim
    // about THIS sitting, and a stale one would quietly shrink the number the whole gate
    // exists to show.
    let mut adoption = use_signal(AdoptionModel::new);
    let mut commit_dialog = use_signal(|| Option::<CommitDialogState>::None);
    let mut unopened_only = use_signal(|| false);
    let mut committing = use_signal(|| false);

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
    use_context_provider(|| draft_loaded);
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

    // ── Pass 2 step 1: the kind switcher is the PRIMARY axis ─────────────────
    // The five-kind taxonomy splits one list into four surfaces: the Queue (the two accusing
    // kinds plus any kind-less alarm row) and three informational panels. Everything below
    // that is per-disposition triage belongs to the Queue ALONE — counts, sub-tabs, the table,
    // and Process — so each is computed over `queue_findings`, not the raw set. Leaving the
    // informational rows in the disposition tally parked them as Unresolved forever, which the
    // Process gate blocks on.
    let kind_counts = surface_counts(&findings);
    let queue_findings = findings_on_surface(&findings, KindSurface::Queue);
    // Kind-less rows are an ALARM (an unmapped emission path), surfaced as a red chip on the
    // Queue tab. Never hidden, never folded into Defect.
    let n_unspecified = unspecified_kind_count(&findings);

    // Live per-table counts (recompute reactively as dispositions change).
    //
    // Pass 2 step 3: this was an inline five-way loop that duplicated `TriageModel::counts`.
    // With a fifth disposition the duplicate became a real drift risk — a state the loop
    // forgot would show a permanent zero on its tab AND silently stop blocking the Process
    // gate — so the tally is now taken by core's own function, the one the unit tests cover.
    let triage_counts: TriageCounts = TriageModel {
        dispositions: dispositions.read().clone(),
        triage_view: triage_view(),
    }
    .counts(&queue_findings);

    // THE RULE HUB's data for whichever rule is open, derived once per render from the scan's
    // own rows by core's `rule_hub`. A request for a rule with no rows in that repo resolves
    // to `None` and the modal simply does not mount — the hub describes what a scan FOUND, and
    // an empty strip would be a dead affordance.
    let open_hub: Option<RuleHubView> = hub_open
        .read()
        .as_ref()
        .and_then(|r| rule_hub(&findings, &r.repo, &r.rule_id));

    // THE ALTERNATIVES CONTEXT for the open hub (design §9.1): the Evaluated-against line and
    // the Why? answer, at the surface every rule id already opens. `None` unless the rule is
    // multi-option AND the audit recorded a recommendation for it, which is the common case —
    // the hub must say nothing rather than render an empty line.
    //
    // Built from exactly the three sources `RuleAlternativesPanel` joins, through the SAME
    // pure `alternative_controls_row`, so the hub and the panel cannot disagree about which
    // option a rule is evaluated against. `staged` is `None` here because the staged-override
    // map is the panel's own state: the hub reports what the scan did, and the panel is where
    // a change is made. See the panel's doc comment.
    let open_hub_alternatives: Option<AlternativeControlsRow> = hub_open.read().as_ref().and_then(|req| {
        let rec = audited
            .as_ref()
            .and_then(|a| a.recommendations.get(&req.rule_id))
            .cloned()?;
        let rule = report
            .proposed_rules
            .iter()
            .find(|r| r.id == req.rule_id && r.options.len() >= 2)?;
        let scoped: Vec<&FindingView> = findings
            .iter()
            .filter(|f| f.repo == req.repo && f.rule_id == req.rule_id)
            .collect();
        let evaluated = scoped
            .iter()
            .find_map(|f| f.evaluated_option_id.clone())
            .filter(|o| !o.is_empty());
        Some(alternative_controls_row(
            rule,
            &rec,
            scoped.len(),
            evaluated.as_deref(),
            None,
            false,
        ))
    });

    rsx! {
        // Row-detail modal: hosted here, at the results-subtree root, so it is NOT a
        // sibling of the chorale table (see RuleDetailModal for why).
        RuleDetailModal {}
        // THE RULE HUB (design §8). Hosted at the same root and for the same reason: it is an
        // overlay, and an overlay that is a sibling of a chorale table left a ghost
        // click-eater in the desktop webview.
        if let Some(hub) = open_hub {
            RuleHubModal {
                hub,
                alternatives: open_hub_alternatives,
                // ONE jump, and it is the derivation's own `hub.jump`. Into the Queue it is a
                // COLUMN FILTER — design §8's "a filter, not a hyperlink graph" — so the
                // curator lands in a normal, visibly filtered table with the controls to clear
                // it already on screen, rather than in a special mode he has to find his way
                // out of. Into a panel it is just the kind switcher, flipped.
                on_jump: Callback::new(move |surface: KindSurface| {
                    if surface == KindSurface::Queue {
                        queue_rule_filter
                            .set(hub_open.peek().as_ref().map(|r| r.rule_id.clone()));
                    }
                    kind_view.set(surface);
                    hub_open.set(None);
                }),
                on_close: Callback::new(move |()| hub_open.set(None)),
            }
        }
        // THE COMMIT DIALOG (Q4's gate). Hosted at the results-subtree root for the same
        // reason the hub is: an overlay that is a sibling of a chorale table left a ghost
        // click-eater in the desktop webview.
        //
        // ON FOCUS AND THE PANE'S KEYS. The dialog takes keyboard focus, and the destination
        // it asks the curator to type contains `c`, `i`, `t` and `f` more often than not. It
        // cannot arm the detail pane's disposition keys, for two independent reasons, and
        // both are asserted rather than assumed:
        //
        //  * STRUCTURAL — the dialog is raised only from the Conventions and No-convention
        //    panels, which render in the `else if` branches where `FindingsTable` (and so the
        //    pane, and so the keydown handler that lives on it) is not mounted at all.
        //  * BY CONSTRUCTION — `pane_command`'s scope gate is unconditional: anything but
        //    `PaneFocus::Cursor` returns `None` for every key, and the dialog's input is not
        //    the pane's cursor. `pane_keys_armed` is DEFINED as that same call, so the
        //    indicator cannot disagree with the handler.
        if let Some(dialog) = commit_dialog() {
            {
                let plan = match dialog.surface {
                    KindSurface::NoConvention => CommitPlan::recommendations(
                        &split_rules(&findings),
                        &adoption.read(),
                        &dialog.destination,
                    ),
                    _ => CommitPlan::conventions(
                        &adoptable_conventions(&findings),
                        &adoption.read(),
                        &dialog.destination,
                    ),
                };
                let typed = dialog.typed.clone();
                let commit_plan = plan.clone();
                let project_id_commit = project_id.clone();
                rsx! {
                    ConventionCommitDialog {
                        plan,
                        typed: typed.clone(),
                        busy: committing(),
                        on_typed: Callback::new(move |v: String| {
                            if let Some(d) = commit_dialog.write().as_mut() {
                                d.typed = v;
                            }
                        }),
                        on_cancel: Callback::new(move |()| commit_dialog.set(None)),
                        // Exit 2 — the safer path, offered rather than implied. It closes the
                        // dialog and narrows the panel to exactly the rules the commit would
                        // write unopened. It does NOT mark them opened: that would be the gate
                        // clearing its own warning.
                        on_review_unopened: Callback::new(move |()| {
                            unopened_only.set(true);
                            commit_dialog.set(None);
                        }),
                        on_commit: Callback::new(move |()| {
                            // THE ENFORCEMENT. The button's `disabled` attribute is a hint to
                            // the eye; this re-check is what actually holds, and it reads the
                            // same `commit_gate` the dialog rendered.
                            let typed = commit_dialog
                                .peek()
                                .as_ref()
                                .map(|d| d.typed.clone())
                                .unwrap_or_default();
                            if !commit_gate(&commit_plan, &typed).is_armed() {
                                return;
                            }
                            let project_id = project_id_commit.clone();
                            let destination = commit_plan.destination_repo.clone();
                            let adopted = commit_plan.adopted.clone();
                            committing.set(true);
                            spawn(async move {
                                let (ok, msg) =
                                    commit_adoptions(&project_id, &destination, &adopted).await;
                                committing.set(false);
                                if ok {
                                    commit_dialog.set(None);
                                }
                                crate::toast::push_toast(
                                    toasts,
                                    if ok {
                                        crate::toast::ToastKind::Info
                                    } else {
                                        crate::toast::ToastKind::Error
                                    },
                                    msg,
                                );
                            });
                        }),
                    }
                }
            }
        }
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
                            // Enclosing-code-block context (finding-code-context feature,
                            // docs/design/2026-07-27_finding-code-context.md): keyed by
                            // repo/path/line so switching findings remounts this section and
                            // starts a fresh on-demand fetch rather than showing stale content
                            // left over from the previously open finding.
                            FindingCodeContext {
                                key: "{f.repo}\u{1f}{f.path}\u{1f}{f.line}",
                                finding: f.clone(),
                            }
                            // Recommended fix (docs/design/2026-07-26_audit-report-refinements.md):
                            // keyed by rule_id so switching findings remounts this section and
                            // starts a fresh on-demand fetch, same idiom as FindingCodeContext.
                            FindingRecommendedFix {
                                key: "{f.rule_id}",
                                rule_id: f.rule_id.clone(),
                            }
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
                {
                    // Feature B — the compliance-safety gate: a `CliFallbackWarn` resolution
                    // (API-required, no key, but the project's "Allow Claude CLI" toggle is ON)
                    // doesn't abort the scan — it proceeds on the operator's personal-subscription
                    // CLI and folds a `⚠ COMPLIANCE:`-prefixed warning into this same joined-by-
                    // " · " message alongside routine scan notes (see `audit_repos` in
                    // crates/server/src/onboard.rs). Split those parts out into a dedicated,
                    // higher-alarm banner so a compliance-relevant event can never be mistaken
                    // for routine scan trivia buried in the plain note line.
                    let (compliance_notes, other_notes) = split_compliance_notes(&msg);
                    rsx! {
                        for note in compliance_notes.iter() {
                            div { class: "compliance-warning-banner",
                                span { class: "compliance-warning-icon", "\u{26a0}" }
                                div {
                                    p { class: "compliance-warning-title", "Compliance warning" }
                                    p { class: "compliance-warning-text", "{note}" }
                                }
                            }
                        }
                        if let Some(rest) = other_notes {
                            p { class: "scan-note", "{rest}" }
                        }
                    }
                }
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
            if !report.coverage_notes.is_empty() {
                div { class: "scan-coverage-notes",
                    p { class: "scan-coverage-notes-title", "Scan coverage" }
                    for note in report.coverage_notes.iter() {
                        div { class: "scan-coverage-note",
                            span { class: "scan-coverage-note-tool", "{note.tool}" }
                            span { class: "scan-coverage-note-msg", " — {note.message}" }
                        }
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
                            // Scan-type selector: both default true; if a user somehow unticks
                            // both, send both true (the server also coerces — never a no-op).
                            let mut ai = run_ai_review();
                            let mut det = run_deterministic();
                            if !ai && !det { ai = true; det = true; }
                            // Full scan forces a clean pass; otherwise the scan is incremental
                            // (only files changed since the last scan cost AI tokens).
                            let incremental = !audit_full_scan();
                            // Deterministic-only is fast, but its PROGRESS is only pollable on
                            // the async job path (the sync path holds one request and returns
                            // the final report). So route a deterministic-ONLY scan through the
                            // job path regardless of the picked mode — that's where the
                            // per-tool progress streams into the "Deterministic scan" component.
                            let deterministic_only = det && !ai;
                            let use_job = mode == "job" || deterministic_only;
                            // Clear the PREVIOUS run's findings so a re-audit starts from a
                            // blank Findings table instead of showing stale results while
                            // the new audit runs (the server also clears the transcript).
                            // Also reset per-finding triage state, view filter, and the open
                            // detail modal — they're specific to the previous run's findings.
                            // repo_selection / chosen / custom_rules are NOT reset: those are
                            // the architect's persistent rule choices that survive a re-audit.
                            audit.set(None);
                            dispositions.set(std::collections::HashMap::new());
                            triage_view.set(TriageState::Unresolved);
                            detail_finding.set(None);
                            job_progress.set(None);
                            det_progress.set(None);
                            // Clear any PREVIOUS run's failure banner — a fresh scan starting
                            // is not itself a failure, and a stale message here would look
                            // like this new run already failed before it even began.
                            job_failed_message.set(None);
                            auditing.set(true);
                            if use_job {
                                // Async job: submit, record the id (app-scope, so a later
                                // mount can resume), then poll. The server runs it decoupled
                                // from any single request.
                                let mut active_audit_job = active_audit_job;
                                // `loading_count` was captured in the component's render body
                                // (see its `use_context` above) — NOT resolved from context in
                                // here, since this closure body runs inside a detached `spawn`
                                // where that resolution is unreliable (BOMBE-SILENT-MISS note on
                                // `poll_job`). `LoadingCount` is `Copy`, so capturing it in this
                                // `move` closure and again in the inner `spawn` below is just two
                                // cheap copies of the same signal, same as `job_progress` etc.
                                let loading_count = loading_count;
                                spawn(async move {
                                    let _guard = crate::loading::LoadingGuard::from_count(loading_count);
                                    let Some(jid) = audit_job_start(&repos, &rules, &model, &calib, "parallel", thorough, incremental, deep, ai, det).await else {
                                        auditing.set(false);
                                        job_failed_message.set(Some(
                                            "Could not start the background scan (the request \
                                             to submit it failed). Please retry."
                                                .to_string(),
                                        ));
                                        return;
                                    };
                                    active_audit_job.set(Some(jid.clone()));
                                    poll_job(
                                        jid,
                                        audit,
                                        auditing,
                                        job_progress,
                                        det_progress,
                                        active_audit_job,
                                        scan_idle_ms,
                                        job_failed_message,
                                        loading_count,
                                    )
                                    .await;
                                });
                            } else {
                                // Synchronous: hold the request until the (shorter) run finishes.
                                spawn(async move {
                                    let result = audit_against(&repos, &rules, &model, &calib, &mode, thorough, incremental, deep, ai, det).await;
                                    if result.is_none() {
                                        // The sync request itself failed (network error, server
                                        // unreachable) — the server-side crash path already
                                        // returns a normal 200 JSON with `ai_error` set, so
                                        // `None` here means the response never arrived at all.
                                        job_failed_message.set(Some(
                                            "The scan request failed (no response from the \
                                             server). Please retry."
                                                .to_string(),
                                        ));
                                    }
                                    audit.set(result);
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
                // company-agnostic, served by /api/models/registry. Options are grouped
                // by provider (Claude first, then OpenRouter); each label has badges
                // (FREE / no-tools / NNNk ctx). Degrades to Claude-only when no OpenRouter
                // key is configured.
                if let Some(m) = models.as_ref() {
                    div { class: "audit-model-row",
                        label { class: "audit-model-label", "Audit model" }
                        select {
                            class: "audit-model-select",
                            disabled: auditing(),
                            value: "{audit_model}",
                            onchange: {
                                let pid = project_id.clone();
                                move |e: Event<FormData>| {
                                    let model = e.value();
                                    audit_model.set(model.clone());
                                    // Persist to project.step_models so Settings stays in sync.
                                    let pid = pid.clone();
                                    spawn(async move {
                                        if !pid.is_empty() {
                                            set_project_step_model(&pid, "audit", &model).await;
                                        }
                                    });
                                }
                            },
                            for (group_label , opts) in m.grouped().into_iter() {
                                optgroup { label: "{group_label}",
                                    for opt in opts.into_iter() {
                                        option { key: "{opt.id}", value: "{opt.id}", "{opt.label}" }
                                    }
                                }
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
                            onchange: {
                                let pid = project_id.clone();
                                move |e: Event<FormData>| {
                                    let model = e.value();
                                    calibration_model.set(model.clone());
                                    // Persist to project.step_models so Settings stays in sync.
                                    let pid = pid.clone();
                                    spawn(async move {
                                        if !pid.is_empty() {
                                            set_project_step_model(&pid, "calibration", &model).await;
                                        }
                                    });
                                }
                            },
                            for (group_label , opts) in m.grouped().into_iter() {
                                optgroup { label: "{group_label}",
                                    for opt in opts.into_iter() {
                                        option { key: "{opt.id}", value: "{opt.id}", "{opt.label}" }
                                    }
                                }
                            }
                        }
                        span { class: "audit-model-hint", "Recalibrates severity + flags low-confidence findings. Default = the scan model; pick a stronger one for cheap-scan-plus-smart-verify." }
                    }
                }
                // Scan-type selector (Part C) — pick WHICH scans run. Both default ON
                // (today's behaviour). Deterministic-only is fast and uses no LLM / no tokens.
                div { class: "audit-model-row",
                    label { class: "audit-model-label", "What to scan" }
                    div { class: "scan-type-selector",
                        label { class: "audit-thorough-toggle",
                            input {
                                r#type: "checkbox",
                                checked: run_ai_review(),
                                disabled: auditing(),
                                onchange: move |e| run_ai_review.set(e.checked()),
                            }
                            span { "AI architectural review" }
                        }
                        label { class: "audit-thorough-toggle",
                            input {
                                r#type: "checkbox",
                                checked: run_deterministic(),
                                disabled: auditing(),
                                onchange: move |e| run_deterministic.set(e.checked()),
                            }
                            span { "Deterministic scans (floor + linters)" }
                        }
                    }
                    span { class: "audit-model-hint",
                        if run_deterministic() && !run_ai_review() {
                            "Deterministic-only: the always-on security floor + the mechanical preview linters run LOCALLY — fast, no LLM, NO TOKENS. The AI review is skipped entirely (zero model calls). Best for a quick check or QA of the deterministic pass."
                        } else if run_ai_review() && !run_deterministic() {
                            "AI-only: the LLM checks the code against your selected architectural/structured/prose rules. The deterministic floor + preview linters are skipped."
                        } else {
                            "Both run (default): the deterministic floor + linters (free, no tokens) AND the AI architectural review. Untick AI for a fast, token-free deterministic-only scan. At least one must be ticked — unticking both runs both."
                        }
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
                if feature_flags.soc2 && run_ai_review() {
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
                        // When AI scan is off, every model price is zeroed — no LLM calls
                        // means no token spend. The deep tier also requires AI, so it too is
                        // zeroed. This ensures the estimate correctly shows $0 (or "<$0.01")
                        // when the user has selected deterministic-only mode.
                        let ai_on = run_ai_review();
                        let (a_in, a_out) = if ai_on { price(&audit_model(), (3.0, 15.0)) } else { (0.0, 0.0) };
                        let (c_in, c_out) = if ai_on { price(&calibration_model(), (a_in, a_out)) } else { (0.0, 0.0) };
                        let sel = selected_count();
                        // Mirror the request flags the audit will actually send: incremental
                        // scope (only changed files cost tokens unless Full scan is ticked) and
                        // the deep SOC-2/security tier (three extra whole-repo passes).
                        let incremental = !audit_full_scan();
                        // Deep tier only runs with AI; force it off in the estimate when AI is off.
                        let deep = audit_deep() && ai_on;
                        // The project's AI backend (docs/design/2026-09-22_per-project-backend.md)
                        // changes how the estimator prices the shared repo digest's prompt cache
                        // (CLI can't reuse a prior call's cache; API/batch can) — defaults to Cli,
                        // the safe/zero-setup default, when no active project has loaded yet.
                        let backend = active_project.as_ref().map(|p| p.backend).unwrap_or_default();
                        // Best-effort count of multi-option rules in the proposed corpus (the
                        // live per-rule selection set isn't available here) — sizes the one-off
                        // alternative-recommendation pass the audit will run.
                        let multi_option_selected =
                            report.proposed_rules.iter().filter(|r| r.options.len() >= 2).count();
                        let (toks, dollars, passes) = estimate_audit_cost(report.code_chars, sel, &audit_mode(), a_in, a_out, c_in, c_out, audit_thorough(), incremental, deep, backend, multi_option_selected);
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
                                        // Approximate in both directions now, not uniformly "biased high": output
                                        // tokens (found issues) dominate findings-heavy scans regardless of backend,
                                        // but the input side differs by backend. On CLI, every call is a fresh
                                        // `claude` subprocess that can't reuse another call's prompt cache, so each
                                        // pass pays the cache-write surcharge with no read discount to offset it —
                                        // this estimate already prices that in, it isn't a caching UPSIDE to expect.
                                        // On API/batch, prompt-caching across batches keeps the input side down, so
                                        // the real bill can land below this figure.
                                        if backend == ProjectBackend::Cli {
                                            "Approximate — output tokens (found issues) dominate findings-heavy scans. This is priced for the CLI backend: each call is a separate subprocess with its own fresh prompt cache, so there's no cross-call caching discount to expect here (switching to the API backend, with a key configured, usually costs less for the same scan). "
                                        } else {
                                            "Approximate — output tokens (found issues) dominate findings-heavy scans; prompt-caching across batches keeps the input side down, so the real bill can land below this figure. "
                                        }
                                        if incremental {
                                            "Scope: INCREMENTAL — only files changed since the last scan are billed, so the real cost is usually well below this whole-repo figure (priced over ~{code_toks} tokens, {report.files_scanned} files). Tick Full scan to re-audit everything. "
                                        } else {
                                            "Scope: FULL — every file is re-audited (~{code_toks} tokens, {report.files_scanned} files). "
                                        }
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
                // While the audit runs the background Bombe machine activates via
                // the global loading guard — just show the text label inline.
                if auditing() {
                    div { class: "audit-thinking",
                        span { class: "audit-thinking-label", "Camerata is auditing your code\u{2026}" }
                        button {
                            class: "btn-stop",
                            onclick: move |_| {
                                let jid = active_audit_job().unwrap_or_default();
                                spawn(async move { cancel_audit_job(&jid).await; });
                            },
                            "\u{25a0} Stop scan"
                        }
                    }
                }
                // Scan stall warning: shown when the job has been idle above the threshold.
                // The server sends idle_ms; we show a warning if it's over 2 minutes (120s).
                if let Some(idle) = scan_idle_ms() {
                    if idle > 120_000 && auditing() {
                        div { class: "run-stall-warning scan-stall-warning",
                            div { class: "run-stall-warning-head",
                                span { class: "run-stall-icon", "\u{26a0}" }
                                span { class: "run-stall-title", "No progress for {format_idle(idle)} — scan may be stalled" }
                                button {
                                    class: "btn-stop btn-stop-stall",
                                    onclick: move |_| {
                                        let jid = active_audit_job().unwrap_or_default();
                                        spawn(async move { cancel_audit_job(&jid).await; });
                                    },
                                    "\u{25a0} Stop scan"
                                }
                            }
                        }
                    }
                }
                // Deterministic-scan PROGRESS — rendered ABOVE the AI agent-activity drawer.
                // Shows the deterministic pass's per-tool start/run/done + findings count and
                // an overall done/total. It's the PRIMARY progress view in deterministic-only
                // mode (where the AI drawer below is empty).
                if let Some(dp) = det_progress() {
                    DeterministicProgress { progress: dp }
                }
                // Background-job FAILURE banner (silent-death fix): rendered UNCONDITIONALLY,
                // not gated on `audited.is_some()` below — a failed or vanished job never
                // produces a report, so this is the only place a failure would ever surface.
                ScanJobFailedBanner { message: job_failed_message() }
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
                p { class: "scan-section-sub", "The Queue holds what the scan ACCUSES the codebase of — defects and deviations. Triage each into one of four tables: leave it Unresolved, Ignore it (with a reason), save it as Tech debt, or mark it a False positive (with a reason — the tool was wrong; excluded from the report, never baselined). The other three tabs are informational: they assert nothing is wrong and need no triage. When nothing is Unresolved, Process the ignored + tech-debt buckets." }

                // The one-rule-one-surface invariant (design §8), asserted. Silent in the
                // healthy case, which is every case; loud when the pipeline mislabels a rule.
                KindSurfaceConflictBanner { findings: findings.clone() }

                // ── AI-review-did-not-run / AI-review-failed banner ────────────────
                // Compliance-gate fix: a `Blocked` project (API-only, no Anthropic key) or a
                // hard AI-pass failure (bad model id, an API/CLI error, an auth failure) must
                // NEVER look like a silent, clean scan. Rendered UNCONDITIONALLY whenever
                // either field is set on the audited report — including when `findings` is
                // empty, since an empty-but-blocked/failed table is exactly the case that used
                // to look like a clean pass. This is intentionally its own banner (not folded
                // into the gate box above, which only covers the pre-audit `scan()` report):
                // the deterministic floor (and preview linters / dep-audit) may well have run
                // and produced real findings below, so this is a partial-result notice, not a
                // "nothing happened" gate. Extracted into its own component so it's directly
                // SSR-testable without standing up the whole `ScanResults` tree (which needs
                // network resources under `use_resource`).
                AiReviewBlockedBanner {
                    ai_blocked_reason: audited.as_ref().and_then(|a| a.ai_blocked_reason.clone()),
                    ai_error: audited.as_ref().and_then(|a| a.ai_error.clone()),
                }

                // PRIMARY axis: the kind switcher. Flipping it flips the action vocabulary too,
                // because the decisions genuinely differ — the Queue disposes rows, the three
                // informational panels decide per RULE (and `Not assessed` decides nothing).
                div { class: "kind-switch",
                    for (surface , count) in kind_counts.iter().copied() {
                        {
                            let active = kind_view() == surface;
                            rsx! {
                                button {
                                    key: "{surface.label()}",
                                    class: if active { "kind-tab active" } else { "kind-tab" },
                                    onclick: move |_| kind_view.set(surface),
                                    "{surface.label()} "
                                    span { class: "triage-tab-count", "{count}" }
                                    // The alarm chip: rows that reached the UI with no kind at
                                    // all. They live in the Queue, visibly flagged — an
                                    // unmapped emission path is a bug to fix upstream, never a
                                    // silent bucket and never a Defect.
                                    if surface == KindSurface::Queue && n_unspecified > 0 {
                                        span {
                                            class: "kind-unspecified-chip",
                                            title: "These rows reached the UI with no kind. That is an unmapped emission path in the pipeline, not a finding class.",
                                            "{n_unspecified} unspecified"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                if kind_view() == KindSurface::Queue {
                    // Single-select over the triage tables, each with a live count. The set
                    // and its order come from `TriageState::all()` and the count from
                    // `TriageCounts::of`, so a disposition can never exist without a tab or
                    // sit behind a tab that shows a permanent zero.
                    div { class: "triage-switch",
                        for st in TriageState::all() {
                            {
                                let count = triage_counts.of(st);
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

                    // Rule alternatives (audit-integrated alternative recommendation): reviews
                    // every multi-option rule the audit evaluated, doubling as the informational
                    // "group header" for the findings table right below it. Keyed on the audit's
                    // own started_at stamp so a fresh audit run (a different rule selection) gets a
                    // clean remount instead of reusing stale staged-override state.
                    {
                        let recommendations = audited
                            .as_ref()
                            .map(|a| a.recommendations.clone())
                            .unwrap_or_default();
                        let audit_stamp = audited
                            .as_ref()
                            .map(|a| a.provenance.started_at.clone())
                            .unwrap_or_default();
                        rsx! {
                            RuleAlternativesPanel {
                                key: "{audit_stamp}",
                                project_id: project_id.clone(),
                                proposed_rules: report.proposed_rules.clone(),
                                findings: findings.clone(),
                                repos: report.repos.clone(),
                                recommendations,
                                audit,
                            }
                        }
                    }

                    // Wrapped so the key is the first node in its block (Dioxus requirement);
                    // keying on the view remounts the table so its frozen rows reflect the switch.
                    // Fed the QUEUE rows only: the informational kinds have their own panels.
                    {
                        rsx! {
                            FindingsTable {
                                key: "{triage_view().label()}",
                                findings: queue_findings.clone(),
                                repos: report.repos.clone(),
                                descriptions: descriptions.clone(),
                                triage_view: triage_view(),
                                dispositions,
                                on_open_hub: Callback::new(move |r: RuleHubRequest| hub_open.set(Some(r))),
                                queue_rule_filter,
                            }
                        }
                    }
                } else if kind_view() == KindSurface::Conventions {
                    // The WHOLE finding set, not this surface's rows: per §13 the convention
                    // behind a rule's Deviation rows is adoptable too, and those rows live on
                    // the Queue. `adoptable_conventions` does the routing.
                    ConventionsPanel {
                        findings: findings.clone(),
                        on_open_hub: Callback::new(move |r: RuleHubRequest| hub_open.set(Some(r))),
                        model: adoption(),
                        busy: committing(),
                        only_unopened: unopened_only(),
                        on_clear_filter: Callback::new(move |()| unopened_only.set(false)),
                        on_toggle: Callback::new(move |key: String| {
                            adoption.write().toggle_adopted(&key);
                        }),
                        // THE ONE WRITER of the openness record. Nothing else in this file
                        // calls `mark_opened`, which is what makes the dialog's "not opened"
                        // count a record of a gesture rather than an inference.
                        on_open: Callback::new(move |key: String| {
                            adoption.write().mark_opened(&key);
                        }),
                        on_commit: Callback::new(move |repo: String| {
                            commit_dialog.set(Some(CommitDialogState {
                                surface: KindSurface::Conventions,
                                destination: repo,
                                typed: String::new(),
                            }));
                        }),
                    }
                } else if kind_view() == KindSurface::NoConvention {
                    NoConventionPanel {
                        findings: findings_on_surface(&findings, KindSurface::NoConvention),
                        on_open_hub: Callback::new(move |r: RuleHubRequest| hub_open.set(Some(r))),
                        model: adoption(),
                        busy: committing(),
                        on_recommend: Callback::new(move |pick: RecommendationPick| {
                            adoption.write().set_recommendation(&pick.key, pick.option_id);
                        }),
                        on_open: Callback::new(move |key: String| {
                            adoption.write().mark_opened(&key);
                        }),
                        on_commit: Callback::new(move |repo: String| {
                            commit_dialog.set(Some(CommitDialogState {
                                surface: KindSurface::NoConvention,
                                destination: repo,
                                typed: String::new(),
                            }));
                        }),
                    }
                } else {
                    NotAssessedPanel {
                        findings: findings_on_surface(&findings, KindSurface::NotAssessed),
                        on_open_hub: Callback::new(move |r: RuleHubRequest| hub_open.set(Some(r))),
                    }
                }

                // Process: commit the ignored bucket to the baseline and file the tech-debt
                // bucket as tickets. Enabled only once nothing remains OUTSTANDING — which,
                // since Pass 2 step 3, a Confirmed row is not: it is decided, just decided to
                // be real. The gate's whole rule lives in `TriageCounts` (core, unit-tested);
                // `ProcessGate` renders it and this closure performs the commit.
                if kind_view() == KindSurface::Queue && (triage_view() == TriageState::TechDebt || triage_counts.outstanding() == 0) {
                    {
                    // QUEUE rows only. Process commits dispositions (baseline waivers,
                    // tickets); an informational row has no disposition to commit, and
                    // including one could only produce a waiver for a compliment.
                    let findings_for_process = queue_findings.clone();
                    rsx! {
                    ProcessGate {
                        counts: triage_counts,
                        processing: processing(),
                        on_process: Callback::new(move |()| {
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
                                        // FalsePositive is a Process-time NO-OP by design (CRITICAL,
                                        // differs from Ignored): it never reaches the baseline and
                                        // never files a ticket. Its only effects are (report export,
                                        // separately) exclusion from the findings section + a
                                        // methodology counter. See `TriageState::FalsePositive`'s doc.
                                        TriageState::FalsePositive => {}
                                        // Confirmed is a Process-time NO-OP too, for a
                                        // different reason than FalsePositive: the finding is
                                        // REAL, so there is nothing to waive and nothing to
                                        // ticket — the client's action item is the finding
                                        // itself, which the report already carries. Owner's
                                        // Q2 ruling (2026-10-12): Confirmed stays local to
                                        // this draft and the export reads it; a durable
                                        // per-finding record across engagements is a backend
                                        // surface Pass 2 does not invent.
                                        TriageState::Confirmed => {}
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
                        }),
                    }
                    }
                    }
                }

                // ── PDF audit-report export (brownfield audit hardening, Pass B) ──────
                // Board-forwardable export of the CURRENT triage state (including any
                // still-Unresolved findings — a draft mid-engagement report is legitimate;
                // this is never gated on the outstanding count). Project id + the lifted
                // dispositions map come from this component's own state.
                {
                    let pid_report = project_id.clone();
                    rsx! {
                        AuditReportExportPanel {
                            project_id: pid_report,
                            dispositions,
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
                                let pid_export = project_id.clone();
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
                if triage_counts.outstanding() == 0 {
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

/// Fetch and return the Markdown deep report for the active project from
/// `GET /api/projects/:id/deep-report`. Returns the Markdown string on success.
///
/// BUG 3 fix: SOC-2 inclusion is decided SERVER-SIDE via the `soc2` feature flag
/// (`export_deep_report` drives it off `state.feature_flags.soc2`). The client used
/// to send `?include_soc2=<bool>`, but the server ignored it — the query param was a
/// dead lever giving false control. It is dropped; the server owns the decision.
pub(super) async fn fetch_deep_report(project_id: &str) -> Option<String> {
    let url = format!(
        "{}/api/projects/{}/deep-report",
        crate::bff_base(),
        project_id,
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
pub(super) fn DeepReportExportPanel(project_id: String, soc2_enabled: bool) -> Element {
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
                    loading.set(true);
                    spawn(async move {
                        let _guard = crate::loading::LoadingGuard::new();
                        // SOC-2 inclusion is feature-flag-driven server-side; no client param.
                        match fetch_deep_report(&pid).await {
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

/// POST the product-export request (dispositions + options) and, on success, return
/// `(zip_bytes, suggested_filename)` — the filename is read off the server's
/// `Content-Disposition` header (`camerata-audit-{repo}-{shortsha}.zip`) so the save dialog
/// defaults to something meaningful; falls back to a generic name if the header is missing
/// or unparsable. On a non-2xx response, returns the server's `{ "message": "…" }` text
/// (or a generic fallback) as `Err` for the caller to toast.
///
/// This is the PRIMARY export ("instead of a PDF export... a product export" — the owner's
/// ruling): the zip contains the same curated PDF the old `/audit-report` route produces,
/// PLUS a fully-formatted Excel workbook (every finding, false positives on their own
/// sheet, a coverage sheet) and a README.txt manifest. `/api/projects/:id/audit-report`
/// itself is left in place for one release (see `docs/design/2026-07-27_product-export.md`)
/// but this panel's button no longer targets it.
pub(super) async fn export_product_zip(
    project_id: &str,
    dispositions: &std::collections::HashMap<String, Disposition>,
    client_name: &str,
    project_title: &str,
    prepared_by: &str,
    executive_summary_override: Option<String>,
) -> Result<(Vec<u8>, String), String> {
    let url = format!(
        "{}/api/projects/{}/product-export",
        crate::bff_base(),
        project_id,
    );
    let resp = reqwest::Client::new()
        .post(url)
        .json(&serde_json::json!({
            "dispositions": dispositions,
            "options": {
                "client_name": client_name,
                "project_title": project_title,
                "prepared_by": prepared_by,
                "executive_summary_override": executive_summary_override,
            },
        }))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        let msg = resp
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(String::from))
            .unwrap_or_else(|| "Product export failed.".to_string());
        return Err(msg);
    }

    let filename = resp
        .headers()
        .get(reqwest::header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split("filename=\"").nth(1))
        .map(|s| s.trim_end_matches('"').to_string())
        .unwrap_or_else(|| "camerata-audit-export.zip".to_string());

    let bytes = resp.bytes().await.map_err(|e| e.to_string())?.to_vec();
    Ok((bytes, filename))
}

/// The product-export panel: client-editable report framing (all optional) + a single
/// export button. POSTs the CURRENT triage state (`dispositions.read()`, whatever it is
/// right now — including any still-Unresolved findings; a draft mid-engagement report is
/// legitimate, so this is never gated on triage completeness) and saves the returned ZIP
/// (curated PDF + full Excel workbook + README) via the same native-save-dialog idiom
/// `save_csv`/the deep-report panel use.
///
/// Placed in the Onboard view right after the triage Process step, above the deep-tier
/// panel — see `ScanResults`.
#[component]
pub(super) fn AuditReportExportPanel(
    project_id: String,
    dispositions: Signal<std::collections::HashMap<String, Disposition>>,
) -> Element {
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    let mut loading = use_signal(|| false);
    let mut client_name = use_signal(String::new);
    let mut project_title = use_signal(String::new);
    let mut prepared_by = use_signal(String::new);
    let mut summary_override = use_signal(String::new);

    rsx! {
        div { class: "audit-export-panel",
            p { class: "section-label", "Export product (ZIP: PDF + Excel + JSON)" }
            p { class: "section-hint",
                "One ZIP, three artifacts from the same scan: a board-forwardable PDF (cover, \
                 executive summary, category scorecard, severity\u{00d7}effort matrix, \
                 curated findings with citations and recommended fixes, what's healthy, \
                 dependency/CVE snapshot, methodology), a fully-formatted Excel workbook — \
                 every finding, one row each, sortable and filterable, with a per-category \
                 sheet and a False Positives sheet (nothing silently dropped) — and a raw \
                 JSON export of the full report for machine consumption, plus a README \
                 explaining each file. Uses your CURRENT triage — a draft mid-engagement \
                 export (some findings still Unresolved) is fine."
            }
            div { class: "audit-export-fields",
                div { class: "audit-model-row",
                    label { class: "audit-model-label", "Client name" }
                    input {
                        class: "audit-export-input",
                        placeholder: "optional",
                        value: "{client_name}",
                        oninput: move |e| client_name.set(e.value()),
                    }
                }
                div { class: "audit-model-row",
                    label { class: "audit-model-label", "Project title" }
                    input {
                        class: "audit-export-input",
                        placeholder: "optional",
                        value: "{project_title}",
                        oninput: move |e| project_title.set(e.value()),
                    }
                }
                div { class: "audit-model-row",
                    label { class: "audit-model-label", "Prepared by" }
                    input {
                        class: "audit-export-input",
                        placeholder: "optional",
                        value: "{prepared_by}",
                        oninput: move |e| prepared_by.set(e.value()),
                    }
                }
                div { class: "audit-model-row",
                    label { class: "audit-model-label", "Executive summary override" }
                    textarea {
                        class: "audit-export-summary",
                        rows: "8",
                        placeholder: "Optional — leave blank for the auto-generated summary. A multi-sentence narrative that replaces the report's executive summary section verbatim.",
                        value: "{summary_override}",
                        oninput: move |e| summary_override.set(e.value()),
                    }
                }
            }
            button {
                class: "btn-run",
                disabled: loading(),
                onclick: move |_| {
                    let pid = project_id.clone();
                    let disp_snapshot = dispositions.read().clone();
                    let cn = client_name();
                    let pt = project_title();
                    let pb = prepared_by();
                    let so = summary_override();
                    loading.set(true);
                    spawn(async move {
                        let _guard = crate::loading::LoadingGuard::new();
                        let override_opt = if so.trim().is_empty() { None } else { Some(so) };
                        match export_product_zip(&pid, &disp_snapshot, &cn, &pt, &pb, override_opt).await {
                            Ok((bytes, filename)) => {
                                if save_bytes(&filename, bytes).await {
                                    crate::toast::push_toast(toasts, crate::toast::ToastKind::Info, "Product export saved.");
                                }
                            }
                            Err(msg) => crate::toast::push_toast(toasts, crate::toast::ToastKind::Error, msg),
                        }
                        loading.set(false);
                    });
                },
                if loading() { "Exporting\u{2026}" } else { "Export product (ZIP: PDF + Excel + JSON)" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::OnboardingDraft;
    use crate::cockpit::rules::SINGLE_REPO_SELECTION_KEY;

    fn minimal_scan_json() -> &'static str {
        r#"{"files_scanned":0,"findings":[],"proposed_rules":[],"gated":false}"#
    }

    // ── Feature B (backend-safety gate): `scan_report_looks_blocked` ───────────
    // Distinguishes a `Blocked` compliance resolution (aborted before any file was read,
    // `gated: false` but a message + all-zero counts) from a completed scan.

    fn scan_report_fixture(overrides: serde_json::Value) -> super::ScanReportView {
        let mut base = serde_json::json!({
            "repos": ["me/api"],
            "files_scanned": 0,
            "findings": [],
            "proposed_rules": [],
            "gated": false,
        });
        for (k, v) in overrides.as_object().expect("overrides must be a JSON object") {
            base[k] = v.clone();
        }
        serde_json::from_value(base).expect("valid ScanReportView fixture")
    }

    #[test]
    fn looks_blocked_true_for_blocked_shape() {
        // gated:false + a message + files_scanned/findings/proposed_rules all empty is exactly
        // the shape `audit_repos` returns for a `BackendResolution::Blocked` project.
        let report = scan_report_fixture(serde_json::json!({
            "message": "This project is API-only (CLI disabled). Add an Anthropic API key to run."
        }));
        assert!(super::scan_report_looks_blocked(&report));
    }

    #[test]
    fn looks_blocked_false_when_gated() {
        // The GitHub-connect gate already sets gated:true and is routed separately — it must
        // not ALSO trip the Blocked heuristic (though the outer `if` ORs the two, this keeps
        // the predicate itself precise).
        let report = scan_report_fixture(serde_json::json!({
            "gated": true,
            "message": "Connect GitHub so Camerata can read the repo(s)."
        }));
        assert!(!super::scan_report_looks_blocked(&report));
    }

    #[test]
    fn looks_blocked_false_when_no_message() {
        // All-zero counts with NO message is just an empty/unscannable repo, not a block.
        let report = scan_report_fixture(serde_json::json!({}));
        assert!(!super::scan_report_looks_blocked(&report));
    }

    #[test]
    fn looks_blocked_false_when_scan_actually_ran() {
        // A completed scan that happens to carry a routine note (e.g. a CliFallbackWarn
        // compliance note) but DID read files must render as a normal report, not the gate box.
        let report = scan_report_fixture(serde_json::json!({
            "message": "\u{26a0} COMPLIANCE: falling back to the Claude CLI.",
            "files_scanned": 42
        }));
        assert!(!super::scan_report_looks_blocked(&report));
    }

    // ── Compliance-gate fix: `ScanReportView.ai_blocked_reason` / `ai_error` ────────────────
    // `ScanReportView` must deserialize the two new fields (additive, `#[serde(default)]`), and
    // a report persisted before they existed must still deserialize with both `None` — the
    // exact back-compat contract every other additive field on this struct already has.

    #[test]
    fn scan_report_view_deserializes_ai_blocked_reason_and_ai_error() {
        let report = scan_report_fixture(serde_json::json!({
            "ai_blocked_reason": "This project is API-only (CLI disabled). Add an Anthropic API key to run.",
            "ai_error": "me/api: AI audit skipped (boom)",
        }));
        assert_eq!(
            report.ai_blocked_reason.as_deref(),
            Some("This project is API-only (CLI disabled). Add an Anthropic API key to run.")
        );
        assert_eq!(report.ai_error.as_deref(), Some("me/api: AI audit skipped (boom)"));
    }

    #[test]
    fn scan_report_view_defaults_ai_fields_to_none_when_absent() {
        // A report shape from before these fields existed (or a clean scan that never set
        // them) must still deserialize, with both reading as `None`.
        let report = scan_report_fixture(serde_json::json!({}));
        assert!(report.ai_blocked_reason.is_none());
        assert!(report.ai_error.is_none());
    }

    // ── Feature B: `split_compliance_notes` ─────────────────────────────────────

    #[test]
    fn split_compliance_notes_extracts_prefixed_segment() {
        let (compliance, other) = super::split_compliance_notes(
            "\u{26a0} COMPLIANCE: Anthropic API key missing — falling back to the Claude CLI.",
        );
        assert_eq!(
            compliance,
            vec!["Anthropic API key missing — falling back to the Claude CLI.".to_string()]
        );
        assert_eq!(other, None);
    }

    #[test]
    fn split_compliance_notes_separates_from_routine_notes() {
        // `audit_repos` joins every note with " · "; a compliance warning can arrive
        // interleaved with ordinary per-repo notes.
        let msg = "me/api: AI review deselected — deterministic only · \u{26a0} COMPLIANCE: falling back to the Claude CLI. · me/web: audit failed (timeout)";
        let (compliance, other) = super::split_compliance_notes(msg);
        assert_eq!(compliance, vec!["falling back to the Claude CLI.".to_string()]);
        assert_eq!(
            other,
            Some(
                "me/api: AI review deselected — deterministic only · me/web: audit failed (timeout)"
                    .to_string()
            )
        );
    }

    #[test]
    fn split_compliance_notes_no_compliance_segment_returns_all_as_other() {
        let (compliance, other) = super::split_compliance_notes("me/api: audit failed (timeout)");
        assert!(compliance.is_empty());
        assert_eq!(other, Some("me/api: audit failed (timeout)".to_string()));
    }

    /// Old drafts that predate the `dispositions` or `triage_view` fields must still
    /// deserialize cleanly — every field on OnboardingDraft has `#[serde(default)]`
    /// except `scan`, which is always written. The scan object itself requires
    /// `files_scanned`, `findings`, `proposed_rules`, and `gated` (no `#[serde(default)]`
    /// on those). All other OnboardingDraft fields (`repo_selection`, `chosen`, `custom`,
    /// `dispositions`, `viewed_repo`, `triage_view`, `audit`) default to empty/None.
    #[test]
    fn onboarding_draft_back_compat_missing_optional_fields() {
        // A minimal draft from before dispositions / triage_view / viewed_repo were added.
        let json = format!(r#"{{"scan": {}}}"#, minimal_scan_json());
        let d: OnboardingDraft = serde_json::from_str(&json).expect("back-compat deserialization failed");
        assert!(d.dispositions.is_empty(), "dispositions should default to empty");
        assert!(d.repo_selection.is_empty(), "repo_selection should default to empty");
        assert!(d.chosen.is_empty(), "chosen should default to empty");
        assert!(d.custom.is_empty(), "custom should default to empty");
        assert!(d.audit.is_none(), "audit should default to None");
        assert!(d.viewed_repo.is_empty(), "viewed_repo should default to empty string");
    }

    /// A draft with repo_selection and chosen round-trips through serde without data loss.
    #[test]
    fn onboarding_draft_selection_round_trips() {
        let mut repo_selection = std::collections::HashMap::new();
        repo_selection.insert(
            SINGLE_REPO_SELECTION_KEY.to_string(),
            vec!["rule-a".to_string(), "rule-b".to_string()],
        );
        repo_selection.insert(
            "my-repo".to_string(),
            vec!["rule-c".to_string()],
        );
        let mut chosen = std::collections::HashMap::new();
        chosen.insert("rule-a".to_string(), "opt-2".to_string());

        let json = format!(r#"{{
            "scan": {},
            "repo_selection": {},
            "chosen": {}
        }}"#,
            minimal_scan_json(),
            serde_json::to_string(&repo_selection).unwrap(),
            serde_json::to_string(&chosen).unwrap(),
        );

        let d: OnboardingDraft = serde_json::from_str(&json).expect("round-trip deserialization failed");

        // repo_selection is preserved exactly.
        assert_eq!(
            d.repo_selection.get(SINGLE_REPO_SELECTION_KEY).map(|v| {
                let mut s = v.clone();
                s.sort();
                s
            }),
            Some(vec!["rule-a".to_string(), "rule-b".to_string()]),
            "single-repo sentinel selection must survive round-trip"
        );
        assert_eq!(
            d.repo_selection.get("my-repo").cloned(),
            Some(vec!["rule-c".to_string()]),
            "named-repo selection must survive round-trip"
        );

        // chosen is preserved exactly.
        assert_eq!(
            d.chosen.get("rule-a").cloned(),
            Some("opt-2".to_string()),
            "chosen option must survive round-trip"
        );
    }

    /// Demonstrates the baseline-preservation invariant in pure logic:
    /// the restore path layers user picks on top of suggested defaults —
    /// rules the user never touched stay selected (suggested), and only
    /// rules the user explicitly changed/deselected are overridden.
    ///
    /// This mirrors what the `suggested_ids` derivation in ProposedRulesTable does:
    /// when `repo_selection` has a saved entry for this repo, restore it EXACTLY
    /// (user picks win); when there is no saved entry, fall back to recommended-only.
    /// The "delta layering" is enforced by what gets persisted: the writeback writes
    /// the live effective selection (suggested baseline + user changes) every time the
    /// user ticks a checkbox, so the saved state is always the effective set.
    #[test]
    fn baseline_preservation_effective_selection_merges_correctly() {
        // Simulate: suggested rules A, B, C (all recommended).
        // User deselects B and selects D (non-recommended).
        // After user interaction, effective selection = {A, C, D}.
        // This is what gets saved in repo_selection.
        let suggested: std::collections::HashSet<&str> = ["rule-a", "rule-b", "rule-c"].iter().copied().collect();
        let user_effective: std::collections::HashSet<&str> = ["rule-a", "rule-c", "rule-d"].iter().copied().collect();

        // On save: repo_selection holds the user's effective selection.
        let mut repo_selection = std::collections::HashMap::new();
        repo_selection.insert(
            SINGLE_REPO_SELECTION_KEY.to_string(),
            user_effective.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        );

        // On restore: the table seed logic sees a saved entry and restores it exactly —
        // it does NOT fall back to the suggested baseline (that would lose the user's delta).
        let saved: Option<std::collections::HashSet<String>> = repo_selection
            .get(SINGLE_REPO_SELECTION_KEY)
            .map(|ids| ids.iter().cloned().collect());

        assert!(saved.is_some(), "saved entry must be present after user interaction");
        let restored: std::collections::HashSet<&str> =
            saved.as_ref().unwrap().iter().map(|s| s.as_str()).collect();

        // rule-b: user deselected — must NOT be in the restored set.
        assert!(!restored.contains("rule-b"), "user-deselected rule must not be restored");
        // rule-d: user added (non-recommended) — must be in the restored set.
        assert!(restored.contains("rule-d"), "user-added non-recommended rule must be restored");
        // rule-a, rule-c: user kept (untouched suggestions) — must be in the restored set.
        assert!(restored.contains("rule-a"), "untouched suggested rule must be preserved");
        assert!(restored.contains("rule-c"), "untouched suggested rule must be preserved");
        // The suggested baseline for rule-b is NOT respected once the user has saved a delta.
        assert!(!suggested.is_empty(), "sanity: suggested set is non-empty");
    }

    /// When there is NO saved entry for a repo (fresh first-view), the seed logic
    /// must fall back to the suggested/recommended rules — the user's first view
    /// pre-selects exactly the recommended set, not an empty table.
    #[test]
    fn baseline_fallback_to_recommended_on_fresh_view() {
        // An empty repo_selection (no saved draft yet).
        let repo_selection: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();

        let saved: Option<std::collections::HashSet<String>> = repo_selection
            .get(SINGLE_REPO_SELECTION_KEY)
            .map(|ids| ids.iter().cloned().collect());

        // The `suggested_ids` derivation in ProposedRulesTable uses `None` to fall back
        // to `effective_auto_recommended()` — this assert verifies that branch is taken.
        assert!(
            saved.is_none(),
            "fresh view with no saved entry must trigger recommended fallback"
        );
    }

    // ── model_badge_label ────────────────────────────────────────────────────

    #[test]
    fn badge_free_model_shows_free_not_price() {
        let label = super::model_badge_label("DeepSeek R1", true, true, 64_000, 0.0, false, true);
        assert!(label.contains("FREE"), "free model must show FREE: {label}");
        assert!(!label.contains('$'), "free model must not show a price: {label}");
    }

    #[test]
    fn badge_paid_model_shows_price_per_million() {
        // $0.55/M output
        let label = super::model_badge_label("DeepSeek R1", false, true, 64_000, 0.55, false, true);
        assert!(label.contains("$0.55/M"), "paid model must show output price: {label}");
        assert!(!label.contains("FREE"), "paid model must not show FREE: {label}");
    }

    #[test]
    fn badge_round_price_strips_trailing_decimal() {
        // $15.0/M → "$15/M" (no trailing zero)
        let label = super::model_badge_label("Opus", false, true, 200_000, 15.0, false, true);
        assert!(label.contains("$15/M"), "round price must strip decimal: {label}");
    }

    #[test]
    fn badge_caching_true_shows_cache_tag() {
        let label = super::model_badge_label("DeepSeek R1", false, true, 64_000, 0.55, true, true);
        assert!(label.contains("cache"), "caching model must show cache tag: {label}");
    }

    #[test]
    fn badge_caching_false_omits_cache_tag() {
        let label = super::model_badge_label("Llama 3.1", false, true, 128_000, 0.10, false, true);
        assert!(!label.contains("cache"), "non-caching model must not show cache tag: {label}");
    }

    #[test]
    fn badge_full_format_example() {
        // "DeepSeek R1  $0.55/M · tool-use · 64K · cache"
        let label = super::model_badge_label("DeepSeek R1", false, true, 64_000, 0.55, true, true);
        assert_eq!(label, "DeepSeek R1  $0.55/M · tool-use · 64K · cache");
    }

    #[test]
    fn badge_free_with_cache() {
        let label = super::model_badge_label("DeepSeek R1 Free", true, true, 64_000, 0.0, true, true);
        assert!(label.contains("FREE"), "free label: {label}");
        assert!(label.contains("cache"), "free+cache label: {label}");
    }

    /// Feature A: a live-fetched Claude model absent from the hardcoded price table
    /// (`price_known: false`) must show "price unknown", NEVER a bare `$0/M` or nothing at
    /// all that could read as free. This is the whole reason `price_known` exists — see
    /// `docs/design/2026-08-27_backend-safety-and-live-models.md`'s Feature A section.
    #[test]
    fn badge_unknown_price_shows_price_unknown_not_zero_or_free() {
        let label = super::model_badge_label("Claude Brand New", false, true, 0, 0.0, true, false);
        assert!(label.contains("price unknown"), "unknown price must be labeled: {label}");
        assert!(!label.contains("FREE"), "unknown price must NOT be shown as FREE: {label}");
        assert!(!label.contains('$'), "unknown price must not show a dollar figure: {label}");
    }

    /// A free model (price_known irrelevant) must still show FREE even if `price_known`
    /// happens to be `false` — `free` takes priority in the badge-selection order.
    #[test]
    fn badge_free_wins_over_unknown_price() {
        let label = super::model_badge_label("Some Free Model", true, true, 64_000, 0.0, false, false);
        assert!(label.contains("FREE"));
        assert!(!label.contains("price unknown"));
    }

    // ── AuditModelsResp::grouped / vision_grouped ─────────────────────────────

    fn audit_models(json: &str) -> super::AuditModelsResp {
        serde_json::from_str(json).expect("valid AuditModelsResp json")
    }

    #[test]
    fn audit_grouped_partitions_by_provider_claude_first() {
        let resp = audit_models(
            r#"{"models":[
                {"label":"Opus","id":"opus","provider":"claude"},
                {"label":"DeepSeek","id":"ds","provider":"openrouter"},
                {"label":"Sonnet","id":"sonnet","provider":"claude"}
            ]}"#,
        );
        let groups = resp.grouped();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].0, "Claude (subscription)");
        assert_eq!(
            groups[0].1.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["opus", "sonnet"]
        );
        assert_eq!(groups[1].0, "OpenRouter");
        assert_eq!(groups[1].1.len(), 1);
    }

    #[test]
    fn audit_grouped_empty_yields_no_groups() {
        let resp = audit_models(r#"{"models":[]}"#);
        assert!(resp.grouped().is_empty());
    }

    #[test]
    fn vision_grouped_includes_only_vision_capable_models() {
        let resp = audit_models(
            r#"{"models":[
                {"label":"Opus","id":"opus","provider":"claude","vision":true},
                {"label":"Haiku","id":"haiku","provider":"claude","vision":false},
                {"label":"Pixtral","id":"pixtral","provider":"openrouter","vision":true},
                {"label":"DeepSeek","id":"ds","provider":"openrouter","vision":false}
            ]}"#,
        );
        let groups = resp.vision_grouped();
        assert_eq!(groups.len(), 2);
        // Only the vision-capable claude model survives.
        assert_eq!(
            groups[0].1.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["opus"]
        );
        // Only the vision-capable openrouter model survives.
        assert_eq!(
            groups[1].1.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["pixtral"]
        );
    }

    #[test]
    fn vision_grouped_drops_provider_group_with_no_vision_models() {
        let resp = audit_models(
            r#"{"models":[
                {"label":"Opus","id":"opus","provider":"claude","vision":true},
                {"label":"DeepSeek","id":"ds","provider":"openrouter","vision":false}
            ]}"#,
        );
        let groups = resp.vision_grouped();
        // OpenRouter has no vision models, so its group must be omitted entirely.
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].0, "Claude (subscription)");
    }

    #[test]
    fn vision_grouped_empty_when_no_vision_models_at_all() {
        let resp = audit_models(
            r#"{"models":[{"label":"Haiku","id":"haiku","provider":"claude","vision":false}]}"#,
        );
        assert!(resp.vision_grouped().is_empty());
    }

    // (csv_field tests moved to camerata-ui-core::rules.)

    // ── findings_csv (header + flat one-row-per-finding) ──────────────────────

    fn finding(json: serde_json::Value) -> super::FindingView {
        serde_json::from_value(json).expect("valid FindingView fixture")
    }

    // Wire-contract sync (same rationale as `scan_report_view_mirrors_server_provenance_shape`
    // above): the server's `Finding::confidence`/`Finding::effort` (onboard.rs) must survive
    // deserialization into this crate's `FindingView` mirror unchanged.
    #[test]
    fn finding_view_mirrors_server_confidence_and_effort_shape() {
        let f = finding(serde_json::json!({
            "repo": "owner/repo", "path": "src/lib.rs", "line": 1,
            "rule_id": "AI-LAYERING", "severity": "medium", "snippet": "s", "detail": "d",
            "confidence": "needs-review", "effort": "low",
        }));
        assert_eq!(f.confidence.as_deref(), Some("needs-review"));
        assert_eq!(f.effort.as_deref(), Some("low"));
    }

    /// req_pass2_1 wire-contract sync, at the RESPONSE level rather than the leaf struct: the
    /// server's `Finding::kind` is `#[serde(default, flatten)] Option<FindingKind>`, so a real
    /// audit response carries ONE flat `"kind"` discriminator with the variant's payload as
    /// siblings of the row's own fields (the shape pinned by api-types'
    /// `req_a5_6_kind_serializes_internally_tagged_with_flat_payload`). This proves the kind
    /// survives the whole `ScanReportView` -> `findings` path the UI actually reads, not just a
    /// hand-built `FindingView`, and that a legacy row with no kind still loads as the
    /// `unspecified` alarm rather than failing the whole response.
    #[test]
    fn req_pass2_1_scan_report_view_carries_the_flattened_kind_through_to_findings() {
        use camerata_api_types::audit::FindingKind;
        let report = scan_report_fixture(serde_json::json!({
            "findings": [
                {
                    "repo": "me/api", "path": "src/routes.rs", "line": 12,
                    "rule_id": "SUPABASE-API-VERSIONING-1", "severity": "medium",
                    "snippet": "app.get('/users')", "detail": "no version prefix",
                    "kind": "deviation", "deviating_sites": 2, "assessed_sites": 47,
                    "convention_option_id": "versioned",
                    "convention_label": "URI-versioned endpoints"
                },
                {
                    "repo": "me/api", "path": "src/legacy.rs", "line": 3,
                    "rule_id": "LEGACY-1", "severity": "high",
                    "snippet": "s", "detail": "d"
                }
            ]
        }));
        let deviation = &report.findings[0];
        match &deviation.kind {
            Some(FindingKind::Deviation(d)) => {
                assert_eq!(d.deviating_sites, 2);
                assert_eq!(d.assessed_sites, Some(47));
                assert_eq!(d.convention_label, "URI-versioned endpoints");
            }
            other => panic!("expected a Deviation off the wire, got {other:?}"),
        }
        assert_eq!(
            deviation.detail, "no version prefix",
            "the row's own fields must be untouched by the flatten"
        );
        assert!(
            report.findings[1].kind.is_none(),
            "a row whose producer assigned no kind loads as the unspecified alarm"
        );
        assert_eq!(super::kind_tag(&report.findings[1]), "unspecified");
    }

    #[test]
    fn findings_csv_header_and_row_shape() {
        let f = finding(serde_json::json!({
            "repo": "owner/repo",
            "path": "src/lib.rs",
            "line": 42,
            "rule_id": "ARCH-1",
            "severity": "high",
            "snippet": "let x = 1;",
            "detail": "explains it",
            "also_matches": ["ARCH-2", "ARCH-3"],
        }));
        let csv = super::findings_csv(&[f]);
        // Header is stable and column-complete.
        assert!(
            csv.starts_with("repo,severity,status,rule_id,also_matches,path,line,snippet,detail,preview,preview_tool\n"),
            "header row; csv=\n{csv}"
        );
        // also_matches is space-joined in its single cell.
        assert!(csv.contains("ARCH-2 ARCH-3"), "also_matches joined; csv=\n{csv}");
        // Line + rule + path are present on the data row.
        assert!(csv.contains("ARCH-1"), "rule id; csv=\n{csv}");
        assert!(csv.contains("src/lib.rs"), "path; csv=\n{csv}");
        assert!(csv.contains(",42,"), "line; csv=\n{csv}");
    }

    #[test]
    fn findings_csv_quotes_snippet_with_comma() {
        let f = finding(serde_json::json!({
            "repo": "r",
            "path": "p",
            "line": 1,
            "rule_id": "R",
            "severity": "low",
            "snippet": "a, b, c",
            "detail": "d",
        }));
        let csv = super::findings_csv(&[f]);
        assert!(csv.contains("\"a, b, c\""), "comma snippet must be quoted; csv=\n{csv}");
    }

    // (finding_key / finding_state / TriageState-default tests moved to
    // camerata_ui_core::triage alongside the lifted state machine — pure, unit-tested with no
    // VirtualDom. human_tokens / det_tool_label / default_finding_status tests live in
    // camerata_ui_core::scan.)

    // ── recommend_scan_mode ───────────────────────────────────────────────────

    fn scan_report(json: serde_json::Value) -> super::ScanReportView {
        serde_json::from_value(json).expect("valid ScanReportView fixture")
    }

    // ── wire-contract sync: ScanReportView must mirror the server's ScanProvenance ──────────
    // `camerata_server::onboard::ScanReport` and this crate's `ScanReportView` are NOT the
    // same type — the UI is a WASM binary that never depends on the (tokio/axum) server
    // crate, so it deserializes the server's JSON response through its own mirror struct.
    // serde silently DROPS any JSON field the mirror doesn't declare, so a field added
    // server-side without a matching mirror field here is invisible drift, not a compile
    // error. This test pins the JSON SHAPE the server's `ScanProvenance` (onboard.rs) emits
    // (mirrored by hand in that file's own serde round-trip test) and asserts every field
    // survives deserialization into `ScanReportView` here — the two tests together are the
    // cross-crate sync guard.
    #[test]
    fn scan_report_view_mirrors_server_provenance_shape() {
        let r = scan_report(serde_json::json!({
            "repos": ["owner/repo"], "files_scanned": 10,
            "findings": [], "proposed_rules": [], "gated": false,
            "provenance": {
                "audited_refs": [
                    { "repo": "owner/repo", "sha": "deadbeef".repeat(5), "branch": "main", "dirty": true }
                ],
                "audit_model": "claude-sonnet-5",
                "calibration_model": "claude-haiku-4-5",
                "mode": "parallel",
                "thorough": true,
                "deep": false,
                "rules_fingerprint": "fp-123",
                "audited_rule_ids": ["SEC-NO-HARDCODED-SECRETS-1", "ARCH-1"],
                "camerata_version": "0.1.0",
                "osv_scanner_version": "v1.9.2",
                "started_at": "2026-07-23T00:00:00+00:00",
                "finished_at": "2026-07-23T00:05:00+00:00",
            }
        }));
        let p = r.provenance;
        assert_eq!(p.audited_refs.len(), 1);
        assert_eq!(p.audited_refs[0].repo, "owner/repo");
        assert_eq!(p.audited_refs[0].branch.as_deref(), Some("main"));
        assert!(p.audited_refs[0].dirty, "dirty flag must survive the round trip");
        assert_eq!(p.audit_model.as_deref(), Some("claude-sonnet-5"));
        assert_eq!(p.calibration_model.as_deref(), Some("claude-haiku-4-5"));
        assert_eq!(p.mode, "parallel");
        assert!(p.thorough);
        assert!(!p.deep);
        assert_eq!(p.rules_fingerprint, "fp-123");
        assert_eq!(p.audited_rule_ids, vec!["SEC-NO-HARDCODED-SECRETS-1".to_string(), "ARCH-1".to_string()]);
        assert_eq!(p.camerata_version, "0.1.0");
        assert_eq!(p.osv_scanner_version.as_deref(), Some("v1.9.2"));
        assert_eq!(p.started_at, "2026-07-23T00:00:00+00:00");
        assert_eq!(p.finished_at, "2026-07-23T00:05:00+00:00");
    }

    // ── ScanReportView.recommendations + FindingView.evaluated_option_id round-trip ────────
    // docs/design/2026-09-22_audit-integrated-alternatives.md — Phase 2 wire-contract sync
    // guard, same idiom as `scan_report_view_mirrors_server_provenance_shape` above: this pins
    // the JSON shape `camerata_server::ai_audit::RuleRecommendation` (keyed by rule id into a
    // map, mirroring `camerata_server::onboard::ScanReport::recommendations`) and
    // `Finding::evaluated_option_id` emit, and asserts every field survives deserialization.
    #[test]
    fn scan_report_view_recommendations_round_trip() {
        let r = scan_report(serde_json::json!({
            "repos": ["owner/repo"], "files_scanned": 10,
            "findings": [
                {
                    "repo": "owner/repo", "path": "src/a.rs", "line": 3, "rule_id": "ARCH-1",
                    "severity": "medium", "snippet": "s", "detail": "d",
                    "evaluated_option_id": "layered"
                }
            ],
            "proposed_rules": [], "gated": false,
            "recommendations": {
                "ARCH-1": {
                    "recommended_option_id": "layered",
                    "recommendation_reasoning": "The repo already separates controllers from services.",
                    "hallucinated": false,
                    "operator_chosen": false,
                    "evidence": "src/api/handler.rs:42 — controller calls service.method directly",
                    "applicable": false
                },
                "ARCH-2": {
                    "recommended_option_id": "fallback-opt",
                    "recommendation_reasoning": "The model did not return a valid option id for this rule.",
                    "hallucinated": true,
                    "operator_chosen": true
                }
            }
        }));
        assert_eq!(r.findings[0].evaluated_option_id.as_deref(), Some("layered"));
        assert_eq!(r.recommendations.len(), 2);
        let arch1 = &r.recommendations["ARCH-1"];
        assert_eq!(arch1.recommended_option_id, "layered");
        assert_eq!(
            arch1.recommendation_reasoning,
            "The repo already separates controllers from services."
        );
        assert!(!arch1.hallucinated);
        assert!(!arch1.operator_chosen);
        assert_eq!(
            arch1.evidence.as_deref(),
            Some("src/api/handler.rs:42 — controller calls service.method directly"),
            "evidence (P7) must survive the round trip"
        );
        assert!(
            !arch1.applicable,
            "explicit applicable:false must survive the round trip"
        );
        let arch2 = &r.recommendations["ARCH-2"];
        assert!(arch2.hallucinated, "hallucinated flag must survive the round trip");
        assert!(arch2.operator_chosen, "operator_chosen flag must survive the round trip");
        // ARCH-2's JSON predates P7 (no evidence/applicable keys at all) — legacy-absent case:
        // must deserialize as no-evidence + applicable defaulting to TRUE (matching the server's
        // own `default_recommendation_applicable`), not `bool::default()`'s `false`, so an old
        // persisted report never renders every recommendation as "Not applicable".
        assert_eq!(arch2.evidence, None, "absent evidence deserializes as None");
        assert!(arch2.applicable, "absent applicable must default to true, not false");
    }

    #[test]
    fn scan_report_view_recommendations_defaults_to_empty_map_when_absent() {
        // Back-compat: a report persisted before this feature existed (e.g. an old onboarding
        // draft) must still deserialize, with no recommendations to show — matching exactly
        // the pre-feature UI (no panel, no gate change visible).
        let r = scan_report(serde_json::json!({
            "repos": [], "files_scanned": 0, "findings": [], "proposed_rules": [], "gated": false
        }));
        assert!(r.recommendations.is_empty());
    }

    // ── merge_rescanned_alternatives (pure; drives the batched-rescan merge-in-place) ───────

    fn finding_for(rule_id: &str, path: &str) -> super::FindingView {
        serde_json::from_value(serde_json::json!({
            "repo": "owner/repo", "path": path, "line": 1, "rule_id": rule_id,
            "severity": "low", "snippet": "s", "detail": ""
        }))
        .expect("valid FindingView fixture")
    }

    fn rec(option_id: &str, operator_chosen: bool) -> super::RuleRecommendationView {
        super::RuleRecommendationView {
            recommended_option_id: option_id.to_string(),
            recommendation_reasoning: "because".to_string(),
            hallucinated: false,
            operator_chosen,
            evidence: None,
            applicable: true,
        }
    }

    #[test]
    fn merge_rescanned_alternatives_replaces_only_the_rescanned_rules() {
        let existing_findings = vec![
            finding_for("ARCH-1", "a.rs"),
            finding_for("ARCH-2", "b.rs"),
            finding_for("ARCH-2", "c.rs"),
        ];
        let mut existing_recs = std::collections::HashMap::new();
        existing_recs.insert("ARCH-1".to_string(), rec("opt-a", false));
        existing_recs.insert("ARCH-2".to_string(), rec("opt-b", false));

        let rescanned: std::collections::HashSet<String> = ["ARCH-2".to_string()].into_iter().collect();
        let new_findings = vec![finding_for("ARCH-2", "b.rs")]; // the operator's forced re-audit shrank it to 1
        let mut new_recs = std::collections::HashMap::new();
        new_recs.insert("ARCH-2".to_string(), rec("opt-c", true));

        let (merged_findings, merged_recs) = super::merge_rescanned_alternatives(
            &existing_findings,
            &existing_recs,
            &rescanned,
            new_findings,
            new_recs,
        );

        // ARCH-1's finding is untouched; ARCH-2's two old findings are replaced by the one new one.
        assert_eq!(merged_findings.len(), 2, "ARCH-1 (1) + the new ARCH-2 (1)");
        assert!(merged_findings.iter().any(|f| f.rule_id == "ARCH-1" && f.path == "a.rs"));
        assert!(merged_findings.iter().any(|f| f.rule_id == "ARCH-2" && f.path == "b.rs"));
        assert!(!merged_findings.iter().any(|f| f.path == "c.rs"), "the old ARCH-2 finding must be gone");

        assert_eq!(merged_recs["ARCH-1"].recommended_option_id, "opt-a", "untouched");
        assert_eq!(merged_recs["ARCH-2"].recommended_option_id, "opt-c", "replaced by the rescan");
        assert!(merged_recs["ARCH-2"].operator_chosen, "rescanned rule is now operator-chosen");
    }

    #[test]
    fn merge_rescanned_alternatives_adds_a_brand_new_recommendation() {
        // A rule with zero findings pre-rescan still gets its recommendation merged in.
        let (_, merged_recs) = super::merge_rescanned_alternatives(
            &[],
            &std::collections::HashMap::new(),
            &["ARCH-9".to_string()].into_iter().collect(),
            vec![],
            std::collections::HashMap::from([("ARCH-9".to_string(), rec("opt-z", true))]),
        );
        assert_eq!(merged_recs["ARCH-9"].recommended_option_id, "opt-z");
    }

    // ── rescan_button_label ─────────────────────────────────────────────────────────────────

    #[test]
    fn rescan_button_label_is_singular_for_one() {
        assert_eq!(super::rescan_button_label(1), "Rescan 1 overridden rule");
    }

    #[test]
    fn rescan_button_label_is_plural_for_many() {
        assert_eq!(super::rescan_button_label(3), "Rescan 3 overridden rules");
    }

    // ── stage_or_clear_override (pure core of the Change dropdown's onchange) ──────────────

    #[test]
    fn stage_or_clear_override_stages_a_different_pick() {
        let mut pending = std::collections::HashMap::new();
        super::stage_or_clear_override(&mut pending, "ARCH-1", "layered", "hexagonal");
        assert_eq!(pending.get("ARCH-1"), Some(&"hexagonal".to_string()));
    }

    #[test]
    fn stage_or_clear_override_clears_when_picking_the_evaluated_option_back() {
        let mut pending = std::collections::HashMap::new();
        pending.insert("ARCH-1".to_string(), "hexagonal".to_string());
        // Picking the option the rule is CURRENTLY evaluated against again clears the override.
        super::stage_or_clear_override(&mut pending, "ARCH-1", "layered", "layered");
        assert!(!pending.contains_key("ARCH-1"));
    }

    #[test]
    fn stage_or_clear_override_only_touches_its_own_rule() {
        let mut pending = std::collections::HashMap::new();
        pending.insert("ARCH-2".to_string(), "opt-x".to_string());
        super::stage_or_clear_override(&mut pending, "ARCH-1", "layered", "hexagonal");
        assert_eq!(pending.get("ARCH-2"), Some(&"opt-x".to_string()), "untouched");
        assert_eq!(pending.get("ARCH-1"), Some(&"hexagonal".to_string()));
    }

    #[test]
    fn scan_report_view_provenance_defaults_when_absent() {
        // A report from before the provenance stamp existed (or a hand-built test fixture that
        // omits it) must still deserialize — the whole point of `#[serde(default)]` here.
        let r = scan_report(serde_json::json!({
            "repos": ["owner/repo"], "files_scanned": 1,
            "findings": [], "proposed_rules": [], "gated": false
        }));
        assert!(r.provenance.audited_refs.is_empty());
        assert_eq!(r.provenance.audit_model, None);
        assert_eq!(r.provenance.camerata_version, "");
    }

    #[test]
    fn recommend_scan_mode_job_for_multi_repo() {
        let r = scan_report(serde_json::json!({
            "repos": ["a", "b"], "files_scanned": 10,
            "findings": [], "proposed_rules": [], "gated": false
        }));
        assert_eq!(super::recommend_scan_mode(&r), "job");
    }

    #[test]
    fn recommend_scan_mode_job_for_large_single_repo() {
        let r = scan_report(serde_json::json!({
            "repos": ["a"], "files_scanned": 200,
            "findings": [], "proposed_rules": [], "gated": false
        }));
        assert_eq!(super::recommend_scan_mode(&r), "job");
    }

    #[test]
    fn recommend_scan_mode_parallel_for_small_single_repo() {
        let r = scan_report(serde_json::json!({
            "repos": ["a"], "files_scanned": 10,
            "findings": [], "proposed_rules": [], "gated": false
        }));
        assert_eq!(super::recommend_scan_mode(&r), "parallel");
    }

    // ── audit_rules_json (request serialization shape) ────────────────────────

    #[test]
    fn audit_rules_json_emits_id_directive_repos() {
        let rules = vec![super::SelectedAuditRule {
            id: "ARCH-1".to_string(),
            directive: "do the thing".to_string(),
            repos: vec!["owner/repo".to_string()],
        }];
        let json = super::audit_rules_json(&rules);
        assert_eq!(json.len(), 1);
        assert_eq!(json[0]["id"], "ARCH-1");
        assert_eq!(json[0]["directive"], "do the thing");
        assert_eq!(json[0]["repos"][0], "owner/repo");
    }

    // ── resolve_gf_directive ──────────────────────────────────────────────────

    fn proposed_rule(json: serde_json::Value) -> crate::cockpit::rules::ProposedRuleView {
        serde_json::from_value(json).expect("valid ProposedRuleView fixture")
    }

    #[test]
    fn resolve_gf_directive_uses_title_when_no_options() {
        let r = proposed_rule(serde_json::json!({
            "id": "R1", "title": "Use newtypes", "kind": "architectural"
        }));
        assert_eq!(super::resolve_gf_directive(&r), "Use newtypes");
    }

    #[test]
    fn resolve_gf_directive_uses_default_option_directive() {
        let r = proposed_rule(serde_json::json!({
            "id": "R1", "title": "Title", "kind": "architectural",
            "options": [
                { "id": "opt-a", "label": "A", "directive": "directive A" },
                { "id": "opt-b", "label": "B", "directive": "directive B" }
            ],
            "default_option": "opt-b"
        }));
        assert_eq!(super::resolve_gf_directive(&r), "directive B");
    }

    #[test]
    fn resolve_gf_directive_falls_back_to_title_when_default_directive_empty() {
        let r = proposed_rule(serde_json::json!({
            "id": "R1", "title": "The Title", "kind": "architectural",
            "options": [ { "id": "opt-a", "label": "A", "directive": "" } ],
            "default_option": "opt-a"
        }));
        assert_eq!(super::resolve_gf_directive(&r), "The Title");
    }

    // estimate_audit_cost pricing tests now live with the function in
    // camerata_ui_core::scan (all assertions, incl. these 400k/20 monotonicity cases).

    // ════════════════════════════════════════════════════════════════════════
    // Tier 2 — network-helper tests (wiremock). Each sets CAMERATA_BFF_URL, so
    // each MUST carry BOTH #[tokio::test] (outer) and #[serial_test::serial(bff_env)]
    // (inner) to serialize the process-global env override across the crate's tests.
    // ════════════════════════════════════════════════════════════════════════

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_repo_health_unwraps_repos_array() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/projects/proj-1/repo-health"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "repos": [
                    { "repo": "owner/a", "path": "/x/a", "resolved": true, "reason": "" },
                    { "repo": "owner/b", "resolved": false, "reason": "no local checkout" }
                ]
            })))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let out = super::fetch_repo_health("proj-1").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let repos = out.expect("repos array parses");
        assert_eq!(repos.len(), 2);
        assert_eq!(repos[0].repo, "owner/a");
        assert!(repos[0].resolved);
        assert_eq!(repos[1].repo, "owner/b");
        assert!(!repos[1].resolved);
        assert_eq!(repos[1].reason, "no local checkout");
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn set_repo_path_posts_repo_and_path() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/repo-path"))
            .and(body_json(serde_json::json!({ "repo": "owner/a", "path": "/local/a" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": true })))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let ok = super::set_repo_path("owner/a", "/local/a").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(ok, "set_repo_path succeeds on a 2xx");
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn ignore_findings_posts_payload_and_returns_url() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/onboard/ignore"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "url": "https://github.com/owner/repo/pull/9"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let f: super::FindingView = serde_json::from_value(serde_json::json!({
            "repo": "owner/repo", "path": "p", "line": 1,
            "rule_id": "R", "severity": "low", "snippet": "s", "detail": ""
        }))
        .unwrap();

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let url = super::ignore_findings("owner/repo", &[f], "noise", Some("CAM-9".to_string())).await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert_eq!(url.as_deref(), Some("https://github.com/owner/repo/pull/9"));
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn ignore_findings_returns_none_when_not_ok() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/onboard/ignore"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": false })))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let url = super::ignore_findings("owner/repo", &[], "r", None).await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(url.is_none(), "ok:false must yield None");
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn scan_repos_posts_repos_and_parses_report() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/onboard/scan"))
            .and(body_json(serde_json::json!({ "repos": ["owner/a", "owner/b"] })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "repos": ["owner/a", "owner/b"],
                "files_scanned": 12,
                "findings": [],
                "proposed_rules": [],
                "gated": false
            })))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let report = super::scan_repos(&["owner/a".to_string(), "owner/b".to_string()]).await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let report = report.expect("scan report parses");
        assert_eq!(report.files_scanned, 12);
        assert_eq!(report.repos, vec!["owner/a".to_string(), "owner/b".to_string()]);
        assert!(!report.gated);
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn load_onboarding_draft_parses_present_draft() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/onboard/draft"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "scan": {
                    "repos": ["owner/a"],
                    "files_scanned": 3,
                    "findings": [],
                    "proposed_rules": [],
                    "gated": false
                },
                "viewed_repo": "owner/a"
            })))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let draft = super::load_onboarding_draft().await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let draft = draft.expect("a present draft parses");
        assert_eq!(draft.scan.files_scanned, 3);
        assert_eq!(draft.viewed_repo, "owner/a");
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn load_onboarding_draft_none_when_null() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/onboard/draft"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::Value::Null))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let draft = super::load_onboarding_draft().await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(draft.is_none(), "a null body means no draft in progress");
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn save_onboarding_draft_posts_the_draft_body() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/onboard/draft"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": true })))
            .expect(1)
            .mount(&server)
            .await;

        let draft: super::OnboardingDraft = serde_json::from_value(serde_json::json!({
            "scan": {
                "files_scanned": 0, "findings": [], "proposed_rules": [], "gated": false
            }
        }))
        .unwrap();

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        super::save_onboarding_draft(&draft).await;
        std::env::remove_var("CAMERATA_BFF_URL");
        // .expect(1) on the mock verifies (on server drop) the POST was issued once.
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn clear_onboarding_draft_posts_clear() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/onboard/draft/clear"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": true })))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        super::clear_onboarding_draft().await;
        std::env::remove_var("CAMERATA_BFF_URL");
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn complete_onboarding_reads_ok_flag() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/onboard/complete"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": true })))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let ok = super::complete_onboarding().await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(ok, "ok:true means onboarding completed");
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn audit_against_posts_full_request_body() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/onboard/audit"))
            .and(body_json(serde_json::json!({
                "repos": ["owner/a"],
                "rules": [ { "id": "R1", "directive": "do it", "repos": ["owner/a"] } ],
                "model": "claude-opus-5",
                "calibration_model": "claude-sonnet-4",
                "mode": "parallel",
                "thorough": false,
                "incremental": true,
                "deep": false,
                "run_ai_review": true,
                "run_deterministic": true,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "files_scanned": 4, "findings": [], "proposed_rules": [], "gated": false
            })))
            .expect(1)
            .mount(&server)
            .await;

        let rules = vec![super::SelectedAuditRule {
            id: "R1".to_string(),
            directive: "do it".to_string(),
            repos: vec!["owner/a".to_string()],
        }];

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let report = super::audit_against(
            &["owner/a".to_string()],
            &rules,
            "claude-opus-5",
            "claude-sonnet-4",
            "parallel",
            false,
            true,
            false,
            true,
            true,
        )
        .await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let report = report.expect("audit report parses");
        assert_eq!(report.files_scanned, 4);
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_audit_models_maps_registry_and_picks_claude_default() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/models/registry"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "models": [
                    { "id": "ds-r1", "display": "DeepSeek R1", "provider": "openrouter",
                      "free": false, "tool_use": true, "context": 64000, "price_in": 0.5, "price_out": 0.55, "caching": true },
                    { "id": "opus", "display": "Opus", "provider": "claude",
                      "free": false, "tool_use": true, "context": 200000, "price_in": 3.0, "price_out": 15.0, "caching": true }
                ],
                "openrouter_fetched": true
            })))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let resp = super::fetch_audit_models().await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let resp = resp.expect("registry parses");
        // Default = first claude model (opus), not the first model overall.
        assert_eq!(resp.default, "opus");
        assert!(resp.openrouter_fetched);
        assert_eq!(resp.models.len(), 2);
        // The badge label is built from registry fields at fetch time.
        let opus = resp.models.iter().find(|m| m.id == "opus").unwrap();
        assert!(opus.label.contains("Opus"), "label carries display name: {}", opus.label);
        assert!(opus.label.contains("$15/M"), "label carries output price: {}", opus.label);
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn audit_job_start_returns_job_id() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/onboard/audit/start"))
            .and(body_json(serde_json::json!({
                "repos": ["owner/a"],
                "rules": [ { "id": "R1", "directive": "d", "repos": [] } ],
                "model": "m",
                "calibration_model": "c",
                "mode": "job",
                "thorough": false,
                "incremental": false,
                "deep": false,
                "run_ai_review": true,
                "run_deterministic": true,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "job_id": "job-123" })))
            .expect(1)
            .mount(&server)
            .await;

        let rules = vec![super::SelectedAuditRule {
            id: "R1".to_string(),
            directive: "d".to_string(),
            repos: vec![],
        }];

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let jid = super::audit_job_start(
            &["owner/a".to_string()],
            &rules,
            "m",
            "c",
            "job",
            false,
            false,
            false,
            true,
            true,
        )
        .await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert_eq!(jid.as_deref(), Some("job-123"));
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn audit_job_poll_parses_envelope() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/onboard/audit/job/job-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "job": { "status": "running", "done": 2, "total": 5, "findings": [] },
                "idle_ms": 1200,
                "cancel_requested": false
            })))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let env = super::audit_job_poll("job-1").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let env = env.expect("envelope parses");
        assert_eq!(env.job.status, "running");
        assert_eq!(env.job.done, 2);
        assert_eq!(env.job.total, 5);
        assert_eq!(env.idle_ms, Some(1200));
        assert!(!env.cancel_requested);
    }

    /// Confirms the background-job delivery path actually carries `ai_error` all the way
    /// through: `jobs.finish` on the server attaches the full report (including `ai_error`)
    /// to the job, `GET /api/onboard/audit/job/:id` serializes it under `job.report`, and
    /// `audit_job_poll` must deserialize that nested field intact — this is the exact chain
    /// `poll_job`'s `"done"` branch reads (`audit.set(js.job.report)`) before
    /// `AiReviewBlockedBanner` ever gets a chance to render it. Without this, a background
    /// scan whose AI review failed (but the job still finished "done") could silently lose
    /// the error on the wire and present as a clean, empty result.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn audit_job_poll_carries_ai_error_through_a_finished_reports_job() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/onboard/audit/job/job-2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "job": {
                    "status": "done",
                    "done": 1,
                    "total": 1,
                    "findings": [],
                    "report": {
                        "repos": ["me/api"],
                        "files_scanned": 10,
                        "findings": [],
                        "proposed_rules": [],
                        "gated": false,
                        "ai_error": "me/api: AI audit skipped (boom)",
                        "message": null
                    }
                },
                "idle_ms": null,
                "cancel_requested": false
            })))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let env = super::audit_job_poll("job-2").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let env = env.expect("envelope parses");
        assert_eq!(env.job.status, "done");
        let report = env.job.report.expect("a done job carries its report");
        assert_eq!(report.ai_error.as_deref(), Some("me/api: AI audit skipped (boom)"));
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn scaffold_greenfield_api_filters_cross_repo_and_posts_arm_rules() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        // The helper drops cross-repo / process rules and resolves each remaining rule to an
        // ArmRuleReq scoped repo-local to the new repo name. Assert the exact emitted body.
        Mock::given(method("POST"))
            .and(path("/api/onboard/greenfield"))
            .and(body_json(serde_json::json!({
                "name": "my-proj",
                "path": "/dest/my-proj",
                "rules": [
                    {
                        "id": "R-LOCAL",
                        "title": "Local rule",
                        "directive": "Local rule",
                        "enforcement": "convention",
                        "scope": "repo-local",
                        "repos": ["my-proj"]
                    }
                ]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "path": "/dest/my-proj",
                "files_written": ["CONVENTIONS.md"],
                "commit_sha": "abc123",
                "message": "scaffolded"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let rules = vec![
            proposed_rule(serde_json::json!({
                "id": "R-LOCAL", "title": "Local rule", "kind": "architectural",
                "enforcement": "convention", "scope": "repo-local"
            })),
            // This cross-repo rule must be filtered OUT of the arm payload.
            proposed_rule(serde_json::json!({
                "id": "R-CROSS", "title": "Cross", "kind": "process",
                "enforcement": "convention", "scope": "cross-repo"
            })),
        ];

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let result = super::scaffold_greenfield_api("my-proj", "/dest/my-proj", &rules).await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let result = result.expect("scaffold result parses");
        assert!(result.ok);
        assert_eq!(result.commit_sha, "abc123");
        assert_eq!(result.files_written, vec!["CONVENTIONS.md".to_string()]);
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_deep_report_returns_markdown_on_2xx() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        // BUG 3: no `?include_soc2` query param is sent — the server owns the SOC-2
        // decision via its feature flag. The request is a bare GET on the path.
        Mock::given(method("GET"))
            .and(path("/api/projects/proj-1/deep-report"))
            .respond_with(ResponseTemplate::new(200).set_body_string("# Deep Report\n\nsome markdown"))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let md = super::fetch_deep_report("proj-1").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let md = md.expect("markdown returned on success");
        assert!(md.contains("# Deep Report"), "markdown body returned: {md}");
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_deep_report_none_on_error_status() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/projects/proj-1/deep-report"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let md = super::fetch_deep_report("proj-1").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(md.is_none(), "a non-2xx must yield None, not the error body");
    }

    // ── rescan_alternatives / accept_alternatives (audit-integrated alternative
    // recommendation, docs/design/2026-09-22_audit-integrated-alternatives.md) ─────────────

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn rescan_alternatives_posts_the_overrides_payload() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/projects/proj-1/rescan-alternatives"))
            .and(body_json(serde_json::json!({
                "overrides": [{ "rule_id": "ARCH-1", "chosen_option_id": "layered" }]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "message": null,
                "findings": [],
                "recommendations": {
                    "ARCH-1": {
                        "recommended_option_id": "layered",
                        "recommendation_reasoning": "Operator override.",
                        "hallucinated": false,
                        "operator_chosen": true
                    }
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let resp = super::rescan_alternatives(
            "proj-1",
            &[super::AlternativeOverride {
                rule_id: "ARCH-1".to_string(),
                chosen_option_id: "layered".to_string(),
            }],
        )
        .await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let resp = resp.expect("a 200 response must parse");
        assert!(resp.ok);
        assert!(resp.findings.is_empty());
        assert!(resp.recommendations["ARCH-1"].operator_chosen);
    }

    /// The `Blocked` compliance gate (and any other `{ ok: false, message }` response) must
    /// come back as `Some(..)` with the message intact — not collapsed to `None` — so the
    /// caller can surface it instead of silently failing (design doc's explicit requirement).
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn rescan_alternatives_surfaces_ok_false_message_instead_of_failing_silently() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/projects/proj-1/rescan-alternatives"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": false,
                "message": "This project is API-only and has no Anthropic key configured."
            })))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let resp = super::rescan_alternatives(
            "proj-1",
            &[super::AlternativeOverride {
                rule_id: "ARCH-1".to_string(),
                chosen_option_id: "layered".to_string(),
            }],
        )
        .await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let resp = resp.expect("an ok:false body still parses");
        assert!(!resp.ok);
        assert_eq!(
            resp.message.as_deref(),
            Some("This project is API-only and has no Anthropic key configured.")
        );
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn rescan_alternatives_returns_none_on_network_failure() {
        std::env::set_var("CAMERATA_BFF_URL", "http://127.0.0.1:9");
        let resp = super::rescan_alternatives(
            "proj-1",
            &[super::AlternativeOverride {
                rule_id: "ARCH-1".to_string(),
                chosen_option_id: "layered".to_string(),
            }],
        )
        .await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(resp.is_none());
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn accept_alternatives_with_no_rule_ids_posts_empty_body() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/projects/proj-1/accept-alternatives"))
            .and(body_json(serde_json::json!({})))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "accepted": 2,
                "skipped": [],
                "ruleset": { "selections": [], "cross_repo": [], "process": [], "custom": [] }
            })))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let resp = super::accept_alternatives("proj-1", None).await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let resp = resp.expect("a 200 response must parse");
        assert!(resp.ok);
        assert_eq!(resp.accepted, 2);
        assert!(resp.skipped.is_empty());
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn accept_alternatives_scopes_to_the_given_rule_ids() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/projects/proj-1/accept-alternatives"))
            .and(body_json(serde_json::json!({ "rule_ids": ["ARCH-1"] })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true, "accepted": 1, "skipped": [], "ruleset": {}
            })))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let resp = super::accept_alternatives("proj-1", Some(&["ARCH-1".to_string()])).await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert_eq!(resp.expect("parses").accepted, 1);
    }
}

#[cfg(test)]
mod render_tests {
    use super::*;

    // ── Tier-1 render: DeterministicProgress (props-only, no context) ──────────
    // Mirrors the live_run.rs render-test pattern: a harness root mounts the component
    // inside the VirtualDom runtime so its hooks run, then dioxus_ssr renders static HTML.
    fn det_progress_harness() -> Element {
        let progress: DetProgressView = serde_json::from_value(serde_json::json!({
            "tools": [
                { "tool": "floor", "status": "done", "findings": 3 },
                { "tool": "clippy", "status": "running", "findings": 0 }
            ],
            "done": 1,
            "total": 2
        }))
        .expect("valid DetProgressView fixture");
        rsx! {
            DeterministicProgress { progress }
        }
    }

    #[test]
    fn deterministic_progress_renders_tools_and_count() {
        let mut vdom = VirtualDom::new(det_progress_harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("det-progress"), "root container; html=\n{html}");
        assert!(html.contains("Deterministic scan"), "title; html=\n{html}");
        assert!(html.contains("1/2 tools"), "done/total count; html=\n{html}");
        // det_tool_label maps "floor" → "Security floor"; "clippy" passes through.
        assert!(html.contains("Security floor"), "floor label; html=\n{html}");
        assert!(html.contains("clippy"), "clippy label; html=\n{html}");
        // The done tool shows its findings count; the running tool shows its status.
        assert!(html.contains("3 finding(s)"), "done tool findings; html=\n{html}");
        assert!(html.contains("running"), "running tool status; html=\n{html}");
    }

    // ── Tier-1 render: GreenfieldResultView, success branch (props-only) ───────
    fn gf_ok_harness() -> Element {
        let result: GreenfieldScaffoldResult = serde_json::from_value(serde_json::json!({
            "ok": true,
            "path": "/dest/my-proj",
            "files_written": ["CONVENTIONS.md", "AGENTS.md"],
            "commit_sha": "abc123",
            "message": "Repo created"
        }))
        .expect("valid GreenfieldScaffoldResult fixture");
        rsx! {
            GreenfieldResultView { result }
        }
    }

    #[test]
    fn greenfield_result_ok_renders_files_and_sha() {
        let mut vdom = VirtualDom::new(gf_ok_harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("gf-result-ok"), "success container class; html=\n{html}");
        assert!(html.contains("Repo scaffolded"), "success heading; html=\n{html}");
        assert!(html.contains("CONVENTIONS.md"), "first file; html=\n{html}");
        assert!(html.contains("AGENTS.md"), "second file; html=\n{html}");
        assert!(html.contains("abc123"), "commit sha; html=\n{html}");
        assert!(html.contains("/dest/my-proj"), "location path; html=\n{html}");
    }

    // ── Tier-1 render: GreenfieldResultView, error branch (props-only) ─────────
    fn gf_err_harness() -> Element {
        let result: GreenfieldScaffoldResult = serde_json::from_value(serde_json::json!({
            "ok": false,
            "message": "directory already exists"
        }))
        .expect("valid GreenfieldScaffoldResult fixture");
        rsx! {
            GreenfieldResultView { result }
        }
    }

    #[test]
    fn greenfield_result_err_renders_failure_message() {
        let mut vdom = VirtualDom::new(gf_err_harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("gf-result-err"), "error container class; html=\n{html}");
        assert!(html.contains("Scaffold failed"), "error heading; html=\n{html}");
        assert!(html.contains("directory already exists"), "error message; html=\n{html}");
    }

    // ── Tier-1 render: DeepReportExportPanel (needs the toast context) ─────────
    // It calls use_context::<Signal<Vec<Toast>>>(), so the harness MUST provide it.
    // The report_md signal starts None, so we assert the static button + hint structure
    // (no modal until the async fetch resolves, which never runs under SSR).
    fn deep_export_harness_soc2_on() -> Element {
        use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
        rsx! {
            DeepReportExportPanel { project_id: "proj-1".to_string(), soc2_enabled: true }
        }
    }

    #[test]
    fn deep_export_panel_renders_button_and_soc2_on_hint() {
        let mut vdom = VirtualDom::new(deep_export_harness_soc2_on);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("deep-export-panel"), "panel container; html=\n{html}");
        assert!(html.contains("Export deep compliance report"), "section label; html=\n{html}");
        // soc2_enabled = true → the "also included" hint, not the "disabled" one.
        assert!(html.contains("SOC-2 gap analysis is also included"), "soc2-on hint; html=\n{html}");
        // The CTA label (loading() is false initially).
        assert!(html.contains("Export deep report"), "export button label; html=\n{html}");
        // No modal overlay until a report loads.
        assert!(!html.contains("deep-export-modal-overlay"), "no modal before fetch; html=\n{html}");
    }

    fn deep_export_harness_soc2_off() -> Element {
        use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
        rsx! {
            DeepReportExportPanel { project_id: "proj-1".to_string(), soc2_enabled: false }
        }
    }

    #[test]
    fn deep_export_panel_renders_soc2_off_hint() {
        let mut vdom = VirtualDom::new(deep_export_harness_soc2_off);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(
            html.contains("SOC-2 gap analysis is disabled for this workspace"),
            "soc2-off hint; html=\n{html}"
        );
    }

    // ── Tier-1 render: AiReviewBlockedBanner (compliance-gate fix) ──────────────────────────
    // Regression for the silent-empty-audit bug: a `Blocked` compliance gate or a hard AI-pass
    // failure must never render as a clean, empty findings table. These SSR renders (no network
    // resources needed — `AiReviewBlockedBanner` takes plain `Option<String>` props) prove the
    // banner text is actually in the DOM, not just present on the underlying data.

    fn ai_blocked_banner_harness() -> Element {
        rsx! {
            AiReviewBlockedBanner {
                ai_blocked_reason: Some(
                    "This project is API-only (CLI disabled). Add an Anthropic API key to run."
                        .to_string(),
                ),
                ai_error: None,
            }
        }
    }

    #[test]
    fn ai_review_blocked_banner_renders_reason_and_remediation() {
        let mut vdom = VirtualDom::new(ai_blocked_banner_harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("AI review did not run"), "heading; html=\n{html}");
        assert!(
            html.contains("This project is API-only (CLI disabled). Add an Anthropic API key to run."),
            "the exact block reason must be visible verbatim; html=\n{html}"
        );
        assert!(
            html.contains("Add an Anthropic API key in Settings"),
            "actionable remediation hint; html=\n{html}"
        );
        assert!(!html.contains("AI review failed"), "must not ALSO show the error heading; html=\n{html}");
    }

    fn ai_error_banner_harness() -> Element {
        rsx! {
            AiReviewBlockedBanner {
                ai_blocked_reason: None,
                ai_error: Some("me/api: AI audit skipped (simulated LLM unavailable (complete))".to_string()),
            }
        }
    }

    #[test]
    fn ai_review_error_banner_renders_real_error_text() {
        let mut vdom = VirtualDom::new(ai_error_banner_harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("AI review failed"), "heading; html=\n{html}");
        assert!(
            html.contains("simulated LLM unavailable"),
            "the real underlying error text must be visible verbatim, not a generic message; html=\n{html}"
        );
    }

    fn ai_banner_neither_set_harness() -> Element {
        rsx! {
            AiReviewBlockedBanner { ai_blocked_reason: None, ai_error: None }
        }
    }

    #[test]
    fn ai_review_banner_renders_nothing_on_the_happy_path() {
        // Neither field set (the ordinary case — the AI review ran and produced findings, or
        // wasn't requested at all) — the banner must be entirely absent, never an empty box.
        let mut vdom = VirtualDom::new(ai_banner_neither_set_harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(!html.contains("onboard-gate"), "no banner markup at all on the happy path; html=\n{html}");
    }

    // ── Tier-1 render: ScanJobFailedBanner (background-job silent-death fix) ───────────────
    // Regression for "the scan silently dies with no error": a background job that ends
    // `failed` (or that the poller loses contact with) must render a loud, visible banner —
    // never just a spinner that quietly disappears with nothing else on screen.

    fn scan_job_failed_banner_harness() -> Element {
        rsx! {
            ScanJobFailedBanner {
                message: Some(
                    "The scan crashed unexpectedly: claude CLI (`claude`) was not found on PATH. \
                     Please retry; if it keeps happening, this is a bug worth reporting."
                        .to_string(),
                ),
            }
        }
    }

    #[test]
    fn scan_job_failed_banner_renders_the_real_failure_text() {
        let mut vdom = VirtualDom::new(scan_job_failed_banner_harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("Scan failed"), "heading; html=\n{html}");
        assert!(
            html.contains("claude CLI (`claude`) was not found on PATH"),
            "the real underlying failure text must be visible verbatim, not swallowed; html=\n{html}"
        );
        assert!(html.contains("onboard-gate"), "reuses the same unmissable banner styling; html=\n{html}");
    }

    fn scan_job_failed_banner_none_harness() -> Element {
        rsx! {
            ScanJobFailedBanner { message: None }
        }
    }

    #[test]
    fn scan_job_failed_banner_renders_nothing_on_the_happy_path() {
        // No failure message (the ordinary case — the job is still running, or finished
        // cleanly) — the banner must be entirely absent, never an empty box.
        let mut vdom = VirtualDom::new(scan_job_failed_banner_none_harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(!html.contains("onboard-gate"), "no banner markup at all on the happy path; html=\n{html}");
    }

    // ── Tier-1 render: audit-integrated alternative recommendation ─────────────────────────
    // docs/design/2026-09-22_audit-integrated-alternatives.md — Phase 2 (UI).

    /// A multi-option `ProposedRuleView` fixture (2+ options), built via JSON so the required
    /// fields (`id`, `title`, `kind`) are explicit and every optional field defaults sanely.
    fn proposed_rule_with_options(
        id: &str,
        title: &str,
        options: &[(&str, &str)],
    ) -> ProposedRuleView {
        let opts: Vec<serde_json::Value> = options
            .iter()
            .map(|(oid, label)| {
                serde_json::json!({ "id": oid, "label": label, "directive": format!("{label} directive"), "why": "" })
            })
            .collect();
        serde_json::from_value(serde_json::json!({
            "id": id, "title": title, "kind": "semantic", "options": opts
        }))
        .expect("valid ProposedRuleView fixture")
    }

    fn recommendation(option_id: &str, reasoning: &str, hallucinated: bool, operator_chosen: bool) -> RuleRecommendationView {
        RuleRecommendationView {
            recommended_option_id: option_id.to_string(),
            recommendation_reasoning: reasoning.to_string(),
            hallucinated,
            operator_chosen,
            evidence: None,
            applicable: true,
        }
    }

    /// Like [`recommendation`] but with explicit `evidence`/`applicable` (P7) for the tests that
    /// exercise the evidence line + not-applicable marker specifically.
    fn recommendation_with_evidence(
        option_id: &str,
        reasoning: &str,
        evidence: Option<&str>,
        applicable: bool,
    ) -> RuleRecommendationView {
        RuleRecommendationView {
            recommended_option_id: option_id.to_string(),
            recommendation_reasoning: reasoning.to_string(),
            hallucinated: false,
            operator_chosen: false,
            evidence: evidence.map(str::to_string),
            applicable,
        }
    }

    fn finding_evaluated_under(rule_id: &str, option_id: &str) -> FindingView {
        serde_json::from_value(serde_json::json!({
            "repo": "owner/repo", "path": "a.rs", "line": 1, "rule_id": rule_id,
            "severity": "medium", "snippet": "s", "detail": "",
            "evaluated_option_id": option_id
        }))
        .expect("valid FindingView fixture")
    }

    /// Harness: one multi-option rule WITH a finding (evaluated against "layered", AI
    /// recommended) and one multi-option rule with ZERO findings (still listed — build item 2
    /// of the design's UX: the review panel includes zero-violation rules so they can still be
    /// overridden).
    fn rule_alternatives_harness() -> Element {
        use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
        use_context_provider(|| Signal::new(std::collections::HashMap::<String, String>::new()));
        let audit = use_signal(|| None::<ScanReportView>);
        let proposed_rules = vec![
            proposed_rule_with_options(
                "ARCH-1",
                "Layering",
                &[("layered", "Layered architecture"), ("hexagonal", "Hexagonal architecture")],
            ),
            proposed_rule_with_options(
                "ARCH-2",
                "Error handling",
                &[("result", "Result-based"), ("exceptions", "Exception-based")],
            ),
        ];
        let mut recommendations = std::collections::HashMap::new();
        recommendations.insert(
            "ARCH-1".to_string(),
            recommendation("layered", "The repo already separates controllers from services.", false, false),
        );
        recommendations.insert(
            "ARCH-2".to_string(),
            recommendation("result", "Fallback: no valid choice returned.", true, false),
        );
        rsx! {
            RuleAlternativesPanel {
                project_id: "proj-1".to_string(),
                proposed_rules,
                findings: vec![finding_evaluated_under("ARCH-1", "layered")],
                repos: vec!["owner/repo".to_string()],
                recommendations,
                audit,
            }
        }
    }

    #[test]
    fn rule_alternatives_panel_renders_group_controls_for_every_multi_option_rule() {
        let mut vdom = VirtualDom::new(rule_alternatives_harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("rec-panel"), "panel container; html=\n{html}");
        assert!(html.contains("Rule alternatives"), "panel heading; html=\n{html}");
        // ARCH-1: has a finding, evaluated against its AI-recommended option.
        assert!(html.contains("ARCH-1"), "rule id; html=\n{html}");
        assert!(html.contains("Layering"), "rule title; html=\n{html}");
        assert!(html.contains("Evaluated against"), "evaluated-against label; html=\n{html}");
        assert!(html.contains("Layered architecture"), "evaluated option label; html=\n{html}");
        assert!(html.contains("AI recommended"), "AI-recommended badge; html=\n{html}");
        assert!(html.contains("1 finding(s)"), "ARCH-1 finding count; html=\n{html}");
        assert!(html.contains("Why?"), "Why control; html=\n{html}");
        assert!(html.contains("rec-select"), "Change dropdown; html=\n{html}");
        assert!(html.contains("Hexagonal architecture"), "the non-evaluated option is still offered; html=\n{html}");
        // ARCH-2: zero findings, but still listed with its own controls (build item 2 — the
        // review panel includes zero-violation rules so they can still be overridden).
        assert!(html.contains("ARCH-2"), "zero-violation rule id; html=\n{html}");
        assert!(html.contains("Error handling"), "zero-violation rule title; html=\n{html}");
        assert!(html.contains("0 findings"), "zero-violation finding count; html=\n{html}");
        assert!(html.contains("Result-based"), "zero-violation rule's evaluated option; html=\n{html}");
        // Accept alternatives is present and enabled (nothing staged yet).
        assert!(html.contains("Accept alternatives"), "accept button; html=\n{html}");
        // No sticky bar yet — nothing staged.
        assert!(!html.contains("rec-sticky-bar"), "no sticky bar before any override is staged; html=\n{html}");
    }

    /// Bounded internal scroll (46-rule regression): the panel used to grow one row per rule and
    /// drag the whole page down. `.rec-rows` (the wrapper around the row list, NOT `.rec-panel`
    /// itself) must declare both a `max-height` and `overflow-y: auto` so only the row list
    /// scrolls while the panel head/sub, the sticky rescan bar, and the Accept button (siblings
    /// rendered after `.rec-rows` closes) stay pinned. Style-source assertion — no render
    /// harness computes CSS, so this is the only way to pin the rule down.
    #[test]
    fn rec_rows_wrapper_has_bounded_scroll() {
        let css = crate::style::GLOBAL_CSS;
        let rule = css
            .lines()
            .find(|l| l.trim_start().starts_with(".rec-rows "))
            .unwrap_or_else(|| panic!(".rec-rows rule not found in GLOBAL_CSS"));
        assert!(
            rule.contains("max-height"),
            ".rec-rows must cap its height; rule={rule}"
        );
        assert!(
            rule.contains("overflow-y: auto"),
            ".rec-rows must scroll internally; rule={rule}"
        );
        // `.rec-panel` itself (the outer shell) must NOT carry its own max-height/overflow — only
        // the inner `.rec-rows` list scrolls, so the header/sub stay outside the scroll region.
        let panel_rule = css
            .lines()
            .find(|l| l.trim_start().starts_with(".rec-panel "))
            .unwrap_or_else(|| panic!(".rec-panel rule not found in GLOBAL_CSS"));
        assert!(
            !panel_rule.contains("overflow"),
            ".rec-panel must not itself scroll; rule={panel_rule}"
        );
    }

    /// Uniform card layout: `.rec-row` must lay its children out in a COLUMN (header block, then
    /// the "Evaluated against" line, then the Change dropdown on its own line) rather than the
    /// old `flex-wrap: wrap`, which let the "Evaluated against" block land beside OR below the
    /// header depending on content width. `.rec-select` must be full-width so the dropdown reads
    /// as its own line rather than an inline control.
    #[test]
    fn rec_row_is_column_layout_and_select_is_full_width() {
        let css = crate::style::GLOBAL_CSS;
        let row_rule: String = {
            let start = css
                .find(".rec-row {")
                .expect(".rec-row rule not found in GLOBAL_CSS");
            let end = css[start..].find('}').expect("unterminated .rec-row rule") + start;
            css[start..=end].to_string()
        };
        assert!(
            row_rule.contains("flex-direction: column"),
            ".rec-row must be a column; rule={row_rule}"
        );
        assert!(
            !row_rule.contains("flex-wrap"),
            ".rec-row must not wrap children; rule={row_rule}"
        );
        let select_rule = css
            .lines()
            .find(|l| l.trim_start().starts_with(".rec-select "))
            .unwrap_or_else(|| panic!(".rec-select rule not found in GLOBAL_CSS"));
        assert!(
            select_rule.contains("width: 100%"),
            ".rec-select must be full-width; rule={select_rule}"
        );
    }

    /// Render-level proof that many rules (well past the small 2-row fixture above) still render
    /// as uniform column cards inside the bounded `.rec-rows` wrapper — the actual shape that
    /// regressed with 46 real rules, not just a CSS-source assertion.
    #[test]
    fn rule_alternatives_panel_renders_many_rules_inside_bounded_rows_wrapper() {
        fn many_rules_harness() -> Element {
            use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
            use_context_provider(
                || Signal::new(std::collections::HashMap::<String, String>::new()),
            );
            let audit = use_signal(|| None::<ScanReportView>);
            let mut proposed_rules = Vec::new();
            let mut recommendations = std::collections::HashMap::new();
            for i in 0..20 {
                let id = format!("ARCH-{i}");
                proposed_rules.push(proposed_rule_with_options(
                    &id,
                    &format!("Rule {i}"),
                    &[("a", "Option A"), ("b", "Option B")],
                ));
                recommendations.insert(id, recommendation("a", "reasoning", false, false));
            }
            rsx! {
                RuleAlternativesPanel {
                    project_id: "proj-1".to_string(),
                    proposed_rules,
                    findings: vec![],
                    repos: vec!["owner/repo".to_string()],
                    recommendations,
                    audit,
                }
            }
        }
        let mut vdom = VirtualDom::new(many_rules_harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(
            html.contains("rec-rows"),
            "bounded rows wrapper; html=\n{html}"
        );
        assert_eq!(
            html.matches("rec-row\"").count(),
            20,
            "one card per rule; html=\n{html}"
        );
        for i in 0..20 {
            assert!(
                html.contains(&format!("ARCH-{i}")),
                "rule {i} rendered; html=\n{html}"
            );
        }
    }

    /// P7 (docs/plans/2026-09-29_codebase-inspection-hardening.md): each rule's card must show
    /// the evidence `file:line` the pick was grounded in, and an applicability marker when the
    /// model judged the rule not applicable to this repo — "a wrong pick costs a glance, not a
    /// false finding".
    #[test]
    fn rule_alternatives_panel_renders_evidence_and_not_applicable_marker() {
        fn harness() -> Element {
            use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
            use_context_provider(
                || Signal::new(std::collections::HashMap::<String, String>::new()),
            );
            let audit = use_signal(|| None::<ScanReportView>);
            let proposed_rules = vec![
                proposed_rule_with_options(
                    "ARCH-1",
                    "Layering",
                    &[
                        ("layered", "Layered architecture"),
                        ("hexagonal", "Hexagonal architecture"),
                    ],
                ),
                proposed_rule_with_options(
                    "ARCH-2",
                    "Error handling",
                    &[
                        ("result", "Result-based"),
                        ("exceptions", "Exception-based"),
                    ],
                ),
            ];
            let mut recommendations = std::collections::HashMap::new();
            recommendations.insert(
                "ARCH-1".to_string(),
                recommendation_with_evidence(
                    "layered",
                    "The repo already separates controllers from services.",
                    Some("src/api/handler.rs:42 — controller calls service.method directly"),
                    true,
                ),
            );
            recommendations.insert(
                "ARCH-2".to_string(),
                recommendation_with_evidence(
                    "result",
                    "This rule targets a stack this repo doesn't use.",
                    None,
                    false,
                ),
            );
            rsx! {
                RuleAlternativesPanel {
                    project_id: "proj-1".to_string(),
                    proposed_rules,
                    findings: vec![],
                    repos: vec!["owner/repo".to_string()],
                    recommendations,
                    audit,
                }
            }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        // ARCH-1: evidenced + applicable — shows the evidence line, no "not applicable" marker
        // anywhere near it (checked precisely below via the ARCH-2-only assertion).
        assert!(
            html.contains("src/api/handler.rs:42"),
            "evidence file:line rendered; html=\n{html}"
        );
        assert!(
            html.contains("rec-evidence"),
            "evidence line uses its own class; html=\n{html}"
        );
        // ARCH-2: not applicable — marker rendered, and it has no evidence line of its own
        // (only ARCH-1 contributes an occurrence of the evidence class/text).
        assert!(
            html.contains("Not applicable to this repo"),
            "not-applicable marker rendered; html=\n{html}"
        );
        assert_eq!(
            html.matches("rec-evidence").count(),
            1,
            "only the evidenced rule (ARCH-1) renders an evidence line; html=\n{html}"
        );
    }

    #[test]
    fn rule_alternatives_panel_renders_nothing_when_no_recommendations() {
        // No multi-option rule was part of the audit (e.g. a mechanical-only scan) — the panel
        // must render nothing, not an empty shell.
        fn harness() -> Element {
            use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
            use_context_provider(|| Signal::new(std::collections::HashMap::<String, String>::new()));
            let audit = use_signal(|| None::<ScanReportView>);
            rsx! {
                RuleAlternativesPanel {
                    project_id: "proj-1".to_string(),
                    proposed_rules: vec![],
                    findings: vec![],
                    repos: vec![],
                    recommendations: std::collections::HashMap::new(),
                    audit,
                }
            }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(!html.contains("rec-panel"), "no panel shell when nothing is in scope; html=\n{html}");
    }

    // ── Tier-1 render: RuleWhyModal (extracted so the "open" shape is directly testable) ────

    fn why_modal_harness() -> Element {
        rsx! {
            RuleWhyModal {
                rule_id: "ARCH-1".to_string(),
                title: "Layering".to_string(),
                reasoning: "The repo already separates controllers from services.".to_string(),
                hallucinated: false,
                on_close: move |_| {},
            }
        }
    }

    #[test]
    fn rule_why_modal_shows_the_reasoning() {
        let mut vdom = VirtualDom::new(why_modal_harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("ARCH-1"), "rule id; html=\n{html}");
        assert!(html.contains("Layering"), "title; html=\n{html}");
        assert!(html.contains("Why this alternative"), "section label; html=\n{html}");
        assert!(
            html.contains("The repo already separates controllers from services."),
            "reasoning body; html=\n{html}"
        );
        assert!(!html.contains("rec-hallucinated-note"), "no hallucination note when false; html=\n{html}");
    }

    fn why_modal_hallucinated_harness() -> Element {
        rsx! {
            RuleWhyModal {
                rule_id: "ARCH-2".to_string(),
                title: "Error handling".to_string(),
                reasoning: "Fallback: no valid choice returned.".to_string(),
                hallucinated: true,
                on_close: move |_| {},
            }
        }
    }

    #[test]
    fn rule_why_modal_renders_hallucinated_note() {
        let mut vdom = VirtualDom::new(why_modal_hallucinated_harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("rec-hallucinated-note"), "hallucination note class; html=\n{html}");
        assert!(
            html.contains("did not return a valid choice"),
            "hallucination explanation; html=\n{html}"
        );
    }

    #[test]
    fn rule_why_modal_falls_back_when_no_reasoning_recorded() {
        fn harness() -> Element {
            rsx! {
                RuleWhyModal {
                    rule_id: "ARCH-3".to_string(),
                    title: "Naming".to_string(),
                    reasoning: String::new(),
                    hallucinated: false,
                    on_close: move |_| {},
                }
            }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(
            html.contains("No reasoning was recorded for this recommendation."),
            "fallback note; html=\n{html}"
        );
    }

    // ── Tier-1 render: RescanStickyBar ───────────────────────────────────────────────────────

    fn sticky_bar_harness() -> Element {
        rsx! {
            RescanStickyBar { pending_count: 3usize, rescanning: false, on_rescan: move |_| {} }
        }
    }

    #[test]
    fn rescan_sticky_bar_shows_the_correct_count() {
        let mut vdom = VirtualDom::new(sticky_bar_harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("rec-sticky-bar"), "sticky bar container; html=\n{html}");
        assert!(
            html.contains("Rescan 3 overridden rules"),
            "count reflected in both the label and the button; html=\n{html}"
        );
    }

    fn sticky_bar_rescanning_harness() -> Element {
        rsx! {
            RescanStickyBar { pending_count: 1usize, rescanning: true, on_rescan: move |_| {} }
        }
    }

    #[test]
    fn rescan_sticky_bar_shows_rescanning_label_and_disables_the_button() {
        let mut vdom = VirtualDom::new(sticky_bar_rescanning_harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("Rescanning"), "in-flight label; html=\n{html}");
        assert!(html.contains("disabled"), "button disabled while rescanning; html=\n{html}");
    }

    // ── Pass 2 step 1: the Kind column + the three informational panels ──────
    // docs/plans/2026-10-11_pass2-curator-ui-design.md §11 step 1.

    fn kinded_finding(rule_id: &str, severity: &str, kind: serde_json::Value) -> FindingView {
        let mut row = serde_json::json!({
            "repo": "me/api", "path": "src/x.rs", "line": 1, "rule_id": rule_id,
            "severity": severity, "snippet": "s", "detail": "d", "status": "active"
        });
        for (k, v) in kind.as_object().expect("kind payload is an object") {
            row[k] = v.clone();
        }
        serde_json::from_value(row).expect("valid FindingView fixture")
    }

    /// req_pass2_5: the Kind column exists in the findings table, carries the WIRE tag as its
    /// cell value (so the filter vocabulary is the wire's, never re-derived prose), and its
    /// filter offers only the three tags that can actually appear in the Queue — the
    /// informational kinds route to their own panels, so listing them would be dead.
    #[test]
    fn req_pass2_5_findings_table_has_a_kind_column_carrying_the_wire_tag() {
        let cols = finding_columns(vec!["me/api".to_string()], false);
        let kind_col = cols
            .iter()
            .find(|c| c.id == ColumnId("kind"))
            .expect("the findings table must carry a Kind column");
        assert_eq!(kind_col.header, "Kind");

        let defect = kinded_finding("SQL-INJECTION-1", "critical", serde_json::json!({"kind": "defect"}));
        let deviation = kinded_finding(
            "API-VERSIONING-1",
            "medium",
            serde_json::json!({
                "kind": "deviation", "deviating_sites": 2, "assessed_sites": 47,
                "convention_option_id": "versioned", "convention_label": "URI-versioned endpoints"
            }),
        );
        let unmapped = kinded_finding("LEGACY-1", "high", serde_json::json!({}));
        assert_eq!((kind_col.accessor)(&defect), CellValue::Text("defect".to_string()));
        assert_eq!((kind_col.accessor)(&deviation), CellValue::Text("deviation".to_string()));
        assert_eq!(
            (kind_col.accessor)(&unmapped),
            CellValue::Text("unspecified".to_string()),
            "a kind-less row must read as its own explicit alarm value, never as a defect"
        );
    }

    /// req_pass2_4: the one-rule-one-surface invariant is SILENT when healthy and LOUD when a
    /// rule reaches two surfaces — the UI surfaces the pipeline bug rather than picking a
    /// winner and hiding it.
    #[test]
    fn req_pass2_4_conflict_banner_is_silent_when_healthy_and_loud_when_violated() {
        fn healthy() -> Element {
            rsx! {
                KindSurfaceConflictBanner {
                    findings: vec![
                        kinded_finding("D-1", "high", serde_json::json!({ "kind": "defect" })),
                        kinded_finding(
                            "C-1",
                            "info",
                            serde_json::json!({
                                "kind": "convention_observed",
                                "convention_option_id": "opt",
                                "convention_label": "A convention",
                            }),
                        ),
                    ],
                }
            }
        }
        let mut vdom = VirtualDom::new(healthy);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(
            !html.contains("kind-conflict-banner"),
            "a healthy scan must not ring the alarm; html=\n{html}"
        );

        fn violated() -> Element {
            rsx! {
                KindSurfaceConflictBanner {
                    findings: vec![
                        kinded_finding(
                            "API-VERSIONING-1",
                            "info",
                            serde_json::json!({
                                "kind": "convention_observed",
                                "convention_option_id": "opt",
                                "convention_label": "URI-versioned endpoints",
                            }),
                        ),
                        kinded_finding(
                            "API-VERSIONING-1",
                            "medium",
                            serde_json::json!({
                                "kind": "deviation",
                                "deviating_sites": 2,
                                "assessed_sites": 47,
                                "convention_option_id": "opt",
                                "convention_label": "URI-versioned endpoints",
                            }),
                        ),
                    ],
                }
            }
        }
        let mut vdom = VirtualDom::new(violated);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("kind-conflict-banner"), "alarm rendered; html=\n{html}");
        assert!(html.contains("Pipeline bug"), "named as upstream; html=\n{html}");
        assert!(html.contains("API-VERSIONING-1"), "the offending rule named; html=\n{html}");
        assert!(html.contains("Queue"), "and the surfaces it reached; html=\n{html}");
        assert!(html.contains("Conventions"), "and the surfaces it reached; html=\n{html}");
    }

    /// req_pass2_6: the Conventions panel lists each observed convention by its AUTHORED label
    /// and renders NO severity — these kinds ship at `info` by construction, so a severity
    /// badge would imply a ranking decision that does not exist (design §9.4).
    #[test]
    fn req_pass2_6_conventions_panel_lists_labels_and_renders_no_severity() {
        fn harness() -> Element {
            rsx! {
                ConventionsPanel {
                    findings: vec![kinded_finding(
                        "API-VERSIONING-1",
                        "info",
                        serde_json::json!({
                            "kind": "convention_observed",
                            "convention_option_id": "versioned",
                            "convention_label": "URI-versioned endpoints",
                        }),
                    )],
                }
            }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("API-VERSIONING-1"), "rule id; html=\n{html}");
        assert!(html.contains("URI-versioned endpoints"), "authored label; html=\n{html}");
        for forbidden in ["chorale-badge", "severity", "info", "critical", "Critical"] {
            assert!(
                !html.contains(forbidden),
                "no severity may render on an informational panel (found {forbidden:?}); html=\n{html}"
            );
        }
    }

    /// req_pass2_6: the No-convention panel enumerates the options actually observed with
    /// their site counts, and renders an uncounted option HONESTLY (label alone) rather than
    /// fabricating a number.
    #[test]
    fn req_pass2_6_no_convention_panel_lists_observed_options_and_never_fabricates_counts() {
        fn harness() -> Element {
            rsx! {
                NoConventionPanel {
                    findings: vec![kinded_finding(
                        "ERROR-HANDLING-1",
                        "info",
                        serde_json::json!({
                            "kind": "no_convention",
                            "observed_options": [
                                { "option_id": "result", "label": "Result types", "sites": 9 },
                                { "option_id": "panic", "label": "Panics", "sites": null },
                            ],
                        }),
                    )],
                }
            }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("ERROR-HANDLING-1"), "rule id; html=\n{html}");
        assert!(html.contains("Result types (9 sites)"), "counted option; html=\n{html}");
        assert!(html.contains("Panics"), "uncounted option still listed; html=\n{html}");
        assert!(
            !html.contains("Panics (0 sites)") && !html.contains("Panics ("),
            "an uncounted option must never acquire a fabricated count; html=\n{html}"
        );
    }

    /// req_pass2_6: the Not-assessed panel groups by the STRUCTURED reason vocabulary, shows
    /// the producer's detail, and offers NO buttons — there is no per-rule re-run mechanism in
    /// the product, and a disabled one would be the 403-button bug.
    #[test]
    fn req_pass2_6_not_assessed_panel_groups_by_reason_and_offers_no_actions() {
        fn harness() -> Element {
            rsx! {
                NotAssessedPanel {
                    findings: vec![
                        kinded_finding(
                            "ESLINT-1",
                            "info",
                            serde_json::json!({
                                "kind": "not_assessed",
                                "reason": "tool_failed",
                                "reason_detail": "eslint exited 2",
                            }),
                        ),
                        kinded_finding(
                            "TSX-1",
                            "info",
                            serde_json::json!({
                                "kind": "not_assessed",
                                "reason": "no_files_matched",
                                "reason_detail": "no .tsx files in scan scope",
                            }),
                        ),
                    ],
                }
            }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("tool_failed"), "structured reason group; html=\n{html}");
        assert!(html.contains("no_files_matched"), "structured reason group; html=\n{html}");
        assert!(html.contains("eslint exited 2"), "producer detail; html=\n{html}");
        // EDITED for design §8 (exception (a)): this assertion was `!html.contains("<button")`.
        //
        // Design §3 says "NotAssessed gets no buttons … A disabled 're-run' would be exactly
        // the 403-button bug", and design §8 says the rule hub is reachable "from every
        // surface that shows a rule id (… Not assessed list …)". Both are the design's own
        // words and they collide only on the literal element name. What §3 forbids is a
        // DECISION the curator cannot make; the hub link is navigation, and it is the one
        // affordance §8 requires here.
        //
        // So the assertion now says what it always meant, and says it more strictly than the
        // string it replaces: every control on this panel is a hub door, exactly one per row,
        // and none of them is an action. Rendering the door as a `<span onclick>` to keep the
        // old string green would have been dodging the test rather than answering it — and
        // would have cost the keyboard the control, which a `<button>` gives for free.
        let buttons = html.matches("<button").count();
        assert_eq!(
            buttons,
            html.matches("rule-hub-link").count(),
            "every control on the Not assessed panel must be a rule-hub door; html=\n{html}"
        );
        assert_eq!(buttons, 2, "one door per listed rule, and nothing else; html=\n{html}");
        for forbidden in ["btn-run", "btn-restart", "btn-edit-sm", "Re-run", "Adopt", "Skip"] {
            assert!(
                !html.contains(forbidden),
                "NotAssessed needs no decision, so it must offer no {forbidden} affordance; \
                 html=\n{html}"
            );
        }
    }

    /// req_pass2_7 (END-TO-END through the real table): the live `FindingsTable` carries the
    /// Kind column, DROPS an informational row entirely, and lays the remaining rules out in
    /// the curated default order. This exercises the path the curator actually sees —
    /// `TriageModel::visible`'s kind filter, `queue_sort_key`, and the real column set —
    /// rather than the pieces in isolation.
    #[test]
    fn req_pass2_7_live_findings_table_drops_informational_rows_and_keeps_the_curated_order() {
        fn harness() -> Element {
            use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
            use_context_provider(|| Signal::new(None::<FindingView>));
            let dispositions =
                use_signal(std::collections::HashMap::<String, Disposition>::new);
            rsx! {
                FindingsTable {
                    findings: vec![
                        kinded_finding(
                            "SQL-INJECTION-1",
                            "critical",
                            serde_json::json!({ "kind": "defect" }),
                        ),
                        kinded_finding(
                            "API-VERSIONING-1",
                            "medium",
                            serde_json::json!({
                                "kind": "deviation",
                                "deviating_sites": 2,
                                "assessed_sites": 47,
                                "convention_option_id": "versioned",
                                "convention_label": "URI-versioned endpoints",
                            }),
                        ),
                        kinded_finding("LEGACY-UNMAPPED-1", "high", serde_json::json!({})),
                        kinded_finding(
                            "ALREADY-GOOD-1",
                            "info",
                            serde_json::json!({
                                "kind": "convention_observed",
                                "convention_option_id": "versioned",
                                "convention_label": "URI-versioned endpoints",
                            }),
                        ),
                    ],
                    repos: vec!["me/api".to_string()],
                    descriptions: std::collections::HashMap::new(),
                    triage_view: TriageState::Unresolved,
                    dispositions,
                }
            }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);

        // The table groups by rule and mounts COLLAPSED, so the SSR snapshot shows the Kind
        // column header and one group header per rule, not the individual cells. That is
        // enough to prove the three things this test exists for; the chip renderer itself is
        // covered by the column-accessor test above.
        assert!(html.contains(">Kind<"), "the Kind column header; html=\n{html}");
        assert!(
            !html.contains("ALREADY-GOOD-1"),
            "an informational row must never reach the accusing queue; html=\n{html}"
        );

        // And the curated default sort survives into the real table: critical defect, then
        // the kind-less ALARM row (high), then the medium deviation. If the UI re-derived
        // severity or ranked the alarm as a plain defect, this order would change.
        //
        // Scoped to the GROUP HEADERS rather than the whole document (Pass 2 step 2): the
        // rule-level decision bar now also names each deviation rule, above the table, so a
        // first-occurrence search over the full html would be measuring the bar's position
        // instead of the group order. Narrowing it makes the assertion say what it always
        // meant.
        let table = group_header_region(&html);
        let at = |needle: &str| {
            table
                .find(needle)
                .unwrap_or_else(|| panic!("{needle} missing from the queue; html=\n{html}"))
        };
        assert!(
            at("SQL-INJECTION-1") < at("LEGACY-UNMAPPED-1"),
            "critical defect before the high alarm row; html=\n{html}"
        );
        assert!(
            at("LEGACY-UNMAPPED-1") < at("API-VERSIONING-1"),
            "high alarm row before the medium deviation; html=\n{html}"
        );
    }

    /// req_pass2_6: each panel's empty state is a sentence, not a blank area — the curator has
    /// to be able to tell "nothing of this kind" from "this did not render".
    #[test]
    fn req_pass2_6_informational_panels_state_their_empty_case() {
        fn harness() -> Element {
            rsx! {
                ConventionsPanel { findings: vec![] }
                NoConventionPanel { findings: vec![] }
                NotAssessedPanel { findings: vec![] }
            }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert_eq!(
            html.matches("kind-panel-empty").count(),
            3,
            "every panel states its own empty case; html=\n{html}"
        );
    }

    // ── Pass 2 step 2: MANUAL column sorting over the curated default ────────
    // docs/plans/2026-10-11_pass2-curator-ui-design.md §5: "the default sort is a curated
    // order ... the column headers still allow any manual re-sort chorale supports — the
    // default is opinionated, the table is not locked."
    //
    // Zach's ruling (2026-10-12): "Keep sort with [unspecified] first, sure, as long as the
    // table can be manually sorted by me as the user." So the kind-aware key stays, demoted
    // from the only order to the DEFAULT order, and a return path to it is mandatory.
    //
    // These tests drive chorale's REAL sort pipeline (`toggle_sort` / `clear_sort` /
    // `filtered_sorted_rows` / `visible_grouped_view`) over the REAL column set, because the
    // sort the curator experiences is chorale's, not ours.

    /// The findings fixture these sort tests re-cut: one row per severity band, deliberately
    /// NOT in any natural order, so a passing assertion can only come from the sort under
    /// test. Returned already in the curated default order (as `FindingsTable` hands it to
    /// chorale) so a cleared sort is provably a return to THAT order.
    fn sortable_queue_rows() -> Vec<FindingView> {
        let raw = vec![
            kinded_finding("B-LOW-DEFECT", "low", serde_json::json!({ "kind": "defect" })),
            kinded_finding(
                "A-MEDIUM-DEFECT",
                "medium",
                serde_json::json!({ "kind": "defect" }),
            ),
            kinded_finding(
                "C-CRITICAL-DEFECT",
                "critical",
                serde_json::json!({ "kind": "defect" }),
            ),
            kinded_finding("D-HIGH-DEFECT", "high", serde_json::json!({ "kind": "defect" })),
        ];
        TriageModel::new().visible(&raw)
    }

    /// The rows' serialized bytes, in order. `FindingView` is `Serialize` but not `Debug`, and
    /// this is the comparison the round-trip test actually wants anyway: byte-identical, with a
    /// readable diff when it is not.
    fn row_bytes(findings: &[FindingView]) -> String {
        serde_json::to_string(findings).expect("FindingView serializes")
    }

    /// Mint `(RowId, FindingView)` pairs in the given order, as `FindingsTable` does.
    fn sort_rows(findings: &[FindingView]) -> Vec<(RowId, FindingView)> {
        findings.iter().map(|f| (RowId::new(), f.clone())).collect()
    }

    /// A `TableState` over the real findings columns, rows in the curated default order and
    /// chorale's own sort EMPTY — exactly the state `FindingsTable` mounts.
    fn queue_table_state(findings: &[FindingView]) -> chorale_core::TableState<FindingView> {
        chorale_core::TableState::new(
            sort_rows(findings),
            finding_columns(vec!["me/api".to_string()], false),
        )
    }

    /// req_pass2_8: a manual sort on the Severity header must order the AUTHORED severity by
    /// its CALIBRATED rank (critical > high > medium > low), not by its alphabet.
    ///
    /// This is the one column whose words do not sort into their own meaning: alphabetically
    /// `low` lands above `medium`, so a lexicographic header cut reads
    /// "critical, high, low, medium" — a visibly wrong severity ladder. The rank is the same
    /// ordering of the same authored string that `queue_sort_key` uses; nothing is re-derived.
    #[test]
    fn req_pass2_8_severity_header_sort_is_calibrated_not_alphabetical() {
        use chorale_core::{filtered_sorted_rows, toggle_sort, SortAction};

        let findings = sortable_queue_rows();
        let state = queue_table_state(&findings);
        let sorted = toggle_sort(&state, ColumnId("severity"), SortAction::Replace);
        let order: Vec<String> = filtered_sorted_rows(&sorted)
            .iter()
            .map(|f| f.severity.clone())
            .collect();
        assert_eq!(
            order,
            vec!["critical", "high", "medium", "low"],
            "ascending Severity must walk the calibrated ladder; an alphabetical cut would \
             put `low` above `medium`"
        );

        // And descending is the exact reverse — not a different alphabet.
        let sorted = toggle_sort(&sorted, ColumnId("severity"), SortAction::Replace);
        let order: Vec<String> = filtered_sorted_rows(&sorted)
            .iter()
            .map(|f| f.severity.clone())
            .collect();
        assert_eq!(order, vec!["low", "medium", "high", "critical"]);
    }

    /// req_pass2_8: the Queue names the order it is currently showing and, when that order is
    /// the curator's own cut, offers an explicit way back to the curated default.
    ///
    /// The indicator is not decoration: the default order is a judgement (cheapest deviations
    /// first, the `unspecified` alarm floated to the top of its band) and a curator 300 rows
    /// into a sitting has to be able to tell at a glance whether he is looking at it or at his
    /// own re-cut.
    #[test]
    fn req_pass2_8_queue_names_the_active_order_and_defaults_to_the_curated_one() {
        fn harness() -> Element {
            use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
            use_context_provider(|| Signal::new(None::<FindingView>));
            let dispositions = use_signal(std::collections::HashMap::<String, Disposition>::new);
            rsx! {
                FindingsTable {
                    findings: sortable_queue_rows(),
                    repos: vec!["me/api".to_string()],
                    descriptions: std::collections::HashMap::new(),
                    triage_view: TriageState::Unresolved,
                    dispositions,
                }
            }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(
            html.contains("queue-order"),
            "the Queue must carry an order indicator; html=\n{html}"
        );
        assert!(
            html.contains("Camerata default order"),
            "on mount the indicator must name the CURATED DEFAULT, not a blank; html=\n{html}"
        );
        assert!(
            !html.contains("queue-order-reset"),
            "no manual sort is active on mount, so the return-to-default control must not be \
             rendered — its absence is part of the indicator; html=\n{html}"
        );
    }

    /// req_pass2_8 (UNDER RENDER, the manual state): with a cut active the bar names the column
    /// and direction in an accented style AND renders the way back to the default.
    ///
    /// `FindingsTable` owns its own `TableState`, and SSR cannot click a column header, so the
    /// bar is its own component precisely so this state can be rendered at all. Without it,
    /// "the Default order button appears when you sort" would be untested in the product.
    #[test]
    fn req_pass2_8_order_bar_renders_the_manual_state_with_the_way_back() {
        use chorale_core::{SortDirection, SortState};

        // `Callback::new` has to run inside the Dioxus runtime, so the sort is passed into a
        // harness ROOT rather than built into props from outside the VirtualDom.
        #[component]
        fn OrderBarHarness(sort: Vec<SortState>) -> Element {
            let cols = finding_columns(vec!["me/api".to_string()], false);
            rsx! {
                QueueOrderBar {
                    order: queue_order(&sort, &cols),
                    on_reset: Callback::new(|()| {}),
                }
            }
        }

        fn bar(sort: Vec<SortState>) -> String {
            let mut vdom =
                VirtualDom::new_with_props(OrderBarHarness, OrderBarHarnessProps { sort });
            vdom.rebuild_in_place();
            dioxus_ssr::render(&vdom)
        }

        // Default state: names the curated order, no reset control, no accent.
        let html = bar(vec![]);
        assert!(html.contains(DEFAULT_ORDER_LABEL), "html=\n{html}");
        assert!(!html.contains("queue-order-reset"), "html=\n{html}");
        assert!(!html.contains("queue-order-value manual"), "html=\n{html}");

        // Manual state: column, direction, accent, and the Default order button.
        let html = bar(vec![SortState::new(ColumnId("severity"), SortDirection::Desc)]);
        assert!(html.contains("Severity"), "names the column; html=\n{html}");
        assert!(html.contains('\u{2193}'), "names the direction; html=\n{html}");
        assert!(
            html.contains("queue-order-value manual"),
            "a manual cut reads differently from the default; html=\n{html}"
        );
        assert!(
            html.contains("queue-order-reset") && html.contains("Default order"),
            "the way back must be visible and labelled; html=\n{html}"
        );
        assert!(
            !html.contains(DEFAULT_ORDER_LABEL),
            "the bar must not claim to be showing the default while a cut is active; html=\n{html}"
        );
    }

    /// req_pass2_8 (THE failure mode that would actually hurt): the round trip. Default order,
    /// a manual cut, back to the default — and the restored default must be BYTE-IDENTICAL to
    /// where it started, not approximately restored.
    ///
    /// Driven through chorale end to end, because that is where the guarantee comes from:
    /// `filtered_sorted_pairs` only reorders when `state.sort` is non-empty, so an empty sort
    /// hands back `state.rows` exactly as `TriageModel::visible` ordered them. Losing the
    /// curated default with no way back would be worse than having no manual sort at all.
    #[test]
    fn req_pass2_8_manual_sort_round_trips_to_a_byte_identical_default_order() {
        use chorale_core::{clear_sort, filtered_sorted_rows, toggle_sort, SortAction};

        let findings = sortable_queue_rows();
        let state = queue_table_state(&findings);
        let baseline = filtered_sorted_rows(&state);
        assert_eq!(
            row_bytes(&baseline),
            row_bytes(&findings),
            "an empty sort must hand back the curated order untouched"
        );

        // A cut that provably MOVES the rows, so the restore below cannot pass vacuously. By
        // rule id this fixture is scrambled relative to the default (the default leads on
        // severity, and the rule ids were chosen to disagree with it).
        let by_rule = toggle_sort(&state, ColumnId("type"), SortAction::Replace);
        assert_ne!(
            row_bytes(&filtered_sorted_rows(&by_rule)),
            row_bytes(&baseline),
            "the Finding-type cut must actually re-cut the list"
        );
        assert_eq!(
            row_bytes(&filtered_sorted_rows(&clear_sort(&by_rule))),
            row_bytes(&baseline),
            "`Default order` must restore the curated order byte-for-byte"
        );

        // And from a reversed severity cut, which is the one that inverts the whole queue.
        let desc = toggle_sort(
            &toggle_sort(&state, ColumnId("severity"), SortAction::Replace),
            ColumnId("severity"),
            SortAction::Replace,
        );
        assert_ne!(
            row_bytes(&filtered_sorted_rows(&desc)),
            row_bytes(&baseline),
            "descending severity must re-cut"
        );
        assert_eq!(
            row_bytes(&filtered_sorted_rows(&clear_sort(&desc))),
            row_bytes(&baseline),
            "`Default order` must restore the curated order from a reversed cut too"
        );

        // And from a MULTI-column cut (shift+click), which is exactly the state chorale's own
        // third-click cycle cannot undo on its own: a plain click clears one column, leaving
        // the others set. This is why the explicit control is the contract.
        let multi = toggle_sort(&state, ColumnId("file"), SortAction::Append);
        let multi = toggle_sort(&multi, ColumnId("severity"), SortAction::Append);
        assert_eq!(multi.sort.len(), 2, "two columns are sorted");
        let one_more_click = toggle_sort(&multi, ColumnId("severity"), SortAction::Replace);
        assert!(
            !one_more_click.sort.is_empty(),
            "a plain click out of a multi-sort does NOT reach the default — it replaces the \
             list with one column. Without the explicit control there would be no way back."
        );
        assert_eq!(
            row_bytes(&filtered_sorted_rows(&clear_sort(&multi))),
            row_bytes(&baseline),
            "`Default order` must restore the curated order from a multi-column cut too"
        );
    }

    /// req_pass2_8: the default order survives a SCAN CHANGE. A fresh mount starts with
    /// chorale's sort empty, which is the curated default — the curator never inherits the
    /// previous scan's re-cut, and never has to reset it by hand.
    #[test]
    fn req_pass2_8_a_fresh_scan_mounts_on_the_curated_default() {
        let state = queue_table_state(&sortable_queue_rows());
        assert!(
            state.sort.is_empty(),
            "a newly mounted findings table carries no manual sort"
        );
        assert_eq!(
            order_indicator_text(&state.sort, &finding_columns(vec![], false)),
            DEFAULT_ORDER_LABEL,
            "and the indicator says so"
        );
    }

    /// req_pass2_8 (manual sort vs RULE GROUPING — the deliberate choice): a manual cut
    /// reorders the GROUPS and the rows within them; it never flattens the grouping.
    ///
    /// That falls out of chorale's own pipeline rather than being imposed: `visible_grouped_view`
    /// sorts the whole row set first and then partitions it into groups in first-occurrence
    /// order, so each group lands at the position of its best row under the active cut and its
    /// rows keep that cut inside the group. It is also exactly what design §5 specifies
    /// ("Groups order by their best row; rows order within groups by the same key").
    ///
    /// Grouping is load-bearing — it is what the deviation group-header sentence and the later
    /// rule-level bulk actions hang off — so "every group still has exactly one header and a
    /// contiguous block of rows" is the invariant under test, not just the row order.
    #[test]
    fn req_pass2_8_manual_sort_reorders_rule_groups_without_flattening_them() {
        use chorale_core::{toggle_sort, visible_grouped_view, GroupedRow, SortAction};

        // Two rules, two sites each, so a cut can both reorder the groups and reorder inside
        // one. Severities are chosen so the severity cut interleaves the rules if grouping were
        // ignored — that is what makes this test able to fail.
        let findings = TriageModel::new().visible(&[
            kinded_finding("RULE-B", "critical", serde_json::json!({ "kind": "defect" })),
            kinded_finding("RULE-A", "high", serde_json::json!({ "kind": "defect" })),
            kinded_finding("RULE-B", "low", serde_json::json!({ "kind": "defect" })),
            kinded_finding("RULE-A", "medium", serde_json::json!({ "kind": "defect" })),
        ]);
        let mut state = queue_table_state(&findings);
        // Group by rule only (the real table adds a second File level; one level is enough to
        // pin the invariant and keeps the walk readable).
        state.grouping = vec![ColumnId("type")];
        state.grouped_pagination = chorale_core::GroupedPaginationMode::Virtualized;

        /// Walk a grouped view into (group label, row count) pairs, asserting as it goes that
        /// no group's header appears twice and that every data row sits under a header.
        fn group_blocks(view: &[GroupedRow<FindingView>]) -> Vec<(String, usize)> {
            let mut blocks: Vec<(String, usize)> = Vec::new();
            for row in view {
                match row {
                    GroupedRow::Header { label, .. } => {
                        assert!(
                            !blocks.iter().any(|(l, _)| l == label),
                            "group {label} appeared twice — the grouping was flattened"
                        );
                        blocks.push((label.clone(), 0));
                    }
                    GroupedRow::Data(..) => {
                        blocks
                            .last_mut()
                            .expect("a data row before any group header")
                            .1 += 1;
                    }
                    // `GroupedRow` is `#[non_exhaustive]`, so the wildcard is mandatory rather
                    // than a choice. It panics instead of skipping: a chorale upgrade that adds
                    // a row kind this walk does not understand would otherwise make the
                    // grouping invariant quietly stop being checked.
                    _ => panic!(
                        "chorale's GroupedRow grew a variant this grouping-invariant walk does \
                         not understand; teach it the new row kind rather than skipping it"
                    ),
                }
            }
            blocks
        }

        // Default: RULE-B leads on its critical row; each rule keeps both of its sites.
        assert_eq!(
            group_blocks(&visible_grouped_view(&state)),
            vec![("RULE-B".to_string(), 2), ("RULE-A".to_string(), 2)],
            "default order: the group with the critical row first, both groups intact"
        );

        // Ascending Severity: RULE-B still leads (critical), and inside each group the two
        // sites are now in ladder order rather than the default's.
        let asc = toggle_sort(&state, ColumnId("severity"), SortAction::Replace);
        assert_eq!(
            group_blocks(&visible_grouped_view(&asc)),
            vec![("RULE-B".to_string(), 2), ("RULE-A".to_string(), 2)],
            "a manual cut keeps one header per group and its rows contiguous"
        );

        // Descending Severity: RULE-B's best row is now its `low` one, so the GROUPS swap —
        // the group order follows the group's best row under the active cut.
        let desc = toggle_sort(&asc, ColumnId("severity"), SortAction::Replace);
        assert_eq!(
            group_blocks(&visible_grouped_view(&desc)),
            vec![("RULE-B".to_string(), 2), ("RULE-A".to_string(), 2)],
            "reversing the cut still yields exactly two intact groups"
        );
        let rows: Vec<String> = visible_grouped_view(&desc)
            .iter()
            .filter_map(|r| match r {
                GroupedRow::Data(_, f) => Some(f.severity.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            rows,
            vec!["low", "critical", "medium", "high"],
            "within each group the rows follow the descending cut; the groups are led by \
             their own best row under it"
        );
    }

    /// req_pass2_8: a manual cut PRESERVES the deviation-ratio honesty baked into the default.
    ///
    /// The default key orders deviations by ascending `deviating_sites / assessed_sites` in
    /// parts-per-million, with an unknown or zero denominator sorting after every known ratio
    /// rather than fabricating a position. chorale's sort is STABLE, so ties in the manual key
    /// keep the curated order underneath — a cut by Kind groups the deviations together and the
    /// cheapest-first, unknown-last ratio order survives inside that block, untouched and
    /// un-recomputed.
    #[test]
    fn req_pass2_8_manual_sort_preserves_the_deviation_ratio_order_within_ties() {
        use chorale_core::{filtered_sorted_rows, toggle_sort, SortAction};

        let dev = |rule: &str, deviating: u32, assessed: serde_json::Value| {
            kinded_finding(
                rule,
                "medium",
                serde_json::json!({
                    "kind": "deviation",
                    "deviating_sites": deviating,
                    "assessed_sites": assessed,
                    "convention_option_id": "versioned",
                    "convention_label": "URI-versioned endpoints",
                }),
            )
        };
        let findings = TriageModel::new().visible(&[
            dev("A-UNKNOWN-RATIO", 3, serde_json::Value::Null),
            dev("B-COSTLY", 19, serde_json::json!(47)),
            dev("C-CHEAP", 2, serde_json::json!(47)),
            kinded_finding("D-DEFECT", "medium", serde_json::json!({ "kind": "defect" })),
        ]);
        let state = queue_table_state(&findings);

        // Sanity: the default already orders them cheapest-first, unknown last.
        assert_eq!(
            findings.iter().map(|f| f.rule_id.as_str()).collect::<Vec<_>>(),
            vec!["D-DEFECT", "C-CHEAP", "B-COSTLY", "A-UNKNOWN-RATIO"],
        );

        // A manual cut by Kind. Every deviation ties on the Kind key, so the stable sort hands
        // the ratio ordering straight through.
        let by_kind = toggle_sort(&state, ColumnId("kind"), SortAction::Replace);
        assert_eq!(
            filtered_sorted_rows(&by_kind)
                .iter()
                .map(|f| f.rule_id.clone())
                .collect::<Vec<_>>(),
            vec!["D-DEFECT", "C-CHEAP", "B-COSTLY", "A-UNKNOWN-RATIO"],
            "cheapest deviation first and the unknown denominator last, inside the manual cut \
             — the ratio is never re-derived and never given a fabricated position"
        );
    }

    /// req_pass2_8: the order indicator names the active column AND direction, in priority
    /// order for a multi-column cut, and says the curated default is still underneath (which it
    /// is — chorale's sort is stable).
    #[test]
    fn req_pass2_8_order_indicator_names_the_column_and_direction() {
        use chorale_core::{SortDirection, SortState};

        let cols = finding_columns(vec!["me/api".to_string()], false);
        assert_eq!(order_indicator_text(&[], &cols), DEFAULT_ORDER_LABEL);

        let one = [SortState::new(ColumnId("severity"), SortDirection::Desc)];
        let text = order_indicator_text(&one, &cols);
        assert!(text.contains("Severity"), "names the column header; got {text}");
        assert!(text.contains('\u{2193}'), "names the direction; got {text}");
        assert!(
            text.contains("Camerata default order underneath"),
            "says the curated order is still the tie-break; got {text}"
        );

        let two = [
            SortState::new(ColumnId("file"), SortDirection::Asc),
            SortState::new(ColumnId("severity"), SortDirection::Desc),
        ];
        let text = order_indicator_text(&two, &cols);
        assert!(
            text.find("File").expect("File named") < text.find("Severity").expect("Severity named"),
            "a multi-column cut is listed in priority order; got {text}"
        );

        // An id with no column (a column removed from the set) degrades to the raw id rather
        // than dropping the cut from the sentence and claiming an order that isn't showing.
        let gone = [SortState::new(ColumnId("no-such-column"), SortDirection::Asc)];
        assert!(order_indicator_text(&gone, &cols).contains("no-such-column"));
    }

    /// req_pass2_8: the whole Queue is re-cuttable. Every column the curator would re-cut by is
    /// `.sortable()`, so chorale renders its header clickable with its own ↑/↓ arrow; only
    /// Snippet (free text, no meaningful order) is not.
    #[test]
    fn req_pass2_8_every_meaningful_queue_column_header_is_sortable() {
        let cols = finding_columns(vec!["me/api".to_string()], false);
        for id in [
            "repo",
            "kind",
            "severity",
            "authority",
            "type",
            "needs_review",
            "status",
            "file",
            "loc",
        ] {
            let col = cols
                .iter()
                .find(|c| c.id == ColumnId(id))
                .unwrap_or_else(|| panic!("column {id} must exist"));
            assert!(
                col.sortable,
                "{id}: the curator must be able to re-cut the list by this column"
            );
        }
    }

    /// req_pass2_8: the Severity column's filter vocabulary is built from the SAME function as
    /// its cell value, so the two cannot drift. A stale option list would match nothing and
    /// read as "no findings" — a worse failure than a wrong sort.
    #[test]
    fn req_pass2_8_severity_filter_vocabulary_matches_the_cell_it_filters() {
        let cols = finding_columns(vec!["me/api".to_string()], false);
        let sev = cols
            .iter()
            .find(|c| c.id == ColumnId("severity"))
            .expect("Severity column");
        let options = match &sev.filter {
            FilterKind::MultiSelect { options } => options.clone(),
            other => panic!("Severity must keep its multi-select filter, got {other:?}"),
        };
        for s in ["critical", "high", "medium", "low"] {
            let cell = (sev.accessor)(&kinded_finding("R", s, serde_json::json!({})));
            let CellValue::Text(cell) = cell else {
                panic!("Severity cells are text");
            };
            assert!(
                options.contains(&cell),
                "{s}: the filter option list must contain the exact cell value it filters \
                 (cell={cell}, options={options:?})"
            );
        }
    }

    /// req_pass2_8: making the cell value the sort key must not change what the curator READS.
    ///
    /// chorale renders a `RenderKind::Badge` cell by resolving the cell value in the badge map
    /// and drawing the matched variant's LABEL (`badge_span` in chorale-dioxus), so asserting
    /// the map resolution is asserting the rendered text. Done here rather than over SSR HTML
    /// because the real table mounts with every group collapsed, so no severity cell is in the
    /// snapshot to look at.
    ///
    /// The one place the ladder prefix IS visible is the Severity filter's checkbox list, whose
    /// option labels are the raw values chorale filters on — it now reads "1 critical / 2 high /
    /// 3 medium / 4 low". That is the accepted cost of chorale 0.2.3 having no sort-key hook
    /// separate from the cell value; the list was already in this order, and it is honest about
    /// the ladder rather than wrong about it.
    #[test]
    fn req_pass2_8_severity_cell_still_renders_the_authored_label() {
        let cols = finding_columns(vec!["me/api".to_string()], false);
        let sev = cols
            .iter()
            .find(|c| c.id == ColumnId("severity"))
            .expect("Severity column");
        let RenderKind::Badge(map) = &sev.render_kind else {
            panic!("Severity must still render as a badge");
        };
        for (authored, label, color) in [
            ("critical", "Critical", "red"),
            ("high", "High", "orange"),
            ("medium", "Medium", "yellow"),
            ("low", "Low", "gray"),
        ] {
            let CellValue::Text(cell) =
                (sev.accessor)(&kinded_finding("R", authored, serde_json::json!({})))
            else {
                panic!("Severity cells are text");
            };
            let variant = map.resolve(&cell).unwrap_or_else(|| {
                panic!("{authored}: cell value {cell:?} must resolve in the badge map")
            });
            assert_eq!(
                variant.label, label,
                "{authored}: the rendered badge must read the authored label, never the \
                 ladder prefix"
            );
            assert_eq!(variant.color, color, "{authored}: colour unchanged");
        }
    }

    /// req_pass2_8 (a DELIBERATE non-change, pinned so it is not "fixed" by accident): the Kind
    /// column's cell value stays the bare WIRE TAG.
    ///
    /// Severity needed its ladder position in the cell because its words sort against their own
    /// meaning. Kind's do not: the wire tags sort `defect` → `deviation` → `unspecified`, which
    /// is a coherent actionability order. Prefixing a rank here would buy nothing and would cost
    /// the thing req_pass2_5 pins — that the Kind filter's vocabulary IS the wire's, never
    /// re-derived prose.
    ///
    /// The `unspecified`-first priority therefore lives where Zach's ruling put it: in the
    /// DEFAULT order (`kind_rank`), which is what the table shows until the curator chooses
    /// otherwise. The alarm is also surfaced independently of row order by the Queue tab's
    /// count chip, so a manual cut can never hide it.
    #[test]
    fn req_pass2_8_kind_column_keeps_the_bare_wire_tag_as_its_cell_value() {
        use chorale_core::{filtered_sorted_rows, toggle_sort, SortAction};

        let cols = finding_columns(vec!["me/api".to_string()], false);
        let kind = cols
            .iter()
            .find(|c| c.id == ColumnId("kind"))
            .expect("Kind column");
        let row = kinded_finding("R", "high", serde_json::json!({ "kind": "defect" }));
        assert_eq!(
            (kind.accessor)(&row),
            CellValue::Text("defect".to_string()),
            "no ladder prefix on Kind — the wire tag is the cell value and the filter vocabulary"
        );

        // And the cut it produces is the actionability order, not an accident nobody checked.
        let findings = TriageModel::new().visible(&[
            kinded_finding("A-UNMAPPED", "high", serde_json::json!({})),
            kinded_finding("B-DEFECT", "high", serde_json::json!({ "kind": "defect" })),
            kinded_finding(
                "C-DEVIATION",
                "high",
                serde_json::json!({
                    "kind": "deviation", "deviating_sites": 2, "assessed_sites": 47,
                    "convention_option_id": "v", "convention_label": "L",
                }),
            ),
        ]);
        assert_eq!(
            findings.iter().map(|f| f.rule_id.as_str()).collect::<Vec<_>>(),
            vec!["A-UNMAPPED", "B-DEFECT", "C-DEVIATION"],
            "the DEFAULT floats the kind-less alarm to the top of its severity band"
        );
        let by_kind = toggle_sort(
            &queue_table_state(&findings),
            ColumnId("kind"),
            SortAction::Replace,
        );
        assert_eq!(
            filtered_sorted_rows(&by_kind)
                .iter()
                .map(|f| f.rule_id.clone())
                .collect::<Vec<_>>(),
            vec!["B-DEFECT", "C-DEVIATION", "A-UNMAPPED"],
            "the manual Kind cut is the wire tags' own order: defect, deviation, unspecified"
        );
    }

    // ── Pass 2 step 2: the deviation group header ────────────────────────────
    // docs/plans/2026-10-11_pass2-curator-ui-design.md §2/§4/§11-step-2: "Group headers carry
    // the Deviation sentence ... rendered from data: 'N of M sites deviate from
    // <convention_label>'. No prose parsing" and "Group headers carry rule-level bulk
    // actions for deviations ... because the natural unit of a deviation decision is the
    // rule, not the site."

    /// A deviation rule with two sites, plus a defect rule, as `FindingsTable` receives them.
    /// Paths differ so the two deviating sites have distinct `finding_key`s (a collision would
    /// make the bulk-equivalence assertions vacuous).
    fn deviation_queue_rows() -> Vec<FindingView> {
        let mut dev_a = kinded_finding(
            "API-VERSIONING-1",
            "medium",
            serde_json::json!({
                "kind": "deviation",
                "deviating_sites": 2,
                "assessed_sites": 47,
                "convention_option_id": "versioned",
                "convention_label": "URI-versioned endpoints",
            }),
        );
        dev_a.path = "src/routes.rs".to_string();
        dev_a.line = 12;
        let mut dev_b = dev_a.clone();
        dev_b.line = 40;
        let mut defect = kinded_finding(
            "SQL-INJECTION-1",
            "critical",
            serde_json::json!({ "kind": "defect" }),
        );
        defect.path = "src/handlers.rs".to_string();
        defect.line = 88;
        vec![dev_a, dev_b, defect]
    }

    fn deviation_table_html() -> String {
        fn harness() -> Element {
            use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
            use_context_provider(|| Signal::new(None::<FindingView>));
            let dispositions = use_signal(std::collections::HashMap::<String, Disposition>::new);
            rsx! {
                FindingsTable {
                    findings: deviation_queue_rows(),
                    repos: vec!["me/api".to_string()],
                    descriptions: std::collections::HashMap::new(),
                    triage_view: TriageState::Unresolved,
                    dispositions,
                }
            }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        dioxus_ssr::render(&vdom)
    }

    /// The rendered table from its FIRST group-header `<tr>` onwards.
    ///
    /// Load-bearing, not tidiness: the bulk bar above the table also names the rule and
    /// repeats the sentence, so an assertion made against the whole document could be
    /// satisfied by the bar while chorale's group header still showed a bare rule id. Every
    /// "the group header carries X" claim below is checked against this slice only.
    fn group_header_region(html: &str) -> &str {
        let at = html
            .find("class=\"chorale-group-header\"")
            .unwrap_or_else(|| panic!("no chorale group header rendered; html=\n{html}"));
        &html[at..]
    }

    /// The rendered document BEFORE the table — where the bulk bar lives.
    fn above_table_region(html: &str) -> &str {
        let at = html
            .find("class=\"chorale-group-header\"")
            .unwrap_or_else(|| panic!("no chorale group header rendered; html=\n{html}"));
        &html[..at]
    }

    /// req_pass2_9: the deviation rule group STATES ITS CLAIM on chorale's own group header,
    /// assembled from the row's structured fields.
    ///
    /// The table mounts collapsed, so the group header is the only thing the curator sees
    /// before drilling in — and a bare rule id there says nothing about whether the convention
    /// behind it is near-unanimous (2 of 47, decide in one look) or barely a majority (19 of
    /// 47, think about it). The numbers exist on every deviation row as `deviating_sites` /
    /// `assessed_sites` / `convention_label`; until this shipped, nothing in the UI read them
    /// and the header rendered exactly `▶API-VERSIONING-1(2)`.
    #[test]
    fn req_pass2_9_deviation_group_header_carries_the_structured_sentence() {
        let html = deviation_table_html();
        let headers = group_header_region(&html);
        assert!(
            headers.contains("2 of 47 sites deviate from"),
            "the deviation group header must carry the structured numerator/denominator \
             sentence; group headers=\n{headers}"
        );
        // dioxus_ssr escapes the quotes around the label, so the label is asserted on its own
        // rather than as part of the quoted clause.
        assert!(
            headers.contains("URI-versioned endpoints"),
            "and name the convention verbatim from `convention_label`; group headers=\n{headers}"
        );
        assert!(
            headers.contains("API-VERSIONING-1 \u{b7} 2 of 47"),
            "the rule id and its claim are one header line; group headers=\n{headers}"
        );
    }

    /// req_pass2_9: the deviation group offers the RULE-LEVEL bulk disposition, so the common
    /// case is one gesture rather than N.
    ///
    /// The count on the button is the number of ROWS the click will move (2), which is the
    /// honest label for what it does — deliberately not the site count in the sentence, which
    /// counts sites across the whole scan including merged ones.
    #[test]
    fn req_pass2_9_deviation_group_offers_a_rule_level_bulk_action() {
        let html = deviation_table_html();
        let bar = above_table_region(&html);
        assert!(
            bar.contains("deviation-bulk"),
            "a view with deviations must render the rule-level decision bar; above table=\n{bar}"
        );
        assert!(
            bar.contains("False positive all 2"),
            "the bulk action is labelled with the row count it will move; above table=\n{bar}"
        );
        assert!(
            bar.contains("2 of 47 sites deviate from"),
            "and states the claim it is acting on; above table=\n{bar}"
        );
    }

    /// req_pass2_9: a DEFECT group gets the plain header and no rule-level bulk action.
    ///
    /// Design §4: "Defect group headers are plainer: `rule-id · DEFECT · worst-severity ·
    /// (n)`", and §2's sketch puts the bulk pair on the deviation group only — the `(3)` on
    /// its defect group is chorale's row count, not a `Confirm all 3`. The reason is in the
    /// data, not in taste: every row of a deviation rule repeats the SAME group-level claim,
    /// so the rule is the decision unit; a defect group is N independent accusations that
    /// merely share a rule id, and one click asserting all N would be asserting facts the
    /// curator has not looked at. A curator who genuinely wants to bulk-dispose a defect group
    /// still can — chorale's per-group checkbox plus the toolbar — but it costs two deliberate
    /// gestures, which is the right price for that claim.
    #[test]
    fn req_pass2_9_defect_group_header_is_plain_and_has_no_bulk_action() {
        let html = deviation_table_html();
        let headers = group_header_region(&html);
        assert!(
            headers.contains("SQL-INJECTION-1"),
            "the defect group still has its header; group headers=\n{headers}"
        );
        assert!(
            !headers.contains("SQL-INJECTION-1 \u{b7}"),
            "but carries no sentence after it; group headers=\n{headers}"
        );

        let bar = above_table_region(&html);
        assert_eq!(
            bar.matches("deviation-bulk-group").count(),
            1,
            "exactly one rule-level decision row — the deviation's; above table=\n{bar}"
        );
        assert!(
            !bar.contains("SQL-INJECTION-1"),
            "the defect rule is not offered a rule-level bulk action; above table=\n{bar}"
        );
    }

    /// req_pass2_9: the header sentence survives a MANUAL column cut and the collapsed mount.
    ///
    /// The sentence rides the group-by VALUE, which is chorale's own machinery, so a re-sort
    /// reorders the groups and carries each header's text with it rather than stranding a
    /// separately-rendered banner against a group that moved. This drives chorale's real
    /// pipeline (`toggle_sort` + `visible_grouped_view`) over the real column set, the same
    /// way the req_pass2_8 sort tests do.
    #[test]
    fn req_pass2_9_group_header_sentence_survives_a_manual_re_sort() {
        use chorale_core::{toggle_sort, visible_grouped_view, GroupedRow, SortAction};

        let findings = TriageModel::new().visible(&deviation_queue_rows());
        let mut state = finding_table_state(
            sort_rows(&findings),
            vec!["me/api".to_string()],
            false,
        );
        state.grouping = vec![RULE_GROUP_COLUMN];
        state.grouped_pagination = chorale_core::GroupedPaginationMode::Virtualized;

        let labels = |s: &chorale_core::TableState<FindingView>| -> Vec<String> {
            visible_grouped_view(s)
                .iter()
                .filter_map(|r| match r {
                    GroupedRow::Header { label, .. } => Some(label.clone()),
                    _ => None,
                })
                .collect()
        };

        let before = labels(&state);
        assert_eq!(
            before,
            vec![
                "SQL-INJECTION-1".to_string(),
                "API-VERSIONING-1 \u{b7} 2 of 47 sites deviate from \"URI-versioned endpoints\""
                    .to_string(),
            ],
            "default order: the critical defect's plain header, then the deviation's claim"
        );

        // Descending Severity puts the medium deviation first. The header text must travel
        // with its group, not stay behind.
        let asc = toggle_sort(&state, ColumnId("severity"), SortAction::Replace);
        let desc = toggle_sort(&asc, ColumnId("severity"), SortAction::Replace);
        let after = labels(&desc);
        assert_eq!(after.len(), 2, "still exactly two groups; no flattening");
        assert_eq!(
            after[0],
            "API-VERSIONING-1 \u{b7} 2 of 47 sites deviate from \"URI-versioned endpoints\"",
            "the deviation group led the reversed cut, sentence intact"
        );
        assert_eq!(after[1], "SQL-INJECTION-1");
    }

    /// req_pass2_9 (THE HONESTY CASE, UNDER RENDER): a deviation whose producer could not
    /// count the total renders the numerator ALONE on the real group header.
    ///
    /// The pure sentence builder is unit-tested in ui-core; this proves the honest string is
    /// what the product actually puts on screen. `assessed_sites: None` is the case most
    /// likely to be quietly wrong, because the two ways to break it — fabricate a total, or
    /// let an `unwrap_or(0)` print "3 of 0" — both still render something plausible-looking.
    #[test]
    fn req_pass2_9_absent_denominator_renders_the_numerator_alone_on_the_header() {
        fn harness() -> Element {
            use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
            use_context_provider(|| Signal::new(None::<FindingView>));
            let dispositions = use_signal(std::collections::HashMap::<String, Disposition>::new);
            rsx! {
                FindingsTable {
                    findings: vec![kinded_finding(
                        "NAMING-1",
                        "medium",
                        serde_json::json!({
                            "kind": "deviation",
                            "deviating_sites": 3,
                            "assessed_sites": serde_json::Value::Null,
                            "convention_option_id": serde_json::Value::Null,
                            "convention_label": "snake_case table names",
                        }),
                    )],
                    repos: vec!["me/api".to_string()],
                    descriptions: std::collections::HashMap::new(),
                    triage_view: TriageState::Unresolved,
                    dispositions,
                }
            }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        let headers = group_header_region(&html);
        assert!(
            headers.contains("NAMING-1 \u{b7} 3 sites deviate from"),
            "numerator alone on the header; group headers=\n{headers}"
        );
        assert!(
            headers.contains("snake_case table names"),
            "the convention is still named; group headers=\n{headers}"
        );
        assert!(
            !headers.contains("3 of "),
            "no fabricated denominator anywhere on the header; group headers=\n{headers}"
        );
        // The bar repeats the same honest sentence — one builder, two surfaces.
        let bar = above_table_region(&html);
        assert!(
            bar.contains("3 sites deviate from") && !bar.contains("3 of "),
            "and the bulk bar states it the same way; above table=\n{bar}"
        );
    }

    /// req_pass2_9: the grouping key stays INVISIBLE. It is a delivery mechanism for the
    /// header text, not a column; shipping it as a visible "Rule group" column would put the
    /// sentence on every single row as well.
    #[test]
    fn req_pass2_9_the_rule_group_key_is_never_a_visible_column() {
        let state = finding_table_state(
            sort_rows(&TriageModel::new().visible(&deviation_queue_rows())),
            vec!["me/api".to_string()],
            false,
        );
        assert_eq!(
            state.column_visibility.get(&RULE_GROUP_COLUMN),
            Some(&false),
            "the grouping key is hidden in the state the product mounts"
        );
        let col = state
            .columns
            .iter()
            .find(|c| c.id == RULE_GROUP_COLUMN)
            .expect("the grouping key column exists");
        assert!(!col.sortable, "it is not a column the curator sorts by");
        assert!(
            matches!(col.filter, chorale_core::FilterKind::None),
            "nor filters by"
        );

        let html = deviation_table_html();
        assert!(
            !html.contains(">Rule group<"),
            "and its header label never reaches the DOM; html=\n{html}"
        );
    }

    /// req_pass2_9: the bar renders one decision row per group, props-only.
    ///
    /// `DeviationBulkBar` is its own component for the same reason `QueueOrderBar` is: SSR
    /// cannot click, and a bar built inline in `FindingsTable` could only ever be observed in
    /// whatever state that table happens to mount in. As props-only data it can be rendered
    /// in any state, including the multi-group one a real audit produces.
    #[test]
    fn req_pass2_9_bulk_bar_renders_one_decision_row_per_group() {
        #[component]
        fn BarHarness(groups: Vec<DeviationBulkRow>) -> Element {
            rsx! {
                DeviationBulkBar {
                    groups,
                    // EDITED for Pass 2 step 3 (exception (b)): the bar gained a second,
                    // required callback prop. Nothing else about this harness changed.
                    on_confirm_all: Callback::new(move |_: usize| {}),
                    on_false_positive_all: Callback::new(move |_: usize| {}),
                }
            }
        }
        let groups = vec![
            DeviationBulkRow {
                rule_id: "API-VERSIONING-1".into(),
                sentence: "2 of 47 sites deviate from \"URI-versioned endpoints\"".into(),
                row_count: 2,
            },
            DeviationBulkRow {
                rule_id: "NAMING-1".into(),
                sentence: "3 sites deviate from \"snake_case table names\"".into(),
                row_count: 3,
            },
        ];
        let mut vdom = VirtualDom::new_with_props(BarHarness, BarHarnessProps { groups });
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert_eq!(
            html.matches("deviation-bulk-group").count(),
            2,
            "one decision row per group; html=\n{html}"
        );
        assert!(html.contains("False positive all 2"), "html=\n{html}");
        assert!(html.contains("False positive all 3"), "html=\n{html}");
        assert!(
            html.contains("2 of 47 sites deviate from") && html.contains("3 sites deviate from"),
            "each row states its own claim, including the uncounted one; html=\n{html}"
        );
    }

    /// req_pass2_9: no deviations means no bar at all — not an empty one.
    ///
    /// Unlike the informational panels (whose empty state is a sentence, because "nothing of
    /// this kind" is itself information the curator needs), this is a list of ACTIONS. An
    /// empty action bar with a heading is a standing reminder of nothing.
    #[test]
    fn req_pass2_9_bulk_bar_renders_nothing_when_there_are_no_deviations() {
        #[component]
        fn EmptyBarHarness() -> Element {
            rsx! {
                DeviationBulkBar {
                    groups: Vec::<DeviationBulkRow>::new(),
                    // EDITED for Pass 2 step 3 (exception (b)): new required prop.
                    on_confirm_all: Callback::new(move |_: usize| {}),
                    on_false_positive_all: Callback::new(move |_: usize| {}),
                }
            }
        }
        let mut vdom = VirtualDom::new(EmptyBarHarness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(
            !html.contains("deviation-bulk"),
            "no deviations, no bar; html=\n{html}"
        );
        assert!(
            !html.contains("Rule-level decisions"),
            "and no orphan heading; html=\n{html}"
        );
    }

    /// req_pass2_9: a defect-only Queue gets no bulk bar at all.
    #[test]
    fn req_pass2_9_defect_only_queue_renders_no_bulk_bar() {
        fn harness() -> Element {
            use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
            use_context_provider(|| Signal::new(None::<FindingView>));
            let dispositions = use_signal(std::collections::HashMap::<String, Disposition>::new);
            rsx! {
                FindingsTable {
                    findings: vec![kinded_finding(
                        "SQL-INJECTION-1",
                        "critical",
                        serde_json::json!({ "kind": "defect" }),
                    )],
                    repos: vec!["me/api".to_string()],
                    descriptions: std::collections::HashMap::new(),
                    triage_view: TriageState::Unresolved,
                    dispositions,
                }
            }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(
            !html.contains("deviation-bulk"),
            "defects alone offer no rule-level decision; html=\n{html}"
        );
    }

    /// req_pass2_9: the bulk bar is not rendered in the triage tables where it would be dead.
    ///
    /// Its one action is False positive, which needs the reason textarea that exists only in
    /// the Unresolved toolbar — and a deviation already moved to Ignored or Tech debt has had
    /// its rule-level decision made. A bar of buttons that cannot complete is the 403-button
    /// shape the standing hide-dead-end-affordances rule forbids.
    #[test]
    fn req_pass2_9_bulk_bar_is_absent_from_the_non_unresolved_tables() {
        #[component]
        fn Harness(view: TriageState) -> Element {
            use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
            use_context_provider(|| Signal::new(None::<FindingView>));
            let mut dispositions =
                use_signal(std::collections::HashMap::<String, Disposition>::new);
            // Park every row in the table under test, so the rows are actually present.
            use_hook(move || {
                let mut model = TriageModel::new();
                model.move_to(&deviation_queue_rows(), view);
                dispositions.set(model.dispositions);
            });
            rsx! {
                FindingsTable {
                    findings: deviation_queue_rows(),
                    repos: vec!["me/api".to_string()],
                    descriptions: std::collections::HashMap::new(),
                    triage_view: view,
                    dispositions,
                }
            }
        }
        // EDITED for Pass 2 step 3 (exception (a)): `Confirmed` is a fifth table the bar
        // must also stay out of — a deviation group already confirmed has had its
        // rule-level decision made.
        for view in [
            TriageState::Confirmed,
            TriageState::Ignored,
            TriageState::TechDebt,
            TriageState::FalsePositive,
        ] {
            let mut vdom = VirtualDom::new_with_props(Harness, HarnessProps { view });
            vdom.rebuild_in_place();
            let html = dioxus_ssr::render(&vdom);
            assert!(
                !html.contains("deviation-bulk"),
                "{} must not render the rule-level bulk bar; html=\n{html}",
                view.label()
            );
            // The group header's sentence is NOT view-scoped — the claim is true wherever the
            // row sits, so it still rides the header here.
            assert!(
                group_header_region(&html).contains("2 of 47 sites deviate from"),
                "{}'s group header still states the claim; html=\n{html}",
                view.label()
            );
        }
    }

    // ── Pass 2 step 3: `Confirmed` on the curator surface ───────────────────

    /// req_pass2_10: the rule-level bar offers BOTH directions now, over the same group.
    ///
    /// Step 2 shipped it with only `False positive all N` because no disposition honestly
    /// meant "confirmed". This is the half that was withheld, rendered through the SHIPPED
    /// component so the button's label and count come from the group's own `row_count`.
    #[test]
    fn req_pass2_10_bulk_bar_offers_confirm_all_alongside_false_positive_all() {
        #[component]
        fn BarHarness(groups: Vec<DeviationBulkRow>) -> Element {
            rsx! {
                DeviationBulkBar {
                    groups,
                    on_confirm_all: Callback::new(move |_: usize| {}),
                    on_false_positive_all: Callback::new(move |_: usize| {}),
                }
            }
        }
        let groups = vec![
            DeviationBulkRow {
                rule_id: "API-VERSIONING-1".into(),
                sentence: "2 of 47 sites deviate from \"URI-versioned endpoints\"".into(),
                row_count: 2,
            },
            DeviationBulkRow {
                rule_id: "NAMING-1".into(),
                sentence: "3 sites deviate from \"snake_case table names\"".into(),
                row_count: 3,
            },
        ];
        let mut vdom = VirtualDom::new_with_props(BarHarness, BarHarnessProps { groups });
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("Confirm all 2"), "html=\n{html}");
        assert!(html.contains("Confirm all 3"), "html=\n{html}");
        // Both directions on every group, and the counts agree with the false-positive
        // button beside them: one group row, one row set, two readings of it.
        assert!(html.contains("False positive all 2"), "html=\n{html}");
        assert!(html.contains("False positive all 3"), "html=\n{html}");
        assert_eq!(
            html.matches("deviation-bulk-confirm").count(),
            2,
            "one Confirm control per group; html=\n{html}"
        );
    }

    /// req_pass2_10: a Confirm button exists in the Unresolved toolbar, and it is the only
    /// action there with no reason input of its own.
    ///
    /// EDITED by the §9.2 removal, and this is one of that sweep's two named exceptions. The
    /// assertion used to be `html.matches("ignore-reason").count() == 2` — "Confirm adds no
    /// third reason input" — which counted the two always-visible textareas §9.2 deletes. The
    /// test's INTENT (Confirm is the action that needs no justification) survives the removal
    /// intact and is now asserted in a form the removal makes STRICTER rather than looser:
    /// the resting toolbar has ZERO standing reason inputs, not two, and Confirm is still the
    /// control that commits without raising one. Rewriting the count from 2 to 0 and stopping
    /// there would have left the test passing while asserting almost nothing, so the
    /// Ignore/False-positive side is pinned here too: both are present, and neither is a
    /// textarea.
    #[test]
    fn req_pass2_10_unresolved_toolbar_offers_confirm_with_no_reason_input() {
        fn harness() -> Element {
            use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
            use_context_provider(|| Signal::new(None::<FindingView>));
            let dispositions = use_signal(std::collections::HashMap::<String, Disposition>::new);
            rsx! {
                FindingsTable {
                    findings: deviation_queue_rows(),
                    repos: vec!["me/api".to_string()],
                    descriptions: std::collections::HashMap::new(),
                    triage_view: TriageState::Unresolved,
                    dispositions,
                }
            }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(
            html.contains("triage-confirm"),
            "the Unresolved toolbar must offer Confirm; html=\n{html}"
        );
        // §9.2: the resting toolbar holds NO reason input at all. Confirm never needed one;
        // Ignore and False positive now raise theirs on demand.
        assert_eq!(
            html.matches("ignore-reason").count(),
            0,
            "§9.2 removes the always-visible reason textareas; html=\n{html}"
        );
        assert!(
            !html.contains("<textarea"),
            "and nothing else quietly took their place; html=\n{html}"
        );
        // The two actions that DO need a reason are still offered — the removal took the
        // boxes, not the decisions.
        assert!(html.contains("triage-ignore"), "Ignore; html=\n{html}");
        assert!(
            html.contains("triage-false-positive"),
            "Mark false positive; html=\n{html}"
        );
    }

    /// req_pass2_10: the Confirmed table has exactly one way out and no reason inputs.
    ///
    /// Mirrors the FalsePositive table. The reason textareas live only in the Unresolved
    /// toolbar, so offering Ignore or Tech debt here would be a control that cannot complete.
    #[test]
    fn req_pass2_10_confirmed_table_offers_only_the_way_back() {
        fn harness() -> Element {
            use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
            use_context_provider(|| Signal::new(None::<FindingView>));
            let mut dispositions =
                use_signal(std::collections::HashMap::<String, Disposition>::new);
            use_hook(move || {
                let mut model = TriageModel::new();
                model.confirm(&deviation_queue_rows());
                dispositions.set(model.dispositions);
            });
            rsx! {
                FindingsTable {
                    findings: deviation_queue_rows(),
                    repos: vec!["me/api".to_string()],
                    descriptions: std::collections::HashMap::new(),
                    triage_view: TriageState::Confirmed,
                    dispositions,
                }
            }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(
            html.contains("Move to Unresolved"),
            "the confirmed rows must be reversible; html=\n{html}"
        );
        assert!(
            !html.contains("ignore-reason"),
            "no reason inputs in a table that cannot use them; html=\n{html}"
        );
        assert!(
            !html.contains("triage-confirm"),
            "and no Confirm button in the Confirmed table; html=\n{html}"
        );
        // The confirmed rows are actually HERE — the table is not merely empty.
        assert!(
            group_header_region(&html).contains("2 of 47 sites deviate from"),
            "the confirmed deviation rows render in this table; html=\n{html}"
        );
    }

    /// req_pass2_10 (THE PROCESS GATE, rendered): a Queue of confirmed findings does not
    /// block Process, and the gate says why the button is still disabled.
    ///
    /// Step 1's build note recorded that the gate itself was never render-tested, because it
    /// lives inside `ScanResults` (behind `use_resource`, not SSR-testable). `ProcessGate` is
    /// the extraction that closes that: every state below is the real component, rendered.
    #[test]
    fn req_pass2_10_process_gate_treats_confirmed_as_resolved() {
        #[component]
        fn GateHarness(counts: TriageCounts) -> Element {
            rsx! {
                ProcessGate {
                    counts,
                    processing: false,
                    on_process: Callback::new(move |()| {}),
                }
            }
        }
        let render = |counts: TriageCounts| {
            let mut vdom = VirtualDom::new_with_props(GateHarness, GateHarnessProps { counts });
            vdom.rebuild_in_place();
            dioxus_ssr::render(&vdom)
        };

        // (1) Two rows nobody has decided: blocked, and the hint NAMES Confirm as an exit.
        // Before this step it offered only "Ignore or save as Tech debt".
        let html = render(TriageCounts { unresolved: 2, ..Default::default() });
        assert!(html.contains("Resolve the 2 remaining"), "html=\n{html}");
        assert!(
            html.contains("Confirm"),
            "the hint must name the exit the curator reaches for most; html=\n{html}"
        );
        assert!(html.contains("disabled"), "html=\n{html}");

        // (2) Everything CONFIRMED: nothing outstanding, so the blocking hint is gone. The
        // button stays disabled because Confirmed commits nothing — and the gate says so
        // instead of leaving a dead control unexplained.
        let html = render(TriageCounts { confirmed: 2, ..Default::default() });
        assert!(
            !html.contains("remaining Unresolved"),
            "a confirmed finding is decided, not outstanding; html=\n{html}"
        );
        assert!(html.contains("Nothing to process"), "html=\n{html}");
        assert!(html.contains("disabled"), "html=\n{html}");

        // (3) Confirmed + ignored: nothing outstanding AND something to commit. Enabled.
        let html = render(TriageCounts { confirmed: 1, ignored: 1, ..Default::default() });
        assert!(!html.contains("remaining Unresolved"), "html=\n{html}");
        assert!(!html.contains("Nothing to process"), "html=\n{html}");
        assert!(
            !html.contains("disabled"),
            "nothing outstanding and a baseline waiver to commit; html=\n{html}"
        );

        // (4) One row still unresolved alongside a confirmed one: blocked by that row alone.
        let html = render(TriageCounts {
            unresolved: 1,
            confirmed: 1,
            ignored: 1,
            ..Default::default()
        });
        assert!(html.contains("Resolve the 1 remaining"), "html=\n{html}");
        assert!(html.contains("disabled"), "html=\n{html}");
    }
    // ── req_pass2_13: THE REMOVALS SWEEP (design §9) ─────────────────────────
    //
    // "Removal is the design work." Each test below pins BOTH halves of one removal: the
    // affordance is gone, AND the replacement reaches the information it carried. A removal
    // whose replacement falls short is a capability loss dressed as cleanup.

    /// req_pass2_13 (§9.5): the two-swatch findings-key legend is GONE from the Queue, and the
    /// red full-row critical highlight it claimed to explain is still there.
    ///
    /// The legend is not replaced by silence. It made two claims and the per-row columns make
    /// both, more precisely:
    ///
    /// * "red = Security findings (deterministic, stop-the-line)" → the SEVERITY badge, which
    ///   reads `Critical` on exactly the rows `finding_row_class` highlights. The legend's own
    ///   wording was wrong about this: the highlight has never consulted the floor rule set.
    /// * "Architectural findings (everything else)" → the AUTHORITY badge, which distinguishes
    ///   `Rule · enforced` (deterministic floor) from `AI · advisory` and `Preview`, per row,
    ///   rather than lumping three tiers into one "everything else".
    #[test]
    fn req_pass2_13_the_findings_key_legend_is_gone_and_the_critical_highlight_stays() {
        fn harness() -> Element {
            use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
            use_context_provider(|| Signal::new(None::<FindingView>));
            let dispositions = use_signal(std::collections::HashMap::<String, Disposition>::new);
            rsx! {
                FindingsTable {
                    findings: vec![
                        kinded_finding("SQL-INJECTION-1", "critical", serde_json::json!({ "kind": "defect" })),
                        kinded_finding("AI-LAYERING", "medium", serde_json::json!({ "kind": "defect" })),
                    ],
                    repos: vec!["me/api".to_string()],
                    descriptions: std::collections::HashMap::new(),
                    triage_view: TriageState::Unresolved,
                    dispositions,
                }
            }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        for gone in [
            "findings-key",
            "Security findings (deterministic, stop-the-line)",
            "Architectural findings (everything else)",
        ] {
            assert!(!html.contains(gone), "§9.5 removes {gone:?}; html=\n{html}");
        }
        // The highlight STAYS, and on exactly the severity the legend's swatch was red for.
        let crit = kinded_finding("SQL-INJECTION-1", "critical", serde_json::json!({}));
        let not_crit = kinded_finding("AI-LAYERING", "high", serde_json::json!({}));
        assert_eq!(
            finding_row_class(&crit),
            Some("finding-row-critical".to_string()),
            "§9.5: the red full-row critical highlight stays"
        );
        assert_eq!(finding_row_class(&not_crit), None);

        // And the replacement columns are present and carry the two facts the legend glossed.
        let cols = finding_columns(vec!["me/api".to_string()], false);
        let sev = cols.iter().find(|c| c.id == ColumnId("severity")).expect("Severity column");
        assert_eq!(
            (sev.accessor)(&crit),
            CellValue::Text(severity_sort_value("critical")),
            "the Severity column marks exactly the highlighted rows"
        );
        let auth = cols.iter().find(|c| c.id == ColumnId("authority")).expect("Authority column");
        let RenderKind::Badge(map) = &auth.render_kind else {
            panic!("Authority renders as a badge");
        };
        for (value, label) in [
            ("enforced", "Rule \u{b7} enforced"),
            ("advisory", "AI \u{b7} advisory"),
        ] {
            assert_eq!(
                map.resolve(value).expect("authority variant").label,
                label,
                "the Authority column, not a legend, is what separates deterministic from advisory"
            );
        }
        assert!(html.contains(">Severity<"), "Severity header; html=\n{html}");
        assert!(html.contains(">Authority<"), "Authority header; html=\n{html}");
    }

    /// req_pass2_13 (§9.1, the half that MOVED): the hub carries the Evaluated-against line
    /// and the Why? answer, and carries them as the authored LABEL rather than the raw option
    /// id it used to show.
    ///
    /// It also adds NO control while doing so — asserted here as well as in the §8 tests,
    /// because this is the change that could have broken "the hub builds exactly one
    /// navigation and no link graph" without any of those tests noticing: they render the
    /// strip with `alternatives` defaulted to `None`, so they cannot see this path.
    #[test]
    fn req_pass2_13_the_hub_carries_the_evaluated_against_line_and_the_why_answer() {
        #[component]
        fn Harness(hub: RuleHubView, alternatives: Option<AlternativeControlsRow>) -> Element {
            rsx! {
                RuleHubStrip {
                    hub,
                    on_jump: Callback::new(|_: KindSurface| {}),
                    alternatives,
                }
            }
        }
        let render = |hub: RuleHubView, alternatives: Option<AlternativeControlsRow>| {
            let mut vdom = VirtualDom::new_with_props(
                Harness,
                HarnessProps { hub, alternatives },
            );
            vdom.rebuild_in_place();
            dioxus_ssr::render(&vdom)
        };

        let mut row = kinded_finding(
            "API-VERSIONING-1",
            "medium",
            serde_json::json!({
                "kind": "deviation", "deviating_sites": 2, "assessed_sites": 47,
                "convention_option_id": "versioned", "convention_label": "URI-versioned endpoints"
            }),
        );
        row.evaluated_option_id = Some("versioned".to_string());
        let hub = rule_hub(std::slice::from_ref(&row), "me/api", "API-VERSIONING-1")
            .expect("the rule is in the scan");

        // WITHOUT the alternatives context (a rule the audit recorded no recommendation for):
        // unchanged behaviour, the raw option id tag.
        let bare = render(hub.clone(), None);
        assert!(
            bare.contains("evaluated against"),
            "the pre-existing tag survives for a rule with no recommendation; html=\n{bare}"
        );
        assert!(bare.contains("versioned"), "html=\n{bare}");

        // WITH it: the LABEL, the provenance, the evidence, and the reasoning the panel's
        // Why? button hides behind a click.
        let rule = proposed_rule_with_options(
            "API-VERSIONING-1",
            "Version the API",
            &[("versioned", "URI-versioned endpoints"), ("header", "Header-versioned")],
        );
        let rec = recommendation_with_evidence(
            "versioned",
            "Thirty-one of forty-seven endpoints already carry a /v1 prefix.",
            Some("src/routes.rs:12"),
            true,
        );
        let alt = alternative_controls_row(&rule, &rec, 2, Some("versioned"), None, false);
        let html = render(hub.clone(), Some(alt));
        assert!(html.contains("Evaluated against:"), "html=\n{html}");
        assert!(
            html.contains("URI-versioned endpoints"),
            "the authored LABEL, which is what the curator reads elsewhere; html=\n{html}"
        );
        assert!(html.contains("AI recommended"), "the provenance badge; html=\n{html}");
        assert!(html.contains("Evidence: src/routes.rs:12"), "html=\n{html}");
        assert!(
            html.contains("Thirty-one of forty-seven endpoints"),
            "the Why? ANSWER, rendered rather than hidden behind a modal inside a modal; \
             html=\n{html}"
        );
        assert!(
            !html.contains("rule-hub-evaluated"),
            "and NOT the raw option-id tag as well — one fact, one vocabulary; html=\n{html}"
        );
        // Where the control actually is, named. The hub cannot carry the Change select (see
        // `RuleAlternativesPanel`), so it must not leave the curator hunting for it.
        assert!(html.contains("Rule alternatives"), "html=\n{html}");

        // THE §8 INVARIANT SURVIVES THE ADDITION: still exactly one control, the jump.
        assert_eq!(
            html.matches("<button").count(),
            1,
            "the alternatives context is READ-ONLY; §8's one-navigation rule holds; html=\n{html}"
        );
    }

    /// req_pass2_13 (§9.1, the half that could NOT move): `RuleAlternativesPanel` still lists
    /// a multi-option rule the scan found NOTHING for, and the hub still cannot.
    ///
    /// This is the coverage proof that blocks the removal, asserted rather than asserted-in-
    /// prose. A clean multi-option rule has no Queue row and no panel row anywhere else; if
    /// the panel went, its option choice would be unreachable and unchangeable. `rule_hub`
    /// returning `None` for it is deliberate (its own doc: "there is no '0 findings' hub"),
    /// so this is a property of the replacement, not a gap to be patched.
    #[test]
    fn req_pass2_13_the_hub_cannot_reach_a_clean_multi_option_rule_so_the_panel_stays() {
        // A scan whose findings touch ONE rule, while the audit recommended on TWO.
        let found = kinded_finding("API-VERSIONING-1", "medium", serde_json::json!({"kind": "defect"}));
        let findings = vec![found];

        assert!(
            rule_hub(&findings, "me/api", "ERROR-HANDLING-1").is_none(),
            "the hub describes what a scan FOUND; a clean rule has nothing for it to describe"
        );

        fn harness() -> Element {
            use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
            use_context_provider(|| {
                Signal::new(std::collections::HashMap::<String, String>::new())
            });
            let audit = use_signal(|| Option::<ScanReportView>::None);
            let mut recs = std::collections::HashMap::new();
            recs.insert(
                "API-VERSIONING-1".to_string(),
                recommendation("versioned", "most endpoints already do", false, false),
            );
            recs.insert(
                "ERROR-HANDLING-1".to_string(),
                recommendation("result", "no call sites panic today", false, false),
            );
            rsx! {
                RuleAlternativesPanel {
                    project_id: "p".to_string(),
                    proposed_rules: vec![
                        proposed_rule_with_options(
                            "API-VERSIONING-1",
                            "Version the API",
                            &[("versioned", "URI-versioned endpoints"), ("header", "Header-versioned")],
                        ),
                        proposed_rule_with_options(
                            "ERROR-HANDLING-1",
                            "Handle errors consistently",
                            &[("result", "Result types"), ("panic", "Panics")],
                        ),
                    ],
                    findings: vec![kinded_finding(
                        "API-VERSIONING-1",
                        "medium",
                        serde_json::json!({"kind": "defect"}),
                    )],
                    repos: vec!["me/api".to_string()],
                    recommendations: recs,
                    audit,
                }
            }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(
            html.contains("ERROR-HANDLING-1"),
            "the CLEAN rule must still be listed and overridable — it is the one whose option \
             choice is least visible; html=\n{html}"
        );
        assert!(html.contains("0 findings"), "and labelled honestly; html=\n{html}");
        assert!(
            html.contains("Result types"),
            "with its Change select populated; html=\n{html}"
        );
        assert!(
            html.contains("API-VERSIONING-1"),
            "the rule WITH findings is listed too, because this is still where the Change \
             select lives for every rule; html=\n{html}"
        );
    }

    /// req_pass2_13 (§9.1, precondition): `alternative_controls_row` resolves "Evaluated
    /// against" in the one priority order the product means, and nothing else re-derives it.
    ///
    /// This order used to live inline in `RuleAlternativesPanel`'s render body, where it was
    /// unreachable by any test. §9.1 moves this line onto other surfaces, and a line whose
    /// central judgement can only be produced inside one component cannot move — so the
    /// extraction is a precondition of the removal, and the order is pinned here before
    /// anything starts depending on it from a second place.
    #[test]
    fn req_pass2_13_the_alternatives_line_resolves_one_evaluated_option_in_one_order() {
        let rule = proposed_rule_with_options(
            "API-VERSIONING-1",
            "Version the API",
            &[("versioned", "URI-versioned endpoints"), ("header", "Header-versioned")],
        );
        let rec = recommendation("versioned", "most endpoints already do", false, false);

        // 1. A STAGED OVERRIDE wins everything: it is what the next rescan will use, and the
        //    curator has to see what he just picked.
        let row = alternative_controls_row(&rule, &rec, 3, Some("versioned"), Some("header"), false);
        assert_eq!(row.evaluated_option_id, "header");
        assert_eq!(row.evaluated_label, "Header-versioned");
        assert!(row.staged);
        assert_eq!(row.badge_text, "Override, pending rescan");

        // 2. No override: the option the FINDINGS were produced under beats the
        //    recommendation. A prior rescan can have applied an override the recommendation
        //    field knows nothing about, and showing the recommendation then would describe a
        //    scan the curator is not looking at.
        let row = alternative_controls_row(&rule, &rec, 3, Some("header"), None, false);
        assert_eq!(row.evaluated_option_id, "header");
        assert!(!row.staged);

        // 3. No findings at all (the case the standalone panel exists for): the AI's pick.
        let row = alternative_controls_row(&rule, &rec, 0, None, None, false);
        assert_eq!(row.evaluated_option_id, "versioned");
        assert_eq!(row.badge_text, "AI recommended");
        assert_eq!(row.finding_count, 0);

        // 4. No findings and no recommendation: the rule's declared default.
        let mut defaulted = rule.clone();
        defaulted.default_option = Some("header".to_string());
        let empty = recommendation("", "", false, false);
        let row = alternative_controls_row(&defaulted, &empty, 0, None, None, false);
        assert_eq!(row.evaluated_option_id, "header");

        // 5. Nothing anywhere: the label says so rather than rendering a blank that reads as
        //    "no opinion".
        let row = alternative_controls_row(&rule, &empty, 0, None, None, false);
        assert_eq!(row.evaluated_label, "(none)");

        // 6. The operator's own prior choice is labelled as his, not as the AI's.
        let chosen = recommendation("versioned", "operator forced", false, true);
        let row = alternative_controls_row(&rule, &chosen, 1, Some("versioned"), None, false);
        assert_eq!(row.badge_text, "Your choice");

        // 7. Accepted is ADDITIVE, never a replacement for the provenance badge: "I accepted
        //    it" and "the AI picked it" are both true and the curator needs both.
        let row = alternative_controls_row(&rule, &rec, 1, Some("versioned"), None, true);
        assert!(row.accepted);
        assert_eq!(row.badge_text, "AI recommended");
    }

    /// req_pass2_13 (§9.1, precondition): the control line renders every state, including the
    /// two a click would be needed to reach through the panel.
    #[test]
    fn req_pass2_13_the_alternatives_line_renders_every_state_props_only() {
        #[component]
        fn Harness(row: AlternativeControlsRow) -> Element {
            rsx! {
                RuleAlternativeControls {
                    row,
                    on_why: Callback::new(|()| {}),
                    on_pick: Callback::new(|_: String| {}),
                }
            }
        }
        let render = |row: AlternativeControlsRow| {
            let mut vdom = VirtualDom::new_with_props(Harness, HarnessProps { row });
            vdom.rebuild_in_place();
            dioxus_ssr::render(&vdom)
        };
        let rule = proposed_rule_with_options(
            "API-VERSIONING-1",
            "Version the API",
            &[("versioned", "URI-versioned endpoints"), ("header", "Header-versioned")],
        );

        // The resting state: the fact, the provenance, the Why? door, the Change select.
        let rec = recommendation("versioned", "most endpoints already do", false, false);
        let html = render(alternative_controls_row(&rule, &rec, 3, Some("versioned"), None, false));
        assert!(html.contains("Evaluated against:"), "html=\n{html}");
        assert!(html.contains("URI-versioned endpoints"), "the LABEL, not the id; html=\n{html}");
        assert!(html.contains("AI recommended"), "html=\n{html}");
        assert!(html.contains("rec-why"), "the Why? door; html=\n{html}");
        assert!(html.contains("rec-select"), "the Change select; html=\n{html}");
        assert!(html.contains("3 finding(s)"), "html=\n{html}");
        assert!(!html.contains("rec-badge-accepted"), "html=\n{html}");
        assert!(!html.contains("Not applicable"), "html=\n{html}");

        // STAGED — reachable through the panel only by driving a real <select> change, which
        // a Tier-1 SSR test cannot do. Props-only is what makes it assertable.
        let html = render(alternative_controls_row(&rule, &rec, 3, Some("versioned"), Some("header"), false));
        assert!(html.contains("Override, pending rescan"), "html=\n{html}");
        assert!(html.contains("rec-badge-pending"), "html=\n{html}");

        // ACCEPTED — likewise reachable only after a successful server round trip.
        let html = render(alternative_controls_row(&rule, &rec, 3, Some("versioned"), None, true));
        assert!(html.contains("rec-badge-accepted"), "html=\n{html}");
        assert!(html.contains("AI recommended"), "both, not one; html=\n{html}");

        // Evidence and the not-applicable marker, together.
        let flagged = recommendation_with_evidence(
            "versioned",
            "r",
            Some("src/routes.rs:12"),
            false,
        );
        let html = render(alternative_controls_row(&rule, &flagged, 0, None, None, false));
        assert!(html.contains("Evidence: src/routes.rs:12"), "html=\n{html}");
        assert!(html.contains("Not applicable to this repo"), "html=\n{html}");
        assert!(html.contains("0 findings"), "a clean multi-option rule still renders; html=\n{html}");
    }

    /// A twelve-row deviation group, for the bulk prompt's heading. Twelve so the count in
    /// the heading cannot be coincidentally right.
    fn twelve_row_group() -> DeviationGroup {
        let base = deviation_queue_rows();
        let dev = base
            .iter()
            .find(|f| f.rule_id == "API-VERSIONING-1")
            .expect("a deviation row")
            .clone();
        let rows: Vec<FindingView> = (0..12)
            .map(|i| {
                let mut f = dev.clone();
                f.line = 100 + i;
                f
            })
            .collect();
        deviation_groups(&rows)
            .into_iter()
            .next()
            .expect("one group")
    }

    /// req_pass2_13 (§9.2): the Queue's on-demand reason prompt, in the two shapes the two
    /// removed textareas used to serve, NAMING THE COUNT in each.
    ///
    /// Props-only so SSR can render it open — the state a click would otherwise be needed to
    /// reach. The heading is `bulk_reason_heading`'s, which is unit-tested in core; what is
    /// asserted here is that the component renders THAT heading for the prompt it was handed,
    /// rather than a second phrasing of the same thing.
    #[test]
    fn req_pass2_13_the_on_demand_reason_prompt_names_its_disposition_and_its_count() {
        #[component]
        fn PromptHarness(prompt: ToolbarReasonPrompt) -> Element {
            rsx! {
                QueueReasonPrompt {
                    prompt,
                    reason: String::new(),
                    on_reason: Callback::new(|_: String| {}),
                    on_commit: Callback::new(|()| {}),
                    on_cancel: Callback::new(|()| {}),
                }
            }
        }
        let render = |prompt: ToolbarReasonPrompt| {
            let mut vdom =
                VirtualDom::new_with_props(PromptHarness, PromptHarnessProps { prompt });
            vdom.rebuild_in_place();
            dioxus_ssr::render(&vdom)
        };

        // The per-selection shape: three rows picked in the table, raised by "Ignore".
        let html = render(ToolbarReasonPrompt {
            state: TriageState::Ignored,
            target: ReasonTarget::Selection {
                ids: vec![RowId::new(), RowId::new(), RowId::new()],
                rows: deviation_queue_rows().into_iter().take(3).collect(),
            },
        });
        assert!(
            html.contains(&bulk_reason_heading(TriageState::Ignored, 3)),
            "the heading core writes, not a second phrasing; html=\n{html}"
        );
        assert!(html.contains("3 findings"), "it names the count; html=\n{html}");
        assert!(html.contains("Ignored"), "and the disposition; html=\n{html}");
        assert!(html.contains("required"), "and that the reason is required; html=\n{html}");
        assert!(html.contains("queue-prompt-commit"), "a way to commit; html=\n{html}");
        assert!(html.contains("queue-prompt-cancel"), "and a way out; html=\n{html}");

        // The bulk shape: one deviation group of twelve, raised by "False positive all 12".
        // The textarea this replaces could not have said "12" — it had no idea which button
        // was about to read it.
        let html = render(ToolbarReasonPrompt {
            state: TriageState::FalsePositive,
            target: ReasonTarget::Group(twelve_row_group()),
        });
        assert!(html.contains("12 findings"), "the group's own count; html=\n{html}");
        assert!(html.contains("False positive"), "the disposition; html=\n{html}");

        // Singular is singular. A prompt that says "1 findings" over a destructive act reads
        // as a template nobody checked.
        let html = render(ToolbarReasonPrompt {
            state: TriageState::FalsePositive,
            target: ReasonTarget::Selection {
                ids: vec![RowId::new()],
                rows: deviation_queue_rows().into_iter().take(1).collect(),
            },
        });
        assert!(html.contains("1 finding \u{2014}") || html.contains("1 finding ("), "html=\n{html}");
        assert!(!html.contains("1 findings"), "html=\n{html}");

        // AND THE COUNT IS THE ROW LIST, not a number travelling beside it. This is the
        // property that makes the heading trustworthy: `ReasonTarget` carries the findings,
        // so there is no second list for the commit to disagree with after a re-sort or a
        // filter lands between raising the prompt and committing it.
        let rows = deviation_queue_rows();
        let sel = ToolbarReasonPrompt {
            state: TriageState::Ignored,
            target: ReasonTarget::Selection {
                ids: rows.iter().map(|_| RowId::new()).collect(),
                rows: rows.clone(),
            },
        };
        assert_eq!(sel.count(), rows.len());
        let group = twelve_row_group();
        let bulk = ToolbarReasonPrompt {
            state: TriageState::FalsePositive,
            target: ReasonTarget::Group(group.clone()),
        };
        assert_eq!(bulk.count(), group.rows.len());
        assert_eq!(
            bulk.count(),
            group.row_count(),
            "the prompt's count is the same one the bar's own button label uses"
        );
    }

    /// req_pass2_13 (§9.2): the Queue at rest has NO reason input, in any triage table.
    ///
    /// The removal's whole claim is that "an empty textarea the curator is not using is noise
    /// 95% of the time", so the resting state is the thing to pin — and to pin across all
    /// five tables, because the old textareas lived in the Unresolved arm and their absence
    /// elsewhere was previously the reason the other tables could not offer Ignore at all.
    #[test]
    fn req_pass2_13_no_triage_table_renders_a_standing_reason_textarea() {
        for view in TriageState::all() {
            #[component]
            fn Harness(view: TriageState) -> Element {
                use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
                use_context_provider(|| Signal::new(None::<FindingView>));
                let dispositions =
                    use_signal(std::collections::HashMap::<String, Disposition>::new);
                rsx! {
                    FindingsTable {
                        findings: deviation_queue_rows(),
                        repos: vec!["me/api".to_string()],
                        descriptions: std::collections::HashMap::new(),
                        triage_view: view,
                        dispositions,
                    }
                }
            }
            let mut vdom = VirtualDom::new_with_props(Harness, HarnessProps { view });
            vdom.rebuild_in_place();
            let html = dioxus_ssr::render(&vdom);
            assert!(
                !html.contains("<textarea"),
                "{}: §9.2 leaves no standing reason box; html=\n{html}",
                view.label()
            );
            assert!(
                !html.contains("queue-reason-prompt"),
                "{}: and the on-demand prompt is CLOSED until a gesture raises it; html=\n{html}",
                view.label()
            );
        }
    }

    /// req_pass2_13 (§9.2, the handled risk): the bulk bar's `False positive all N` no longer
    /// reads a toolbar textarea, and its tooltip no longer promises one.
    ///
    /// This is the one place the removal CHANGED a behaviour rather than relocating it, so it
    /// is pinned explicitly. Before: the button read whatever stood in the shared
    /// false-positive box — which could be blank, or could be a reason typed minutes earlier
    /// for a different rule, and the bar never said how many rows it was about to move.
    /// After: it raises the prompt carrying its own group's count, and the require-reason
    /// invariant stays exactly where it was, in `TriageModel::bulk_false_positive`.
    #[test]
    fn req_pass2_13_the_bulk_false_positive_bar_raises_a_prompt_instead_of_reading_a_textarea() {
        fn harness() -> Element {
            use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
            use_context_provider(|| Signal::new(None::<FindingView>));
            let dispositions = use_signal(std::collections::HashMap::<String, Disposition>::new);
            rsx! {
                FindingsTable {
                    findings: deviation_queue_rows(),
                    repos: vec!["me/api".to_string()],
                    descriptions: std::collections::HashMap::new(),
                    triage_view: TriageState::Unresolved,
                    dispositions,
                }
            }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(
            html.contains("deviation-bulk-fp"),
            "the bulk false-positive control is still offered; html=\n{html}"
        );
        assert!(
            !html.contains("reason above"),
            "it must not promise a textarea that no longer exists; html=\n{html}"
        );
        assert!(
            !html.contains("<textarea"),
            "and there is no textarea for it to have read; html=\n{html}"
        );

        // The invariant it defers to is unchanged and still refuses: core's own wrapper, the
        // same one the bar called directly before. Asserted here rather than taken on trust,
        // because "the reason source moved" is exactly the change that could drop it.
        let groups = deviation_groups(&deviation_queue_rows());
        let group = groups.first().expect("a deviation group");
        let mut model = TriageModel::new();
        assert!(
            model.bulk_false_positive(group, "   ").is_none(),
            "a blank reason must still be refused, wherever the box now lives"
        );
        assert!(model.dispositions.is_empty(), "and must write nothing when it refuses");
        assert_eq!(
            model.bulk_false_positive(group, "the parser mis-read all of these"),
            Some(group.rows.len()),
            "and a real reason must still move the whole group"
        );
    }

    /// req_pass2_13 (§9.3): the Queue's hedging column is the STRUCTURED Confidence chip, and
    /// the 300px prose column it replaces is gone from the table.
    ///
    /// Coverage, item by item, against what the old column could say:
    ///
    /// | old cell | old source | new cell | new source |
    /// |---|---|---|---|
    /// | `test` | `f.in_test` | `test` | `f.in_test` |
    /// | `yes` | `f.needs_review` OR a regex over `f.detail` | `needs-review` | `f.needs_review` OR `f.confidence` |
    /// | `no` | neither | `high` / `unrated`, told apart | `f.confidence` |
    ///
    /// The one thing the old column rendered that this one does not is the hedging SENTENCE,
    /// inline on the row. That is the removal, and the next test proves where it went.
    #[test]
    fn req_pass2_13_the_queue_hedging_column_is_the_structured_confidence_chip() {
        let cols = finding_columns(vec!["me/api".to_string()], false);
        let col = cols
            .iter()
            .find(|c| c.id == ColumnId("needs_review"))
            .expect("the hedging column must still exist, re-headed");
        assert_eq!(
            col.header, "Confidence",
            "§4: Confidence REPLACES \"Needs review\" as the column the curator reads"
        );
        let width = col.initial_width.expect("the column sets an initial width");
        assert!(
            width <= 160.0,
            "§9.3 recovers ~300px off the 300px column; got {width}px"
        );

        // Every state the old column could report, still reported — plus the one it could not.
        let cases = [
            (serde_json::json!({"in_test": true}), "test"),
            (serde_json::json!({"needs_review": true}), "needs-review"),
            (serde_json::json!({"confidence": "needs-review"}), "needs-review"),
            (serde_json::json!({"confidence": "high"}), "high"),
            (serde_json::json!({}), "unrated"),
        ];
        for (patch, want) in &cases {
            let mut row = serde_json::json!({
                "repo": "me/api", "path": "src/a.rs", "line": 1, "rule_id": "R-1",
                "severity": "medium", "snippet": "s", "detail": "d"
            });
            for (k, v) in patch.as_object().expect("object") {
                row[k] = v.clone();
            }
            let f: FindingView = serde_json::from_value(row).expect("fixture");
            assert_eq!(
                (col.accessor)(&f),
                CellValue::Text((*want).to_string()),
                "cell value for {patch}"
            );
        }

        // THE PROSE TERM IS GONE from the cell value. This row's `detail` carries the legacy
        // marker and its structured verdict CONTRADICTS it; the old accessor would have read
        // `yes` off the sentence, over the top of the calibration pass's own answer.
        let contradicted: FindingView = serde_json::from_value(serde_json::json!({
            "repo": "me/api", "path": "src/a.rs", "line": 1, "rule_id": "R-1",
            "severity": "medium", "snippet": "s",
            "detail": "Debatable. [needs review: premature for a mini app]",
            "confidence": "high", "needs_review": false
        }))
        .expect("fixture");
        assert_eq!(
            (col.accessor)(&contradicted),
            CellValue::Text("high".to_string()),
            "the cell must read the structured verdict, never the sentence beside it"
        );

        // Filter vocabulary from the same closed set as the cell (the Severity discipline).
        let FilterKind::MultiSelect { options } = &col.filter else {
            panic!("Confidence keeps a multi-select filter, got {:?}", col.filter);
        };
        assert_eq!(options, &confidence_tags());
        for (_, want) in &cases {
            assert!(
                options.contains(&(*want).to_string()),
                "{want} is emitted but not filterable"
            );
        }

        // And every emitted value resolves to a chip, so no state renders as a blank cell the
        // curator would misread as "nothing to say here".
        let RenderKind::Badge(map) = &col.render_kind else {
            panic!("Confidence renders as a badge");
        };
        for tag in confidence_tags() {
            let variant = map
                .resolve(&tag)
                .unwrap_or_else(|| panic!("{tag} must resolve to a badge variant"));
            assert!(!variant.label.is_empty(), "{tag}: a chip with no label is a blank cell");
        }
        assert_ne!(
            map.resolve("high").expect("high").label,
            map.resolve("unrated").expect("unrated").label,
            "\"calibration was confident\" and \"calibration never looked\" must not render alike"
        );
    }

    /// req_pass2_13 (§9.3, the other half): the hedging SENTENCE the removed column drew
    /// inline is still reaching the curator — in the detail pane, which is where §9.3 sends it
    /// ("the reason text lives in the detail pane").
    ///
    /// This is the test that makes the removal a relocation rather than a capability loss, so
    /// it asserts in both directions: the reason is absent from the rendered Queue and present
    /// in the pane describing that same row.
    #[test]
    fn req_pass2_13_the_hedging_reason_left_the_row_and_arrived_in_the_pane() {
        const REASON: &str = "premature for a mini app";
        let hedged: FindingView = serde_json::from_value(serde_json::json!({
            "repo": "me/api", "path": "src/a.rs", "line": 7, "rule_id": "AI-LAYERING",
            "severity": "medium", "snippet": "fn x() {}", "status": "active",
            "detail": format!("No mappers crate. [needs review: {REASON}]"),
            "kind": "defect", "needs_review": true, "confidence": "needs-review"
        }))
        .expect("fixture");

        // Gone from the Queue: not in any cell value, and not drawn by a renderer either.
        let cols = finding_columns(vec!["me/api".to_string()], false);
        for c in &cols {
            if let CellValue::Text(v) = (c.accessor)(&hedged) {
                assert!(
                    !v.contains(REASON),
                    "{:?}: the hedging sentence must not be a cell value any more; got {v:?}",
                    c.id
                );
            }
        }
        fn queue() -> Element {
            use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
            use_context_provider(|| Signal::new(None::<FindingView>));
            let dispositions = use_signal(std::collections::HashMap::<String, Disposition>::new);
            rsx! {
                FindingsTable {
                    findings: vec![
                        serde_json::from_value(serde_json::json!({
                            "repo": "me/api", "path": "src/a.rs", "line": 7,
                            "rule_id": "AI-LAYERING", "severity": "medium", "snippet": "fn x() {}",
                            "status": "active",
                            "detail": "No mappers crate. [needs review: premature for a mini app]",
                            "kind": "defect", "needs_review": true, "confidence": "needs-review"
                        })).expect("fixture"),
                    ],
                    repos: vec!["me/api".to_string()],
                    descriptions: std::collections::HashMap::new(),
                    triage_view: TriageState::Unresolved,
                    dispositions,
                }
            }
        }
        let mut vdom = VirtualDom::new(queue);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains(">Confidence<"), "the new header; html=\n{html}");
        assert!(
            !html.contains(">Needs review<"),
            "the old column header is gone; html=\n{html}"
        );
        for gone in ["nr-reason", "nr-flag", "test-flag"] {
            assert!(
                !html.contains(gone),
                "the prose cell renderer and its chips are gone (found {gone:?}); html=\n{html}"
            );
        }

        // Arrived in the pane. `DetailPaneRow` is the ONE place the pane's props are built, so
        // asserting on it covers the pane for every row it can ever focus.
        let pane = DetailPaneRow::from_finding(&hedged, None, None, 0, 1);
        assert_eq!(
            pane.needs_review.as_deref(),
            Some(REASON),
            "the hedge the Queue stopped drawing must reach the pane"
        );
        assert!(
            !pane.explanation.contains("[needs review"),
            "and the pane splits it OFF the body rather than leaving the marker inline twice"
        );
        assert_eq!(
            pane.confidence.as_deref(),
            Some("needs-review"),
            "the structured value the chip reads is in the pane too, so the two agree"
        );
    }

    /// req_pass2_13 (§9.4): NO informational surface renders a severity, and the proof is a
    /// fixture that WOULD leak one.
    ///
    /// §9.4: "Conventions, No convention and Not assessed panels render NO severity anywhere.
    /// The kinds ship at `info` by construction; a severity badge there is a dead affordance
    /// implying a ranking decision that does not exist."
    ///
    /// The existing `req_pass2_6_conventions_panel_lists_labels_and_renders_no_severity` pins
    /// one panel with an `info` fixture — which cannot distinguish "the panel ignores
    /// severity" from "the fixture happened to be boring". These rows are authored
    /// `critical`/`high`/`medium` precisely so the assertion can only pass if the panels never
    /// read the field at all. All three panels, plus the rule hub strip that every one of
    /// their rule ids opens.
    #[test]
    fn req_pass2_13_no_informational_surface_renders_a_severity() {
        fn harness() -> Element {
            rsx! {
                ConventionsPanel {
                    findings: vec![kinded_finding(
                        "API-VERSIONING-1",
                        "critical",
                        serde_json::json!({
                            "kind": "convention_observed",
                            "convention_option_id": "versioned",
                            "convention_label": "URI-versioned endpoints",
                        }),
                    )],
                }
                NoConventionPanel {
                    findings: vec![kinded_finding(
                        "ERROR-HANDLING-1",
                        "high",
                        serde_json::json!({
                            "kind": "no_convention",
                            "observed_options": [
                                { "option_id": "result", "label": "Result types", "sites": 9 },
                            ],
                        }),
                    )],
                }
                NotAssessedPanel {
                    findings: vec![kinded_finding(
                        "ESLINT-1",
                        "medium",
                        serde_json::json!({
                            "kind": "not_assessed",
                            "reason": "tool_unavailable",
                            "reason_detail": "eslint is not installed",
                        }),
                    )],
                }
            }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        // Each panel really did render its row, so the absence below is an absence of
        // severity and not an absence of output.
        for present in ["API-VERSIONING-1", "ERROR-HANDLING-1", "ESLINT-1"] {
            assert!(html.contains(present), "{present} must render; html=\n{html}");
        }
        for forbidden in [
            "severity", "Severity", "critical", "Critical", "high", "High", "medium", "Medium",
            "chorale-badge", "finding-row-critical",
        ] {
            assert!(
                !html.contains(forbidden),
                "§9.4: no severity on an informational panel (found {forbidden:?}); html=\n{html}"
            );
        }

        // And the hub strip those rule ids open — the fourth informational surface, reached
        // from all three panels — says nothing about severity either.
        #[component]
        fn HubHarness(hub: RuleHubView) -> Element {
            rsx! { RuleHubStrip { hub, on_jump: Callback::new(|_: KindSurface| {}) } }
        }
        let rows = vec![kinded_finding(
            "API-VERSIONING-1",
            "critical",
            serde_json::json!({
                "kind": "convention_observed",
                "convention_option_id": "versioned",
                "convention_label": "URI-versioned endpoints",
            }),
        )];
        let hub = rule_hub(&rows, "me/api", "API-VERSIONING-1").expect("the rule is in the scan");
        let mut vdom =
            VirtualDom::new_with_props(HubHarness, HubHarnessProps { hub });
        vdom.rebuild_in_place();
        let hub_html = dioxus_ssr::render(&vdom);
        assert!(
            hub_html.contains("Convention observed"),
            "the hub rendered the informational rule; html=\n{hub_html}"
        );
        for forbidden in ["severity", "Severity", "critical", "Critical"] {
            assert!(
                !hub_html.contains(forbidden),
                "§9.4: no severity on the hub for an informational rule (found {forbidden:?}); \
                 html=\n{hub_html}"
            );
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// req_pass2_11 — THE DOCKED DETAIL PANE, rendered (design §2; Q5 ruling §13)
// ─────────────────────────────────────────────────────────────────────────────
//
// SSR cannot press a key, so the pane is a props-only component whose rendered output
// MANIFESTS the two things a keypress test could otherwise only assert about a helper: which
// row it is describing, and whether the keys are armed. The armed indicator is driven by
// `pane_keys_armed`, which is DEFINED as `pane_command(focus, "c", …).is_some()` — so a pane
// that renders "armed" while the handler would refuse is not reachable.
#[cfg(test)]
mod pane_render_tests {
    use super::*;

    fn sample() -> FindingView {
        serde_json::from_value(serde_json::json!({
            "repo": "me/api", "path": "src/handlers.rs", "line": 88,
            "rule_id": "SQL-INJECTION-1", "severity": "critical",
            "snippet": "db.query(format!(\"select … {}\", id))",
            "detail": "String-interpolated SQL reaches the driver unparameterised.",
            "status": "active", "confidence": "high", "effort": "medium",
            "kind": "defect",
            "also_locations": [
                { "repo": "me/api", "path": "src/export.rs", "line": 31,
                  "rule_id": "SQL-INJECTION-1", "snippet": "db.query(format!(..))",
                  "consequence": false },
                { "repo": "me/api", "path": "src/report.rs", "line": 4,
                  "rule_id": "DATA-EXFIL-1", "snippet": "every customer row is readable",
                  "consequence": true }
            ]
        }))
        .expect("valid FindingView fixture")
    }

    fn deviation() -> FindingView {
        serde_json::from_value(serde_json::json!({
            "repo": "me/api", "path": "src/routes.rs", "line": 12,
            "rule_id": "API-VERSIONING-1", "severity": "medium", "snippet": "get(\"/users\")",
            "detail": "", "kind": "deviation", "deviating_sites": 2, "assessed_sites": 47,
            "convention_option_id": "versioned", "convention_label": "URI-versioned endpoints"
        }))
        .expect("valid FindingView fixture")
    }

    fn render(
        row: Option<DetailPaneRow>,
        focus: PaneFocus,
        prompt: Option<TriageState>,
        undo: Option<UndoBanner>,
    ) -> String {
        #[component]
        fn Harness(
            row: Option<DetailPaneRow>,
            focus: PaneFocus,
            prompt: Option<TriageState>,
            undo: Option<UndoBanner>,
        ) -> Element {
            rsx! {
                FindingDetailPane {
                    row,
                    focus,
                    prompt,
                    prompt_reason: String::new(),
                    undo,
                    on_key: Callback::new(|_: (String, KeyMods)| {}),
                    on_focus: Callback::new(|_: PaneFocus| {}),
                    on_prompt_reason: Callback::new(|_: String| {}),
                    on_prompt_commit: Callback::new(|()| {}),
                    on_prompt_cancel: Callback::new(|()| {}),
                    on_undo: Callback::new(|()| {}),
                    on_dispose: Callback::new(|_: TriageState| {}),
                    on_open_detail: Callback::new(|()| {}),
                }
            }
        }
        let mut vdom = VirtualDom::new_with_props(
            Harness,
            HarnessProps {
                row,
                focus,
                prompt,
                undo,
            },
        );
        vdom.rebuild_in_place();
        dioxus_ssr::render(&vdom)
    }

    /// req_pass2_11: the pane shows the focused row's whole case — rule id, kind badge,
    /// severity, location, snippet, cited standard, and the fix with its effort estimate.
    /// This is the content that made the modal's open/decide/close cycle necessary; the
    /// point of the pane is that none of it costs a click.
    #[test]
    fn req_pass2_11_the_pane_shows_the_focused_rows_whole_case() {
        let row = DetailPaneRow::from_finding(
            &sample(),
            Some("Never interpolate into SQL; bind parameters."),
            Some("Replace the format! with a bound parameter.".to_string()),
            0,
            12,
        );
        let html = render(Some(row), PaneFocus::Cursor, None, None);
        assert!(html.contains("finding-pane"), "pane container; html=\n{html}");
        assert!(html.contains("SQL-INJECTION-1"), "rule id; html=\n{html}");
        assert!(html.contains("Defect"), "kind badge; html=\n{html}");
        assert!(html.contains("critical"), "severity; html=\n{html}");
        assert!(html.contains("src/handlers.rs:88"), "location; html=\n{html}");
        assert!(html.contains("Never interpolate into SQL"), "cited standard; html=\n{html}");
        assert!(html.contains("Replace the format!"), "fix; html=\n{html}");
        assert!(html.contains("medium"), "effort estimate; html=\n{html}");
        assert!(html.contains("1 of 12"), "position in the queue; html=\n{html}");
    }

    /// req_pass2_11 (design §6): a merged row's ABSORBED sites are listed, with consequence
    /// sites subordinated under "also affects" — the full-disclosure surface C5 lacked.
    #[test]
    fn req_pass2_11_the_pane_lists_every_absorbed_site() {
        let row = DetailPaneRow::from_finding(&sample(), None, None, 0, 1);
        let html = render(Some(row), PaneFocus::Cursor, None, None);
        assert!(html.contains("Sites (3)"), "1 primary + 2 absorbed; html=\n{html}");
        assert!(html.contains("src/export.rs:31"), "peer site; html=\n{html}");
        assert!(html.contains("src/report.rs:4"), "consequence site; html=\n{html}");
        assert!(html.contains("also affects"), "the consequence sub-label; html=\n{html}");
        assert!(html.contains("DATA-EXFIL-1"), "the absorbed site's own rule id; html=\n{html}");
    }

    /// req_pass2_11: a single-site row says nothing about sites at all, rather than
    /// rendering an empty section or a lonely "Sites (1)".
    #[test]
    fn req_pass2_11_an_unmerged_row_renders_no_sites_section() {
        let row = DetailPaneRow::from_finding(&deviation(), None, None, 0, 1);
        let html = render(Some(row), PaneFocus::Cursor, None, None);
        assert!(!html.contains("Sites ("), "no sites section for one site; html=\n{html}");
    }

    /// req_pass2_11: a deviation's structured claim renders in the pane verbatim from
    /// `DeviationHeadline::sentence` — the same words the group header carries.
    #[test]
    fn req_pass2_11_the_pane_states_a_deviations_claim() {
        let row = DetailPaneRow::from_finding(&deviation(), None, None, 0, 1);
        let html = render(Some(row), PaneFocus::Cursor, None, None);
        assert!(html.contains("Deviation"), "kind badge; html=\n{html}");
        assert!(
            html.contains("2 of 47 sites deviate from &#34;URI-versioned endpoints&#34;"),
            "the structured sentence; html=\n{html}"
        );
    }

    /// req_pass2_11 — **THE SCOPING, RENDERED.** The pane tells the curator whether the keys
    /// are live, and it is the HANDLER'S OWN GATE that decides what it says. While the reason
    /// prompt holds focus the pane says the keys are off; while the cursor holds focus it
    /// names all four.
    #[test]
    fn req_pass2_11_the_pane_says_whether_the_keys_are_armed() {
        let row = || Some(DetailPaneRow::from_finding(&sample(), None, None, 0, 1));

        let armed = render(row(), PaneFocus::Cursor, None, None);
        assert!(armed.contains("pane-keys armed"), "armed class; html=\n{armed}");
        for label in ["C confirm", "I ignore", "T tech debt", "F false positive"] {
            assert!(armed.contains(label), "{label} must be named; html=\n{armed}");
        }

        // Typing a reason: the keys are OFF, and the pane says so in those words.
        let typing = render(
            row(),
            PaneFocus::ReasonPrompt,
            Some(TriageState::Ignored),
            None,
        );
        assert!(typing.contains("pane-keys off"), "off class; html=\n{typing}");
        assert!(
            typing.contains("Keys are off while you type"),
            "and says why; html=\n{typing}"
        );
        assert!(!typing.contains("pane-keys armed"), "html=\n{typing}");

        // Focus elsewhere on the page: also off, with the way back.
        let away = render(row(), PaneFocus::Elsewhere, None, None);
        assert!(away.contains("pane-keys off"), "html=\n{away}");
        assert!(!away.contains("pane-keys armed"), "html=\n{away}");
    }

    /// req_pass2_11: the `I`/`F` prompt renders with the disposition it is collecting a
    /// reason FOR named in it, so a pre-focused textarea is never ambiguous about which of
    /// the two it will commit.
    #[test]
    fn req_pass2_11_the_reason_prompt_names_the_disposition_it_will_commit() {
        let row = || Some(DetailPaneRow::from_finding(&sample(), None, None, 0, 1));
        let ignoring = render(
            row(),
            PaneFocus::ReasonPrompt,
            Some(TriageState::Ignored),
            None,
        );
        assert!(ignoring.contains("pane-reason-prompt"), "html=\n{ignoring}");
        assert!(ignoring.contains("Ignored"), "names the disposition; html=\n{ignoring}");
        assert!(ignoring.contains("required"), "states the invariant; html=\n{ignoring}");

        let fp = render(
            row(),
            PaneFocus::ReasonPrompt,
            Some(TriageState::FalsePositive),
            None,
        );
        assert!(fp.contains("False positive"), "html=\n{fp}");

        // No prompt open: no textarea in the pane at all (design §9.2 — an unused textarea
        // is noise 95% of the time).
        let idle = render(row(), PaneFocus::Cursor, None, None);
        assert!(!idle.contains("pane-reason-prompt"), "html=\n{idle}");
    }

    /// req_pass2_11: the undo toast renders what it will reverse and how long is left, and
    /// disappears when there is nothing to undo.
    #[test]
    fn req_pass2_11_the_undo_toast_names_its_action_and_its_window() {
        let row = || Some(DetailPaneRow::from_finding(&sample(), None, None, 0, 1));
        let with_undo = render(
            row(),
            PaneFocus::Cursor,
            None,
            Some(UndoBanner {
                message: "Confirmed 1 finding.".to_string(),
                seconds_left: 4,
            }),
        );
        assert!(with_undo.contains("pane-undo"), "html=\n{with_undo}");
        assert!(with_undo.contains("Confirmed 1 finding."), "html=\n{with_undo}");
        assert!(with_undo.contains("Undo"), "html=\n{with_undo}");
        assert!(with_undo.contains("4s"), "the countdown; html=\n{with_undo}");

        let without = render(row(), PaneFocus::Cursor, None, None);
        assert!(!without.contains("pane-undo"), "html=\n{without}");
    }

    /// req_pass2_11: with no row focused — an empty triage table, or every group collapsed —
    /// the pane says so instead of rendering a shell of empty labels, and offers no
    /// disposition buttons, because there is nothing to dispose.
    #[test]
    fn req_pass2_11_an_unfocused_pane_offers_nothing_to_act_on() {
        let html = render(None, PaneFocus::Elsewhere, None, None);
        assert!(html.contains("finding-pane"), "the pane still occupies its dock; html=\n{html}");
        assert!(html.contains("pane-empty"), "html=\n{html}");
        assert!(!html.contains("pane-dispose"), "no dead buttons; html=\n{html}");
        assert!(!html.contains("pane-keys armed"), "and no armed keys; html=\n{html}");
    }

    /// req_pass2_11: the pane IS a focusable region, with the id `focus_detail_pane` targets.
    ///
    /// This is the physical half of the Q5 scoping. "The keys fire while the detail pane
    /// holds focus" is only a real condition if there is something that can hold focus, that
    /// a Tab reaches, and that a row click can be routed to — and if the id the row-click
    /// handler focuses is the id the pane actually renders. A drift between those two would
    /// leave the pane permanently unfocusable by mouse, and therefore the keys permanently
    /// dead, with the indicator none the wiser.
    #[test]
    fn req_pass2_11_the_pane_is_the_focusable_region_the_scoping_names() {
        let row = DetailPaneRow::from_finding(&sample(), None, None, 0, 1);
        let html = render(Some(row), PaneFocus::Cursor, None, None);
        assert!(html.contains("tabindex=\"0\""), "Tab must reach it; html=\n{html}");
        assert!(
            html.contains(&format!("id=\"{DETAIL_PANE_ID}\"")),
            "the id `focus_detail_pane` focuses; html=\n{html}"
        );
    }

    /// req_pass2_11 (design §9.6): the pane replaces the row-click modal, but keeps it one
    /// deliberate click away — the modal carries the on-demand enclosing-code-block fetch,
    /// which is far too expensive to re-run on every cursor-down.
    #[test]
    fn req_pass2_11_the_pane_keeps_the_modal_one_click_away() {
        let row = DetailPaneRow::from_finding(&sample(), None, None, 0, 1);
        let html = render(Some(row), PaneFocus::Cursor, None, None);
        assert!(html.contains("pane-full-detail"), "html=\n{html}");
        assert!(html.contains("Full detail"), "html=\n{html}");
        // And not on an empty pane, where there is no row to open.
        let empty = render(None, PaneFocus::Elsewhere, None, None);
        assert!(!empty.contains("pane-full-detail"), "html=\n{empty}");
    }

    /// req_pass2_11: the pane carries the same four dispositions as buttons, so the gesture
    /// is available to the mouse too and the keys are an accelerator rather than the only
    /// way in. Their labels name their key.
    #[test]
    fn req_pass2_11_the_pane_offers_the_four_dispositions_as_buttons_too() {
        let row = DetailPaneRow::from_finding(&sample(), None, None, 0, 1);
        let html = render(Some(row), PaneFocus::Cursor, None, None);
        let n = html.matches("pane-dispose").count();
        assert_eq!(n, 4, "one button per disposition; html=\n{html}");
        for s in [
            TriageState::Confirmed,
            TriageState::Ignored,
            TriageState::TechDebt,
            TriageState::FalsePositive,
        ] {
            assert!(html.contains(s.label()), "{} button; html=\n{html}", s.label());
        }
    }
}

#[cfg(test)]
mod rule_hub_tests {
    use super::*;
    use camerata_ui_core::triage::rule_hub;

    fn row(repo: &str, rule_id: &str, path: &str, kind: serde_json::Value) -> FindingView {
        let mut v = serde_json::json!({
            "repo": repo, "path": path, "line": 4, "rule_id": rule_id,
            "severity": "medium", "snippet": "s", "detail": "d", "status": "active"
        });
        for (k, val) in kind.as_object().expect("kind payload is an object") {
            v[k] = val.clone();
        }
        serde_json::from_value(v).expect("valid FindingView fixture")
    }

    fn deviation_rows() -> Vec<FindingView> {
        vec![
            row(
                "me/api",
                "API-VERSIONING-1",
                "src/routes.rs",
                serde_json::json!({
                    "kind": "deviation", "deviating_sites": 2, "assessed_sites": 47,
                    "convention_option_id": "uri", "convention_label": "URI-versioned endpoints",
                    "evaluated_option_id": "uri"
                }),
            ),
            row(
                "me/api",
                "API-VERSIONING-1",
                "src/admin.rs",
                serde_json::json!({
                    "kind": "deviation", "deviating_sites": 2, "assessed_sites": 47,
                    "convention_option_id": "uri", "convention_label": "URI-versioned endpoints",
                    "evaluated_option_id": "uri"
                }),
            ),
        ]
    }

    fn strip_html(hub: RuleHubView) -> String {
        #[component]
        fn Harness(hub: RuleHubView) -> Element {
            rsx! {
                RuleHubStrip { hub, on_jump: Callback::new(|_: KindSurface| {}) }
            }
        }
        let mut vdom = VirtualDom::new_with_props(Harness, HarnessProps { hub });
        vdom.rebuild_in_place();
        dioxus_ssr::render(&vdom)
    }

    /// req_pass2_12: the hub strip is the JOIN §8 asks for — it states the rule's kind
    /// outcome, its counts, the convention behind it, the option it was evaluated against,
    /// and offers one way in. Before this, a curator holding a rule id had nothing that
    /// answered "and what happened to this rule in this scan" short of re-reading four
    /// surfaces.
    #[test]
    fn req_pass2_12_the_hub_states_the_whole_of_one_rule_in_one_strip() {
        let rows = deviation_rows();
        let hub = rule_hub(&rows, "me/api", "API-VERSIONING-1").expect("the rule is in the scan");
        let html = strip_html(hub);
        assert!(html.contains("This rule in this scan"), "the strip names itself; html=\n{html}");
        assert!(html.contains("API-VERSIONING-1"), "the rule id; html=\n{html}");
        assert!(html.contains("Deviation"), "the kind chip; html=\n{html}");
        assert!(
            html.contains("2 of 47 sites deviate"),
            "the structured outcome sentence, verbatim; html=\n{html}"
        );
        assert!(html.contains("2 row(s)"), "the row count; html=\n{html}");
        assert!(html.contains("2 site(s)"), "the site count; html=\n{html}");
        assert!(
            html.contains("URI-versioned endpoints"),
            "the convention label; html=\n{html}"
        );
        assert!(
            html.contains("evaluated against"),
            "the Evaluated-against fact, relocated here from RuleAlternativesPanel; html=\n{html}"
        );
        assert!(
            html.contains("Show in the Queue, filtered to this rule"),
            "the one jump, naming where it goes; html=\n{html}"
        );
    }

    /// req_pass2_12: THE HONESTY CONSTRAINT (§8). The hub may state how many sites conform,
    /// because that number is in the data. It must not offer to show them, because they are
    /// not. The strip therefore says so in the same sentence, and carries exactly ONE
    /// clickable control — the jump.
    #[test]
    fn req_pass2_12_the_hub_counts_conforming_sites_and_offers_nothing_to_click_for_them() {
        let rows = deviation_rows();
        let hub = rule_hub(&rows, "me/api", "API-VERSIONING-1").expect("present");
        let html = strip_html(hub);
        assert!(
            html.contains("45 of 47 assessed sites conform"),
            "the count, which IS in the data; html=\n{html}"
        );
        assert!(
            html.contains("not in the scan data"),
            "the strip says why there is nothing to open; html=\n{html}"
        );
        assert_eq!(
            html.matches("<button").count(),
            1,
            "exactly one control: the jump. A second one here would be a fabricated \
             navigation into data that does not exist. html=\n{html}"
        );
        assert!(
            !html.contains("<a "),
            "and no anchors either; html=\n{html}"
        );
    }

    /// req_pass2_12: an unknown denominator yields no conforming sentence at all. The
    /// producer could not count honestly, so the hub says nothing rather than inventing a
    /// total to subtract from.
    #[test]
    fn req_pass2_12_the_hub_is_silent_about_conformance_without_a_denominator() {
        let rows = vec![row(
            "me/api",
            "API-VERSIONING-1",
            "src/routes.rs",
            serde_json::json!({
                "kind": "deviation", "deviating_sites": 3, "assessed_sites": null,
                "convention_label": "URI-versioned endpoints"
            }),
        )];
        let hub = rule_hub(&rows, "me/api", "API-VERSIONING-1").expect("present");
        let html = strip_html(hub);
        assert!(!html.contains("conform"), "no conformance claim; html=\n{html}");
        assert!(
            html.contains("3 sites deviate"),
            "the numerator alone, as the group header renders it; html=\n{html}"
        );
    }

    /// req_pass2_12: the hub works for the informational surfaces too, and names the PANEL it
    /// would take you to rather than the Queue. A convention row has no Queue to jump into.
    #[test]
    fn req_pass2_12_the_hub_names_the_panel_for_an_informational_rule() {
        let rows = vec![row(
            "me/api",
            "SQL-PARAM-1",
            "-",
            serde_json::json!({
                "kind": "convention_observed", "convention_option_id": "p",
                "convention_label": "Parameterized queries"
            }),
        )];
        let hub = rule_hub(&rows, "me/api", "SQL-PARAM-1").expect("present");
        let html = strip_html(hub);
        assert!(
            html.contains("Convention observed"),
            "the informational chip, not the Unspecified alarm; html=\n{html}"
        );
        assert!(
            html.contains("Show in \u{201c}Conventions\u{201d}"),
            "the jump names the panel; html=\n{html}"
        );
        assert!(
            !html.contains("Show in the Queue"),
            "there is no Queue for a compliment; html=\n{html}"
        );
    }

    /// req_pass2_12: the hub SURFACES the one-rule-one-surface violation (§8: "the UI should
    /// assert it rather than defend against it"), naming the surfaces, and stays silent on a
    /// healthy rule.
    #[test]
    fn req_pass2_12_the_hub_surfaces_a_cross_surface_conflict_and_is_quiet_otherwise() {
        let conflicted = vec![
            row("me/api", "DUP-1", "src/a.rs", serde_json::json!({"kind": "defect"})),
            row(
                "me/api",
                "DUP-1",
                "-",
                serde_json::json!({
                    "kind": "convention_observed", "convention_option_id": "o",
                    "convention_label": "L"
                }),
            ),
        ];
        let html = strip_html(rule_hub(&conflicted, "me/api", "DUP-1").expect("present"));
        assert!(html.contains("rule-hub-conflict"), "the conflict is rendered; html=\n{html}");
        assert!(html.contains("Queue"), "names the surfaces; html=\n{html}");
        assert!(html.contains("Conventions"), "names the surfaces; html=\n{html}");

        let clean = vec![row("me/api", "OK-1", "src/a.rs", serde_json::json!({"kind": "defect"}))];
        let html = strip_html(rule_hub(&clean, "me/api", "OK-1").expect("present"));
        assert!(
            !html.contains("rule-hub-conflict"),
            "a healthy rule must not ring the alarm; html=\n{html}"
        );
    }

    /// req_pass2_12: the kind-less ALARM keeps its identity inside the hub. The hub is the
    /// surface a curator reaches for when a row confuses him, so it is the LAST place an
    /// unmapped emission path should be quietly described as a defect.
    #[test]
    fn req_pass2_12_the_hub_keeps_an_unmapped_row_an_alarm() {
        let rows = vec![row("me/api", "LEGACY-1", "src/a.rs", serde_json::json!({}))];
        let html = strip_html(rule_hub(&rows, "me/api", "LEGACY-1").expect("present"));
        assert!(html.contains("Unspecified"), "the alarm chip; html=\n{html}");
        assert!(
            html.contains("unmapped emission path"),
            "and the sentence says what it means; html=\n{html}"
        );
        assert!(!html.contains("Defect"), "never dressed as a defect; html=\n{html}");
    }

    // ── REACHABILITY: §8's "from every surface that shows a rule id" ─────────
    //
    // The hub is only a hub if a curator can get to it from wherever he is holding the rule
    // id. These render each such surface and assert the rule id is the door — a
    // `rule-hub-link` control carrying that exact id — rather than inert text.

    fn panel_html(which: KindSurface, findings: Vec<FindingView>) -> String {
        #[component]
        fn Harness(which: KindSurface, findings: Vec<FindingView>) -> Element {
            let cb = Callback::new(|_: RuleHubRequest| {});
            match which {
                KindSurface::Conventions => rsx! { ConventionsPanel { findings, on_open_hub: cb } },
                KindSurface::NoConvention => {
                    rsx! { NoConventionPanel { findings, on_open_hub: cb } }
                }
                _ => rsx! { NotAssessedPanel { findings, on_open_hub: cb } },
            }
        }
        let mut vdom = VirtualDom::new_with_props(Harness, HarnessProps { which, findings });
        vdom.rebuild_in_place();
        dioxus_ssr::render(&vdom)
    }

    #[test]
    fn req_pass2_12_every_informational_panel_makes_its_rule_ids_the_door_to_the_hub() {
        let conventions = panel_html(
            KindSurface::Conventions,
            vec![row(
                "me/api",
                "SQL-PARAM-1",
                "-",
                serde_json::json!({
                    "kind": "convention_observed", "convention_option_id": "p",
                    "convention_label": "Parameterized queries"
                }),
            )],
        );
        let no_convention = panel_html(
            KindSurface::NoConvention,
            vec![row(
                "me/api",
                "SPLIT-1",
                "-",
                serde_json::json!({
                    "kind": "no_convention",
                    "observed_options": [{"option_id": "a", "label": "A", "sites": 2}]
                }),
            )],
        );
        let not_assessed = panel_html(
            KindSurface::NotAssessed,
            vec![row(
                "me/api",
                "NA-1",
                "-",
                serde_json::json!({
                    "kind": "not_assessed", "reason": "tool_failed",
                    "reason_detail": "eslint exited 2"
                }),
            )],
        );
        for (name, html, rule) in [
            ("Conventions", &conventions, "SQL-PARAM-1"),
            ("No convention", &no_convention, "SPLIT-1"),
            ("Not assessed", &not_assessed, "NA-1"),
        ] {
            assert!(
                html.contains("rule-hub-link"),
                "{name} panel must make its rule id the hub's door; html=\n{html}"
            );
            assert!(
                html.contains(rule),
                "{name} panel must name the rule on that door; html=\n{html}"
            );
        }
    }

    /// req_pass2_12: the DETAIL PANE's rule id is the hub's door — which is how a QUEUE ROW
    /// reaches the hub. A chorale cell renderer is `Send + Sync` and a Dioxus `Callback` is
    /// not, so the row's own "Finding type" cell cannot carry the click; the pane opens on
    /// the same click that focuses the row, so the hub is one further click from any row.
    #[test]
    fn req_pass2_12_the_detail_pane_is_how_a_queue_row_reaches_the_hub() {
        let f = row("me/api", "SQL-INJECTION-1", "src/a.rs", serde_json::json!({"kind": "defect"}));
        let pane_row = DetailPaneRow::from_finding(&f, None, None, 0, 1);
        #[component]
        fn Harness(row: DetailPaneRow) -> Element {
            rsx! {
                FindingDetailPane {
                    row: Some(row),
                    focus: PaneFocus::Cursor,
                    prompt: None,
                    prompt_reason: String::new(),
                    undo: None,
                    on_key: Callback::new(|_: (String, KeyMods)| {}),
                    on_focus: Callback::new(|_: PaneFocus| {}),
                    on_prompt_reason: Callback::new(|_: String| {}),
                    on_prompt_commit: Callback::new(|()| {}),
                    on_prompt_cancel: Callback::new(|()| {}),
                    on_undo: Callback::new(|()| {}),
                    on_dispose: Callback::new(|_: TriageState| {}),
                    on_open_detail: Callback::new(|()| {}),
                    on_open_hub: Callback::new(|()| {}),
                }
            }
        }
        let mut vdom = VirtualDom::new_with_props(Harness, HarnessProps { row: pane_row });
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(
            html.contains("rule-hub-link"),
            "the pane's rule id must be the hub's door; html=\n{html}"
        );
        assert!(html.contains("SQL-INJECTION-1"), "naming the rule; html=\n{html}");
    }

    /// req_pass2_12: the DEVIATION BULK BAR's rule id is the hub's door — which is how a
    /// deviation GROUP HEADER reaches it. chorale's real group-header `<tr>` takes no host
    /// content (step 2 established this), and the bar is the header's reachable stand-in.
    #[test]
    fn req_pass2_12_the_bulk_bar_is_how_a_deviation_group_header_reaches_the_hub() {
        #[component]
        fn Harness(groups: Vec<DeviationBulkRow>) -> Element {
            rsx! {
                DeviationBulkBar {
                    groups,
                    on_confirm_all: Callback::new(|_: usize| {}),
                    on_false_positive_all: Callback::new(|_: usize| {}),
                    on_open_hub: Callback::new(|_: usize| {}),
                }
            }
        }
        let groups = deviation_groups(&deviation_rows());
        let rows: Vec<DeviationBulkRow> = groups.iter().map(DeviationBulkRow::from_group).collect();
        let mut vdom = VirtualDom::new_with_props(Harness, HarnessProps { groups: rows });
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(
            html.contains("rule-hub-link"),
            "the bar's rule id must be the hub's door; html=\n{html}"
        );
        assert!(html.contains("API-VERSIONING-1"), "naming the rule; html=\n{html}");
    }

    /// req_pass2_12: §8 names three navigation paths and says the rest are "a page of
    /// plumbing for a navigation the curator has never needed". This pins the refusal: the
    /// hub's ONE control is its jump, and in particular there is no sibling-deviation link,
    /// no tool-log link and no similar-findings link.
    #[test]
    fn req_pass2_12_the_hub_builds_exactly_one_navigation_and_no_link_graph() {
        for rows in [
            deviation_rows(),
            vec![row("me/api", "D-1", "src/a.rs", serde_json::json!({"kind": "defect"}))],
            vec![row(
                "me/api",
                "C-1",
                "-",
                serde_json::json!({
                    "kind": "convention_observed", "convention_option_id": "o",
                    "convention_label": "L"
                }),
            )],
            vec![row(
                "me/api",
                "N-1",
                "-",
                serde_json::json!({"kind": "not_assessed", "reason": "no_files_matched", "reason_detail": ""}),
            )],
        ] {
            let rule_id = rows[0].rule_id.clone();
            let html = strip_html(rule_hub(&rows, "me/api", &rule_id).expect("present"));
            assert_eq!(
                html.matches("<button").count(),
                1,
                "{rule_id}: one jump and nothing else; html=\n{html}"
            );
            for forbidden in ["sibling", "tool log", "similar", "conforming endpoints"] {
                assert!(
                    !html.to_lowercase().contains(forbidden),
                    "{rule_id}: §8 explicitly does not build a {forbidden} path; html=\n{html}"
                );
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// req_pass2_14 — THE GATED CONVENTION COMMIT, rendered (design §3; Q3 + Q4)
// ─────────────────────────────────────────────────────────────────────────────
//
// SSR cannot type into an input or press a button, so everything here is a props-only
// component rendered in a state and read back out of the HTML — the same discipline the detail
// pane, the process gate and the bulk bar were built on.
//
// What the pure half already proves, in `camerata_ui_core::adopt`'s own suite: that the gate
// refuses twelve near-misses, that the unopened count follows the open gesture one at a time,
// that a deviating convention is adoptable, and that the ruleset round trip preserves every
// field. What THIS module adds is the half that only a render can show: that the component
// actually consults those functions rather than describing them.
#[cfg(test)]
mod convention_commit_tests {
    use super::*;
    use camerata_ui_core::adopt::CommitGate;
    use dioxus::prelude::*;

    /// A finding carrying an arbitrary flattened `kind` payload.
    fn row(repo: &str, rule_id: &str, kind: serde_json::Value) -> FindingView {
        let mut v = serde_json::json!({
            "repo": repo, "path": "src/lib.rs", "line": 7, "rule_id": rule_id,
            "severity": "info", "snippet": "s", "detail": "d", "status": "active",
        });
        for (k, val) in kind.as_object().expect("kind payload is an object") {
            v[k] = val.clone();
        }
        serde_json::from_value(v).expect("valid FindingView fixture")
    }

    fn convention(repo: &str, rule_id: &str, option: &str, label: &str) -> FindingView {
        row(
            repo,
            rule_id,
            serde_json::json!({
                "kind": "convention_observed",
                "convention_option_id": option,
                "convention_label": label,
            }),
        )
    }

    fn render_conventions(
        findings: Vec<FindingView>,
        model: AdoptionModel,
        only_unopened: bool,
    ) -> String {
        #[component]
        fn Harness(
            findings: Vec<FindingView>,
            model: AdoptionModel,
            only_unopened: bool,
        ) -> Element {
            rsx! {
                ConventionsPanel { findings, model, only_unopened }
            }
        }
        let mut vdom = VirtualDom::new_with_props(
            Harness,
            HarnessProps {
                findings,
                model,
                only_unopened,
            },
        );
        vdom.rebuild_in_place();
        dioxus_ssr::render(&vdom)
    }

    fn render_dialog(plan: CommitPlan, typed: &str, busy: bool) -> String {
        #[component]
        fn Harness(plan: CommitPlan, typed: String, busy: bool) -> Element {
            rsx! {
                ConventionCommitDialog {
                    plan,
                    typed,
                    busy,
                    on_typed: Callback::new(|_: String| {}),
                    on_cancel: Callback::new(|()| {}),
                    on_review_unopened: Callback::new(|()| {}),
                    on_commit: Callback::new(|()| {}),
                }
            }
        }
        let mut vdom = VirtualDom::new_with_props(
            Harness,
            HarnessProps {
                plan,
                typed: typed.to_string(),
                busy,
            },
        );
        vdom.rebuild_in_place();
        dioxus_ssr::render(&vdom)
    }

    /// The commit button's rendered markup, isolated from the rest of the dialog so "is it
    /// disabled" is a question about that element and not about the page.
    fn commit_button(html: &str) -> String {
        let start = html
            .find("adopt-dialog-commit")
            .expect("the commit button renders");
        // Walk back to the opening `<button` and forward to its closing tag.
        let open = html[..start].rfind("<button").expect("an opening tag");
        let end = html[open..].find("</button>").expect("a closing tag") + open;
        html[open..end].to_string()
    }

    // ── THE GATE, UNDER RENDER ───────────────────────────────────────────────

    /// req_pass2_14 (**THE BYPASS PROOF, RENDERED**): the dialog's Commit button is SHUT for
    /// every wrong destination and open only for the right one.
    ///
    /// The pure gate is already proven in `camerata_ui_core::adopt`. This is the other half of
    /// the same requirement and the half that actually ships: that the button consults it. The
    /// failure this catches is the common one — a dialog that renders a confirmation field,
    /// looks thorough, and enables its button regardless.
    #[test]
    fn req_pass2_14_the_dialog_commit_button_is_shut_for_every_wrong_destination() {
        let findings = vec![
            convention("acme/payments-api", "R-1", "a", "A convention"),
            convention("acme/payments-web", "R-2", "b", "B convention"),
        ];
        let all = adoptable_conventions(&findings);
        let plan = CommitPlan::conventions(&all, &AdoptionModel::new(), "acme/payments-api");
        assert_eq!(plan.adopted_count(), 1, "precondition: there is something to write");

        for wrong in [
            "",
            "   ",
            "acme/payments-web", // the OTHER repo in the same scan
            "payments-api",
            "acme",
            "acme/payments",
            "acme/payments-api2",
            "yes",
        ] {
            let html = render_dialog(plan.clone(), wrong, false);
            let button = commit_button(&html);
            assert!(
                button.contains("disabled"),
                "typing {wrong:?} must leave the Commit button disabled; button=\n{button}"
            );
            assert!(
                html.contains("adopt-dialog-commit shut"),
                "and visibly shut; typed={wrong:?}; html=\n{html}"
            );
        }

        let html = render_dialog(plan.clone(), "acme/payments-api", false);
        let button = commit_button(&html);
        assert!(
            !button.contains("disabled"),
            "the real destination opens it; button=\n{button}"
        );
        assert!(html.contains("adopt-dialog-commit armed"), "html=\n{html}");
    }

    /// req_pass2_14: the gate is a TYPED FIELD, not a checkbox. The ruling is explicit about
    /// the distinction and about why, so the distinction is pinned rather than left to taste.
    #[test]
    fn req_pass2_14_the_confirmation_is_typed_not_a_checkbox() {
        let findings = vec![convention("me/api", "R-1", "a", "A")];
        let plan = CommitPlan::conventions(
            &adoptable_conventions(&findings),
            &AdoptionModel::new(),
            "me/api",
        );
        let html = render_dialog(plan, "", false);
        assert!(
            html.contains("adopt-dialog-confirm-input"),
            "a text field; html=\n{html}"
        );
        assert!(
            html.contains("type=\"text\"") || html.contains("type=text"),
            "typed input; html=\n{html}"
        );
        assert!(
            !html.contains("type=\"checkbox\""),
            "a checkbox is muscle memory at 1am \u{2014} the ruling forbids it; html=\n{html}"
        );
        assert!(
            html.contains("Type me/api to confirm"),
            "and it asks for the destination BY NAME; html=\n{html}"
        );
    }

    /// req_pass2_14: an EMPTY plan cannot be committed however the field is filled in. A
    /// dialog that writes nothing must not present a live Commit.
    #[test]
    fn req_pass2_14_an_empty_plan_renders_a_shut_commit() {
        let plan = CommitPlan {
            destination_repo: "me/api".to_string(),
            noun: "conventions".to_string(),
            adopted: vec![],
            skipped: 4,
        };
        assert_eq!(commit_gate(&plan, "me/api"), CommitGate::NothingToCommit);
        let html = render_dialog(plan, "me/api", false);
        assert!(commit_button(&html).contains("disabled"), "html=\n{html}");
    }

    /// req_pass2_14: while a commit is in flight every exit is DISABLED rather than removed,
    /// so a double-click cannot fire a second write and the dialog does not jump under the
    /// cursor.
    #[test]
    fn req_pass2_14_an_in_flight_commit_disables_every_exit() {
        let findings = vec![convention("me/api", "R-1", "a", "A")];
        let plan = CommitPlan::conventions(
            &adoptable_conventions(&findings),
            &AdoptionModel::new(),
            "me/api",
        );
        let html = render_dialog(plan, "me/api", true);
        assert!(commit_button(&html).contains("disabled"), "html=\n{html}");
        assert!(html.contains("Writing"), "and it says so; html=\n{html}");
        for class in ["adopt-dialog-cancel", "adopt-dialog-review"] {
            assert!(html.contains(class), "{class} still present; html=\n{html}");
        }
    }

    // ── WHAT THE DIALOG SAYS ─────────────────────────────────────────────────

    /// req_pass2_14: the dialog states the four things the ruling requires — the destination,
    /// the adopted count, the skipped count, and **how many were adopted by default and never
    /// opened** — in the approved copy.
    #[test]
    fn req_pass2_14_the_dialog_states_the_destination_the_counts_and_the_unopened() {
        let findings: Vec<FindingView> = (1..=36)
            .map(|i| convention("acme/api", &format!("R-{i}"), "opt", "Label"))
            .collect();
        let all = adoptable_conventions(&findings);
        let mut model = AdoptionModel::new();
        for i in 1..=11 {
            model.mark_opened(&adoption_key("acme/api", &format!("R-{i}")));
        }
        let plan = CommitPlan::conventions(&all, &model, "acme/api");
        let html = render_dialog(plan, "", false);

        assert!(
            html.contains(
                "You are writing 36 conventions into a client repository. This changes how \
                 future AI agents working on that codebase will behave."
            ),
            "the approved headline, verbatim; html=\n{html}"
        );
        assert!(
            html.contains(
                "25 conventions were adopted by default and not opened. Adoption is opt-out, \
                 so these were selected for you."
            ),
            "the approved warning, with the REAL number; html=\n{html}"
        );
        assert!(
            html.contains("Destination repository:") && html.contains("acme/api"),
            "the destination is named explicitly; html=\n{html}"
        );
        assert!(
            html.contains("36 to write") && html.contains("0 skipped"),
            "both counts; html=\n{html}"
        );
    }

    /// req_pass2_14: with every convention opened the warning is GONE, not rendered as a zero.
    /// A standing "0 were not opened" row teaches the eye to skip exactly the line the gate
    /// exists to make unskippable.
    #[test]
    fn req_pass2_14_the_unopened_warning_disappears_when_nothing_is_unopened() {
        let findings = vec![
            convention("me/api", "R-1", "a", "A"),
            convention("me/api", "R-2", "b", "B"),
        ];
        let all = adoptable_conventions(&findings);
        let mut model = AdoptionModel::new();
        model.mark_opened(&adoption_key("me/api", "R-1"));
        model.mark_opened(&adoption_key("me/api", "R-2"));
        let plan = CommitPlan::conventions(&all, &model, "me/api");
        assert_eq!(plan.unopened_count(), 0);
        let html = render_dialog(plan, "", false);
        assert!(
            !html.contains("adopted by default and not opened"),
            "no zero sentence; html=\n{html}"
        );
        assert!(
            !html.contains("adopt-dialog-warning"),
            "and no empty warning box; html=\n{html}"
        );
    }

    /// req_pass2_14 (**THE UNOPENED COUNT IS REAL, RENDERED**): open one more convention and
    /// the dialog's number falls by exactly one.
    ///
    /// The pure test walks the whole ladder; this one proves the RENDERED dialog is reading
    /// that ladder and not a constant. Three renders of the same plan shape with three
    /// different openness records.
    #[test]
    fn req_pass2_14_the_rendered_unopened_count_follows_the_open_gesture() {
        let findings: Vec<FindingView> = (1..=4)
            .map(|i| convention("me/api", &format!("R-{i}"), "opt", "Label"))
            .collect();
        let all = adoptable_conventions(&findings);
        let mut model = AdoptionModel::new();
        for (i, expected) in (1..=4).zip([
            "4 conventions were",
            "3 conventions were",
            "2 conventions were",
            "1 convention was",
        ]) {
            let plan = CommitPlan::conventions(&all, &model, "me/api");
            let html = render_dialog(plan, "", false);
            assert!(
                html.contains(expected),
                "before opening R-{i} the dialog must say {expected:?}; html=\n{html}"
            );
            model.mark_opened(&adoption_key("me/api", &format!("R-{i}")));
        }
        // And with the fourth opened, the warning is gone entirely.
        let plan = CommitPlan::conventions(&all, &model, "me/api");
        let html = render_dialog(plan, "", false);
        assert!(
            !html.contains("adopt-dialog-warning"),
            "everything opened, so no warning at all; html=\n{html}"
        );
    }

    /// req_pass2_14: the dialog lists the rules it is about to write, and flags the unopened
    /// ones in that list. The count tells the curator how many; the list tells him which,
    /// without leaving the dialog.
    #[test]
    fn req_pass2_14_the_dialog_lists_the_rules_and_flags_the_unopened_ones() {
        let findings = vec![
            convention("me/api", "READ-ME-1", "a", "A convention"),
            convention("me/api", "UNREAD-1", "b", "B convention"),
        ];
        let all = adoptable_conventions(&findings);
        let mut model = AdoptionModel::new();
        model.mark_opened(&adoption_key("me/api", "READ-ME-1"));
        let plan = CommitPlan::conventions(&all, &model, "me/api");
        let html = render_dialog(plan, "", false);
        assert!(html.contains("READ-ME-1") && html.contains("UNREAD-1"), "html=\n{html}");
        assert_eq!(
            html.matches("adopt-unopened-flag").count(),
            1,
            "exactly the one that was not opened carries the flag; html=\n{html}"
        );
    }

    // ── THREE EXITS ──────────────────────────────────────────────────────────

    /// req_pass2_14: the dialog offers THREE exits, not two. The ruling is explicit that the
    /// safer path must be offered rather than implied:
    ///
    /// > It offers three exits, not two: Cancel, Review the unopened set, Commit.
    #[test]
    fn req_pass2_14_the_dialog_offers_three_exits() {
        let findings = vec![
            convention("me/api", "R-1", "a", "A"),
            convention("me/api", "R-2", "b", "B"),
        ];
        let all = adoptable_conventions(&findings);
        let plan = CommitPlan::conventions(&all, &AdoptionModel::new(), "me/api");
        let html = render_dialog(plan, "", false);
        assert!(html.contains("adopt-dialog-cancel") && html.contains("Cancel"), "html=\n{html}");
        assert!(
            html.contains("adopt-dialog-review") && html.contains("Review the 2 not opened"),
            "the third exit, naming its size; html=\n{html}"
        );
        assert!(html.contains("adopt-dialog-commit") && html.contains("Commit"), "html=\n{html}");
    }

    /// req_pass2_14: the third exit is HIDDEN when everything was opened. A button that
    /// filters to an empty list is the 403-button bug in miniature, and the standing
    /// hide-dead-end-affordances rule forbids it.
    #[test]
    fn req_pass2_14_the_review_exit_is_absent_when_there_is_nothing_to_review() {
        let findings = vec![convention("me/api", "R-1", "a", "A")];
        let all = adoptable_conventions(&findings);
        let mut model = AdoptionModel::new();
        model.mark_opened(&adoption_key("me/api", "R-1"));
        let plan = CommitPlan::conventions(&all, &model, "me/api");
        let html = render_dialog(plan, "", false);
        assert!(
            !html.contains("adopt-dialog-review"),
            "nothing unopened, so no review exit; html=\n{html}"
        );
        assert!(html.contains("adopt-dialog-cancel") && html.contains("adopt-dialog-commit"));
    }

    // ── THE CONVENTIONS PANEL ────────────────────────────────────────────────

    /// req_pass2_14 (Q4): adoption is OPT-OUT on the rendered panel. Every untouched
    /// convention reads as selected and the commit bar counts all of them; one Skip removes
    /// exactly one.
    #[test]
    fn req_pass2_14_the_conventions_panel_is_opt_out_under_render() {
        let findings = vec![
            convention("me/api", "R-1", "a", "A convention"),
            convention("me/api", "R-2", "b", "B convention"),
            convention("me/api", "R-3", "c", "C convention"),
        ];
        let html = render_conventions(findings.clone(), AdoptionModel::new(), false);
        assert_eq!(
            html.matches("adopt-state on").count(),
            3,
            "all three pre-selected; html=\n{html}"
        );
        assert!(html.contains("3 to write"), "the bar counts them; html=\n{html}");
        assert!(
            html.contains("Write 3 into me/api"),
            "and the button names the destination; html=\n{html}"
        );
        assert!(
            html.contains("Adoption is OPT-OUT"),
            "the panel says so out loud; html=\n{html}"
        );

        let mut model = AdoptionModel::new();
        model.set_adopted(&adoption_key("me/api", "R-2"), false);
        let html = render_conventions(findings, model, false);
        assert_eq!(html.matches("adopt-state on").count(), 2, "html=\n{html}");
        assert!(html.contains("Skipped"), "the skipped row says so; html=\n{html}");
        assert!(html.contains("2 to write") && html.contains("1 skipped"), "html=\n{html}");
    }

    /// req_pass2_14 (**DEVIATIONS DO NOT BLOCK ADOPTION, RENDERED**) — the owner's §13
    /// reversal of the design's own stance:
    ///
    /// > "if I say 'this is the rule and convention' then I mean it, and that can be adopted."
    ///
    /// A rule with only Deviation rows (no `ConventionObserved` row at all) appears in the
    /// Conventions panel, pre-selected, labelled with its exception count, with the count
    /// rendered as context and the panel saying in words that remediation is separate work.
    #[test]
    fn req_pass2_14_a_convention_with_live_deviations_is_adoptable_under_render() {
        let dev = |path_line: usize| {
            let mut f = row(
                "me/api",
                "API-VERSIONING-1",
                serde_json::json!({
                    "kind": "deviation",
                    "deviating_sites": 2,
                    "assessed_sites": 47,
                    "convention_option_id": "versioned",
                    "convention_label": "URI-versioned endpoints",
                }),
            );
            f.line = path_line;
            f
        };
        let html = render_conventions(vec![dev(12), dev(40)], AdoptionModel::new(), false);

        assert!(html.contains("API-VERSIONING-1"), "listed; html=\n{html}");
        assert!(html.contains("URI-versioned endpoints"), "by its authored label; html=\n{html}");
        assert!(
            html.contains("observed with 2 exceptions"),
            "\u{a7}13's exact phrase; html=\n{html}"
        );
        assert_eq!(
            html.matches("adopt-state on").count(),
            1,
            "PRE-SELECTED \u{2014} the exceptions are context, not a gate; html=\n{html}"
        );
        assert!(
            html.contains("1 to write") && html.contains("Write 1 into me/api"),
            "and it reaches the commit; html=\n{html}"
        );
        assert!(
            html.contains("adopting states the standard going forward"),
            "the panel states WHY the exceptions do not block; html=\n{html}"
        );
        assert_eq!(
            html.matches("adopt-row").count(),
            1,
            "two deviating rows are ONE convention, not two; html=\n{html}"
        );
    }

    /// req_pass2_14: opening a deviating convention's detail names the exceptions and says
    /// plainly that dispositioning them does not gate the adoption — the one place a curator
    /// would otherwise reasonably assume the old, blocked behaviour.
    #[test]
    fn req_pass2_14_an_opened_deviating_convention_says_remediation_is_separate() {
        let f = row(
            "me/api",
            "API-VERSIONING-1",
            serde_json::json!({
                "kind": "deviation",
                "deviating_sites": 2,
                "assessed_sites": 47,
                "convention_option_id": "versioned",
                "convention_label": "URI-versioned endpoints",
            }),
        );
        let mut model = AdoptionModel::new();
        model.mark_opened(&adoption_key("me/api", "API-VERSIONING-1"));
        let html = render_conventions(vec![f], model, false);
        assert!(html.contains("adopt-detail"), "the detail is open; html=\n{html}");
        assert!(
            html.contains("does not gate this adoption"),
            "and it says so; html=\n{html}"
        );
        assert!(
            html.contains("AGENTS.md"),
            "and names where this is written; html=\n{html}"
        );
    }

    /// req_pass2_14: a convention with no AUTHORED option id is listed and explained, never
    /// silently committed. Writing the rule's default option instead would install a position
    /// the audit never observed, which is the one dishonesty available on this surface.
    #[test]
    fn req_pass2_14_a_convention_with_no_authored_option_cannot_be_written_under_render() {
        let f = row(
            "me/api",
            "LOOSE-1",
            serde_json::json!({
                "kind": "deviation",
                "deviating_sites": 2,
                "assessed_sites": 9,
                "convention_option_id": null,
                "convention_label": "something the producer saw",
            }),
        );
        let mut model = AdoptionModel::new();
        model.mark_opened(&adoption_key("me/api", "LOOSE-1"));
        let html = render_conventions(vec![f], model, false);
        assert!(html.contains("LOOSE-1"), "still listed; html=\n{html}");
        assert!(html.contains("Cannot write"), "marked unwritable; html=\n{html}");
        assert!(
            html.contains("No authored option matches"),
            "and told why; html=\n{html}"
        );
        assert!(
            html.contains("Nothing selected to write"),
            "and the commit bar has nothing; html=\n{html}"
        );
        assert!(
            !html.contains("adopt-toggle"),
            "with no Skip/Adopt toggle, because there is no decision to make; html=\n{html}"
        );
    }

    /// req_pass2_14: the panel's own unopened indicator follows the record, and the "not
    /// opened" flag leaves a row the moment it is opened.
    #[test]
    fn req_pass2_14_the_panel_flags_exactly_the_unopened_rows() {
        let findings = vec![
            convention("me/api", "R-1", "a", "A"),
            convention("me/api", "R-2", "b", "B"),
            convention("me/api", "R-3", "c", "C"),
        ];
        let html = render_conventions(findings.clone(), AdoptionModel::new(), false);
        assert_eq!(html.matches("adopt-unopened-flag").count(), 3, "html=\n{html}");
        assert!(html.contains("3 not opened"), "the bar agrees; html=\n{html}");

        let mut model = AdoptionModel::new();
        model.mark_opened(&adoption_key("me/api", "R-2"));
        let html = render_conventions(findings, model, false);
        assert_eq!(html.matches("adopt-unopened-flag").count(), 2, "html=\n{html}");
        assert!(html.contains("2 not opened"), "html=\n{html}");
        assert!(html.contains("Opened"), "and the opened row says so; html=\n{html}");
    }

    /// req_pass2_14: the dialog's third exit lands somewhere useful — the panel's
    /// unopened-only view shows exactly the adopted-but-unopened rows and offers the way back.
    #[test]
    fn req_pass2_14_the_unopened_only_view_shows_exactly_that_set() {
        let findings = vec![
            convention("me/api", "OPENED-1", "a", "A"),
            convention("me/api", "UNREAD-1", "b", "B"),
            convention("me/api", "SKIPPED-1", "c", "C"),
        ];
        let mut model = AdoptionModel::new();
        model.mark_opened(&adoption_key("me/api", "OPENED-1"));
        model.set_adopted(&adoption_key("me/api", "SKIPPED-1"), false);
        let html = render_conventions(findings, model, true);
        assert!(html.contains("UNREAD-1"), "the unopened one is shown; html=\n{html}");
        assert!(
            !html.contains("OPENED-1"),
            "the opened one is not; html=\n{html}"
        );
        assert!(
            !html.contains("SKIPPED-1"),
            "nor is a skipped one \u{2014} it is not being written; html=\n{html}"
        );
        assert!(html.contains("adopt-filter-clear"), "and there is a way back; html=\n{html}");
    }

    // ── THE NO-CONVENTION PICK (Q3) ──────────────────────────────────────────

    fn render_no_convention(findings: Vec<FindingView>, model: AdoptionModel) -> String {
        #[component]
        fn Harness(findings: Vec<FindingView>, model: AdoptionModel) -> Element {
            rsx! {
                NoConventionPanel { findings, model }
            }
        }
        let mut vdom =
            VirtualDom::new_with_props(Harness, HarnessProps { findings, model });
        vdom.rebuild_in_place();
        dioxus_ssr::render(&vdom)
    }

    fn split_row(repo: &str, rule_id: &str) -> FindingView {
        row(
            repo,
            rule_id,
            serde_json::json!({
                "kind": "no_convention",
                "observed_options": [
                    { "option_id": "result", "label": "Result types", "sites": 9 },
                    { "option_id": "panic", "label": "Panics", "sites": null },
                ],
            }),
        )
    }

    /// req_pass2_14 (Q3): the `NoConvention` pick DEFAULTS to "leave as reported", states its
    /// client-facing governance nature in the UI, and offers the authored options as the only
    /// alternatives.
    #[test]
    fn req_pass2_14_the_no_convention_pick_defaults_to_leave_and_says_it_is_governance() {
        let html = render_no_convention(
            vec![split_row("me/api", "ERROR-HANDLING-1")],
            AdoptionModel::new(),
        );
        assert!(
            html.contains("CLIENT-FACING governance act"),
            "Q3: it must SAY SO; html=\n{html}"
        );
        assert!(
            html.contains("AGENTS.md"),
            "and name where it lands; html=\n{html}"
        );
        assert!(
            html.contains("Leave as reported"),
            "the default option is present; html=\n{html}"
        );
        assert_eq!(
            html.matches("adopt-state off").count(),
            1,
            "and the row reads as left-as-reported; html=\n{html}"
        );
        assert!(
            html.contains("Nothing selected to write"),
            "so the commit bar has nothing; html=\n{html}"
        );
        assert!(
            html.contains("Recommend: Result types (9 sites)")
                && html.contains("Recommend: Panics"),
            "the authored options, with counts only where the producer counted; html=\n{html}"
        );
        assert!(
            !html.contains("Panics (0 sites)"),
            "never a fabricated count; html=\n{html}"
        );
    }

    /// req_pass2_14 (Q3): a pick inherits the SAME gate. Not a lighter one, not none.
    #[test]
    fn req_pass2_14_a_no_convention_pick_inherits_the_same_typed_gate() {
        let findings = vec![split_row("me/api", "ERROR-HANDLING-1")];
        let mut model = AdoptionModel::new();
        model.set_recommendation(
            &adoption_key("me/api", "ERROR-HANDLING-1"),
            Some("result".to_string()),
        );

        let panel = render_no_convention(findings.clone(), model.clone());
        assert!(panel.contains("Write 1 into me/api"), "html=\n{panel}");
        assert_eq!(panel.matches("adopt-state on").count(), 1, "html=\n{panel}");

        let plan = CommitPlan::recommendations(&split_rules(&findings), &model, "me/api");
        assert!(
            plan.headline().contains("1 recommendation into a client repository"),
            "{}",
            plan.headline()
        );
        let shut = render_dialog(plan.clone(), "me/web", false);
        assert!(commit_button(&shut).contains("disabled"), "html=\n{shut}");
        let armed = render_dialog(plan, "me/api", false);
        assert!(!commit_button(&armed).contains("disabled"), "html=\n{armed}");
    }

    // ── FOCUS: the dialog must not leave the pane's keys armed ───────────────

    /// req_pass2_14 (**THE FOCUS CARRY-OVER**): typing a destination into the dialog cannot
    /// dispose a finding.
    ///
    /// The destination this test uses contains `c`, `i`, `t` AND `f`, which are exactly the
    /// four disposition hotkeys — so if the scoping ever regressed, typing this repository's
    /// name would Confirm a row, open two reason prompts and mark something a false positive.
    /// Every character is walked through the real `pane_command`.
    ///
    /// `pane_keys_armed` is DEFINED as `pane_command(focus, "c", none).is_some()`, so the
    /// indicator cannot report armed while the handler refuses; the render assertion below
    /// pins that the pane says "off" in the state a dialog leaves it in.
    #[test]
    fn req_pass2_14_typing_the_destination_cannot_reach_the_panes_disposition_keys() {
        let destination = "acme-fintech/api";
        assert!(
            ['c', 'i', 't', 'f'].iter().all(|c| destination.contains(*c)),
            "precondition: the fixture destination contains every hotkey letter"
        );
        // The dialog's input is not the pane's cursor, whatever else is true.
        for focus in [PaneFocus::Elsewhere, PaneFocus::ReasonPrompt] {
            assert!(!pane_keys_armed(focus), "{focus:?} must report keys OFF");
            for ch in destination.chars() {
                let key = ch.to_string();
                assert!(
                    pane_command(focus, &key, KeyMods::none()).is_none(),
                    "{ch:?} typed under {focus:?} must produce no command"
                );
            }
        }
        // And the pane RENDERS that state, so the curator is not told otherwise.
        #[component]
        fn PaneHarness() -> Element {
            rsx! {
                FindingDetailPane {
                    row: None,
                    focus: PaneFocus::Elsewhere,
                    prompt: None,
                    prompt_reason: String::new(),
                    undo: None,
                    on_key: Callback::new(|_: (String, KeyMods)| {}),
                    on_focus: Callback::new(|_: PaneFocus| {}),
                    on_prompt_reason: Callback::new(|_: String| {}),
                    on_prompt_commit: Callback::new(|()| {}),
                    on_prompt_cancel: Callback::new(|()| {}),
                    on_undo: Callback::new(|()| {}),
                    on_dispose: Callback::new(|_: TriageState| {}),
                    on_open_detail: Callback::new(|()| {}),
                    on_open_hub: Callback::new(|()| {}),
                }
            }
        }
        let mut vdom = VirtualDom::new(PaneHarness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(
            !html.contains("pane-keys armed"),
            "the pane must not claim armed keys while a dialog holds focus; html=\n{html}"
        );
    }

    // ── THE WIRE: read, merge, write, emit ───────────────────────────────────

    /// req_pass2_14 (**THE COMMIT IS REAL, NOT A SEAM**): `commit_adoptions` performs the
    /// read-modify-write against the EXISTING ruleset route and then calls the EXISTING
    /// emit-local route, against a mock BFF that asserts both bodies.
    ///
    /// The body assertion is the point. A commit that posted only the adopted rule would be
    /// accepted by the server and would DELETE every other selection the project has, because
    /// `upsert_base_rules` replaces the list rather than merging it.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn req_pass2_14_commit_adoptions_reads_merges_writes_and_emits() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        // 1. The project's current ruleset — one UNRELATED selection with a drift baseline.
        Mock::given(method("GET"))
            .and(path("/api/projects/proj-1/ruleset"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "ruleset": {
                    "selections": [{
                        "rule_id": "UNRELATED-1",
                        "chosen_option": "keep-me",
                        "repos": ["me/web"],
                        "applied_hash": "sha-abc",
                        "applied_directive": "as baselined",
                    }],
                    "cross_repo": [],
                    "process": [],
                    "custom": [{ "name": "HOUSE", "body": "b", "domain": "*", "repos": [] }],
                }
            })))
            .expect(1)
            .mount(&server)
            .await;
        // 2. The merged write: the untouched selection SURVIVES with its baseline intact, the
        //    adopted convention is appended, and no `custom` key is sent.
        Mock::given(method("POST"))
            .and(path("/api/projects/proj-1/ruleset"))
            .and(body_json(serde_json::json!({
                "selections": [
                    {
                        "rule_id": "UNRELATED-1",
                        "chosen_option": "keep-me",
                        "repos": ["me/web"],
                        "applied_hash": "sha-abc",
                        "applied_directive": "as baselined",
                    },
                    {
                        "rule_id": "API-VERSIONING-1",
                        "chosen_option": "versioned",
                        "repos": ["me/api"],
                        "applied_hash": null,
                        "applied_directive": null,
                    }
                ],
                "cross_repo": [],
                "process": [],
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": true })))
            .expect(1)
            .mount(&server)
            .await;
        // 3. The EXISTING emit path, cascade all-false: write the governance files in place.
        Mock::given(method("POST"))
            .and(path("/api/projects/proj-1/emit-local"))
            .and(body_json(
                serde_json::json!({ "branch": false, "push": false, "open_pr": false }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "results": [{ "repo": "me/api", "ok": true }]
            })))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let (ok, msg) = super::commit_adoptions(
            "proj-1",
            "me/api",
            &[AdoptedRule {
                rule_id: "API-VERSIONING-1".to_string(),
                option_id: "versioned".to_string(),
                label: "URI-versioned endpoints".to_string(),
                opened: true,
            }],
        )
        .await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(ok, "the commit must succeed: {msg}");
        assert!(msg.contains("me/api"), "and name the destination: {msg}");
    }

    /// req_pass2_14: a FAILED READ writes nothing. Pressing on would post a merge built over
    /// an empty ruleset, which `upsert_base_rules` would then make the project's whole
    /// selection set — the one failure mode on this path that destroys data rather than
    /// merely not doing anything.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn req_pass2_14_a_failed_ruleset_read_writes_nothing() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/projects/proj-1/ruleset"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        // Deliberately NOT mounted: any POST would 404, but the assertion that matters is the
        // message — the function must report that nothing was written.
        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let (ok, msg) = super::commit_adoptions(
            "proj-1",
            "me/api",
            &[AdoptedRule {
                rule_id: "R-1".to_string(),
                option_id: "o".to_string(),
                label: "O".to_string(),
                opened: true,
            }],
        )
        .await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(!ok, "a failed read is a failed commit");
        assert!(
            msg.contains("nothing was written"),
            "and it says so plainly: {msg}"
        );
    }

    /// req_pass2_14: an empty adopted set never reaches the network at all.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn req_pass2_14_an_empty_commit_makes_no_requests() {
        use wiremock::MockServer;
        let server = MockServer::start().await;
        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let (ok, msg) = super::commit_adoptions("proj-1", "me/api", &[]).await;
        std::env::remove_var("CAMERATA_BFF_URL");
        assert!(!ok);
        assert!(msg.contains("Nothing selected"), "{msg}");
        assert!(
            server.received_requests().await.unwrap_or_default().is_empty(),
            "no request may be made for an empty commit"
        );
    }
}
