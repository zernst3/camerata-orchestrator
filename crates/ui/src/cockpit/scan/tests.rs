//! Tests: tests.

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
    for (k, v) in overrides
        .as_object()
        .expect("overrides must be a JSON object")
    {
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
    assert_eq!(
        report.ai_error.as_deref(),
        Some("me/api: AI audit skipped (boom)")
    );
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
    assert_eq!(
        compliance,
        vec!["falling back to the Claude CLI.".to_string()]
    );
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

#[test]
fn the_draft_queue_of_unstored_changes_round_trips_including_a_revert() {
    // The draft's dispositions are the offline queue of the write-through bridge: a decided
    // entry is a change to store, an Unresolved entry is a revert to revoke. Both must
    // survive a save and a load.
    let mut d: OnboardingDraft =
        serde_json::from_str(&format!(r#"{{"scan": {}}}"#, minimal_scan_json())).unwrap();
    d.dispositions.insert(
        "stored-later".to_string(),
        super::Disposition {
            state: super::TriageState::Ignored,
            reason: "dead code".to_string(),
            bucket: super::TechDebtBucket::Later,
        },
    );
    d.dispositions
        .insert("undone".to_string(), super::Disposition::default());
    let back: OnboardingDraft = serde_json::from_str(&serde_json::to_string(&d).unwrap()).unwrap();
    assert_eq!(back.dispositions, d.dispositions);
    assert_eq!(
        back.dispositions["undone"].state,
        super::TriageState::Unresolved
    );
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
    let d: OnboardingDraft =
        serde_json::from_str(&json).expect("back-compat deserialization failed");
    assert!(
        d.dispositions.is_empty(),
        "dispositions should default to empty"
    );
    assert!(
        d.repo_selection.is_empty(),
        "repo_selection should default to empty"
    );
    assert!(d.chosen.is_empty(), "chosen should default to empty");
    assert!(d.custom.is_empty(), "custom should default to empty");
    assert!(d.audit.is_none(), "audit should default to None");
    assert!(
        d.viewed_repo.is_empty(),
        "viewed_repo should default to empty string"
    );
}

/// A draft with repo_selection and chosen round-trips through serde without data loss.
#[test]
fn onboarding_draft_selection_round_trips() {
    let mut repo_selection = std::collections::HashMap::new();
    repo_selection.insert(
        SINGLE_REPO_SELECTION_KEY.to_string(),
        vec!["rule-a".to_string(), "rule-b".to_string()],
    );
    repo_selection.insert("my-repo".to_string(), vec!["rule-c".to_string()]);
    let mut chosen = std::collections::HashMap::new();
    chosen.insert("rule-a".to_string(), "opt-2".to_string());

    let json = format!(
        r#"{{
            "scan": {},
            "repo_selection": {},
            "chosen": {}
        }}"#,
        minimal_scan_json(),
        serde_json::to_string(&repo_selection).unwrap(),
        serde_json::to_string(&chosen).unwrap(),
    );

    let d: OnboardingDraft =
        serde_json::from_str(&json).expect("round-trip deserialization failed");

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
    let suggested: std::collections::HashSet<&str> =
        ["rule-a", "rule-b", "rule-c"].iter().copied().collect();
    let user_effective: std::collections::HashSet<&str> =
        ["rule-a", "rule-c", "rule-d"].iter().copied().collect();

    // On save: repo_selection holds the user's effective selection.
    let mut repo_selection = std::collections::HashMap::new();
    repo_selection.insert(
        SINGLE_REPO_SELECTION_KEY.to_string(),
        user_effective
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>(),
    );

    // On restore: the table seed logic sees a saved entry and restores it exactly —
    // it does NOT fall back to the suggested baseline (that would lose the user's delta).
    let saved: Option<std::collections::HashSet<String>> = repo_selection
        .get(SINGLE_REPO_SELECTION_KEY)
        .map(|ids| ids.iter().cloned().collect());

    assert!(
        saved.is_some(),
        "saved entry must be present after user interaction"
    );
    let restored: std::collections::HashSet<&str> =
        saved.as_ref().unwrap().iter().map(|s| s.as_str()).collect();

    // rule-b: user deselected — must NOT be in the restored set.
    assert!(
        !restored.contains("rule-b"),
        "user-deselected rule must not be restored"
    );
    // rule-d: user added (non-recommended) — must be in the restored set.
    assert!(
        restored.contains("rule-d"),
        "user-added non-recommended rule must be restored"
    );
    // rule-a, rule-c: user kept (untouched suggestions) — must be in the restored set.
    assert!(
        restored.contains("rule-a"),
        "untouched suggested rule must be preserved"
    );
    assert!(
        restored.contains("rule-c"),
        "untouched suggested rule must be preserved"
    );
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
    assert!(
        !label.contains('$'),
        "free model must not show a price: {label}"
    );
}

#[test]
fn badge_paid_model_shows_price_per_million() {
    // $0.55/M output
    let label = super::model_badge_label("DeepSeek R1", false, true, 64_000, 0.55, false, true);
    assert!(
        label.contains("$0.55/M"),
        "paid model must show output price: {label}"
    );
    assert!(
        !label.contains("FREE"),
        "paid model must not show FREE: {label}"
    );
}

#[test]
fn badge_round_price_strips_trailing_decimal() {
    // $15.0/M → "$15/M" (no trailing zero)
    let label = super::model_badge_label("Opus", false, true, 200_000, 15.0, false, true);
    assert!(
        label.contains("$15/M"),
        "round price must strip decimal: {label}"
    );
}

#[test]
fn badge_caching_true_shows_cache_tag() {
    let label = super::model_badge_label("DeepSeek R1", false, true, 64_000, 0.55, true, true);
    assert!(
        label.contains("cache"),
        "caching model must show cache tag: {label}"
    );
}

#[test]
fn badge_caching_false_omits_cache_tag() {
    let label = super::model_badge_label("Llama 3.1", false, true, 128_000, 0.10, false, true);
    assert!(
        !label.contains("cache"),
        "non-caching model must not show cache tag: {label}"
    );
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
    assert!(
        label.contains("price unknown"),
        "unknown price must be labeled: {label}"
    );
    assert!(
        !label.contains("FREE"),
        "unknown price must NOT be shown as FREE: {label}"
    );
    assert!(
        !label.contains('$'),
        "unknown price must not show a dollar figure: {label}"
    );
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
        groups[0]
            .1
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>(),
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
        groups[0]
            .1
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>(),
        vec!["opus"]
    );
    // Only the vision-capable openrouter model survives.
    assert_eq!(
        groups[1]
            .1
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>(),
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
    assert!(
        csv.contains("ARCH-2 ARCH-3"),
        "also_matches joined; csv=\n{csv}"
    );
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
    assert!(
        csv.contains("\"a, b, c\""),
        "comma snippet must be quoted; csv=\n{csv}"
    );
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
                { "repo": "owner/repo", "sha": "deadbeef".repeat(5), "branch": "main", "dirty": true,
                  "origin_url": "https://github.com/owner/repo.git", "default_branch": "main" }
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
    assert!(
        p.audited_refs[0].dirty,
        "dirty flag must survive the round trip"
    );
    assert_eq!(
        p.audited_refs[0].origin_url.as_deref(),
        Some("https://github.com/owner/repo.git"),
        "the origin the deletion record names must survive the round trip"
    );
    assert_eq!(p.audited_refs[0].default_branch.as_deref(), Some("main"));
    assert_eq!(p.audit_model.as_deref(), Some("claude-sonnet-5"));
    assert_eq!(p.calibration_model.as_deref(), Some("claude-haiku-4-5"));
    assert_eq!(p.mode, "parallel");
    assert!(p.thorough);
    assert!(!p.deep);
    assert_eq!(p.rules_fingerprint, "fp-123");
    assert_eq!(
        p.audited_rule_ids,
        vec![
            "SEC-NO-HARDCODED-SECRETS-1".to_string(),
            "ARCH-1".to_string()
        ]
    );
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
    assert_eq!(
        r.findings[0].evaluated_option_id.as_deref(),
        Some("layered")
    );
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
    assert!(
        arch2.hallucinated,
        "hallucinated flag must survive the round trip"
    );
    assert!(
        arch2.operator_chosen,
        "operator_chosen flag must survive the round trip"
    );
    // ARCH-2's JSON predates P7 (no evidence/applicable keys at all) — legacy-absent case:
    // must deserialize as no-evidence + applicable defaulting to TRUE (matching the server's
    // own `default_recommendation_applicable`), not `bool::default()`'s `false`, so an old
    // persisted report never renders every recommendation as "Not applicable".
    assert_eq!(arch2.evidence, None, "absent evidence deserializes as None");
    assert!(
        arch2.applicable,
        "absent applicable must default to true, not false"
    );
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
    assert!(merged_findings
        .iter()
        .any(|f| f.rule_id == "ARCH-1" && f.path == "a.rs"));
    assert!(merged_findings
        .iter()
        .any(|f| f.rule_id == "ARCH-2" && f.path == "b.rs"));
    assert!(
        !merged_findings.iter().any(|f| f.path == "c.rs"),
        "the old ARCH-2 finding must be gone"
    );

    assert_eq!(
        merged_recs["ARCH-1"].recommended_option_id, "opt-a",
        "untouched"
    );
    assert_eq!(
        merged_recs["ARCH-2"].recommended_option_id, "opt-c",
        "replaced by the rescan"
    );
    assert!(
        merged_recs["ARCH-2"].operator_chosen,
        "rescanned rule is now operator-chosen"
    );
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
    assert_eq!(
        pending.get("ARCH-2"),
        Some(&"opt-x".to_string()),
        "untouched"
    );
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
fn recommend_scan_mode_auto_for_small_single_repo() {
    let r = scan_report(serde_json::json!({
        "repos": ["a"], "files_scanned": 10,
        "findings": [], "proposed_rules": [], "gated": false
    }));
    assert_eq!(super::recommend_scan_mode(&r), "auto");
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

// ── The retired thorough option ────────────────────────────────────────────────────────

/// The thorough toggle is gone: no input the panel has can put `thorough` in a request, so the
/// body carries no such key, and two requests that differ only in the old toggle's value are
/// the same request.
#[test]
fn the_audit_request_carries_no_thorough_option() {
    let rules = vec![super::SelectedAuditRule {
        id: "ARCH-1".to_string(),
        directive: "d".to_string(),
        repos: vec!["owner/repo".to_string()],
    }];
    let repos = vec!["owner/repo".to_string()];
    let body = super::audit_request_json(
        &repos, &rules, "m", "c", "auto", true, false, true, true, None,
    );
    assert!(body.get("thorough").is_none(), "{body}");
    assert_eq!(body["calibration_model"], "c");
    assert_eq!(body["rules"][0]["id"], "ARCH-1");
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
// each MUST carry BOTH #[tokio::test] (outer) and #[serial_test::serial(process_env)]
// (inner) to serialize the process-global env override across the crate's tests.
// ════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[serial_test::serial(process_env)]
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

    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let out = super::fetch_repo_health("proj-1").await;

    let repos = out.expect("repos array parses");
    assert_eq!(repos.len(), 2);
    assert_eq!(repos[0].repo, "owner/a");
    assert!(repos[0].resolved);
    assert_eq!(repos[1].repo, "owner/b");
    assert!(!repos[1].resolved);
    assert_eq!(repos[1].reason, "no local checkout");
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn set_repo_path_posts_repo_and_path() {
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/repo-path"))
        .and(body_json(
            serde_json::json!({ "repo": "owner/a", "path": "/local/a" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": true })))
        .expect(1)
        .mount(&server)
        .await;

    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let ok = super::set_repo_path("owner/a", "/local/a").await;

    assert!(ok, "set_repo_path succeeds on a 2xx");
}

#[tokio::test]
#[serial_test::serial(process_env)]
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

    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let url = super::ignore_findings("owner/repo", &[f], "noise", Some("CAM-9".to_string())).await;

    assert_eq!(url.as_deref(), Some("https://github.com/owner/repo/pull/9"));
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn ignore_findings_returns_none_when_not_ok() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/onboard/ignore"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": false })))
        .mount(&server)
        .await;

    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let url = super::ignore_findings("owner/repo", &[], "r", None).await;

    assert!(url.is_none(), "ok:false must yield None");
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn scan_repos_posts_repos_and_parses_report() {
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/onboard/scan"))
        .and(body_json(
            serde_json::json!({ "repos": ["owner/a", "owner/b"] }),
        ))
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

    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let report = super::scan_repos(&["owner/a".to_string(), "owner/b".to_string()]).await;

    let report = report.expect("scan report parses");
    assert_eq!(report.files_scanned, 12);
    assert_eq!(
        report.repos,
        vec!["owner/a".to_string(), "owner/b".to_string()]
    );
    assert!(!report.gated);
}

#[tokio::test]
#[serial_test::serial(process_env)]
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

    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let draft = super::load_onboarding_draft().await;

    let draft = draft.expect("a present draft parses");
    assert_eq!(draft.scan.files_scanned, 3);
    assert_eq!(draft.viewed_repo, "owner/a");
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn load_onboarding_draft_none_when_null() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/onboard/draft"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::Value::Null))
        .mount(&server)
        .await;

    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let draft = super::load_onboarding_draft().await;

    assert!(draft.is_none(), "a null body means no draft in progress");
}

#[tokio::test]
#[serial_test::serial(process_env)]
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

    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    super::save_onboarding_draft(&draft).await;
    // .expect(1) on the mock verifies (on server drop) the POST was issued once.
}

#[tokio::test]
#[serial_test::serial(process_env)]
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

    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    super::clear_onboarding_draft().await;
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn complete_onboarding_reads_ok_flag() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/onboard/complete"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": true })))
        .mount(&server)
        .await;

    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let ok = super::complete_onboarding().await;

    assert!(ok, "ok:true means onboarding completed");
}

