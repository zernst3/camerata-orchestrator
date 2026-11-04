//! The export invariants gate: the standing invariants every export artifact must hold.

// ════════════════════════════════════════════════════════════════════════════════════════
// C5-3: the consolidated export-invariants regression gate
// ════════════════════════════════════════════════════════════════════════════════════════
//
// The last several cycles (W3/W4, R1/R2, C3-6, C4-P2/P3/P4, C5-1, REG-5 — see `git log
// --oneline` on this file for the full list) each fixed ONE cross-cutting export defect, then
// added ONE narrowly-scoped regression test for it, usually in `mod tests` above or in
// `xlsx_export`'s own test module. Each test is real and still passes, but there has never
// been a SINGLE place that builds one representative report and checks the whole FAMILY of
// invariants together — so a change that re-breaks one of them (a new code path that forgets
// to call `client_headline_and_detail`, a new bucket gate that skips `effective_bucket`, a new
// text field that skips `sanitize_report_findings`) only fails loudly if it happens to also
// break one of the narrow fixture-specific tests. This module is that single place: ONE
// synthetic report (`build_fixture`, below) run through the REAL `build_report_json` /
// `xlsx_export::build_findings_export` / `xlsx_export::build_workbook` pipeline, then a set of
// named, independent `#[tokio::test]` functions — one invariant (or one closely related pair)
// per function — so a future regression in ANY of them fails here first, by name, rather than
// waiting to be independently rediscovered.
//
// The twelve invariants this gate owns (add new ones here as future cycles close more gaps —
// this doc comment is the canonical index, not the individual test doc comments):
//
//  1. No client-facing field (headline/detail/snippet/fix) in the JSON or the xlsx begins with
//     an imperative gate-verb (`^(Deny|Require|Disallow|Forbid|Enforce)\b`) — the internal
//     gate's own enforcement prose must always be rewritten before it reaches a client. Traces
//     to W3 (2026-09-30) / C4-P2 (`client_headline_and_detail`) / C4-P4 defect 1.
//     STANDING (passes): `gate_verb_directives_never_leak_into_any_client_facing_field`.
//  2. No field carries a zero-width/BOM codepoint (U+200B, U+200C, U+200D, U+FEFF). Traces to
//     C4-R3 (`sanitize_report_findings`/`strip_zero_width`).
//     STANDING (passes): `zero_width_and_bom_codepoints_never_survive_into_any_export_field`.
//  3. No field contains the identifier-spacing artifact (`[A-Za-z0-9]\. [a-z0-9]` outside a
//     genuine sentence boundary or a known abbreviation) — a dotted call, filename, version
//     string, or abbreviation must survive untouched. Traces to REG-5 (2026-10-02).
//     STANDING (passes): `identifier_heavy_text_never_gets_a_period_space_corruption_artifact`.
//  4. No headline contains a raw rule-id token (`[A-Z]+(-[A-Z0-9]+){2,}`); an absorbed
//     `also_matches` member renders as its corpus TITLE (or not at all), never its bare id.
//     Traces to C4-P4 defect 2 (`also_matches_titles`) / C5-1 (deterministic merge primacy).
//     STANDING (passes): `also_matches_renders_titles_never_raw_rule_id_tokens`.
//  5. Hedge single source of truth: `needs_review ⇔ confidence == "needs-review"` for every
//     row (biconditional — PASSES, by construction of `is_hedged`/`hedge_confidence`, C4-P2);
//     the executive summary's "held for review" count equals the total number of
//     `needs_review` rows. C5-4 (this cycle, second attempt — cycle-4's P2 regressed): the
//     root cause was that `is_informational`'s hard "critical/high is never informational" rule
//     was the ONLY gate a `needs-review` hedge went through, so a high/critical hedge stayed in
//     `do_now`/`do_next` and was never counted. Fixed by splitting the hedge signal into its
//     own gate (`is_held_for_review`) and its own bucket (`matrix.held`, severity-unbounded),
//     checked BEFORE `is_informational` in `effective_bucket`; the executive summary's
//     `held_for_review` is now `matrix.informational.len() + matrix.held.len()`. STANDING (both
//     halves now pass): `hedge_confidence_is_a_strict_biconditional_on_every_row` and
//     `held_for_review_count_equals_total_needs_review_count`.
//  6. Bucket invariant: a row with `bucket ∈ {informational}` has `severity <= medium`. CLOSED
//     (C5-7, was OPEN/`#[ignore]`d as C5-5): `is_uncited_ai_finding` (P3) is deliberately
//     severity-blind (see that function's own doc comment: "a critical, uncited finding is
//     PRECISELY the case this gate exists to catch"), so a high/critical uncited finding used
//     to be shoved into `informational` anyway, with its real (high/critical) severity shown —
//     the one documented exception to this invariant, and the ignored test pinned it as a
//     known gap rather than silently passing. Fixed by giving that signal its OWN destination
//     in `effective_bucket`: a high/critical uncited finding now routes to `matrix.held` (the
//     same severity-unbounded "a human must decide" bucket [`is_held_for_review`] uses) instead
//     of `informational`; only a medium/low/info uncited finding still lands in `informational`,
//     where the severity cap was always meant to apply. STANDING (no exception remains):
//     `informational_bucket_severity_is_bounded_with_no_exceptions`.
//  7. Provenance equals the tier of the row's OWN rule; a merged row's citation is its PRIMARY
//     site's own resolved citation, never an absorbed member's. Traces to C4-P2
//     (`citation_for_finding` called per-site, not per-group-first-site-only in error) / C3-6.
//     STANDING (passes): `provenance_and_citation_belong_to_the_rows_own_rule_never_an_absorbed_member`.
//  8. Coverage: a rule that appears ONLY as a merged `also_matches` member still counts as
//     matched — never listed in "what's healthy" / a category's clean-rule count. Traces to
//     C5-1 (2026-10-02, `rule_ids_with_any_finding` / `matched_rule_ids_by_category`).
//     STANDING (passes): `a_rule_absorbed_only_via_also_matches_counts_as_matched`.
//  9. Cover/scorecard severity totals equal the counts over the RENDERED (non-FP, non-
//     dependency) row set — both are sums over the exact same `code_findings` partition by
//     construction. STANDING (passes): `cover_and_scorecard_severity_totals_match_the_rendered_row_set`.
//  10. No fix field contains an unfilled template artifact: a literal `{`, an un-substituted
//      `<token>` span, or `TODO`. STANDING (passes):
//      `fix_fields_never_contain_raw_template_syntax`. Separately (OPEN, C5-6 — a product-
//      polish gap, not a correctness bug): the GENERIC placeholder filler
//      (`generic_placeholder_filler`) legitimately renders "the affected table"/"the affected
//      file"/etc. when a detector captured no concrete object name, which reads as boilerplate
//      next to an otherwise-specific fix: `fix_fields_can_fall_back_to_the_generic_affected_noun_phrase`.
//  11. Every curated (action-bucket) row for a rule with AUTHORED remediation has a non-empty
//      `fix` and a non-empty hour estimate — never a silently missing Fix block for a rule this
//      corpus actually authors one for. (This is deliberately scoped to AUTHORED rules — a
//      rule with no corpus entry at all legitimately renders `fix: None`; see
//      `curated_finding_site_fix_is_none_when_remediation_is_unauthored` above, which pins that
//      as the accepted exception, not a regression.) STANDING (passes):
//      `curated_rows_for_authored_floor_rules_have_a_non_empty_fix_and_estimate`.
//  12. Pipeline-integrity ledger: a representative synthetic scan run through BOTH row-reducing
//      merge stages (`ai_audit::group_semantic` — "cross-family-merge" — and
//      `ai_audit::group_structural_needs_review` — "structural-needs-review-merge"), accounted
//      for via `onboard::account_location_merge`, produces ZERO unaccounted rows at EVERY
//      recorded stage. Traces to the real-world "cross-family-merge rows 45 -> 32 merged=8 ...
//      unaccounted=5" defect: the OLD instrumentation diffed `also_matches` (a deduplicated
//      rule-id SET) instead of `also_locations` (one entry per absorbed FINDING), so an
//      absorption whose rule id collided with the primary's — the common "N occurrences of one
//      rule" shape — vanished from the ledger with no recorded disposition even though its
//      evidence site survived. STANDING (passes):
//      `pipeline_integrity_ledger_has_zero_unaccounted_rows_across_every_merge_stage`.
//  13. (2026-10-07 grading pass, defect 1) Client-facing "no wired detector" disclosure is ONE
//      plain sentence naming a count, never a per-rule bullet list in internal vocabulary
//      (no "arch_checker registry"/"gateway rule registry"/"Semgrep mapping" phrases, no raw
//      rule ids) — regardless of how many rules the ledger excludes for that reason. STANDING
//      (passes): `many_no_wired_detector_exclusions_collapse_to_one_plain_sentence`.
//  14. (defect 2) A row with `Finding.status == "suppressed-baseline"` and `preview == true`
//      (the scan-time preview/external-tool shape — no real baseline record can ever back it;
//      see `classify`'s doc comment) NEVER classifies as `Disposition::BaselineAccepted` and
//      NEVER renders "Pre-existing accepted debt" / "accepted in a prior run" anywhere in the
//      exported report, no matter how many such rows exist. STANDING (passes):
//      `tests::a_preview_finding_with_suppressed_baseline_status_is_never_baseline_accepted`.
//  15. (defect 3) A preview/external-tool row's `xlsx_export` provenance column always names
//      its tool ("Preview: {tool}"), even when its citation independently resolves to
//      `"grounded"` via the P3/commodity-taint grounding map — provenance and citation-kind
//      are different axes, and only the citation axis is allowed to say "grounded"/cite a
//      standard; the provenance axis never says "Deterministic" for a row this scan's own
//      preview pass produced. STANDING (passes), in `xlsx_export::tests` (this invariant is
//      about that module's own provenance computation, not this file's):
//      `a_grounded_preview_row_never_renders_deterministic_provenance`.
//  16. (defect 4) A zero-finding rule whose own `RuleRecommendation::evidence` text negates the
//      rule's requirement (a conservative negation-phrase match) is NEVER listed under "what's
//      healthy" — it is held for review instead, with its own advisory finding anchored at the
//      evidence's citation. The positive twin (evidence CONFIRMS compliance, no negation
//      phrase) stays healthy. STANDING (passes):
//      `evidence_negating_rule_requirement_is_held_not_healthy_positive_twin_stays_healthy`.

use super::*;
use crate::onboard::{AuditedRef, LanguageVolume, ScanProvenance};
use crate::xlsx_export;

// ── Fixture builders (own copies, matching the convention every test module in this
// crate already follows — `report_export::tests` and `xlsx_export::tests` each keep their
// own, never importing a sibling module's private helpers) ──────────────────────────────

fn finding(rule_id: &str, repo: &str, path: &str, line: usize, severity: &str) -> Finding {
    Finding {
        repo: repo.to_string(),
        path: path.to_string(),
        line,
        rule_id: rule_id.to_string(),
        severity: severity.to_string(),
        snippet: format!("snippet-{line}"),
        detail: format!("detail for {rule_id}"),
        ..Finding::default()
    }
}

