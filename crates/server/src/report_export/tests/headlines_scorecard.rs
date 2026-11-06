//! Defect headlines, the narrative, three things, categories, cover stats, the
//! scorecard and the merge counts.

use super::*;

#[test]
fn defect_headline_takes_the_first_sentence_of_detail() {
    assert_eq!(
        defect_headline(
            "The profiles table has no RLS. Anyone with the anon key can read and write \
                 every row.",
            "Every table has Row Level Security enabled"
        ),
        "The profiles table has no RLS."
    );
}

#[test]
fn defect_headline_falls_back_to_the_rule_title_when_detail_is_empty() {
    assert_eq!(
        defect_headline("", "Every table has Row Level Security enabled"),
        "Every table has Row Level Security enabled"
    );
}

// ── C4-P4 residual defect 1: headline truncation at an abbreviation (golden tests) ────

#[test]
fn defect_headline_does_not_truncate_inside_an_eg_parenthetical() {
    assert_eq!(
        defect_headline(
            "The handler accepts an insecure default (e.g. allowing GET requests) before \
                 validating the session. This is exploitable without authentication.",
            "fallback"
        ),
        "The handler accepts an insecure default (e.g. allowing GET requests) before \
             validating the session."
    );
}

#[test]
fn defect_headline_does_not_truncate_at_a_bare_eg_abbreviation() {
    assert_eq!(
        defect_headline(
            "Error handling is inconsistent, e.g. some handlers return 500 and others \
                 throw. Fix this before shipping.",
            "fallback"
        ),
        "Error handling is inconsistent, e.g. some handlers return 500 and others throw."
    );
}

#[test]
fn defect_headline_does_not_truncate_at_a_bare_ie_abbreviation() {
    assert_eq!(
        defect_headline(
            "This bypasses auth, i.e. anyone can reach the admin panel without a token. \
                 That is unacceptable.",
            "fallback"
        ),
        "This bypasses auth, i.e. anyone can reach the admin panel without a token."
    );
}

#[test]
fn defect_headline_does_not_truncate_at_an_etc_abbreviation() {
    assert_eq!(
        defect_headline(
            "The config accepts unsafe values like eval, exec, etc. without sanitization. \
                 This must be fixed.",
            "fallback"
        ),
        "The config accepts unsafe values like eval, exec, etc. without sanitization."
    );
}

#[test]
fn defect_headline_does_not_truncate_at_a_vs_abbreviation() {
    assert_eq!(
        defect_headline(
            "Credentials compare via string equality vs. constant-time comparison, which \
                 leaks timing info. Rotate the credential now.",
            "fallback"
        ),
        "Credentials compare via string equality vs. constant-time comparison, which leaks \
             timing info."
    );
}

#[test]
fn defect_headline_caps_by_length_with_an_ellipsis_when_no_real_sentence_boundary_exists() {
    let run_on = "a".repeat(250);
    let headline = defect_headline(&run_on, "fallback");
    assert!(
        headline.ends_with("..."),
        "a boundary-less run-on must be capped with an ellipsis: {headline}"
    );
    assert!(
        headline.chars().count() <= HEADLINE_LENGTH_CAP + 3,
        "the capped headline must not exceed the length cap plus the ellipsis: {headline}"
    );
}

#[test]
fn defect_headline_still_splits_on_a_genuine_non_abbreviation_period() {
    // Regression guard: the abbreviation/parenthetical carve-outs above must never swallow
    // a REAL sentence boundary that merely happens to sit near a parenthetical elsewhere in
    // the text.
    assert_eq!(
        defect_headline(
            "The profiles table has no RLS (confirmed via migration replay). Anyone with \
                 the anon key can read and write every row.",
            "fallback"
        ),
        "The profiles table has no RLS (confirmed via migration replay)."
    );
}

#[test]
fn sentence_boundary_skips_an_abbreviation_period_even_before_a_capitalized_continuation() {
    // "e.g." followed by a capitalized word looks EXACTLY like a real sentence boundary
    // (period + space + uppercase letter) to a naive scan — the abbreviation check must
    // still skip it, proving this isn't just the capitalization check doing the work.
    let s = "Error handling is inconsistent, e.g. Some handlers return 500 while others \
                 throw. Fix this before shipping.";
    let end = sentence_boundary(s).expect("a real boundary exists later in the string");
    assert_eq!(
        &s[..end],
        "Error handling is inconsistent, e.g. Some handlers return 500 while others throw."
    );
}

#[test]
fn curated_and_matrix_findings_carry_a_defect_headline_distinct_from_the_rule_title() {
    let mut f = finding("SEC-1", "a.rs", 1, "critical");
    f.detail = "A service_role key ships to every browser. It bypasses all Row Level \
                    Security."
        .to_string();
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );

    assert_eq!(
        json.curated_findings[0].sites[0].headline,
        "A service_role key ships to every browser."
    );
    assert_eq!(
        json.matrix.do_now[0].headline,
        "A service_role key ships to every browser."
    );
    // The rule's own title/id is still carried separately, for registry traceability.
    assert_eq!(json.curated_findings[0].rule_id, "SEC-1");
    // FIX 4 (2026-09-13 review): the "Three things this week" box leads with the headline,
    // not the rule id — this is now the ONE place the exec-summary page names a do-now
    // finding (see `narrative_does_not_restate_the_top_do_now_headline` below).
    assert!(json.three_things.items[0]
        .headline
        .starts_with("A service_role key ships"));
}

// ── Item 2 / FIX 4: exec-summary is counts-only, no restated top-3 ─────────

#[test]
fn narrative_does_not_restate_the_top_do_now_headline() {
    // FIX 4 (2026-09-13 review, "page 3 states the top-3 three times"): the narrative used
    // to LEAD with "As shipped: {top do-now headline}" — the same finding the "Three
    // things this week" box (and, before this fix, a "Top priority items" bullet list)
    // already names on the same page. The exec summary is now curation + counts only.
    let mut f = finding("SEC-1", "a.rs", 1, "critical");
    f.detail = "The profiles table has no RLS.".to_string();
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert!(
        !json.executive_summary.narrative.contains("As shipped"),
        "{}",
        json.executive_summary.narrative
    );
    assert!(
        !json
            .executive_summary
            .narrative
            .contains("The profiles table has no RLS."),
        "the narrative must not restate the do-now finding's own headline: {}",
        json.executive_summary.narrative
    );
}