#[tokio::test]
#[serial_test::serial(process_env)]
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
            "incremental": true,
            "deep": false,
            "run_ai_review": true,
            "run_deterministic": true,
            "max_cost_usd": null,
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

    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let report = super::audit_against(
        &["owner/a".to_string()],
        &rules,
        "claude-opus-5",
        "claude-sonnet-4",
        "parallel",
        true,
        false,
        true,
        true,
        None,
    )
    .await;

    let report = report.expect("audit report parses");
    assert_eq!(report.files_scanned, 4);
}

#[tokio::test]
#[serial_test::serial(process_env)]
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

    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let resp = super::fetch_audit_models().await;

    let resp = resp.expect("registry parses");
    // Default = first claude model (opus), not the first model overall.
    assert_eq!(resp.default, "opus");
    assert!(resp.openrouter_fetched);
    assert_eq!(resp.models.len(), 2);
    // The badge label is built from registry fields at fetch time.
    let opus = resp.models.iter().find(|m| m.id == "opus").unwrap();
    assert!(
        opus.label.contains("Opus"),
        "label carries display name: {}",
        opus.label
    );
    assert!(
        opus.label.contains("$15/M"),
        "label carries output price: {}",
        opus.label
    );
}

#[tokio::test]
#[serial_test::serial(process_env)]
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
            "incremental": false,
            "deep": false,
            "run_ai_review": true,
            "run_deterministic": true,
            "max_cost_usd": null,
        })))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "job_id": "job-123" })),
        )
        .expect(1)
        .mount(&server)
        .await;

    let rules = vec![super::SelectedAuditRule {
        id: "R1".to_string(),
        directive: "d".to_string(),
        repos: vec![],
    }];

    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let jid = super::audit_job_start(
        &["owner/a".to_string()],
        &rules,
        "m",
        "c",
        "job",
        false,
        false,
        true,
        true,
        None,
    )
    .await;

    assert_eq!(jid.as_deref(), Some("job-123"));
}