fn report_with(findings: Vec<Finding>, audited_rule_ids: Vec<&str>) -> ScanReport {
    ScanReport {
        run_id: None,
        tree: None,
        discarded: Vec::new(),
        repos_not_inspected: Vec::new(),
        recommendations_by_repo: Vec::new(),
        patterns_across_services: Default::default(),
        volume_by_repo: Default::default(),
        repos: vec!["demo/portal".to_string()],
        stacks: Vec::new(),
        files_scanned: 40,
        test_file_count: 5,
        files_excluded: 3,
        excluded_noise_lines: None,
        code_chars: 20_000,
        code_lines: 1_200,
        code_lines_by_language: vec![LanguageVolume {
            language: "TypeScript".to_string(),
            lines: 1_200,
        }],
        excluded_mechanical_rules: Vec::new(),
        findings,
        proposed_rules: Vec::new(),
        gated: false,
        blocked: false,
        ai_blocked_reason: None,
        ai_error: None,
        message: None,
        actual_usage: None,
        deep: None,
        coverage_notes: Vec::new(),
        provenance: ScanProvenance {
            group_review_model: None,
            audited_refs: vec![AuditedRef {
                repo: "demo/portal".to_string(),
                sha: Some("1234567890abcdef".to_string()),
                branch: Some("main".to_string()),
                dirty: false,
                ..AuditedRef::default()
            }],
            audit_model: Some("test-model".to_string()),
            calibration_model: Some("test-cal-model".to_string()),
            clone_judge_model: None,
            mode: "parallel".to_string(),
            thorough: false,
            deep: false,
            rules_fingerprint: "fp".to_string(),
            audited_rule_ids: audited_rule_ids.into_iter().map(String::from).collect(),
            camerata_version: "0.0.0-test".to_string(),
            osv_scanner_version: Some("1.9.0".to_string()),
            vulnerability_data: None,
            started_at: "2026-10-02T00:00:00Z".to_string(),
            finished_at: "2026-10-02T00:05:00Z".to_string(),
            model_sessions: Vec::new(),
        },
        recommendations: std::collections::HashMap::new(),
        failed_passes: Vec::new(),
        ledger: crate::scan_ledger::ScanLedger::new(),
    }
}

fn empty_opts() -> ReportOptions {
    ReportOptions::default()
}

/// One built-and-exported representative report, shared verbatim by every invariant test
/// below (each test calls [`build_fixture`] itself — corpus load is a local TOML-file walk,
/// cheap enough that re-running it per test is simpler and safer than a shared `OnceCell`
/// racing `cargo test`'s parallel test threads over a mutable fixture).
struct Fixture {
    json: AuditReportJson,
    json_value: serde_json::Value,
    findings_export_value: serde_json::Value,
    workbook: Vec<u8>,
}

/// Build the ONE synthetic report this whole gate shares. Six findings, chosen to cover
/// every scenario the module doc comment's invariant list needs a live example of:
///
/// - `SEC-NO-HARDCODED-SECRETS-1` ("f_secret"): a real, AUTHORED floor rule (critical,
///   deterministic) whose raw `detail` deliberately opens with "Deny" — the gate's own
///   internal directive voice — to prove invariant 1's rewrite. Its `also_matches` carries
///   BOTH an uncorroborated invented AI-tier id (no corpus entry — must be silently
///   dropped, never leaked as a raw id) and a SECOND real floor rule id
///   (`ARCH-NO-SECRETS-IN-URL-1`, deliberately given NO Finding row of its own) — the
///   "deterministic + AI pair merged at one site" case, and the vehicle for invariants 4, 7,
///   and 8.
/// - `SUPABASE-RLS-ENABLED-1` ("f_rls"): a second real, authored floor rule, deliberately
///   given NO `captures`, so its authored `remediation`'s `<table>` token falls through to
///   the generic filler ("the affected table") — the vehicle for invariant 10's two halves.
/// - `ARCH-CUSTOM-FLAGGED-1` ("f_hedge"): a high-severity, non-AI-tier finding the
///   calibrator flagged `needs_review` — C5-4: routes to the `held` bucket (NOT `do_now`;
///   high/critical is never auto-informational, but a hedge is held at ANY severity) while
///   still exporting `needs_review: true` and keeping its real `high` severity — the vehicle
///   for invariant 5's reconciliation count.
/// - `ARCH-STYLE-NOTE-1` ("f_informational"): a plain `info`-severity note — the "ordinary"
///   held-for-review row (no confidence hedge at all; routed on severity alone).
/// - `ARCH-CUSTOM-IDENTIFIER-1` ("f_identifier"): run through the REAL
///   `ai_audit::group_semantic` pass (a singleton group — nothing to merge WITH, but
///   this is exactly where REG-5 lived: the cross-reference-stripping pass that runs over
///   every finding regardless of group size) — detail text packed with a dotted call, a
///   filename, a migration-line reference, and an abbreviation, to exercise invariant 3.
/// - `AI-STALE-SESSION-INVALIDATION-1` ("f_ai_advisory"): a standalone, high-severity,
///   genuinely uncited AI-tier finding (no RLS/XSS/open-redirect/weak-token/query-
///   injection/CORS vocabulary anywhere in its id or detail, so `classify_ai_finding` can't
///   ground it) — the vehicle for invariant 6 (C5-7: routes to `matrix.held`, at its real
///   `high` severity, never `informational`) and invariant 7's "AI-advisory" provenance
///   check.
async fn build_fixture() -> Fixture {
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(
        errors.is_empty(),
        "the bundled rule corpus must load cleanly for this gate to mean anything: {errors:?}"
    );

    let mut f_secret = finding(
        "SEC-NO-HARDCODED-SECRETS-1",
        "demo/portal",
        "apps/api/src/config.rs",
        10,
        "critical",
    );
    f_secret.snippet = "const API_KEY = \"abcdef1234567890ab\";".to_string();
    f_secret.detail = "Deny committing this credential literal to version control: the \
                            config.rs file ships a live-looking API key that grants full \
                            billing access."
        .to_string();
    f_secret.also_matches = vec![
        "AI-WEAK-AUTH-SESSION-1".to_string(),
        "ARCH-NO-SECRETS-IN-URL-1".to_string(),
    ];

    let mut f_rls = finding(
        "SUPABASE-RLS-ENABLED-1",
        "demo/portal",
        "supabase/migrations/0001_init.sql",
        5,
        "critical",
    );
    f_rls.snippet = "create table public.payments (\n  id uuid primary key\n);".to_string();
    f_rls.detail = "The payments table has no Row Level Security: the anon key can read \
                         and write every row."
        .to_string();
    // Deliberately no `captures` entry for "table" — exercises the generic-filler fallback
    // in the authored `remediation`'s `<table>` token (invariant 10).

    let mut f_hedge = finding(
        "ARCH-CUSTOM-FLAGGED-1",
        "demo/portal",
        "apps/web/src/session.ts",
        22,
        "high",
    );
    f_hedge.snippet = "renewSessionToken(req);".to_string();
    f_hedge.detail = "Session renewal does not independently re-check the caller's role; \
                           whether an unauthenticated request can reach it depends on \
                           middleware this scan cannot fully trace."
        .to_string();
    f_hedge.needs_review = true;
    f_hedge.effort = Some("low".to_string());

    let mut f_informational = finding(
        "ARCH-STYLE-NOTE-1",
        "demo/portal",
        "apps/web/src/utils/formatDate.ts",
        3,
        "info",
    );
    f_informational.snippet = "export function formatDate(d: Date) { /* ... */ }".to_string();
    f_informational.detail = "This date-formatting helper duplicates logic already present \
                                   in the shared date utility module; consider consolidating."
        .to_string();

    let mut f_identifier = finding(
        "ARCH-CUSTOM-IDENTIFIER-1",
        "demo/portal",
        "apps/api/src/invoices.ts",
        42,
        "medium",
    );
    f_identifier.snippet = "const total = client.billing.computeTotal(invoice);".to_string();
    f_identifier.detail = "The call to client.billing.computeTotal() in \
                                config/settings.toml bypasses the discount check. See \
                                0002_pricing_migration.sql:7 for the original schema, e.g. the \
                                v2.4.1 release notes document the same gap. Anyone with the \
                                anon key can read every invoice total."
        .to_string();
    f_identifier.effort = Some("medium".to_string());
    // REG-5 lived in `group_semantic`'s cross-reference-stripping pass, which runs
    // over EVERY finding regardless of group size — route this one through the real
    // function (a singleton group) rather than hand-simulating its output.
    let f_identifier = crate::review_tree::test_support::cluster_heads(vec![f_identifier], &[])
        .into_iter()
        .next()
        .expect("a singleton merge group must still return exactly one finding");

    let mut f_ai_advisory = finding(
        "AI-STALE-SESSION-INVALIDATION-1",
        "demo/portal",
        "apps/api/src/auth/password.ts",
        58,
        "high",
    );
    f_ai_advisory.snippet = "await db.users.update({ id }, { passwordHash });".to_string();
    f_ai_advisory.detail = "Existing sessions are not invalidated when a user changes their \
                                 password, so a previously stolen session token stays valid \
                                 indefinitely even after a successful password reset."
        .to_string();
    f_ai_advisory.confidence = Some("high".to_string());

    let report = report_with(
        vec![
            f_secret,
            f_rls,
            f_hedge,
            f_informational,
            f_identifier,
            f_ai_advisory,
        ],
        vec![
            "SEC-NO-HARDCODED-SECRETS-1",
            "SUPABASE-RLS-ENABLED-1",
            "ARCH-NO-SECRETS-IN-URL-1",
        ],
    );

    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );
    let findings_export = xlsx_export::build_findings_export(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        Some(&corpus),
        &json,
        &HashMap::new(),
    );
    let workbook = xlsx_export::build_workbook(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    )
    .expect("the workbook must build for the representative synthetic report");

    Fixture {
        json_value: serde_json::to_value(&json).expect("AuditReportJson must serialize"),
        findings_export_value: serde_json::to_value(&findings_export)
            .expect("FindingsExport must serialize"),
        json,
        workbook,
    }
}

// ── Shared text-field crawlers (same explicit-per-shape approach REG-5's own gate uses —
// a generic recursive walk would also pick up static narrative/methodology prose and
// `rule_id`-labeled fields that legitimately carry raw ids, both of which would make
// several of these checks false-positive) ─────────────────────────────────────────────