#[test]
fn narrative_bucket_counts_are_a_one_pass_partition_with_no_subset_restatement() {
    // Previously: "... 2 do now, 2 do next, 1 planned, 1 accepted ... and 5 still open"
    // forced the reader to notice 5 was a SUBSET restatement of the four counts just
    // given, not new information. The fix: state the four-bucket partition once and stop.
    let f1 = finding("SEC-1", "a.rs", 1, "critical");
    let f2 = finding("SEC-2", "b.rs", 2, "low");
    let report = report_with(vec![f1, f2], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert!(
        !json.executive_summary.narrative.contains("still open"),
        "{}",
        json.executive_summary.narrative
    );
    assert!(
        json.executive_summary
            .narrative
            .contains("1 do now, 0 do next, 1 planned, 0 already carried as accepted risk"),
        "{}",
        json.executive_summary.narrative
    );
}

// ── Item 7: "If you only do three things this week" ────────────────────────

#[test]
fn effort_hours_bounds_maps_each_tier_to_a_labeled_rough_range() {
    assert_eq!(effort_hours_bounds(Some("low")).1, "2 to 4 hours");
    assert_eq!(
        effort_hours_bounds(Some("medium")).1,
        "1 to 2 days (about 8 to 16 hours)"
    );
    assert_eq!(
        effort_hours_bounds(Some("high")).1,
        "3 to 5 days (about 24 to 40 hours)"
    );
    assert_eq!(effort_hours_bounds(None).1, "not yet estimated");
}

#[test]
fn three_things_box_lists_up_to_three_do_now_items_with_hours_and_a_total() {
    let mut f1 = finding("SEC-1", "a.rs", 1, "critical");
    f1.effort = Some("low".to_string());
    f1.detail = "The profiles table has no RLS.".to_string();
    let mut f2 = finding("SEC-2", "b.rs", 2, "critical");
    f2.effort = Some("low".to_string());
    f2.detail = "The service_role key ships to the browser.".to_string();
    let report = report_with(vec![f1, f2], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );

    assert_eq!(json.three_things.items.len(), 2);
    assert_eq!(json.three_things.items[0].hours_label, "2 to 4 hours");
    assert_eq!(
        json.three_things.total_hours_label,
        "Roughly 4 to 8 hours total (rough estimate)."
    );
}

#[test]
fn three_things_box_flags_items_with_no_effort_estimate_instead_of_guessing() {
    // A do_now item can be uncalibrated (a deterministic-floor critical never gets a
    // calibrated effort, per M3) — the total must say so, never silently fold in a
    // guessed number.
    let f = finding("SEC-1", "a.rs", 1, "critical");
    assert_eq!(f.effort, None);
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert_eq!(json.three_things.items[0].hours_label, "not yet estimated");
    assert!(
        json.three_things
            .total_hours_label
            .contains("None of these items has a calibrated effort estimate"),
        "{}",
        json.three_things.total_hours_label
    );
}

#[test]
fn three_things_box_total_notes_partial_uncalibrated_items_without_guessing() {
    // Two items DO have effort; a third do_now item does not (a mixed run) — the total
    // must sum the calibrated ones and flag the uncalibrated one by count, not fold a
    // guessed number into the sum.
    let mut f1 = finding("SEC-1", "a.rs", 1, "critical");
    f1.effort = Some("low".to_string());
    let mut f2 = finding("SEC-2", "b.rs", 2, "critical");
    f2.effort = Some("low".to_string());
    let f3 = finding("SEC-3", "c.rs", 3, "critical"); // no effort estimate
    let report = report_with(vec![f1, f2, f3], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert_eq!(json.three_things.items.len(), 3);
    assert_eq!(
        json.three_things.total_hours_label,
        "Roughly 4 to 8 hours total (rough estimate); 1 item without an effort estimate yet \
             is not included in this total."
    );
}

#[test]
fn three_things_box_is_empty_when_no_do_now_items_exist() {
    let report = report_with(vec![], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert!(json.three_things.items.is_empty());
    assert_eq!(
        json.three_things.total_hours_label,
        "No do-now items this run."
    );
}

// ── C6-B2: top-3 "three distinct root causes" selector ─────────────────────────────────
// Defense in depth for the case the merge pass (`ai_audit::group_semantic` /
// `backfill_credential_identity`) conservatively leaves several sites of ONE leaked
// credential as separate rows: the top-3 box must still never spend more than one slot on
// them. These findings are deliberately fed to `build_report_json` WITHOUT running the merge
// pipeline first (report_export never merges on its own), so this specifically exercises the
// selector's OWN distinctness check, independent of whether the merge pass also caught it.

#[test]
fn three_things_box_shows_three_distinct_root_causes_not_three_sites_of_one_credential() {
    // Deliberately NOT "AI-"-prefixed: that tier goes through the separate P3 citation
    // gate (`is_ai_tier` / `is_uncited_ai_finding`), which is an orthogonal concern this
    // test is not about. Deterministic-shaped rule ids keep the fixture focused on the
    // root-cause distinctness check alone.
    let mut holder = finding("SEC-NO-HARDCODED-SECRETS-1", "config.ts", 5, "critical");
    holder.detail = "STRIPE_SECRET_KEY is committed in plaintext in this config file.".to_string();
    let mut constructor = finding(
        "SEC-STRIPE-CLIENT-FROM-LEAKED-KEY-1",
        "stripeClient.ts",
        12,
        "critical",
    );
    constructor.detail =
        "Builds a Stripe client directly from the committed STRIPE_SECRET_KEY.".to_string();
    let mut call_site = finding(
        "SEC-PAYMENT-CALL-USES-LEAKED-KEY-1",
        "charge.ts",
        40,
        "critical",
    );
    call_site.detail = "Issues a charge using the client built from STRIPE_SECRET_KEY.".to_string();
    let mut distinct_a = finding("SEC-UNRELATED-CRITICAL-1", "other1.ts", 1, "critical");
    distinct_a.detail = "Totally separate unauthenticated admin export.".to_string();
    let mut distinct_b = finding("SEC-UNRELATED-CRITICAL-2", "other2.ts", 1, "critical");
    distinct_b.detail = "A second, unrelated, totally distinct critical.".to_string();

    let report = report_with(
        vec![holder, constructor, call_site, distinct_a, distinct_b],
        vec![],
    );
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );

    assert_eq!(
        json.matrix.do_now.len(),
        5,
        "nothing is dropped from the queue — all 5 still ship as do-now rows: {:?}",
        json.matrix.do_now
    );
    assert_eq!(
        json.three_things.items.len(),
        3,
        "three slots still fill when enough DISTINCT candidates exist: {:?}",
        json.three_things.items
    );
    let top3_ids: std::collections::HashSet<&str> = json
        .three_things
        .items
        .iter()
        .map(|i| i.rule_id.as_str())
        .collect();
    assert!(
        top3_ids.contains("SEC-UNRELATED-CRITICAL-1")
            && top3_ids.contains("SEC-UNRELATED-CRITICAL-2"),
        "both genuinely distinct criticals must be in the top-3, never crowded out by one \
             root cause: {top3_ids:?}"
    );
    let credential_chain_ids = [
        "SEC-NO-HARDCODED-SECRETS-1",
        "SEC-STRIPE-CLIENT-FROM-LEAKED-KEY-1",
        "SEC-PAYMENT-CALL-USES-LEAKED-KEY-1",
    ];
    let credential_rows_selected = credential_chain_ids
        .iter()
        .filter(|id| top3_ids.contains(**id))
        .count();
    assert_eq!(
        credential_rows_selected, 1,
        "only ONE of the three same-credential rows may fill a top-3 slot, never three: \
             {top3_ids:?}"
    );
}

// ── Calibration-review fix: precondition tie-break in the do-now ranking ──────────
//
// The bug this guards: a High "unverified session read" finding whose own note recorded
// an exploit precondition was promoted to Critical by calibration and took a top-3
// do-now slot from a genuinely unconditional critical (an unauthenticated export
// exploitable with one request). `build_report_json`'s `do_now_sorted` must rank an
// unconditional critical ahead of a critical whose justification records a precondition,
// even though both are the same severity — pure RANKING, no bucket/severity change.

#[test]
fn three_things_ranks_the_unconditional_critical_ahead_of_a_preconditioned_one() {
    // Alphabetically "a.rs" would naturally sort first in the bucket's own (severity,
    // provenance, repo, path, line) ordering — deliberately putting the PRECONDITIONED
    // finding there proves the tie-break actually reorders rather than coincidentally
    // agreeing with path order.
    let mut preconditioned = finding("SEC-PRECONDITIONED", "a.rs", 1, "critical");
    preconditioned.detail =
        "Server code authorizes from an unverified session read; exploitation requires \
             forging a session cookie."
            .to_string();
    let mut unconditional = finding("SEC-UNCONDITIONAL", "b.rs", 2, "critical");
    unconditional.detail =
        "The export endpoint returns every user's records with no authorization check and \
             is reachable with a single unauthenticated request."
            .to_string();
    let report = report_with(vec![preconditioned, unconditional], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );

    assert_eq!(
        json.matrix.do_now.len(),
        2,
        "both are critical do-now items"
    );
    assert_eq!(
        json.three_things.items[0].rule_id, "SEC-UNCONDITIONAL",
        "the unconditional critical must rank first, ahead of the preconditioned one: {:?}",
        json.three_things.items
    );
    assert_eq!(json.three_things.items[1].rule_id, "SEC-PRECONDITIONED");
}

#[test]
fn three_things_order_is_unchanged_for_two_unconditional_criticals() {
    let f1 = finding("SEC-FIRST", "a.rs", 1, "critical");
    let f2 = finding("SEC-SECOND", "b.rs", 2, "critical");
    let report = report_with(vec![f1, f2], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );

    assert_eq!(json.matrix.do_now.len(), 2);
    assert_eq!(
        json.three_things.items[0].rule_id, "SEC-FIRST",
        "with neither finding preconditioned, today's (repo/path/line) order must hold: {:?}",
        json.three_things.items
    );
    assert_eq!(json.three_things.items[1].rule_id, "SEC-SECOND");
}

// ── S5: fallback categories are title-cased (not raw lowercase tokens) ─────

#[test]
fn category_fallback_is_title_cased_without_a_corpus() {
    let f = finding("SUPABASE-RLS-ENABLED-1", "a.sql", 1, "critical");
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    // A9: the label is the closed taxonomy's, not the rule id's own first two tokens.
    // Before A9 this read "Supabase RLS" — which happens to be readable only because
    // this rule id is domain-first; the SAME mechanism produced "Arch No" for
    // `ARCH-NO-CROSS-BOUNDARY-IMPORTS-1`. See `req_a9_*`.
    assert_eq!(json.scorecard.rows[0].category, "RLS Policy");
}

// ── A9: a category is never a slugified rule id ───────────────────────────

/// Rule ids whose own first two tokens are NOT a category. Each one is a real shape the
/// product emits: a native-checker id (`crates/checks`), an AI-tier id invented per
/// occurrence, and a rule that predates the taxonomy backfill. None has a corpus entry,
/// which is exactly the condition the old fallback mangled.
const A9_UNMAPPED_RULE_IDS: &[&str] = &[
    "ARCH-NO-CROSS-BOUNDARY-IMPORTS-1",
    "SEC-INJECTION-RAW-SQL-BUILD-1",
    "LEGACY-UNMAPPED-1",
];

/// req_a9: no category that reaches a client is the rule id's own tokens with title case
/// applied.
///
/// FAILS BEFORE THE CHANGE: `category_for`/`category_for_finding` both terminated in
/// `prettify_category_key(rule_id)`, so these three ids produced the scorecard categories
/// "Arch No", "Sec Injection" and "Legacy Unmapped" — and because `xlsx_export` names its
/// per-area worksheets from the category, "Arch No" was a client-facing Excel tab name.
/// Red output was three assertion failures of the form `scorecard category "Arch No" is
/// ARCH-NO-CROSS-BOUNDARY-IMPORTS-1 slugified`.
#[test]
fn req_a9_no_scorecard_category_is_a_slugified_rule_id() {
    let findings: Vec<Finding> = A9_UNMAPPED_RULE_IDS
        .iter()
        .enumerate()
        .map(|(i, id)| finding(id, &format!("src/a9_{i}.rs"), i + 1, "high"))
        .collect();
    let report = report_with(findings, A9_UNMAPPED_RULE_IDS.to_vec());
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );

    let categories: Vec<&str> = json
        .scorecard
        .rows
        .iter()
        .map(|r| r.category.as_str())
        .collect();
    assert!(
        !categories.is_empty(),
        "the fixture must produce scorecard rows, or this test proves nothing"
    );
    for rule_id in A9_UNMAPPED_RULE_IDS {
        let slug = prettify_category_key(rule_id);
        assert!(
            !categories.contains(&slug.as_str()),
            "scorecard category {slug:?} is {rule_id} slugified — a rule id is internal \
                 vocabulary and must never reach a client as a category. All rows: {categories:?}"
        );
    }
}

/// req_a9: when NO real category can be established — no corpus entry, no calibrated
/// `category`, nothing the closed taxonomy recognises — the label is ONE honest generic
/// bucket, not a per-rule invention.
///
/// FAILS BEFORE THE CHANGE: `LEGACY-UNMAPPED-1` and `VENDOR-THING-2` produced two
/// different mangled categories ("Legacy Unmapped" and "Vendor Thing") instead of both
/// landing in one.
#[test]
fn req_a9_unmappable_rule_ids_share_one_generic_category() {
    assert_eq!(
        category_for("LEGACY-UNMAPPED-1", None),
        UNCATEGORIZED_CATEGORY
    );
    assert_eq!(category_for("VENDOR-THING-2", None), UNCATEGORIZED_CATEGORY);
}

/// req_a9: a rule id the closed semantic taxonomy DOES recognise gets that taxonomy's
/// client label, not the generic bucket — the fallback must not be a blanket "Other" that
/// throws away a category we can actually establish.
#[test]
fn req_a9_taxonomy_recognised_rule_ids_get_their_real_category() {
    assert_eq!(
        category_for("ARCH-NO-CROSS-BOUNDARY-IMPORTS-1", None),
        "Architecture"
    );
    assert_eq!(
        category_for("SEC-INJECTION-RAW-SQL-BUILD-1", None),
        "Injection"
    );
    assert_eq!(
        category_for("ARCH-NO-SECRETS-IN-URL-1", None),
        "Secret Exposure"
    );
}

// ── req_det: the cover's generated-at is the scan time, not the wall clock ─

/// req_det: `cover.generated_at` is derived from the scan's own provenance, so building
/// the same report twice produces the same string. Everything downstream (the workbook's
/// Index row and `docProps/core.xml`, the PDF's creation timestamp) is pinned from this
/// one value, so this is the root of export reproducibility.
///
/// FAILS BEFORE THE CHANGE: `generated_at` was `chrono::Utc::now()`, so it read today's
/// date rather than the 2026-07-23 scan this fixture describes.
#[test]
fn req_det_cover_generated_at_is_the_scan_time() {
    let report = report_with(vec![finding("SEC-1", "a.rs", 1, "high")], vec![]);
    let sealed = report.clone().seal();
    let first = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&sealed, &HashMap::new()),
        None,
        &empty_opts(),
    );
    let second = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&sealed, &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert_eq!(
        first.cover.generated_at, "2026-07-23 00:05 UTC",
        "the cover must state when the AUDIT ran (provenance.finished_at), not when the \
             bytes were serialized"
    );
    assert_eq!(first.cover.generated_at, second.cover.generated_at);
}