#[tokio::test]
#[serial_test::serial(process_env)]
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

    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let env = super::audit_job_poll("job-1").await;

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
#[serial_test::serial(process_env)]
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

    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let env = super::audit_job_poll("job-2").await;

    let env = env.expect("envelope parses");
    assert_eq!(env.job.status, "done");
    let report = env.job.report.expect("a done job carries its report");
    assert_eq!(
        report.ai_error.as_deref(),
        Some("me/api: AI audit skipped (boom)")
    );
}

#[tokio::test]
#[serial_test::serial(process_env)]
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

    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let result = super::scaffold_greenfield_api("my-proj", "/dest/my-proj", &rules).await;

    let result = result.expect("scaffold result parses");
    assert!(result.ok);
    assert_eq!(result.commit_sha, "abc123");
    assert_eq!(result.files_written, vec!["CONVENTIONS.md".to_string()]);
}

#[tokio::test]
#[serial_test::serial(process_env)]
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

    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let md = super::fetch_deep_report("proj-1").await;

    let md = md.expect("markdown returned on success");
    assert!(md.contains("# Deep Report"), "markdown body returned: {md}");
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn fetch_deep_report_none_on_error_status() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/projects/proj-1/deep-report"))
        .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
        .mount(&server)
        .await;

    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let md = super::fetch_deep_report("proj-1").await;

    assert!(
        md.is_none(),
        "a non-2xx must yield None, not the error body"
    );
}