/// Every client-facing per-finding text field across BOTH artifact shapes: the PDF/JSON's
/// `AuditReportJson` (matrix headlines, curated/held-for-review site prose, three-things,
/// the priority grid) and `findings.json`/the xlsx's `FindingRow` set. Returns
/// `(location-label, text)` pairs for debuggable failure messages.
fn all_client_text_fields(fx: &Fixture) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut add = |loc: String, v: Option<&str>| {
        if let Some(s) = v {
            if !s.is_empty() {
                out.push((loc, s.to_string()));
            }
        }
    };

    for bucket in [
        "do_now",
        "do_next",
        "plan",
        "accepted",
        "informational",
        "held",
    ] {
        for item in fx.json_value["matrix"][bucket]
            .as_array()
            .into_iter()
            .flatten()
        {
            add(
                format!("matrix.{bucket}.headline"),
                item["headline"].as_str(),
            );
        }
    }
    for item in fx.json_value["three_things"]["items"]
        .as_array()
        .into_iter()
        .flatten()
    {
        add(
            "three_things.headline".to_string(),
            item["headline"].as_str(),
        );
    }
    for row in fx.json_value["priority_grid"]["rows"]
        .as_array()
        .into_iter()
        .flatten()
    {
        for cell in row["cells"].as_array().into_iter().flatten() {
            for tag in cell["findings"].as_array().into_iter().flatten() {
                add(
                    "priority_grid.headline".to_string(),
                    tag["headline"].as_str(),
                );
            }
        }
    }
    for group_key in ["curated_findings", "held_for_review_findings"] {
        for group in fx.json_value[group_key].as_array().into_iter().flatten() {
            for site in group["sites"].as_array().into_iter().flatten() {
                add(format!("{group_key}.headline"), site["headline"].as_str());
                add(format!("{group_key}.detail"), site["detail"].as_str());
                add(format!("{group_key}.snippet"), site["snippet"].as_str());
                add(format!("{group_key}.fix"), site["fix"].as_str());
                add(
                    format!("{group_key}.fix_for_this_finding"),
                    site["fix_for_this_finding"].as_str(),
                );
            }
        }
    }
    for row in fx.findings_export_value["findings"]
        .as_array()
        .into_iter()
        .flatten()
    {
        add(
            "findings_export.headline".to_string(),
            row["headline"].as_str(),
        );
        add("findings_export.detail".to_string(), row["detail"].as_str());
        add(
            "findings_export.snippet".to_string(),
            row["snippet"].as_str(),
        );
        add("findings_export.fix".to_string(), row["fix"].as_str());
        add(
            "findings_export.fix_specific".to_string(),
            row["fix_specific"].as_str(),
        );
    }
    out
}

/// Read `xl/sharedStrings.xml` out of a built workbook's zip — the shared-string pool every
/// non-numeric cell's text lives in (see `xlsx_export::tests::cell_text`'s identical
/// approach for a single-cell lookup; this gate wants EVERY string in the workbook, not one
/// cell, so it skips the `<c r="...">` indirection entirely).
fn workbook_shared_strings_xml(bytes: &[u8]) -> String {
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("workbook must be a valid zip");
    let mut file = archive
        .by_name("xl/sharedStrings.xml")
        .expect("workbook must have a shared-strings table");
    let mut s = String::new();
    std::io::Read::read_to_string(&mut file, &mut s).expect("shared strings must be UTF-8");
    s
}

/// Every `<t>...</t>` run's decoded text out of a shared-strings XML blob — every write
/// call in `xlsx_export` goes through `write_string_with_format`/`write_string`, which
/// `rust_xlsxwriter` always emits as a single plain `<t>` run (never rich-text runs), so
/// this simple regex extraction is exhaustive for this workbook.
fn workbook_shared_string_texts(xml: &str) -> Vec<String> {
    let re = regex::Regex::new(r"(?s)<t[^>]*>(.*?)</t>").expect("static regex must compile");
    re.captures_iter(xml)
        .map(|c| {
            c[1].replace("&amp;", "&")
                .replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&apos;", "'")
                .replace("&quot;", "\"")
        })
        .collect()
}

// ── Invariant 1 (STANDING) ──────────────────────────────────────────────────────────

#[tokio::test]
async fn gate_verb_directives_never_leak_into_any_client_facing_field() {
    let fx = build_fixture().await;
    let gate_verb = regex::Regex::new(r"^(Deny|Require|Disallow|Forbid|Enforce)\b")
        .expect("static regex must compile");

    let mut offenders = Vec::new();
    for (loc, text) in all_client_text_fields(&fx) {
        if gate_verb.is_match(&text) {
            offenders.push(format!("{loc}: {text:?}"));
        }
    }
    for s in workbook_shared_string_texts(&workbook_shared_strings_xml(&fx.workbook)) {
        if gate_verb.is_match(&s) {
            offenders.push(format!("xlsx shared string: {s:?}"));
        }
    }
    assert!(
        offenders.is_empty(),
        "a client-facing field begins with the gate's own internal enforcement verb \
             (never rewritten — see `client_headline_and_detail`): {offenders:#?}"
    );
    // Sanity check the fixture's raw "Deny ..." detail was actually REWRITTEN away rather
    // than simply never reached: the raw opener must be gone, and the rule's authored
    // template text (which `client_headline_and_detail` substitutes in its place) must be
    // present — proving the rewrite ran, not just that nothing matched the regex because
    // the fixture's own text never made it into either artifact at all.
    let combined = format!("{}{}", fx.json_value, fx.findings_export_value);
    assert!(
        !combined.contains("Deny committing this credential"),
        "fixture's raw gate-voiced detail must not survive verbatim into either artifact"
    );
    assert!(
        combined.contains("credential value hardcoded directly in the source"),
        "the rule's authored finding_headline/finding_detail template must have replaced \
             the raw detail for this gate to mean anything: {combined}"
    );
}

// ── Invariant 2 (STANDING) ──────────────────────────────────────────────────────────

#[tokio::test]
async fn zero_width_and_bom_codepoints_never_survive_into_any_export_field() {
    const ZERO_WIDTH: [char; 4] = ['\u{200B}', '\u{200C}', '\u{200D}', '\u{FEFF}'];

    // Build a SECOND fixture, deliberately contaminated, rather than threading a zero-width
    // char through the shared fixture — keeps this test's intent self-contained and avoids
    // coupling a mutation into the other ten tests' shared input.
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");

    let mut f = finding(
        "SEC-NO-HARDCODED-SECRETS-1",
        "demo/portal",
        "apps/api\u{200B}/src/config.rs",
        10,
        "critical",
    );
    f.detail = "A secret\u{FEFF} literal is hardcoded here\u{200D}.".to_string();
    f.snippet = "const API_KEY = \"abc\u{200C}def\";".to_string();
    let report = report_with(vec![f], vec!["SEC-NO-HARDCODED-SECRETS-1"]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );
    let findings_export = xlsx_export::build_findings_export(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        Some(&corpus),
        &json,
        &HashMap::new(),
    );
    let workbook = xlsx_export::build_workbook(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    )
    .expect("workbook must build");

    let json_text = serde_json::to_string(&json).expect("must serialize");
    let findings_text = serde_json::to_string(&findings_export).expect("must serialize");
    let shared_strings = workbook_shared_strings_xml(&workbook);

    for (artifact, text) in [
        ("AuditReportJson", json_text.as_str()),
        ("FindingsExport", findings_text.as_str()),
        ("xlsx sharedStrings.xml", shared_strings.as_str()),
    ] {
        for c in ZERO_WIDTH {
            assert!(
                !text.contains(c),
                "{artifact} contains a zero-width/BOM codepoint {c:?} that \
                     `sanitize_report_findings` should have stripped"
            );
        }
    }
}

// ── Invariant 3 (STANDING) ──────────────────────────────────────────────────────────

/// Mirrors `tests::find_period_space_corruption` (REG-5) exactly, duplicated rather than
/// shared across the sibling `tests`/`export_invariants_gate` modules — same convention
/// every test module in this crate already follows for its own fixture helpers.
fn find_period_space_corruption(field: &str) -> Option<&str> {
    let corruption =
        regex::Regex::new(r"[A-Za-z0-9]\. [a-z0-9]").expect("static regex must compile");
    for m in corruption.find_iter(field) {
        let period_idx = m.start() + 1;
        if !ends_with_known_abbreviation(&field[..=period_idx]) {
            return Some(m.as_str());
        }
    }
    None
}

#[tokio::test]
async fn identifier_heavy_text_never_gets_a_period_space_corruption_artifact() {
    let fx = build_fixture().await;

    let mut offenders = Vec::new();
    for (loc, text) in all_client_text_fields(&fx) {
        if let Some(m) = find_period_space_corruption(&text) {
            offenders.push(format!("{loc}: {m:?} in {text:?}"));
        }
    }
    assert!(
        offenders.is_empty(),
        "a dotted call / filename / version string / abbreviation was corrupted by a \
             wrongly-inserted space (REG-5): {offenders:#?}"
    );

    // Sanity check the fixture's identifiers actually survived verbatim — this gate means
    // nothing if the identifier-heavy finding never reached either artifact.
    let combined = format!("{}{}", fx.json_value, fx.findings_export_value);
    for needle in [
        "client.billing.computeTotal()",
        "config/settings.toml",
        "0002_pricing_migration.sql:7",
        "v2.4.1",
    ] {
        assert!(
            combined.contains(needle),
            "fixture identifier {needle:?} must survive into at least one artifact \
                 untouched for this gate to mean anything"
        );
    }
}

// ── Invariant 4 (STANDING) ──────────────────────────────────────────────────────────