/// req_det: `scan_instant` prefers `finished_at`, falls back to `started_at`, and reports
/// `None` only when neither is a real instant — the three cases every pinned timestamp
/// downstream depends on.
#[test]
fn req_det_scan_instant_prefers_finished_then_started_then_none() {
    let mut p = report_with(Vec::new(), Vec::new()).provenance;
    assert_eq!(
        scan_instant(&p).map(|t| t.timestamp()),
        chrono::DateTime::parse_from_rfc3339("2026-07-23T00:05:00Z")
            .ok()
            .map(|t| t.timestamp())
    );
    p.finished_at = String::new();
    assert_eq!(
        scan_instant(&p).map(|t| t.timestamp()),
        chrono::DateTime::parse_from_rfc3339("2026-07-23T00:00:00Z")
            .ok()
            .map(|t| t.timestamp()),
        "an unfinished/unstamped run must fall back to when it STARTED"
    );
    p.started_at = "not a timestamp".to_string();
    assert!(
        scan_instant(&p).is_none(),
        "neither field parseable must report None, never a silently invented instant"
    );
}

// ── S6: what's-healthy caps long lists and reports a remainder count ──────

/// W23 — a rule whose finding was ABSORBED by a merge is not "verified clean".
///
/// C5-1 already closed this for `also_matches` (a same-location demotion), and its
/// comment above `rule_ids_with_any_finding` explains exactly why: a rule that survived
/// only as an absorbed member still genuinely fired, so listing it as clean is a false
/// statement about the client's code. But `also_locations` — the merge-absorption
/// channel, one entry per absorbed FINDING rather than per rule id — was never folded in.
///
/// This is not hypothetical. On the 2026-10-18 hearth export `SEC-NO-SECRET-FILE-1`
/// (a committed `.env.production`, CRITICAL) appeared with `also_locations = 2` and no
/// direct row of its own, and `ARCH-OBSERVABILITY-SILENT-FAILURE-1` likewise. Both were
/// therefore eligible for "What's healthy" while their findings sat inside another row —
/// the report telling a client their committed secret is a verified-clean rule.
/// W28 — a row's `status` and its `disposition` must not contradict each other.
///
/// `classify` consults `finding.status` only in its `None`-wire arms. When a wire entry
/// EXISTS but carries a state this function does not recognise, the `_` arm returns
/// `Unresolved` — which renders as "Open (recommended: …)" — without ever looking at the
/// status that says the row is baseline-suppressed.
///
/// An unrecognised state is not a triage decision; it is a parse miss or a producer from
/// a different version. Treating it as "the auditor left this open" asserts a human
/// judgment that nobody made, and it contradicts the row's own status in the same export.
/// The 2026-10-16 sweep shipped 38 rows in exactly this shape.
///
/// A RECOGNISED state must still win: an auditor who explicitly confirmed or ignored a
/// baseline-suppressed row has made a real decision this session, and that is precisely
/// what a disposition is for.
/// W24 — "Three things this week" must name three distinct problems.
///
/// The distinctness check exists, and it works, but it is gated on
/// `!f.root_cause_keys.is_empty()`. A row carrying NO credential token therefore skips
/// the check entirely and is always selected, so three rows of the same rule — three
/// tables missing RLS, three endpoints missing the same guard — can fill all three slots.
///
/// For this box specifically that is wrong even though the rows are genuine. The box asks
/// the client to do three things this week; three instances of one rule are ONE thing
/// ("enable RLS on these tables"), and spending all three slots on it hides the other two
/// problems entirely. The graders saw exactly this: a top-3 listing one root cause three
/// times.
#[test]
fn req_w24_three_things_never_spends_all_slots_on_one_rule() {
    // Three rows of ONE rule, then two genuinely different problems. All do-now severity,
    // and none carries a root-cause credential token — the gap in the existing check.
    let mut findings = vec![
        finding("SUPABASE-RLS-ENABLED-1", "db/a.sql", 1, "critical"),
        finding("SUPABASE-RLS-ENABLED-1", "db/b.sql", 2, "critical"),
        finding("SUPABASE-RLS-ENABLED-1", "db/c.sql", 3, "critical"),
        finding("SEC-NO-SECRET-FILE-1", ".env.production", 1, "critical"),
        finding("ARCH-EXACT-DECIMALS-1", "db/pay.sql", 9, "critical"),
    ];
    for f in &mut findings {
        f.located = true;
    }
    let report = report_with(findings, vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );

    let picked: Vec<&str> = json
        .three_things
        .items
        .iter()
        .map(|i| i.rule_id.as_str())
        .collect();
    let distinct: std::collections::HashSet<&str> = picked.iter().copied().collect();
    assert_eq!(
        picked.len(),
        distinct.len(),
        "the three-things box must name three DIFFERENT problems; one rule filling \
             several slots hides the others: {picked:?}"
    );
    // And the other two problems must actually surface, which is the point of the box.
    assert!(
        distinct.contains("SEC-NO-SECRET-FILE-1") && distinct.contains("ARCH-EXACT-DECIMALS-1"),
        "the genuinely distinct problems must not be crowded out: {picked:?}"
    );
}