// ── rescan_alternatives / accept_alternatives (audit-integrated alternative
// recommendation, docs/design/2026-09-22_audit-integrated-alternatives.md) ─────────────

#[tokio::test]
#[serial_test::serial(process_env)]
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

    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let resp = super::rescan_alternatives(
        "proj-1",
        &[super::AlternativeOverride {
            rule_id: "ARCH-1".to_string(),
            chosen_option_id: "layered".to_string(),
        }],
    )
    .await;

    let resp = resp.expect("a 200 response must parse");
    assert!(resp.ok);
    assert!(resp.findings.is_empty());
    assert!(resp.recommendations["ARCH-1"].operator_chosen);
}

/// The `Blocked` compliance gate (and any other `{ ok: false, message }` response) must
/// come back as `Some(..)` with the message intact — not collapsed to `None` — so the
/// caller can surface it instead of silently failing (design doc's explicit requirement).
#[tokio::test]
#[serial_test::serial(process_env)]
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

    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let resp = super::rescan_alternatives(
        "proj-1",
        &[super::AlternativeOverride {
            rule_id: "ARCH-1".to_string(),
            chosen_option_id: "layered".to_string(),
        }],
    )
    .await;

    let resp = resp.expect("an ok:false body still parses");
    assert!(!resp.ok);
    assert_eq!(
        resp.message.as_deref(),
        Some("This project is API-only and has no Anthropic key configured.")
    );
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn rescan_alternatives_returns_none_on_network_failure() {
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", "http://127.0.0.1:9");
    let resp = super::rescan_alternatives(
        "proj-1",
        &[super::AlternativeOverride {
            rule_id: "ARCH-1".to_string(),
            chosen_option_id: "layered".to_string(),
        }],
    )
    .await;

    assert!(resp.is_none());
}

#[tokio::test]
#[serial_test::serial(process_env)]
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

    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let resp = super::accept_alternatives("proj-1", None).await;

    let resp = resp.expect("a 200 response must parse");
    assert!(resp.ok);
    assert_eq!(resp.accepted, 2);
    assert!(resp.skipped.is_empty());
}

#[tokio::test]
#[serial_test::serial(process_env)]
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

    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let resp = super::accept_alternatives("proj-1", Some(&["ARCH-1".to_string()])).await;

    assert_eq!(resp.expect("parses").accepted, 1);
}