#[tokio::test]
async fn also_matches_renders_titles_never_raw_rule_id_tokens() {
    let fx = build_fixture().await;
    let rule_id_token =
        regex::Regex::new(r"[A-Z]+(-[A-Z0-9]+){2,}").expect("static regex must compile");

    let mut offenders = Vec::new();
    for (loc, text) in all_client_text_fields(&fx) {
        if loc.ends_with(".headline") {
            if let Some(m) = rule_id_token.find(&text) {
                offenders.push(format!("{loc}: {:?} in {text:?}", m.as_str()));
            }
        }
        // The two absorbed ids must never appear verbatim in ANY client-facing field,
        // headline or otherwise — belt-and-suspenders on top of the generic token regex.
        for raw_id in ["AI-WEAK-AUTH-SESSION-1", "ARCH-NO-SECRETS-IN-URL-1"] {
            if text.contains(raw_id) {
                offenders.push(format!(
                    "{loc} leaks the raw absorbed id {raw_id:?}: {text:?}"
                ));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "a headline (or other client field) leaked a raw rule-id token instead of a \
             resolved title: {offenders:#?}"
    );

    // Positive check: the real, resolvable absorbed member's TITLE must actually show up
    // somewhere (silence alone doesn't prove the title resolution ran) — and the
    // unresolvable invented AI id must be the ONLY one dropped, not both.
    let group = fx
        .json
        .curated_findings
        .iter()
        .chain(fx.json.held_for_review_findings.iter())
        .find(|g| g.rule_id == "SEC-NO-HARDCODED-SECRETS-1")
        .expect("the merged finding's group must exist");
    assert_eq!(
        group.sites[0].also_matches_titles.len(),
        1,
        "exactly one of the two absorbed ids has a corpus entry (the invented AI id never \
             does): {:?}",
        group.sites[0].also_matches_titles
    );
    assert!(
        !group.sites[0].also_matches_titles[0].is_empty(),
        "the resolved title must be non-empty"
    );
}

// ── Invariant 5 ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn hedge_confidence_is_a_strict_biconditional_on_every_row() {
    let fx = build_fixture().await;
    let mut offenders = Vec::new();
    for row in fx.findings_export_value["findings"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let needs_review = row["needs_review"].as_bool().unwrap_or(false);
        let confidence = row["confidence"].as_str().unwrap_or("");
        let hedged_by_confidence = confidence == "needs-review";
        if needs_review != hedged_by_confidence {
            offenders.push(format!(
                "rule {:?} at {:?}:{}: needs_review={needs_review} confidence={confidence:?}",
                row["rule_id"], row["path"], row["line"]
            ));
        }
    }
    assert!(
        offenders.is_empty(),
        "needs_review and confidence==\"needs-review\" must be a strict biconditional on \
             every exported row (C4-P2): {offenders:#?}"
    );
}

/// C5-4 (closed, second attempt — cycle-4's P2 regressed): `is_informational`'s hard
/// "critical/high is never informational" rule used to be the ONLY gate a calibration hedge
/// went through, so a high/critical row the calibrator flagged needs-review (`f_hedge` in
/// this fixture) was hedged (`needs_review=true`) but never landed in `matrix.informational`,
/// and `held_for_review` silently undercounted the total `needs_review` rows. Fixed by
/// splitting the hedge signal into its own severity-unbounded gate/bucket
/// (`is_held_for_review` / `matrix.held`, checked ahead of `is_informational` in
/// `effective_bucket`) and reconciling `held_for_review` as
/// `matrix.informational.len() + matrix.held.len()`.
#[tokio::test]
async fn held_for_review_count_equals_total_needs_review_count() {
    let fx = build_fixture().await;
    let held_for_review = fx.json.executive_summary.held_for_review;
    let total_needs_review = fx.findings_export_value["findings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| row["needs_review"].as_bool().unwrap_or(false))
        .count();
    assert_eq!(
        held_for_review, total_needs_review,
        "the executive summary's \"held for review\" count must equal the total number of \
             needs_review rows across the whole export"
    );
}

// ── Invariant 6 (STANDING — C5-7 closed the former C5-5 open exception) ───────────────

/// C5-5 (closed by C5-7): this gate test used to be `#[ignore]`d, pinning a KNOWN
/// exception — `is_uncited_ai_finding` (P3) is deliberately severity-blind (see that
/// function's own doc comment: "a critical, uncited finding is PRECISELY the case this
/// gate exists to catch"), and `f_ai_advisory` (`high` severity, uncited) used to still
/// route to `matrix.informational`, silently breaking the "informational implies
/// low-stakes" invariant this test checks. `effective_bucket` now gives a high/critical
/// uncited finding its OWN destination (`matrix.held`, the same severity-unbounded bucket
/// [`is_held_for_review`] uses) instead of `informational`, so the invariant holds with NO
/// exception left — this test is un-ignored, and a second assertion locks in WHERE the
/// former exception actually went: `f_ai_advisory` must now be in `held`, at its real
/// `high` severity, out of both `informational` and every action bucket.
#[tokio::test]
async fn informational_bucket_severity_is_bounded_with_no_exceptions() {
    let fx = build_fixture().await;
    let mut offenders = Vec::new();
    for item in fx.json.matrix.informational.iter() {
        if severity_rank(&item.severity) < severity_rank("medium") {
            offenders.push(format!(
                "{} ({}): {}",
                item.rule_id, item.severity, item.headline
            ));
        }
    }
    assert!(
        offenders.is_empty(),
        "a bucket==informational row must have severity <= medium, with no exceptions \
             left: {offenders:#?}"
    );

    // The former exception's destination: the uncited, high-severity AI finding must now
    // be in `held`, not informational and not any action bucket, with its real severity.
    let held_ai = fx
        .json
        .matrix
        .held
        .iter()
        .find(|f| f.rule_id == "AI-STALE-SESSION-INVALIDATION-1")
        .expect("the uncited high-severity AI finding must be in matrix.held");
    assert_eq!(
        held_ai.severity, "high",
        "held never downgrades severity: {held_ai:?}"
    );
    assert!(
        !fx.json
            .matrix
            .informational
            .iter()
            .any(|f| f.rule_id == "AI-STALE-SESSION-INVALIDATION-1"),
        "the uncited AI finding must not ALSO appear in informational"
    );
    for (bucket_name, bucket) in [
        ("do_now", &fx.json.matrix.do_now),
        ("do_next", &fx.json.matrix.do_next),
        ("plan", &fx.json.matrix.plan),
        ("accepted", &fx.json.matrix.accepted),
    ] {
        assert!(
            !bucket
                .iter()
                .any(|f| f.rule_id == "AI-STALE-SESSION-INVALIDATION-1"),
            "the uncited AI finding must never land in the action bucket {bucket_name}"
        );
    }
}

// ── Invariant 7 (STANDING) ───────────────────────────────────────────────────────────

#[tokio::test]
async fn provenance_and_citation_belong_to_the_rows_own_rule_never_an_absorbed_member() {
    let fx = build_fixture().await;

    // The merged row's displayed citation must be SEC-NO-HARDCODED-SECRETS-1's own
    // (CWE-798 + OWASP secrets-management), never the absorbed ARCH-NO-SECRETS-IN-URL-1's
    // (CWE-598 / RFC 9110 §17.9).
    let group = fx
        .json
        .curated_findings
        .iter()
        .chain(fx.json.held_for_review_findings.iter())
        .find(|g| g.rule_id == "SEC-NO-HARDCODED-SECRETS-1")
        .expect("the merged finding's group must exist");
    let site = &group.sites[0];
    let urls: Vec<&str> = site
        .citation
        .sources
        .iter()
        .map(|s| s.url.as_str())
        .collect();
    assert!(
        urls.iter().any(|u| u.contains("798")),
        "the merged row's citation must be its OWN rule's (CWE-798), got: {urls:?}"
    );
    assert!(
        !urls.iter().any(|u| u.contains("598") || u.contains("9110")),
        "the merged row's citation must never be the absorbed member's (CWE-598 / \
             RFC 9110), got: {urls:?}"
    );

    // The standalone uncited AI-tier row's exported provenance must say "AI-advisory",
    // never a grounded/preview label borrowed from some other rule.
    let ai_row = fx.findings_export_value["findings"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|r| r["rule_id"].as_str() == Some("AI-STALE-SESSION-INVALIDATION-1"))
        .expect("the AI-tier row must be present in the export");
    assert_eq!(
        ai_row["provenance"].as_str(),
        Some("AI-advisory"),
        "an uncited AI-tier finding must render \"AI-advisory\" provenance, got: {ai_row:?}"
    );
    let citation_label = ai_row["citation_label"].as_str().unwrap_or("");
    assert!(
        citation_label.to_ascii_lowercase().contains("advisory")
            || citation_label
                .to_ascii_lowercase()
                .contains("model-inferred"),
        "the AI-tier row's own citation label must say so, got: {citation_label:?}"
    );
}

// ── Invariant 8 (STANDING) ───────────────────────────────────────────────────────────

#[tokio::test]
async fn a_rule_absorbed_only_via_also_matches_counts_as_matched() {
    let fx = build_fixture().await;
    assert!(
        !fx.json
            .whats_healthy
            .rules
            .iter()
            .any(|r| r.rule_id == "ARCH-NO-SECRETS-IN-URL-1"),
        "ARCH-NO-SECRETS-IN-URL-1 was absorbed into SEC-NO-HARDCODED-SECRETS-1's \
             also_matches this run — it must never be listed as a verified-clean rule just \
             because it has no Finding row of its own: {:?}",
        fx.json
            .whats_healthy
            .rules
            .iter()
            .map(|r| &r.rule_id)
            .collect::<Vec<_>>()
    );
}

// ── Invariant 9 (STANDING) ───────────────────────────────────────────────────────────

#[tokio::test]
async fn cover_and_scorecard_severity_totals_match_the_rendered_row_set() {
    let fx = build_fixture().await;
    let stats = &fx.json.cover.stats;
    let cover_total = stats.critical + stats.high + stats.medium + stats.low;

    let scorecard_total: usize = fx
        .json
        .scorecard
        .rows
        .iter()
        .map(|r| r.critical + r.high + r.medium + r.low)
        .sum();

    // The fixture has 6 non-FP, non-dependency code findings (f_secret, f_rls, f_hedge,
    // f_informational, f_identifier, f_ai_advisory) — every one of them lands in exactly
    // one category row's severity tally (an `info`-severity row folds into "low", same
    // collapse the xlsx Index sheet applies — see `category_status`/Index-sheet matrix).
    const EXPECTED_RENDERED_ROWS: usize = 6;

    assert_eq!(
        cover_total, EXPECTED_RENDERED_ROWS,
        "cover.stats severity totals must equal the rendered (non-FP, non-dependency) row \
             count"
    );
    assert_eq!(
        scorecard_total, EXPECTED_RENDERED_ROWS,
        "scorecard severity totals must equal the rendered (non-FP, non-dependency) row \
             count"
    );
    assert_eq!(
        cover_total, scorecard_total,
        "the cover stat strip and the scorecard must reconcile to the exact same total"
    );
}

// ── Invariant 10 ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn fix_fields_never_contain_raw_template_syntax() {
    let fx = build_fixture().await;
    let unfilled_token = regex::Regex::new(r"<[^<>]*>").expect("static regex must compile");

    let mut offenders = Vec::new();
    for (loc, text) in all_client_text_fields(&fx) {
        if !(loc.ends_with(".fix")
            || loc.ends_with(".fix_for_this_finding")
            || loc.ends_with(".fix_specific"))
        {
            continue;
        }
        if text.contains('{') || text.contains("TODO") || unfilled_token.is_match(&text) {
            offenders.push(format!("{loc}: {text:?}"));
        }
    }
    assert!(
        offenders.is_empty(),
        "a fix field contains raw, un-substituted template syntax ({{, <token>, or TODO): \
             {offenders:#?}"
    );
}

#[tokio::test]
#[ignore = "C5-6 open (product-polish, not a correctness bug): `generic_placeholder_filler` \
                legitimately renders \"the affected table\" when a detector captured no \
                concrete object name for a rule's `<table>`-style token (f_rls in this \
                fixture, which deliberately omits the `table` capture) — this is FILLED text, \
                never a raw/dangling placeholder, but it reads as generic boilerplate right \
                next to an otherwise-specific Fix line. Worth deciding whether the fix line \
                should instead omit the sentence, or whether every AUDIT_RULES floor checker \
                should be required to populate its own `captures` (several already do — see \
                `onboard::audit::enrich_secret_context`)."]
async fn fix_fields_can_fall_back_to_the_generic_affected_noun_phrase() {
    let fx = build_fixture().await;
    let mut offenders = Vec::new();
    for (loc, text) in all_client_text_fields(&fx) {
        if !(loc.ends_with(".fix") || loc.ends_with(".fix_for_this_finding")) {
            continue;
        }
        if text.contains("the affected ") {
            offenders.push(format!("{loc}: {text:?}"));
        }
    }
    assert!(
        offenders.is_empty(),
        "a fix field falls back to the generic \"the affected <noun>\" phrase instead of \
             naming the real object: {offenders:#?}"
    );
}

// ── Invariant 11 (STANDING) ──────────────────────────────────────────────────────────