#[test]
fn req_w28_an_unrecognised_disposition_state_falls_back_to_status_not_to_open() {
    let mut suppressed = finding("SEC-1", "a.rs", 1, "high");
    suppressed.status = "suppressed-baseline".to_string();

    // No wire at all: already correct today, pinned so the fallback cannot regress.
    assert_eq!(classify(&suppressed, None), Disposition::BaselineAccepted);

    // A wire entry whose state this version does not know. Must NOT become Open.
    let unknown = DispositionWire {
        state: "SomeFutureState".to_string(),
        ..Default::default()
    };
    assert_eq!(
        classify(&suppressed, Some(&unknown)),
        Disposition::BaselineAccepted,
        "an unrecognised state is a parse miss, not a decision to leave the row open - \
             and reporting it Open contradicts the row's own baseline-suppressed status"
    );

    // An EMPTY state is the same case and the likelier one in practice.
    let empty = DispositionWire::default();
    assert_eq!(
        classify(&suppressed, Some(&empty)),
        Disposition::BaselineAccepted
    );

    // The converse, so the fix cannot swallow real triage: a RECOGNISED state still wins
    // over the status, because that is an actual human judgment this session.
    let confirmed = DispositionWire {
        state: "Confirmed".to_string(),
        ..Default::default()
    };
    assert_eq!(
        classify(&suppressed, Some(&confirmed)),
        Disposition::Confirmed,
        "an explicit triage decision must still override a baseline suppression"
    );

    // And a row with no suppression and an unknown state is genuinely Unresolved - the
    // fallback must not invent a disposition where the status offers none.
    let plain = finding("SEC-2", "b.rs", 2, "low");
    assert_eq!(classify(&plain, Some(&unknown)), Disposition::Unresolved);
}