#[tokio::test]
async fn curated_rows_for_authored_floor_rules_have_a_non_empty_fix_and_estimate() {
    let fx = build_fixture().await;

    for rule_id in ["SEC-NO-HARDCODED-SECRETS-1", "SUPABASE-RLS-ENABLED-1"] {
        let group = fx
            .json
            .curated_findings
            .iter()
            .find(|g| g.rule_id == rule_id)
            .unwrap_or_else(|| panic!("{rule_id} must be a curated (action-bucket) group"));
        let site = &group.sites[0];
        assert!(
            matches!(
                site.bucket.as_str(),
                "do_now" | "do_next" | "plan" | "accepted"
            ),
            "{rule_id}'s site must be in an action bucket, got {:?}",
            site.bucket
        );
        assert!(
            site.fix.as_deref().is_some_and(|f| !f.trim().is_empty()),
            "{rule_id} is an authored floor rule — its curated site's fix must be \
                 non-empty, got {:?}",
            site.fix
        );

        let row = fx.findings_export_value["findings"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|r| r["rule_id"].as_str() == Some(rule_id))
            .unwrap_or_else(|| panic!("{rule_id} must have a FindingRow in the export"));
        let est_hours = row["est_hours"].as_str().unwrap_or("");
        assert!(
            !est_hours.is_empty() && est_hours != "not yet estimated",
            "{rule_id}'s FindingRow est_hours must be a real estimate, got {est_hours:?}"
        );
    }
}

// ── Invariant 12 (STANDING — REG-3) ─────────────────────────────────────────────────

/// REG-3: `ai_audit::same_cause_signal`'s same-construct-security override used to
/// absorb ANY co-located rule into a security finding's `also_matches` as long as one side
/// was security-tier, with no requirement the two findings share a defect class — so an
/// unrelated indexing-strategy or pagination-convention finding could vanish from its own
/// category/Coverage count by riding along in a security row's "Also violates" list. Fixed
/// in `same_cause_signal` itself (now requires a category-class match or a shared
/// captured object); this is the EXPORT-LEVEL trip wire that makes a future re-loosening of
/// that predicate visible here too, without needing to know the merge internals: across
/// every curated/held-for-review group in the exported report, an `also_matches` member's
/// rule-id category must never differ from its group's primary rule-id category whenever
/// BOTH have a known `categorize_rule_id` class. (An unmapped/invented id has no category to
/// compare and is not flagged by this id-only check — the full category-OR-shared-object
/// predicate is exercised directly, with synthetic same-construct fixtures, by the
/// `p1_c4_same_construct_*` tests in `ai_audit::tests`; this gate is the belt-and-suspenders
/// check that the real export pipeline's `also_matches` never contradicts it.)
#[tokio::test]
async fn also_matches_members_never_cross_category_without_a_shared_captured_object() {
    let fx = build_fixture().await;
    let mut offenders = Vec::new();
    for group in fx
        .json
        .curated_findings
        .iter()
        .chain(fx.json.held_for_review_findings.iter())
    {
        let primary_category = crate::ai_audit::categorize_rule_id(&group.rule_id);
        for site in &group.sites {
            for member_id in &site.also_matches {
                let member_category = crate::ai_audit::categorize_rule_id(member_id);
                if let (Some(pc), Some(mc)) = (&primary_category, &member_category) {
                    if pc != mc {
                        offenders.push(format!(
                            "{} (category {pc:?}) absorbed {member_id} (category {mc:?}) \
                                 into also_matches with no shared captured object",
                            group.rule_id
                        ));
                    }
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "REG-3: an also_matches member must never cross category away from its primary's \
             without a shared captured object: {offenders:#?}"
    );
}

/// REG-3 regression test at the full export layer (mirrors the `ai_audit::tests` unit
/// coverage, but exercised end to end through `group_semantic` →
/// [`build_report_json`]): an injection finding and an unrelated pagination-convention
/// finding, co-located in one construct, with different categories and no shared captured
/// object, must export as TWO distinct curated groups — never one row with the pagination
/// rule swallowed into `also_matches` and hidden from its own category's Coverage count.
#[tokio::test]
async fn same_construct_injection_and_pagination_pair_export_as_two_distinct_groups() {
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");

    let content = "fn handler() {\n let q = raw_sql(id);\n let page = rows.skip(n).take(k);\n}\n";
    let files = vec![("src/api/handler.rs".to_string(), content.to_string())];
    let mut injection = finding(
        "SEC-SQL-INJECTION-SINK-1",
        "demo/portal",
        "src/api/handler.rs",
        2,
        "critical",
    );
    injection.detail = "User-controlled input flows into a raw SQL query.".to_string();
    let mut pagination = finding(
        "PERF-PAGINATION-MISSING-1",
        "demo/portal",
        "src/api/handler.rs",
        3,
        "medium",
    );
    pagination.detail = "This handler returns every row with no pagination.".to_string();

    let merged =
        crate::review_tree::test_support::cluster_heads(vec![injection, pagination], &files);
    assert_eq!(
        merged.len(),
        2,
        "REG-3: an injection finding and an unrelated pagination finding co-located in one \
             construct must stay two rows: {merged:?}"
    );

    let report = report_with(merged, vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );
    let groups: Vec<&str> = json
        .curated_findings
        .iter()
        .chain(json.held_for_review_findings.iter())
        .map(|g| g.rule_id.as_str())
        .collect();
    assert!(
        groups.contains(&"SEC-SQL-INJECTION-SINK-1"),
        "the injection finding must export as its own group: {groups:?}"
    );
    assert!(
        groups.contains(&"PERF-PAGINATION-MISSING-1"),
        "the pagination finding must export as its own group, never absorbed into the \
             injection row's also_matches: {groups:?}"
    );
}

// ── Invariant 13 (STANDING — C6-B1) ──────────────────────────────────────────────────

/// C6-B1: a row counted but never rendered — the exact client-facing failure this
/// invariant exists to catch. Root cause: the `by_rule`-population loop in
/// `build_report_json` used to `continue` past any `is_uncited_ai_finding` row BEFORE a
/// `CuratedSiteJson` was ever built for it, on the stale assumption that the row "was
/// already routed to `matrix.informational`" — true before C5-7, false after it (C5-7
/// routes a high/critical uncited finding to the severity-unbounded `held` bucket
/// instead). The row stayed counted everywhere that tallies `code_findings` directly
/// (`matrix.held`, the cover severity strip, the category scorecard, `findings.json`/the
/// workbook) while the PDF renderer never emitted it in ANY section. Fixed by always
/// building a site (see that loop's updated doc comment); this test is the STANDING gate
/// against a regression of the same SHAPE from a different cause — any future change that
/// drops, skips, or double-renders a row fails this assertion, a build-breaking TEST-gate
/// failure, never a silent count/render divergence a client discovers first.
///
/// Asserted two ways over the one shared fixture (6 live code findings: `f_secret`,
/// `f_rls`, `f_hedge`, `f_informational`, `f_identifier`, `f_ai_advisory` — see
/// `build_fixture`'s doc comment):
/// 1. rows in the JSON (`candidates_reviewed` minus excluded false positives minus
///    dependency-audit rows — the same `code_findings` total the cover/scorecard/xlsx all
///    tally against) equals rendered blocks (`curated_findings` + `held_for_review_findings`
///    site counts combined, which between them account for every informational AND held
///    row, not just curated ones);
/// 2. restated directly against the cover severity strip, the one artifact closest to
///    what a client reads first.
#[tokio::test]
async fn rows_in_json_equal_rendered_blocks_across_curated_and_held_for_review() {
    let fx = build_fixture().await;

    let rendered_blocks: usize = fx
        .json
        .curated_findings
        .iter()
        .chain(fx.json.held_for_review_findings.iter())
        .map(|g| g.sites.len())
        .sum();

    // `code_findings.len()` itself is private to `build_report_json`, but it is exactly
    // `candidates_reviewed - excluded_false_positive - dependency_advisories` by
    // construction (see that function's own executive-summary reconciliation) — the same
    // total `cover.stats`/`scorecard` already tally against directly.
    let rows_in_json = fx.json.executive_summary.candidates_reviewed
        - fx.json.executive_summary.excluded_false_positive
        - fx.json.executive_summary.dependency_advisories;

    assert_eq!(
        rendered_blocks, rows_in_json,
        "every row counted in the JSON must have exactly one rendered block across \
             curated_findings + held_for_review_findings combined — a mismatch means some row \
             is counted (cover/scorecard/xlsx) without ever being rendered, or rendered more \
             than once. rendered_blocks={rendered_blocks}, rows_in_json={rows_in_json}, \
             curated_findings={:?}, held_for_review_findings={:?}",
        fx.json.curated_findings, fx.json.held_for_review_findings
    );

    let cover_total = fx.json.cover.stats.critical
        + fx.json.cover.stats.high
        + fx.json.cover.stats.medium
        + fx.json.cover.stats.low;
    assert_eq!(
        rendered_blocks, cover_total,
        "the cover severity strip must tally exactly as many rows as actually got a \
             rendered block — never a count computed independently of what rendered"
    );
}

// ── W1: pipeline-integrity ledger invariants ───────────────────────────────────────────
// 12. Ledger-sourced coverage: a rule the ledger confirms did NOT run can never appear in
//     "What's healthy"; a rule that genuinely fired can never appear in either "healthy"
//     or "excluded from this audit"; and a fully-reconciled scan (every stage's rows_in ==
//     rows_out + accounted) never emits a "pipeline integrity" disclosure. Traces to W1
//     (docs/plans — the pipeline-integrity ledger): rules that never ran being reported as
//     "verified clean" while the selector's own evidence described the violation.
#[tokio::test]
async fn ledger_sourced_healthy_and_excluded_never_overlap_and_a_reconciled_scan_discloses_nothing()
{
    let f_fired = finding(
        "ARCH-STRICT-LAYERING-1",
        "demo/portal",
        "apps/api/src/handlers/users.rs",
        12,
        "high",
    );
    let mut report = report_with(vec![f_fired], vec![]);

    let mut ledger = crate::scan_ledger::ScanLedger::new();
    // Genuinely ran, zero findings -> healthy.
    ledger.record_rule(
        "",
        "SEC-NO-HARDCODED-SECRETS-1",
        crate::scan_ledger::RuleTier::Deterministic,
        true,
        None,
        40,
        0,
    );
    // Genuinely ran AND fired (matches the one real Finding above) -> neither healthy nor
    // excluded.
    ledger.record_rule(
        "",
        "ARCH-STRICT-LAYERING-1",
        crate::scan_ledger::RuleTier::Deterministic,
        true,
        None,
        40,
        1,
    );
    // The W1-item-4 defect shape: declared mechanical, no wired detector -> excluded with
    // a reason, never healthy.
    ledger.record_rule(
        "",
        "PYTHON-PARAMETERIZED-SQL-1",
        crate::scan_ledger::RuleTier::Deterministic,
        false,
        Some("declares mechanical/architectural enforcement but has no wired detector".to_string()),
        0,
        0,
    );
    // A fully-accounted stage (e.g. a merge that absorbed one row into the fired primary)
    // — rows_in == rows_out + merged, so unaccounted == 0.
    let mut acc = crate::scan_ledger::StageAccounting::default();
    acc.push_merge("AI-SOME-INVENTED-ID-1", "ARCH-STRICT-LAYERING-1");
    ledger.record_stage("cross-family-merge", 2, 1, acc);
    report.ledger = ledger;

    let corpus_path = camerata_rules::corpus_path();
    let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");

    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );

    let healthy_ids: std::collections::HashSet<&str> = json
        .whats_healthy
        .rules
        .iter()
        .map(|r| r.rule_id.as_str())
        .collect();
    let excluded_ids: std::collections::HashSet<&str> = json
        .rules_not_run
        .iter()
        .map(|r| r.rule_id.as_str())
        .collect();

    assert!(
        healthy_ids.contains("SEC-NO-HARDCODED-SECRETS-1"),
        "a rule that ran with zero findings must be healthy: {healthy_ids:?}"
    );
    assert!(
        !healthy_ids.contains("PYTHON-PARAMETERIZED-SQL-1"),
        "a rule the ledger says did NOT run must never be healthy: {healthy_ids:?}"
    );
    assert!(
        excluded_ids.contains("PYTHON-PARAMETERIZED-SQL-1"),
        "a not-run rule must appear in excluded-from-this-audit with its reason: {:?}",
        json.rules_not_run
    );
    let python_entry = json
        .rules_not_run
        .iter()
        .find(|r| r.rule_id == "PYTHON-PARAMETERIZED-SQL-1")
        .expect("present per the assertion above");
    assert!(
        python_entry.reason.contains("no wired detector"),
        "the excluded entry must carry the REAL reason, not a placeholder: {:?}",
        python_entry.reason
    );
    assert!(
        !healthy_ids.contains("ARCH-STRICT-LAYERING-1")
            && !excluded_ids.contains("ARCH-STRICT-LAYERING-1"),
        "a rule that genuinely fired must appear in neither healthy nor excluded: \
             healthy={healthy_ids:?} excluded={excluded_ids:?}"
    );
    assert!(
        healthy_ids.is_disjoint(&excluded_ids),
        "no rule id may EVER appear in both healthy and excluded: healthy={healthy_ids:?} \
             excluded={excluded_ids:?}"
    );

    // A fully-reconciled scan (every stage's rows_in == rows_out + accounted, which this
    // fixture's single merge stage satisfies by construction, and the report-build
    // partition stage satisfies because the one real finding lands squarely in an action
    // bucket with none excluded/held/informational) must emit NO "pipeline integrity"
    // disclosure anywhere a reader would see it.
    let all_disclosures = json
        .methodology
        .failed_passes
        .iter()
        .chain(json.executive_summary.failed_passes.iter());
    for note in all_disclosures {
        assert!(
            !note.to_ascii_lowercase().contains("pipeline integrity"),
            "a fully-reconciled scan must never disclose a pipeline-integrity gap: {note}"
        );
    }
}

/// The flip side: a stage the ledger could NOT fully reconcile (a row vanished with no
/// recorded disposition) DOES surface as an explicit "pipeline integrity" disclosure —
/// proving the invariant above is actually load-bearing, not vacuously true because
/// nothing ever checks the unreconciled case.
#[tokio::test]
async fn an_unreconciled_stage_discloses_a_pipeline_integrity_gap() {
    let report = report_with(Vec::new(), vec![]);
    let mut report = report;
    let mut ledger = crate::scan_ledger::ScanLedger::new();
    // 5 rows in, 3 out, NOTHING explained -> 2 unaccounted.
    ledger.record_stage(
        "cross-family-merge",
        5,
        3,
        crate::scan_ledger::StageAccounting::default(),
    );
    report.ledger = ledger;

    let corpus_path = camerata_rules::corpus_path();
    let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );

    let found = json
        .methodology
        .failed_passes
        .iter()
        .chain(json.executive_summary.failed_passes.iter())
        .any(|note| {
            note.to_ascii_lowercase().contains("pipeline integrity")
                && note.contains("cross-family-merge")
        });
    assert!(
        found,
        "an unreconciled stage must disclose a pipeline-integrity gap naming the stage: \
             methodology={:?} executive_summary={:?}",
        json.methodology.failed_passes, json.executive_summary.failed_passes
    );
}

// ── 13 (defect 1): no-wired-detector exclusions collapse to one plain sentence ──────

/// Invariant 13: N rules the ledger excludes for the W1-item-4 "no wired detector" reason
/// must read as ONE plain, factual sentence naming the real count — never one bullet per
/// rule, never duplicated beyond the ONE sentence each of the executive summary and
/// methodology carries (by the SAME pre-existing shared-disclosure design every other
/// `FailedPass` note uses), and never leaking the internal registry/mapping vocabulary a
/// real such reason string names. The full per-rule detail (rule id + real reason) is
/// NOT lost — it is still in `rules_not_run`, which is exactly what `camerata inspect`'s
/// `render_ledger_summary` (`crates/cli/src/inspect_cmd.rs`) prints in full under its own
/// `== NOT RUN (N) ==` section.
#[tokio::test]
async fn many_no_wired_detector_exclusions_collapse_to_one_plain_sentence() {
    let mut report = report_with(Vec::new(), vec![]);
    let mut ledger = crate::scan_ledger::ScanLedger::new();
    let rule_ids = [
        "PYTHON-PARAMETERIZED-SQL-1",
        "RUBY-FROZEN-STRING-LITERAL-1",
        "JAVA-PREPARED-STATEMENT-1",
        "GO-NO-RAW-SQL-1",
        "RUST-NO-UNWRAP-IN-HANDLER-1",
    ];
    for rid in rule_ids {
        ledger.record_rule(
            "",
            rid,
            crate::scan_ledger::RuleTier::Deterministic,
            false,
            Some(
                "declares mechanical/architectural enforcement but has no wired detector \
                     (not in the arch_checker registry, gateway rule registry, Semgrep \
                     mapping, or scan-preview linter source)"
                    .to_string(),
            ),
            0,
            0,
        );
    }
    report.ledger = ledger;

    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );

    assert_eq!(
        json.executive_summary.failed_passes.len(),
        1,
        "5 rules sharing the same reason must collapse to ONE sentence, not 5: {:?}",
        json.executive_summary.failed_passes
    );
    assert_eq!(
        json.methodology.failed_passes.len(),
        1,
        "methodology must carry exactly one sentence too, never a per-rule list: {:?}",
        json.methodology.failed_passes
    );
    let sentence = &json.executive_summary.failed_passes[0];
    assert!(
        sentence.contains('5'),
        "the sentence must name the real count: {sentence}"
    );
    for banned in [
        "arch_checker",
        "registry",
        "Semgrep mapping",
        "scan-preview linter",
        "PYTHON-PARAMETERIZED-SQL-1",
        "no wired detector",
    ] {
        assert!(
            !sentence.contains(banned),
            "client-facing sentence must never leak internal vocabulary {banned:?}: \
                 {sentence}"
        );
    }

    // The full per-rule detail still lives in `rules_not_run` — never dropped, just kept
    // out of the client narrative.
    assert_eq!(
        json.rules_not_run.len(),
        5,
        "the full per-rule detail must still be present in rules_not_run: {:?}",
        json.rules_not_run
    );
    for rid in rule_ids {
        assert!(
            json.rules_not_run.iter().any(|r| r.rule_id == rid),
            "rules_not_run must still carry {rid}"
        );
    }
}

/// A single-digit edge case: exactly one excluded rule still gets the collapsed-sentence
/// treatment (singular "1 rule ... was reviewed", not a pluralization bug, and no bullet
/// list of one).
#[tokio::test]
async fn one_no_wired_detector_exclusion_still_renders_the_plain_sentence() {
    let mut report = report_with(Vec::new(), vec![]);
    let mut ledger = crate::scan_ledger::ScanLedger::new();
    ledger.record_rule(
        "",
        "PYTHON-PARAMETERIZED-SQL-1",
        crate::scan_ledger::RuleTier::Deterministic,
        false,
        Some("declares mechanical/architectural enforcement but has no wired detector".to_string()),
        0,
        0,
    );
    report.ledger = ledger;
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert_eq!(json.executive_summary.failed_passes.len(), 1);
    assert!(json.executive_summary.failed_passes[0].contains("1 rule"));
    assert!(!json.executive_summary.failed_passes[0].contains("PYTHON-PARAMETERIZED-SQL-1"));
}

// ── 16 (defect 4): the evidence-of-violation gate ────────────────────────────────────

/// Invariant 16, negative case: a zero-finding rule whose own `RuleRecommendation`
/// evidence NEGATES the rule's requirement (contains a conservative negation phrase —
/// here, literally the three real-world examples the grading pass surfaced) must NEVER
/// appear in "what's healthy" — it is held for review instead, as an advisory finding
/// anchored at the evidence's own citation.
#[tokio::test]
async fn evidence_negating_rule_requirement_is_held_not_healthy_positive_twin_stays_healthy() {
    for (evidence, label) in [
        (
            "src/orders/service.rs:142 — three sequential writes with no transaction",
            "no-transaction",
        ),
        (
            "src/billing/charge.rs:58 — an external chargeable call with no \
                 idempotency-key handling",
            "no-idempotency",
        ),
        (
            "src/api/admin.rs:30 — authorization done as ad hoc checks inside business \
                 logic",
            "ad-hoc-authz",
        ),
    ] {
        let mut report = report_with(Vec::new(), vec!["ARCH-EXPLICIT-TX-1"]);
        let mut ledger = crate::scan_ledger::ScanLedger::new();
        ledger.record_rule(
            "",
            "ARCH-EXPLICIT-TX-1",
            crate::scan_ledger::RuleTier::Semantic,
            true,
            None,
            10,
            0,
        );
        report.ledger = ledger;
        report.recommendations.insert(
            "ARCH-EXPLICIT-TX-1".to_string(),
            crate::ai_audit::RuleRecommendation {
                repo: String::new(),
                rule_id: "ARCH-EXPLICIT-TX-1".to_string(),
                recommended_option_id: "some-option".to_string(),
                recommendation_reasoning: "the model's pick".to_string(),
                hallucinated: false,
                operator_chosen: false,
                evidence: Some(evidence.to_string()),
                applicable: true,
                ..Default::default()
            },
        );
        // Pass-1 W1 seal fix (exception (a) — a deliberate code/data-structure change this
        // test must adapt to): the evidence gate used to run IMPLICITLY inside
        // `build_report_json` (see `apply_evidence_gate`'s doc comment for why that was
        // itself the bug); it is now an explicit upstream pipeline step the caller runs
        // BEFORE sealing, exactly like `onboard_audit`/`onboard_audit_start` do in
        // `lib.rs`. Every assertion below is unchanged — only the missing call is added.
        apply_evidence_gate(&mut report);

        let json = build_report_json(
            &crate::report_export::ReviewedReport::unreviewed(
                &report.clone().seal(),
                &HashMap::new(),
            ),
            None,
            &empty_opts(),
        );

        assert!(
            !json
                .whats_healthy
                .rules
                .iter()
                .any(|r| r.rule_id == "ARCH-EXPLICIT-TX-1"),
            "[{label}] a rule whose own evidence negates its requirement must NEVER be \
                 healthy: {:?}",
            json.whats_healthy.rules
        );
        let held = json
            .held_for_review_findings
            .iter()
            .find(|g| g.rule_id == "ARCH-EXPLICIT-TX-1");
        assert!(
            held.is_some(),
            "[{label}] the preferred behavior (an advisory held row) must be present: \
                 {:?}",
            json.held_for_review_findings
        );
        let (expected_path, expected_line) = parse_evidence_citation(evidence);
        let site = &held.unwrap().sites[0];
        assert_eq!(
            site.path, expected_path,
            "[{label}] the advisory row must anchor at the evidence's own citation"
        );
        assert_eq!(
            site.line, expected_line,
            "[{label}] line must match the citation"
        );
        assert_eq!(site.bucket, "held");
    }
}