#[test]
fn req_w23_a_rule_absorbed_into_another_rows_locations_is_never_verified_clean() {
    let mut carrier = finding("SEC-NO-VENDOR-TOKEN-1", ".env.production", 4, "critical");
    carrier.also_locations = vec![crate::onboard::MergedLocation::site(
        "owner/repo".to_string(),
        ".env.production".to_string(),
        1,
        "SEC-NO-SECRET-FILE-1".to_string(),
        "committed secret file".to_string(),
        false,
    )];

    // Both rules were audited. Only the carrier has a row of its own.
    let report = report_with(
        vec![carrier],
        vec![
            "SEC-NO-VENDOR-TOKEN-1",
            "SEC-NO-SECRET-FILE-1",
            "SEC-UNRELATED-1",
        ],
    );
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );

    let healthy: Vec<&str> = json
        .whats_healthy
        .rules
        .iter()
        .map(|r| r.rule_id.as_str())
        .collect();
    assert!(
        !healthy.contains(&"SEC-NO-SECRET-FILE-1"),
        "a rule absorbed into another row's `also_locations` still FIRED; listing it as \
             verified clean tells the client a real finding does not exist. healthy = {healthy:?}"
    );
    // The converse, so this cannot pass by emptying the section: a rule that genuinely
    // found nothing is still reported clean.
    assert!(
        healthy.contains(&"SEC-UNRELATED-1"),
        "a genuinely clean rule must still be listed: {healthy:?}"
    );
}

#[test]
fn whats_healthy_caps_at_ten_and_reports_further_clean_count() {
    let audited_owned: Vec<String> = (0..15).map(|i| format!("SEC-{i}")).collect();
    let audited: Vec<&str> = audited_owned.iter().map(String::as_str).collect();
    let report = report_with(vec![], audited);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert_eq!(json.whats_healthy.rules.len(), 10);
    assert_eq!(json.whats_healthy.further_clean_count, 5);
}

// ── M8: an unbounded snippet is capped, never claims unlimited page space ──

#[test]
fn snippet_is_capped_at_max_lines_with_a_truncated_marker() {
    let mut f = finding("SEC-1", "a.rs", 1, "high");
    f.snippet = (0..50)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    let capped = &json.curated_findings[0].sites[0].snippet;
    assert!(capped.lines().count() <= SNIPPET_MAX_LINES + 1, "{capped}");
    assert!(capped.contains("(truncated)"), "{capped}");
}

// ── Nice-to-have: cover stat strip + scorecard status ordering ─────────────

#[test]
fn cover_stats_summarize_severity_and_disposition_counts() {
    let critical = finding("SEC-1", "a.rs", 1, "critical");
    let high = finding("SEC-2", "b.rs", 2, "high");
    let ignored = finding("SEC-3", "c.rs", 3, "medium");
    let mut dep = finding(DEP_AUDIT_RULE_ID, "Cargo.lock", 0, "high");
    dep.snippet = "foo@1.0.0".to_string();
    let mut dispositions = HashMap::new();
    dispositions.insert(finding_key(&ignored), wire("Ignored", "known", ""));
    let report = report_with(vec![critical, high, ignored, dep], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &dispositions),
        None,
        &empty_opts(),
    );
    assert_eq!(json.cover.stats.critical, 1);
    assert_eq!(json.cover.stats.high, 1);
    assert_eq!(json.cover.stats.medium, 1);
    assert_eq!(json.cover.stats.accepted, 1);
    assert_eq!(json.cover.stats.dependency_advisories, 1);
}

// ── P1: deduplication + cross-tier merge, end-to-end through build_report_json ──────────
// the codebase-inspection hardening notes, P1. Runs the SAME two merge
// passes the real audit pipeline runs (`ai_audit::collapse_identical` then
// `ai_audit::group_semantic`) over a faktura-SHAPED synthetic fixture — never the
// real fixture, never a fixture-specific rule/file/table name — then builds the report and
// asserts the cover's counts reflect the MERGED, distinct-defect set.

/// The rows of a report whose rule id is `rule_id`.
fn report_rows_with<'a>(
    report: &'a crate::onboard::ScanReport,
    rule_id: &str,
) -> Vec<&'a crate::onboard::Finding> {
    report
        .findings
        .iter()
        .filter(|f| f.rule_id == rule_id)
        .collect()
}