/// Invariant 16, positive twin: the SAME rule, SAME shape, but evidence that CONFIRMS
/// compliance (no negation phrase) must stay healthy — the gate is conservative and must
/// never punish a genuinely clean rule.
#[tokio::test]
async fn evidence_confirming_compliance_still_renders_healthy() {
    let mut report = report_with(Vec::new(), vec!["ARCH-EXPLICIT-TX-1"]);
    let mut ledger = crate::scan_ledger::ScanLedger::new();
    ledger.record_rule(
        "",
        "ARCH-EXPLICIT-TX-1",
        crate::scan_ledger::RuleTier::Semantic,
        true,
        None,
        10,
        0,
    );
    report.ledger = ledger;
    report.recommendations.insert(
        "ARCH-EXPLICIT-TX-1".to_string(),
        crate::ai_audit::RuleRecommendation {
            repo: String::new(),
            rule_id: "ARCH-EXPLICIT-TX-1".to_string(),
            recommended_option_id:
                "services-own-the-transaction-lifecycle-repositories-receive-the-handle"
                    .to_string(),
            recommendation_reasoning: "the codebase already does it this way".to_string(),
            hallucinated: false,
            operator_chosen: false,
            evidence: Some(
                "src/orders/service.rs:140 — opens a transaction and passes the handle to \
                     each repository call"
                    .to_string(),
            ),
            applicable: true,
            ..Default::default()
        },
    );
    // Pass-1 W1 seal fix (exception (a), same as the negative-case test above): the gate
    // must run explicitly now. Without this call the test would still pass, but
    // VACUOUSLY — it would no longer actually exercise the gate's "confirms compliance,
    // never flags" branch at all.
    apply_evidence_gate(&mut report);

    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );

    assert!(
        json.whats_healthy
            .rules
            .iter()
            .any(|r| r.rule_id == "ARCH-EXPLICIT-TX-1"),
        "a rule whose evidence CONFIRMS compliance must stay healthy: {:?}",
        json.whats_healthy.rules
    );
    assert!(
        !json
            .held_for_review_findings
            .iter()
            .any(|g| g.rule_id == "ARCH-EXPLICIT-TX-1"),
        "a genuinely healthy rule must not ALSO render a spurious advisory row: {:?}",
        json.held_for_review_findings
    );
}

/// A rule with NO recommendation at all (no multi-option evidence ever recorded — the
/// overwhelming majority of rules/scans) must be completely unaffected by this gate: it
/// stays healthy exactly as before.
#[tokio::test]
async fn a_zero_finding_rule_with_no_recommendation_is_unaffected_by_the_evidence_gate() {
    let mut report = report_with(Vec::new(), vec!["SEC-NO-HARDCODED-SECRETS-1"]);
    let mut ledger = crate::scan_ledger::ScanLedger::new();
    ledger.record_rule(
        "",
        "SEC-NO-HARDCODED-SECRETS-1",
        crate::scan_ledger::RuleTier::Deterministic,
        true,
        None,
        10,
        0,
    );
    report.ledger = ledger;
    // Pass-1 W1 seal fix: explicit now (a no-op here — `report.recommendations` is empty —
    // but called for consistency with the real pipeline order; see the sibling tests above).
    apply_evidence_gate(&mut report);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert!(json
        .whats_healthy
        .rules
        .iter()
        .any(|r| r.rule_id == "SEC-NO-HARDCODED-SECRETS-1"));
}

// ── Pass-1 W7 (C2): the sites-vs-rows integrity check, retired ───────────────────────

#[tokio::test]
async fn req_c2_the_sites_vs_rows_check_is_gone_because_a_sub_groups_members_are_its_sites() {
    use camerata_api_types::audit::{DeviationKind, FindingKind};
    // A rule whose rows claim `deviating_sites == 3` while one row is present used to export
    // a synthetic "site count integrity" row. Nothing absorbs a site into another row any
    // more, so the number of rows IS the number of sites and there is nothing to reconcile:
    // the evidence gate adds no row and records no `sites-vs-rows-gate` stage.
    let mut f = finding(
        "ARCH-API-VERSIONING-1",
        "demo/portal",
        "src/api/routes.rs",
        42,
        "medium",
    );
    f.kind = Some(FindingKind::Deviation(DeviationKind {
        deviating_sites: 3,
        assessed_sites: Some(47),
        convention_option_id: Some("versioned".to_string()),
        convention_label: "URI-versioned endpoints".to_string(),
    }));
    let mut report = report_with(vec![f], vec!["ARCH-API-VERSIONING-1"]);
    let rows_before = report.findings.len();
    apply_evidence_gate(&mut report);
    assert_eq!(
        report.findings.len(),
        rows_before,
        "no disclosure row is synthesized"
    );
    assert!(
        report
            .ledger
            .stages()
            .iter()
            .all(|s| s.stage != "sites-vs-rows-gate"),
        "{:#?}",
        report.ledger.stages()
    );
}

// ── Pass-1 W1: the seal (the audit findings tracker) ─────────────
//
// C1 — every row the engine produces reaches the reader; nothing may be added after the
//      row set is final.
// D1 — the PDF, the spreadsheet and the JSON contain exactly the same rows.
// D2 — every count printed anywhere is computed over that same set of rows.
//
// The shipped defect (verified by two independent audits): `apply_evidence_gate` (then
// `inject_evidence_gate_findings`) ran INSIDE `build_report_json` against a CLONED report
// that silently shadowed the function's own parameter — `xlsx_export::build_workbook` /
// `build_findings_export` read the ORIGINAL, un-injected `ScanReport` directly, so the PDF
// rendered N+K rows while `findings.json`/the xlsx rendered N. The fix moves the gate
// upstream of `ScanReport::seal` (see that method's doc comment for the full pipeline
// order) and changes all three exporters to take `&SealedReport`.
//
// CRITICAL FIXTURE NOTE: the standing `export_invariants_gate` fixture above
// (`build_fixture`) sets `recommendations: HashMap::new()` via `report_with`, which makes
// the evidence gate return early — that is EXACTLY why this bug shipped undetected. The
// fixture below deliberately populates `recommendations` so the gate actually fires, and
// `req_d1_the_seal_fixture_actually_exercises_the_evidence_gate_and_preview_merge` below
// asserts `k > 0` directly, so this cannot silently regress to a no-op.

/// Build one report whose evidence gate AND external-tool preview merge BOTH fire, run
/// through the exact same scan-pipeline steps `onboard_audit`/`onboard_audit_start`
/// (`crates/server/src/lib.rs`) apply in production — a simulated
/// `external-tool-preview-merge` ledger stage (standing in for `merge_scan_preview`, which
/// mutates `report.findings` in place before this point in the real pipeline), then
/// [`apply_evidence_gate`], then [`crate::onboard::ScanReport::seal`] — never the other
/// order. Returns `(sealed, k)` where `k` is how many rows the evidence gate actually
/// synthesized, so callers can assert `k > 0` per the CRITICAL fixture note above.
async fn build_seal_fixture() -> (crate::onboard::SealedReport, usize) {
    let mut f_floor = finding(
        "SEC-NO-HARDCODED-SECRETS-1",
        "demo/portal",
        "apps/api/src/config.rs",
        10,
        "critical",
    );
    f_floor.snippet = "const API_KEY = \"abcdef1234567890ab\";".to_string();

    // Simulates `merge_scan_preview` having already appended a scan-time external-tool
    // finding to `report.findings` BEFORE the evidence gate / seal run, exactly as the real
    // pipeline orders it (see `crate::merge_scan_preview`'s doc comment).
    let mut f_preview = finding(
        "SEC-NO-RAW-SQL-CONCAT-1",
        "demo/portal",
        "apps/api/src/db.rs",
        22,
        "high",
    );
    f_preview.preview_tool = Some("semgrep".to_string());

    let mut report = report_with(
        vec![f_floor, f_preview],
        vec![
            "SEC-NO-HARDCODED-SECRETS-1",
            "SEC-NO-RAW-SQL-CONCAT-1",
            "ARCH-EXPLICIT-TX-1",
        ],
    );

    let mut ledger = crate::scan_ledger::ScanLedger::new();
    ledger.record_rule(
        "",
        "SEC-NO-HARDCODED-SECRETS-1",
        crate::scan_ledger::RuleTier::Deterministic,
        true,
        None,
        40,
        1,
    );
    ledger.record_rule(
        "",
        "SEC-NO-RAW-SQL-CONCAT-1",
        crate::scan_ledger::RuleTier::ExternalTool,
        true,
        None,
        40,
        1,
    );
    // The zero-finding, multi-option rule the evidence gate will flag.
    ledger.record_rule(
        "",
        "ARCH-EXPLICIT-TX-1",
        crate::scan_ledger::RuleTier::Semantic,
        true,
        None,
        10,
        0,
    );
    report.ledger = ledger;

    // The external-tool-preview-merge ledger stage the real pipeline records in
    // `merge_scan_preview` (`lib.rs`): 1 row in (the floor finding), 2 out (the preview
    // finding net-added) — see `crate::scan_ledger::record_addition_stage`.
    crate::scan_ledger::record_addition_stage(
        &mut report.ledger,
        "external-tool-preview-merge",
        1,
        2,
        "external_tool_preview_findings",
    );

    report.recommendations.insert(
        "ARCH-EXPLICIT-TX-1".to_string(),
        crate::ai_audit::RuleRecommendation {
            repo: String::new(),
            rule_id: "ARCH-EXPLICIT-TX-1".to_string(),
            recommended_option_id: "some-option".to_string(),
            recommendation_reasoning: "the model's pick".to_string(),
            hallucinated: false,
            operator_chosen: false,
            evidence: Some(
                "src/orders/service.rs:142 — three sequential writes with no transaction"
                    .to_string(),
            ),
            applicable: true,
            ..Default::default()
        },
    );

    let rows_before_gate = report.findings.len();
    apply_evidence_gate(&mut report);
    let k = report.findings.len() - rows_before_gate;

    (report.seal(), k)
}

/// Guards the fixture itself against silently regressing to a no-op (the CRITICAL fixture
/// note above): the evidence gate must actually synthesize at least one row, and the
/// external-tool-preview-merge ledger stage must actually be present with a nonzero
/// addition — otherwise every test below would pass vacuously.
#[tokio::test]
async fn req_d1_the_seal_fixture_actually_exercises_the_evidence_gate_and_preview_merge() {
    let (sealed, k) = build_seal_fixture().await;
    assert!(
        k > 0,
        "fixture precondition failed: the evidence gate produced zero rows, so this \
             fixture cannot prove anything about the gate's cross-artifact behavior — see the \
             CRITICAL fixture note"
    );
    let preview_stage = sealed
        .ledger
        .stages()
        .iter()
        .find(|s| s.stage == "external-tool-preview-merge")
        .expect("the preview-merge stage must be present in the fixture");
    assert!(
        preview_stage.added_total() > 0,
        "fixture precondition failed: the external-tool preview merge must have added at \
             least one row: {preview_stage:?}"
    );
}

/// req_d1: the PDF (via `build_report_json`), the spreadsheet (`build_workbook`) and the
/// machine-readable sibling (`build_findings_export`) must contain EXACTLY the same rows —
/// both by COUNT and by IDENTITY (rule_id, repo, path, line) — when built from one sealed
/// report whose evidence gate AND external-tool preview merge both fired. Before the fix,
/// the PDF's row set (via the clone-shadowed `inject_evidence_gate_findings`) held N+K rows
/// while `findings.json`/the xlsx held only N; this asserts the gap is fully closed.
#[tokio::test]
async fn req_d1_pdf_findings_json_and_xlsx_contain_exactly_the_same_rows() {
    let (sealed, k) = build_seal_fixture().await;
    assert!(
        k > 0,
        "fixture must exercise the evidence gate — see the dedicated guard test"
    );

    let corpus_path = camerata_rules::corpus_path();
    let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");

    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&sealed, &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );
    let workbook = xlsx_export::build_workbook(
        &crate::report_export::ReviewedReport::unreviewed(&sealed, &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    )
    .expect("the workbook must build");
    let findings_export = xlsx_export::build_findings_export(
        &crate::report_export::ReviewedReport::unreviewed(&sealed, &HashMap::new()),
        Some(&corpus),
        &json,
        &HashMap::new(),
    );

    // The PDF's row-identity set: every FindingRefJson across all six matrix buckets (the
    // full `code_findings` partition — do_now/do_next/plan/accepted/informational/held).
    let mut pdf_ids: std::collections::BTreeSet<(String, String, String, usize)> =
        std::collections::BTreeSet::new();
    for bucket in [
        &json.matrix.do_now,
        &json.matrix.do_next,
        &json.matrix.plan,
        &json.matrix.accepted,
        &json.matrix.informational,
        &json.matrix.held,
    ] {
        for f in bucket {
            pdf_ids.insert((f.rule_id.clone(), f.repo.clone(), f.path.clone(), f.line));
        }
    }

    // `findings.json`'s row-identity set — read via `serde_json::Value` since
    // `xlsx_export::FindingRow`'s fields are private to that module (same convention
    // `all_client_text_fields` above already uses for this exact type).
    let findings_export_value =
        serde_json::to_value(&findings_export).expect("FindingsExport must serialize");
    let mut findings_json_ids: std::collections::BTreeSet<(String, String, String, usize)> =
        std::collections::BTreeSet::new();
    for row in findings_export_value["findings"]
        .as_array()
        .expect("must be an array")
    {
        findings_json_ids.insert((
            row["rule_id"].as_str().expect("rule_id").to_string(),
            row["repo"].as_str().expect("repo").to_string(),
            row["path"].as_str().expect("path").to_string(),
            row["line"].as_u64().expect("line") as usize,
        ));
    }

    // The comparison is the release gate's own `row_parity` (the identity set, in both
    // directions): the test and the runtime check call the same function.
    let problems = crate::release_gate::row_parity(&json, &findings_export);
    assert!(
        problems.is_empty(),
        "the PDF and findings.json must render the IDENTICAL row-identity set: {problems:?} \
             (pdf={pdf_ids:#?}, findings_json={findings_json_ids:#?})"
    );
    // Both the evidence-gate-synthesized row and the external-tool-preview row must
    // actually be present in BOTH artifacts — not merely equal-sized by coincidence.
    assert!(
        pdf_ids.iter().any(|(rid, ..)| rid == "ARCH-EXPLICIT-TX-1"),
        "the evidence-gate-synthesized row must reach the PDF: {pdf_ids:#?}"
    );
    assert!(
        findings_json_ids
            .iter()
            .any(|(rid, ..)| rid == "ARCH-EXPLICIT-TX-1"),
        "the evidence-gate-synthesized row must reach findings.json: {findings_json_ids:#?}"
    );
    assert!(
        pdf_ids
            .iter()
            .any(|(rid, ..)| rid == "SEC-NO-RAW-SQL-CONCAT-1"),
        "the external-tool-preview row must reach the PDF: {pdf_ids:#?}"
    );
    assert!(
        findings_json_ids
            .iter()
            .any(|(rid, ..)| rid == "SEC-NO-RAW-SQL-CONCAT-1"),
        "the external-tool-preview row must reach findings.json: {findings_json_ids:#?}"
    );

    // The xlsx: proven via its shared-strings pool (the same technique invariants 1/2 at
    // the top of this module already use) rather than parsing worksheet XML relationships —
    // both rule ids, including the evidence-gate-synthesized one, must appear somewhere in
    // the workbook's text, proving it was ALSO exported there, not just in findings.json.
    let shared_strings = workbook_shared_string_texts(&workbook_shared_strings_xml(&workbook));
    let joined = shared_strings.join("\n");
    assert!(
        joined.contains("ARCH-EXPLICIT-TX-1"),
        "the evidence-gate-synthesized row's rule id must appear in the xlsx"
    );
    assert!(
        joined.contains("SEC-NO-RAW-SQL-CONCAT-1"),
        "the external-tool-preview row's rule id must appear in the xlsx"
    );
}

/// req_d2: every count printed anywhere (the executive summary's `candidates_reviewed`, the
/// cover's severity totals, and the hedge/held-for-review counts) must reconcile against the
/// SAME sealed row set — never computed over a set an exporter did not actually render.
#[tokio::test]
async fn req_d2_candidates_reviewed_cover_totals_and_hedge_counts_reconcile_against_the_sealed_set()
{
    let (sealed, k) = build_seal_fixture().await;
    assert!(
        k > 0,
        "fixture must exercise the evidence gate — see the dedicated guard test"
    );

    let corpus_path = camerata_rules::corpus_path();
    let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");

    let total_rows = sealed.findings.len();
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&sealed, &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );

    // `candidates_reviewed` must equal the sealed set's total row count exactly — no
    // disposition in this fixture excludes anything as a false positive, and there are no
    // dependency-audit rows.
    assert_eq!(
        json.executive_summary.candidates_reviewed, total_rows,
        "candidates_reviewed must equal the sealed report's own finding count"
    );
    assert_eq!(json.executive_summary.excluded_false_positive, 0);
    assert_eq!(json.executive_summary.dependency_advisories, 0);

    // The cover's severity strip must tally exactly the same total as candidates_reviewed
    // (same identity `rows_in_json_equal_rendered_blocks_across_curated_and_held_for_review`
    // asserts for the standing fixture above — restated here against THIS sealed set).
    let cover_total = json.cover.stats.critical
        + json.cover.stats.high
        + json.cover.stats.medium
        + json.cover.stats.low;
    assert_eq!(
        cover_total, total_rows,
        "the cover severity strip must reconcile against the sealed row count exactly: \
             cover_total={cover_total} total_rows={total_rows}"
    );

    // Hedge count: the evidence-gate-synthesized row carries `needs_review: true` /
    // `confidence: needs-review`, so it must land in the `held` bucket, and the executive
    // summary's `held_for_review` reconciling count must include it.
    assert!(
        json.matrix
            .held
            .iter()
            .any(|f| f.rule_id == "ARCH-EXPLICIT-TX-1"),
        "the evidence-gate row must be a hedge (held bucket): {:?}",
        json.matrix.held
    );
    let held_for_review_total = json.matrix.informational.len() + json.matrix.held.len();
    assert_eq!(
        json.executive_summary.held_for_review, held_for_review_total,
        "held_for_review must equal informational.len() + held.len() over the sealed set"
    );
    assert!(
        json.executive_summary.held_for_review >= 1,
        "at least the evidence-gate row must be held for review"
    );

    // findings.json's `summary` is REUSED verbatim from the PDF's own `executive_summary`
    // (never independently recomputed — see `build_findings_export`'s doc comment), so it
    // is the SAME shared helper by construction; pin that explicitly so a future change
    // that makes it recompute its own summary independently is caught here.
    let findings_export = xlsx_export::build_findings_export(
        &crate::report_export::ReviewedReport::unreviewed(&sealed, &HashMap::new()),
        Some(&corpus),
        &json,
        &HashMap::new(),
    );
    assert_eq!(
        findings_export.summary.candidates_reviewed, total_rows,
        "findings.json's summary must reconcile against the same sealed row count"
    );
    assert_eq!(
        findings_export.summary.held_for_review, held_for_review_total,
        "findings.json's summary must report the SAME held-for-review count as the PDF, \
             never its own independently-recomputed one"
    );
}

/// req_c1: the final row count must equal the last ledger stage's `rows_out`, and the
/// ledger must account for row ADDITIONS (the external-tool-preview-merge and
/// evidence-gate-synthesis stages) exactly as it already accounts for deletions (every
/// pre-existing merge stage) — `total_unaccounted() == 0` across the whole scan.
#[tokio::test]
async fn req_c1_final_row_count_equals_last_ledger_stage_rows_out_additions_are_accounted() {
    let (sealed, k) = build_seal_fixture().await;
    assert!(
        k > 0,
        "fixture must exercise the evidence gate — see the dedicated guard test"
    );

    let stages = sealed.ledger.stages();
    assert!(
        stages.len() >= 2,
        "fixture must record at least the preview-merge AND the \
             evidence-gate-synthesis stages: {stages:#?}"
    );

    let last_stage = stages
        .last()
        .expect("at least one stage was just asserted present");
    assert_eq!(
        last_stage.stage, "evidence-gate-synthesis",
        "the evidence gate must be the LAST row-adding pass before the seal: {stages:#?}"
    );
    assert_eq!(
        last_stage.rows_out,
        sealed.findings.len(),
        "the final row count must equal the last ledger stage's rows_out exactly"
    );

    // Both addition-side stages must have disclosed their additions — the ledger's
    // addition-accounting is real, not merely present-but-empty.
    let preview_stage = stages
        .iter()
        .find(|s| s.stage == "external-tool-preview-merge")
        .expect("preview-merge stage must be present");
    assert!(
        preview_stage.added_total() > 0,
        "preview merge must disclose its additions"
    );
    assert!(
        last_stage.added_total() > 0,
        "evidence gate must disclose its additions"
    );
    assert_eq!(
        last_stage.added_total(),
        k,
        "the evidence-gate stage's disclosed addition count must equal the actual number of \
             synthesized rows"
    );

    // Fully reconciled: not one row, in either direction, is unaccounted anywhere in the
    // scan — the ledger genuinely explains every row it has ever seen appear or vanish.
    assert_eq!(
        sealed.ledger.total_unaccounted(),
        0,
        "a fully-instrumented scan must leave zero unaccounted rows, in either direction: \
             {:#?}",
        sealed.ledger.stages()
    );
}