#[tokio::test]
async fn p1_e2e_cover_counts_and_security_finding_survive_post_merge() {
    // Defect 1: one SQL-injection defect, flagged by BOTH a deterministic floor rule and an
    // AI rule at the exact same location (same file+line+snippet) — must collapse to one
    // critical row, not two.
    let sql_code = "SELECT * FROM t WHERE name ILIKE '%${name}%'";
    let mut det_sql = finding("SEC-NO-RAW-SQL-CONCAT-1", "api/query.ts", 10, "critical");
    det_sql.snippet = sql_code.to_string();
    let mut ai_sql = finding("AI-SQL-INJECTION", "api/query.ts", 10, "critical");
    ai_sql.snippet = sql_code.to_string();

    // Defect 2: a config flag and the handler that trusts it, in DIFFERENT files, tied
    // together only by a shared captured object — must collapse to one medium row.
    let mut config_flag = finding("CONFIG-FLAG-DEBUG-MODE", "config.rs", 5, "medium");
    config_flag
        .captures
        .insert("flag".to_string(), "debug_mode".to_string());
    let mut handler_flag = finding("AI-HANDLER-TRUSTS-DEBUG-FLAG", "handler.rs", 80, "medium");
    handler_flag
        .captures
        .insert("flag".to_string(), "debug_mode".to_string());

    // Defect 3: the canonical security-vs-structure overlap — a low-value structural rule
    // and a genuine AI security finding (reflected-origin CORS with credentials) at the
    // same line. The security finding must survive as the primary, never hidden.
    let cors_code = "app.use(cors())";
    let mut structural = finding("ARCH-MIDDLEWARE-FIRST-1", "middleware.ts", 12, "high");
    structural.snippet = cors_code.to_string();
    let mut security = finding(
        "AI-CORS-REFLECTED-ORIGIN-CREDENTIALS",
        "middleware.ts",
        12,
        "medium",
    );
    security.snippet = cors_code.to_string();

    let files = vec![
        ("api/query.ts".to_string(), sql_code.to_string()),
        ("middleware.ts".to_string(), cors_code.to_string()),
    ];
    let raw = vec![
        det_sql,
        ai_sql,
        config_flag,
        handler_flag,
        structural,
        security,
    ];

    // The real pipeline's two passes, in the real order: collapse, then group. EVERY row
    // goes to the exporter, not only the cluster heads: nothing is merged away, so six raw
    // findings are six rows (two rule families at one line are two rows, linked).
    let merged = crate::review_tree::collapse_and_group(raw, &files);
    assert_eq!(merged.len(), 6, "no row is dropped");

    // P3: the CORS security finding is an AI-tier rule id the corpus never saw (an
    // invented "AI-CORS-REFLECTED-ORIGIN-CREDENTIALS" id) — the citation gate only lets
    // it survive in `curated_findings` when its class maps to a REAL grounded corpus
    // rule (`SEC-NO-PERMISSIVE-CORS-CREDENTIALS-1`), which requires the real corpus to
    // be loaded rather than `None`.
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, corpus_errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(
        corpus_errors.is_empty(),
        "corpus must load cleanly, got errors: {corpus_errors:?}"
    );

    let report = report_with(merged, vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );

    // Cover critical count == the critical ROWS: the floor rule and the model's rule for the
    // SQL-injection defect are two rows, each critical, and each counts.
    assert_eq!(
        json.cover.stats.critical, 2,
        "both critical SQL-injection rows count, since neither is merged away"
    );

    // The CORS security finding must survive as its own curated row, never absorbed under
    // (or hidden behind) the structural rule id, and at medium severity or higher.
    let cors_group = json
        .curated_findings
        .iter()
        .find(|g| g.rule_id == "AI-CORS-REFLECTED-ORIGIN-CREDENTIALS")
        .expect("the security finding must survive the merge as the primary, not be hidden");
    assert!(
        matches!(
            cors_group.sites[0].severity.as_str(),
            "medium" | "high" | "critical"
        ),
        "the security finding must never be downgraded below medium: {}",
        cors_group.sites[0].severity
    );
    // The structural row at the same line is still its own row, linked to the security
    // finding that heads the pair.
    let structural = report_rows_with(&report, "ARCH-MIDDLEWARE-FIRST-1");
    assert_eq!(
        structural.len(),
        1,
        "the structural row survives as its own row"
    );
    assert_eq!(
        structural[0]
            .possible_duplicate_of
            .as_ref()
            .map(|l| l.hint.as_str()),
        Some("same_line")
    );
}

// ── C5-1: REG-1/REG-2 (merge primacy + severity) + coverage accounting ──────────────────
// docs: the deterministic/AI merge-primacy regression + the coverage-accounting bug it
// caused. Synthetic findings only.

/// The canonical REG-1/REG-2 scenario end to end: a deterministic floor rule (medium,
/// grounded, authored fix) and an AI-tier rule (critical, hedged, no fix of its own)
/// describe the SAME defect at the SAME site. Before C5-1, the AI row's inflated severity
/// won primacy (`primary_rank` ranked severity ahead of origin), demoting the clean
/// deterministic finding to `also_matches` and reporting it as "verified clean" in Coverage
/// (REG-1); the merged severity then inherited the AI member's inflated critical (REG-2).
/// After the fix: the deterministic rule is primary, the merged severity is its own grounded
/// "medium" (never the AI member's "critical"), its authored fix survives, and BOTH rule ids
/// count as matched in Coverage — neither is listed healthy/"verified clean".
#[tokio::test]
async fn c5_1_deterministic_primary_wins_merge_and_coverage_counts_both_rule_ids() {
    let code = "db.query(`DELETE FROM t WHERE id=${id}`)";
    let mut det = finding("SEC-DETERMINISTIC-SITE-1", "a.ts", 10, "medium");
    det.snippet = code.to_string();
    det.fix_specific = Some("Use a parameterized query via the shared `db` client.".to_string());
    let mut ai = finding("AI-SITE-DEFECT-1", "a.ts", 10, "critical");
    ai.snippet = code.to_string();
    ai.confidence = Some("needs-review".to_string());
    ai.needs_review = true;

    let files = vec![("a.ts".to_string(), code.to_string())];
    // Two rule families at one site are two rows now (a link, not a merge): each keeps its
    // own severity and the deterministic row heads the pair.
    let merged = crate::review_tree::collapse_and_group(vec![det, ai], &files);
    assert_eq!(
        merged.len(),
        2,
        "same-site findings of two rules are two rows"
    );
    let det_row = merged
        .iter()
        .find(|f| f.rule_id == "SEC-DETERMINISTIC-SITE-1")
        .unwrap();
    assert_eq!(
        det_row.severity, "medium",
        "the deterministic row keeps its own severity"
    );
    assert!(det_row.possible_duplicate_of.is_none(), "it heads the pair");
    let ai_row = merged
        .iter()
        .find(|f| f.rule_id == "AI-SITE-DEFECT-1")
        .unwrap();
    assert_eq!(
        ai_row.severity, "critical",
        "the AI row keeps its own severity"
    );
    assert_eq!(
        ai_row
            .possible_duplicate_of
            .as_ref()
            .map(|l| l.key.as_str()),
        Some(det_row.site_key.as_str()),
        "the lower-ranked row points at the deterministic one"
    );

    let report = report_with(merged, vec!["SEC-DETERMINISTIC-SITE-1", "AI-SITE-DEFECT-1"]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );

    let group = json
        .curated_findings
        .iter()
        .find(|g| g.rule_id == "SEC-DETERMINISTIC-SITE-1")
        .expect("the deterministic rule must survive as the curated primary");
    assert_eq!(group.sites.len(), 1);
    let site = &group.sites[0];
    assert_eq!(site.severity, "medium");
    assert_eq!(
        site.confidence.as_deref(),
        Some("high"),
        "an un-hedged deterministic finding resolves to high confidence"
    );
    assert_eq!(
        site.fix_for_this_finding.as_deref(),
        Some("Use a parameterized query via the shared `db` client."),
        "the deterministic primary's own authored fix must survive"
    );

    // Coverage: BOTH rule ids count as matched this run — neither is "verified clean".
    let healthy_ids: Vec<&str> = json
        .whats_healthy
        .rules
        .iter()
        .map(|r| r.rule_id.as_str())
        .collect();
    assert!(
        !healthy_ids.contains(&"SEC-DETERMINISTIC-SITE-1"),
        "the primary rule must never be listed as healthy/clean: {healthy_ids:?}"
    );
    assert!(
        !healthy_ids.contains(&"AI-SITE-DEFECT-1"),
        "the absorbed AI member must never be listed as healthy/clean either: {healthy_ids:?}"
    );

    let det_category = category_for("SEC-DETERMINISTIC-SITE-1", None);
    let ai_category = category_for("AI-SITE-DEFECT-1", None);
    let det_row = json
        .scorecard
        .rows
        .iter()
        .find(|r| r.category == det_category)
        .expect("the deterministic rule's category must have a scorecard row");
    assert!(
        det_row.audited_rules >= 1 && det_row.clean_rules == 0,
        "the primary must count as matched (>= 1), never clean: {det_row:?}"
    );
    let ai_row = json
        .scorecard
        .rows
        .iter()
        .find(|r| r.category == ai_category)
        .expect("the AI rule's own category must have a scorecard row");
    assert!(
        ai_row.audited_rules >= 1 && ai_row.clean_rules == 0,
        "the absorbed AI member must count as matched (>= 1) in Coverage, never \
             'verified clean': {ai_row:?}"
    );
}

/// Safe twin: two findings of DIFFERENT defect classes/categories at one site (line 0 —
/// uncited/file-level, so `collapse_identical`'s "unrelated file-level issues legitimately
/// share line 0" carve-out applies, and the categories are deliberately disjoint so
/// `group_semantic` has no shared signal either) must NEVER merge — they stay two
/// independent rows, and both are counted. Proves C5-1's primacy/severity changes only
/// affect findings that WERE already matched into one group; they never cause two distinct
/// defects to collapse into one.
#[test]
fn c5_1_safe_twin_different_defect_classes_at_one_site_stay_two_rows() {
    let mut a = finding("ARCH-NO-VERSIONING-1", "app.ts", 0, "medium");
    a.category = Some("api-layer".to_string());
    a.snippet = "no API versioning scheme is in use".to_string();
    let mut b = finding("AI-MISSING-RATE-LIMIT-1", "app.ts", 0, "low");
    b.category = Some("transport-security".to_string());
    b.snippet = "no rate limiting is configured".to_string();

    let merged = crate::review_tree::test_support::cluster_heads(
        crate::review_tree::test_support::collapse_located(vec![a, b], &[]),
        &[],
    );
    assert_eq!(
        merged.len(),
        2,
        "two findings of different defect classes at one site must stay two rows: {merged:?}"
    );

    let report = report_with(merged, vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    // Every code finding gets a `FindingRefJson` in exactly one matrix bucket regardless of
    // citation (the uncited-AI gate only changes WHICH bucket, never whether it's counted at
    // all) — so scanning every bucket is the general "both survive, nothing silently
    // dropped" check, unlike `curated_findings` alone (which the P3 citation gate can hold
    // an uncited AI-tier id out of entirely).
    let rule_ids: std::collections::HashSet<&str> = json
        .matrix
        .do_now
        .iter()
        .chain(json.matrix.do_next.iter())
        .chain(json.matrix.plan.iter())
        .chain(json.matrix.accepted.iter())
        .chain(json.matrix.informational.iter())
        .chain(json.matrix.held.iter())
        .map(|f| f.rule_id.as_str())
        .collect();
    assert!(rule_ids.contains("ARCH-NO-VERSIONING-1"));
    assert!(rule_ids.contains("AI-MISSING-RATE-LIMIT-1"));
}

/// Cover-count invariant: however many raw findings merge into however many real defects,
/// the cover's critical count must equal the number of critical rows ACTUALLY RENDERED in
/// the report body — never the raw pre-merge count. Two real critical defects, each
/// independently double-reported (a deterministic floor rule + a duplicate AI rule at the
/// same site) — 4 raw findings, 2 real defects — must land on a critical count of 2.
#[tokio::test]
async fn c5_1_cover_critical_count_equals_rendered_critical_rows_after_merge() {
    // Deliberately disjoint structural objects (`accountsRepo` vs `sessionsRepo`) and
    // distinct detail prose, so the two sites are genuinely two SEPARATE defects — no
    // cross-file semantic signal (shared captured object / description overlap / same-rule
    // same-file) should fuse them together; only each site's OWN deterministic+AI pair
    // merges via the exact-location pass.
    let code_a = "accountsRepo.query(`DELETE FROM accounts WHERE id=${id}`)";
    let mut det_a = finding("SEC-NO-RAW-SQL-CONCAT-1", "a.ts", 10, "critical");
    det_a.snippet = code_a.to_string();
    det_a.detail = "Unsanitized input flows into a raw SQL delete against accounts.".to_string();
    let mut ai_a = finding("AI-SQL-INJECTION-A", "a.ts", 10, "critical");
    ai_a.snippet = code_a.to_string();
    ai_a.detail = det_a.detail.clone();

    let code_b = "sessionsRepo.execute(`DELETE FROM sessions WHERE token=${t}`)";
    let mut det_b = finding("SEC-NO-RAW-SQL-CONCAT-1", "b.ts", 20, "critical");
    det_b.snippet = code_b.to_string();
    det_b.detail = "Unsanitized input flows into a raw SQL delete against sessions.".to_string();
    let mut ai_b = finding("AI-SQL-INJECTION-B", "b.ts", 20, "critical");
    ai_b.snippet = code_b.to_string();
    ai_b.detail = det_b.detail.clone();

    let files = vec![
        ("a.ts".to_string(), code_a.to_string()),
        ("b.ts".to_string(), code_b.to_string()),
    ];
    let raw = vec![det_a, ai_a, det_b, ai_b];
    // Every row goes to the exporter: four findings are four rows.
    let merged = crate::review_tree::collapse_and_group(raw, &files);
    assert_eq!(merged.len(), 4, "no row is merged away: {merged:?}");

    let report = report_with(merged, vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );

    // Every row is in exactly one matrix bucket, whether or not the citation gate also
    // curates it, so the rows rendered in the body are the matrix rows.
    let rendered_critical_rows = json
        .matrix
        .do_now
        .iter()
        .chain(&json.matrix.do_next)
        .chain(&json.matrix.plan)
        .chain(&json.matrix.accepted)
        .chain(&json.matrix.informational)
        .chain(&json.matrix.held)
        .filter(|r| r.severity == "critical")
        .count();
    assert_eq!(
        json.cover.stats.critical, rendered_critical_rows,
        "the cover's critical count must equal the number of critical rows actually \
             rendered in the body"
    );
    assert_eq!(
        json.cover.stats.critical, 4,
        "each of the four critical rows counts once: nothing is merged away"
    );
}

/// Invariant: a row the `is_informational` gate routes to the appendix (testing-style/
/// absence-type/info-tier — the three signals in that function's doc comment, since C5-4
/// moved the former needs-review §2c signal to its own severity-unbounded `is_held_for_review`
/// gate/`held` bucket) NEVER carries a critical/high severity badge, by the hard invariant at
/// the top of `is_informational` itself. Pinned here as an explicit regression test over
/// `build_report_json`'s actual output, scoped to those three signals specifically: the
/// SEPARATE, pre-existing, and deliberately severity-blind `is_uncited_ai_finding` gate
/// (see that function's own doc comment — "a critical, uncited finding is PRECISELY the
/// case this gate exists to catch") is an intentional report-integrity carve-out, not part
/// of this invariant, and is untouched by C5-1.
#[test]
fn c5_1_is_informational_routed_rows_never_exceed_medium_severity() {
    // One finding that genuinely routes to the informational appendix via a remaining
    // `is_informational` signal (the `info` tier itself), and one SEPARATE needs-review
    // finding (medium severity) proving C5-4's split: it now routes to `held`, NOT
    // `informational` — the inverse of the old (regressed) behavior this test used to pin.
    let mut hedged_medium = finding("ARCH-DEBATABLE-1", "a.rs", 1, "medium");
    hedged_medium.confidence = Some("needs-review".to_string());
    hedged_medium.needs_review = true;
    let info_tier = finding("ARCH-INFO-NOTE-1", "b.rs", 2, "info");

    let report = report_with(vec![hedged_medium, info_tier], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert_eq!(
        json.matrix.informational.len(),
        1,
        "only the info-tier finding routes to the informational appendix: {:?}",
        json.matrix.informational
    );
    for row in &json.matrix.informational {
        assert!(
            severity_rank(&row.severity) >= severity_rank("medium"),
            "an is_informational-routed row must never exceed medium severity: {row:?}"
        );
    }
    // C5-4: the needs-review medium finding routes to `held`, never `informational` —
    // severity is preserved (not downgraded), unlike an `informational` row, and it is
    // never dropped.
    assert_eq!(
        json.matrix.held.len(),
        1,
        "the needs-review finding must route to held, not informational: {:?}",
        json.matrix.held
    );
    assert_eq!(json.matrix.held[0].rule_id, "ARCH-DEBATABLE-1");
    assert_eq!(
        json.matrix.held[0].severity, "medium",
        "held rows keep their real severity, never capped"
    );
}

#[test]
fn scorecard_rows_are_ordered_action_needed_before_clean() {
    // A9: the two rule ids must land in DIFFERENT categories for this test to have two
    // rows to order. Before A9 any two distinct ids did, because each was slugified into
    // its own category; now an id the taxonomy cannot place falls into the shared generic
    // bucket, so the fixture names two ids the taxonomy genuinely separates (the old
    // `ZZZ-1`/`AAA-1` both resolve to "Other" and collapse into one row).
    let action_needed = finding("SEC-NO-HARDCODED-SECRETS-1", "a.rs", 1, "critical");
    let clean_category_finding = finding("PERF-N-PLUS-ONE-1", "b.rs", 2, "critical");
    let mut dispositions = HashMap::new();
    // The AAA-1 finding is accepted, so its category's status is "Clean" (no OPEN
    // high/critical exposure) even though a critical finding exists in it.
    dispositions.insert(
        finding_key(&clean_category_finding),
        wire("Ignored", "accepted", ""),
    );
    let report = report_with(vec![action_needed, clean_category_finding], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &dispositions),
        None,
        &empty_opts(),
    );
    assert_eq!(json.scorecard.rows[0].status, "Action-needed");
    assert_eq!(json.scorecard.rows[0].category, "Secret Exposure");
    assert_eq!(json.scorecard.rows[1].status, "Clean");
}

/// C4-P4 residual defect 6 (verification): a category with >= 1 finding must show "rules
/// checked" >= 1, even when the firing rule id was never in the pre-declared
/// `audited_rule_ids` list (an AI-tier finding invents its own id; a rule can also just not
/// be pre-declared) — `category_for_finding`'s "declared-audited UNION distinct-fired-rule-
/// ids" derivation closes that gap. This is a general, corpus-free regression pin for the
/// same contract `xlsx_export::ai_tier_finding_citation_title_and_category_are_consistent_
/// and_grounded` already exercises end-to-end against the real corpus; confirmed STILL
/// fixed as of this residual-defects pass — see that test's own
/// `cat_row.audited_rules >= 1` assertion.
#[test]
fn scorecard_rules_checked_is_at_least_one_for_a_category_whose_rule_was_never_pre_declared() {
    let f = finding("SOME-UNDECLARED-RULE-1", "a.rs", 1, "critical");
    let report = report_with(vec![f], vec![]); // audited_rule_ids deliberately empty.
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    let row = json
        .scorecard
        .rows
        .iter()
        .find(|r| r.critical == 1)
        .expect("the finding's category must appear in the scorecard");
    assert!(
        row.audited_rules >= 1,
        "a category with >= 1 finding must show >= 1 rules checked, got {}",
        row.audited_rules
    );
}

/// Review of R1-2: a same-id preview merge records which tool also saw the row, but that
/// provenance is not a rule id, so the scorecard's "rules checked" count is unchanged.
#[test]
fn a_same_id_preview_merge_does_not_change_the_scorecard_rule_counts() {
    let id = "SEC-NO-HARDCODED-SECRETS-1";
    let counts = |merged: bool| {
        let mut rows = vec![finding(id, "a.rs", 1, "critical")];
        if merged {
            let mut preview = finding(id, "a.rs", 1, "critical");
            preview.preview = true;
            preview.preview_tool = Some("camerata-arch".to_string());
            let out = crate::dedup_scan_previews(&mut rows, vec![preview]);
            assert!(out.is_empty(), "merged into the existing row");
        }
        let report = report_with(rows, vec![id]);
        let json = build_report_json(
            &crate::report_export::ReviewedReport::unreviewed(
                &report.clone().seal(),
                &HashMap::new(),
            ),
            None,
            &empty_opts(),
        );
        json.scorecard
            .rows
            .iter()
            .map(|r| (r.category.clone(), r.audited_rules, r.clean_rules))
            .collect::<Vec<_>>()
    };
    assert_eq!(counts(true), counts(false));
}

#[test]
fn scorecard_includes_a_clean_row_for_an_audited_category_with_zero_findings() {
    // An audited category with NO real findings at all (every hit was a false
    // positive, or the rule simply never fired) must still get a "Clean" scorecard row
    // — otherwise a fully clean category is silently invisible instead of visibly
    // reading "Clean", and there is nothing for the Action-needed -> Clean ordering to
    // place at the bottom.
    // A9: two ids the taxonomy genuinely separates, for the same reason as
    // `scorecard_rows_are_ordered_action_needed_before_clean` — the old `ZZZ-1`/`AAA-1`
    // now share the generic bucket and would collapse these two rows into one.
    let flagged = finding("SEC-NO-HARDCODED-SECRETS-1", "a.rs", 1, "critical");
    let clean_only = finding("PERF-N-PLUS-ONE-1", "b.rs", 2, "high");
    let mut dispositions = HashMap::new();
    dispositions.insert(
        finding_key(&clean_only),
        wire("FalsePositive", "not exploitable", ""),
    );
    let report = report_with(
        vec![flagged, clean_only],
        vec!["SEC-NO-HARDCODED-SECRETS-1", "PERF-N-PLUS-ONE-1"],
    );
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &dispositions),
        None,
        &empty_opts(),
    );
    assert_eq!(json.scorecard.rows.len(), 2, "{:?}", json.scorecard.rows);
    let clean_row = json
        .scorecard
        .rows
        .iter()
        .find(|r| r.category == "Performance")
        .expect("the clean rule's category must still get a row despite zero real findings");
    assert_eq!(clean_row.status, "Clean");
    assert_eq!(clean_row.audited_rules, 1);
    assert_eq!(clean_row.clean_rules, 1);
    assert_eq!(
        clean_row.critical + clean_row.high + clean_row.medium + clean_row.low,
        0
    );
}

// ── Suppressed-baseline -> pre-existing accepted debt ──────────────────────
