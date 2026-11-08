//! Brand and prepared-by, the priority grid, business impact, next steps, the
//! shipped template, the Typst budget and review state.

use super::*;

#[test]
fn resolve_brand_prefers_the_per_report_field_over_env() {
    let mut opts = empty_opts();
    opts.brand = "Cantus Works".to_string();
    assert_eq!(
        resolve_brand(&opts, Some("Some Other Agency")),
        Some("Cantus Works".to_string())
    );
}

#[test]
fn resolve_brand_falls_back_to_env_when_the_report_field_is_blank() {
    let opts = empty_opts();
    assert_eq!(
        resolve_brand(&opts, Some("Cantus Works")),
        Some("Cantus Works".to_string())
    );
    // Whitespace-only counts as "not supplied" too.
    let mut whitespace_opts = empty_opts();
    whitespace_opts.brand = "   ".to_string();
    assert_eq!(
        resolve_brand(&whitespace_opts, Some("Cantus Works")),
        Some("Cantus Works".to_string())
    );
}

#[test]
fn resolve_brand_is_none_when_neither_the_field_nor_env_is_set() {
    let opts = empty_opts();
    assert_eq!(resolve_brand(&opts, None), None);
    assert_eq!(resolve_brand(&opts, Some("")), None);
    assert_eq!(resolve_brand(&opts, Some("   ")), None);
}

#[test]
fn resolve_prepared_by_follows_the_same_precedence() {
    let mut opts = empty_opts();
    opts.prepared_by = "Zachary Ernst".to_string();
    assert_eq!(
        resolve_prepared_by(&opts, Some("Someone Else")),
        "Zachary Ernst"
    );
    let empty = empty_opts();
    assert_eq!(
        resolve_prepared_by(&empty, Some("Someone Else")),
        "Someone Else"
    );
    // P6: with neither the per-report field nor the env var set, the resolver falls all
    // the way to the real default name, never an empty string ("Prepared by N/A" is the
    // bug this closes).
    assert_eq!(resolve_prepared_by(&empty, None), DEFAULT_PREPARED_BY);
}

#[test]
fn cover_brand_is_some_when_report_options_brand_is_set() {
    let f = finding("SEC-1", "a.rs", 1, "low");
    let report = report_with(vec![f], vec![]);
    let mut opts = empty_opts();
    opts.brand = "Cantus Works".to_string();
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &opts,
    );
    assert_eq!(json.cover.brand, Some("Cantus Works".to_string()));
}

#[test]
fn cover_brand_is_none_with_no_brand_option_set_the_neutral_fallback_case() {
    // `build_report_json` never reads the `CAMERATA_REPORT_BRAND` env var itself (see
    // `ReportOptions::apply_env_defaults`'s doc comment) — with a blank `opts.brand` (the
    // state an un-mutated `ReportOptions` is always in), `cover.brand` must be `None`, the
    // exact case the template renders as `NEUTRAL_COVER_TITLE` with no agency name and no
    // "Camerata" on the cover.
    let f = finding("SEC-1", "a.rs", 1, "low");
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert_eq!(json.cover.brand, None);
}

#[test]
fn neutral_cover_title_constant_matches_the_documented_fallback_string() {
    assert_eq!(NEUTRAL_COVER_TITLE, "Codebase Inspection Report");
}

#[test]
#[serial_test::serial(process_env)]
fn apply_env_defaults_fills_blank_fields_only() {
    // Real env mutation, scoped to this one test and restored by the `EnvGuard`s on drop —
    // every other precedence case above goes through the pure `resolve_brand`/
    // `resolve_prepared_by` helpers instead specifically to avoid relying on process-global
    // env state. `cargo test` runs this crate's tests on threads of one process, and other
    // tests (the product-export / client-artifact ones) DO read these vars through
    // `apply_env_defaults`, so every test that touches them carries the shared
    // `process_env` serial key.
    let _env = camerata_test_env::EnvGuard::set(REPORT_BRAND_ENV, "Env Agency");
    let _env2 = camerata_test_env::EnvGuard::set(REPORT_PREPARED_BY_ENV, "Env Preparer");

    let mut blank = ReportOptions::default();
    blank.apply_env_defaults();
    assert_eq!(blank.brand, "Env Agency");
    assert_eq!(blank.prepared_by, "Env Preparer");

    let mut already_set = ReportOptions {
        brand: "Cantus Works".to_string(),
        prepared_by: "Zachary Ernst".to_string(),
        ..Default::default()
    };
    already_set.apply_env_defaults();
    assert_eq!(already_set.brand, "Cantus Works");
    assert_eq!(already_set.prepared_by, "Zachary Ernst");
}

// ── FIX 6: the real severity x effort priority grid ─────────────────────────

#[test]
fn priority_grid_places_findings_in_the_right_severity_by_effort_cell() {
    let mut critical_low = finding("SUPABASE-RLS-ENABLED-1", "a.sql", 1, "critical");
    critical_low.effort = Some("low".to_string());
    let mut high_medium = finding("SUPABASE-AUTH-EDGE-JWT-1", "b.toml", 2, "high");
    high_medium.effort = Some("medium".to_string());
    let mut high_uncalibrated = finding("ARCH-1", "c.rs", 3, "high");
    high_uncalibrated.effort = None; // deterministic-floor: never calibrated
    let mut low_sev = finding("STYLE-1", "d.rs", 4, "low");
    low_sev.effort = Some("low".to_string());

    let report = report_with(
        vec![critical_low, high_medium, high_uncalibrated, low_sev],
        vec![],
    );
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    let grid = &json.priority_grid;

    // Row/column presence: critical, high, low severities all appear (all land in an
    // action tier); low/medium/unscoped effort columns all appear.
    assert!(grid.rows.iter().any(|r| r.severity == "critical"));
    assert!(grid.rows.iter().any(|r| r.severity == "high"));
    assert!(grid.rows.iter().any(|r| r.severity == "low"));
    assert!(grid.columns.contains(&"low".to_string()));
    assert!(grid.columns.contains(&"medium".to_string()));
    assert!(grid.columns.contains(&"unscoped".to_string()));

    let cell = |sev: &str, eff: &str| -> &GridCellJson {
        let row = grid.rows.iter().find(|r| r.severity == sev).unwrap();
        let col_idx = grid.columns.iter().position(|c| c == eff).unwrap();
        &row.cells[col_idx]
    };

    assert_eq!(cell("critical", "low").findings.len(), 1);
    assert_eq!(
        cell("critical", "low").findings[0].rule_id,
        "SUPABASE-RLS-ENABLED-1"
    );
    // `high_medium` is do_next (high severity, medium effort is not "low"); still an
    // ACTION-tier finding, so it must still land in the grid.
    assert_eq!(cell("high", "medium").findings.len(), 1);
    assert_eq!(
        cell("high", "medium").findings[0].rule_id,
        "SUPABASE-AUTH-EDGE-JWT-1"
    );
    // An uncalibrated effort normalizes to the "unscoped" column, never dropped.
    assert_eq!(cell("high", "unscoped").findings.len(), 1);
    assert_eq!(cell("high", "unscoped").findings[0].rule_id, "ARCH-1");
    assert_eq!(cell("low", "low").findings.len(), 1);
    assert_eq!(cell("low", "low").findings[0].rule_id, "STYLE-1");
}

#[test]
fn priority_grid_excludes_accepted_and_informational_but_counts_them() {
    let mut accepted = finding("SEC-1", "a.rs", 1, "high");
    accepted.effort = Some("low".to_string());
    let info = finding("SOME-RULE-1", "b.rs", 2, "info");
    let mut dispositions = HashMap::new();
    dispositions.insert(
        finding_key(&accepted),
        wire("Ignored", "accepted for now", ""),
    );
    let report = report_with(vec![accepted, info], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &dispositions),
        None,
        &empty_opts(),
    );

    // Neither finding appears in any grid cell.
    for row in &json.priority_grid.rows {
        for cell in &row.cells {
            assert!(
                cell.findings
                    .iter()
                    .all(|f| f.rule_id != "SEC-1" && f.rule_id != "SOME-RULE-1"),
                "accepted/informational findings must never appear in the priority grid"
            );
        }
    }
    assert_eq!(json.priority_grid.accepted_count, 1);
    assert_eq!(json.priority_grid.informational_count, 1);
}

#[test]
fn priority_grid_is_empty_when_no_action_tier_findings_exist() {
    let report = report_with(vec![], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert!(json.priority_grid.rows.is_empty());
    assert!(json.priority_grid.columns.is_empty());
}

// ── FIX 7: business-impact map ───────────────────────────────────────────────

#[test]
fn business_impact_is_authored_for_the_sample_fixture_do_now_rules() {
    for rule_id in [
        "SUPABASE-RLS-ENABLED-1",
        "SUPABASE-KEY-SERVICE-ROLE-CLIENT-1",
        "SUPABASE-STORAGE-PUBLIC-BUCKET-1",
    ] {
        assert!(
            business_impact_for_rule(rule_id).is_some(),
            "{rule_id} should have an authored business-impact sentence"
        );
    }
}

#[test]
fn business_impact_is_none_for_an_unauthored_rule_never_fabricated() {
    assert_eq!(business_impact_for_rule("SOME-RULE-NEVER-AUTHORED-1"), None);
}

#[test]
fn three_things_items_carry_the_authored_impact_when_present() {
    let mut f = finding("SUPABASE-RLS-ENABLED-1", "a.sql", 1, "critical");
    f.effort = Some("low".to_string());
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert_eq!(
        json.three_things.items[0].impact,
        business_impact_for_rule("SUPABASE-RLS-ENABLED-1").map(str::to_string)
    );
}

#[test]
fn three_things_items_impact_is_none_for_an_unauthored_rule() {
    let f = finding("ARCH-1", "a.rs", 1, "critical");
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert_eq!(json.three_things.items[0].impact, None);
}

// ── FIX 3 / C4-P4 residual defect 4: Next steps + the fix rate ─────────────────

#[test]
fn methodology_next_steps_reports_no_fix_rate_for_a_zero_candidate_run() {
    let report = report_with(vec![], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert!(!json.methodology.next_steps.is_empty());
    assert!(
        json.methodology
            .next_steps
            .contains("no candidate findings, so there is no fix rate to report"),
        "a clean run must honestly say there is no fix rate, never fabricate one or divide \
             by zero: {}",
        json.methodology.next_steps
    );
    assert!(!json.methodology.next_steps.contains('%'));
}

/// Public promise 2 ("the fix rate is written into the report before the client decides"):
/// the next-steps paragraph must contain a real NUMBER, never the old placeholder sentence
/// ("at the rate set out in the engagement").
#[test]
fn methodology_next_steps_contains_a_numeric_fix_rate_not_the_placeholder_sentence() {
    let f1 = finding("SEC-1", "a.rs", 1, "critical");
    let f2 = finding("SEC-2", "b.rs", 2, "high");
    let f3 = finding("SEC-3", "c.rs", 3, "medium");
    let f4 = finding("SEC-4", "d.rs", 4, "low");
    let mut dispositions = HashMap::new();
    dispositions.insert(
        finding_key(&f4),
        wire("FalsePositive", "generated fixture", ""),
    );
    let report = report_with(vec![f1, f2, f3, f4], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &dispositions),
        None,
        &empty_opts(),
    );

    assert_eq!(json.methodology.candidates_reviewed, 4);
    assert_eq!(json.methodology.excluded_false_positive, 1);

    let notes = &json.methodology.next_steps;
    assert!(
        !notes.contains("at the rate set out in the engagement"),
        "the old placeholder sentence must be gone: {notes}"
    );
    // 3 of 4 kept -> 75% fix rate.
    assert!(
        notes.contains("kept 3 of 4 candidate findings"),
        "the paragraph must state the real kept/total counts: {notes}"
    );
    assert!(
        notes.contains("75% fix rate"),
        "the paragraph must state the computed numeric fix rate: {notes}"
    );
    let has_digit = notes.chars().any(|c| c.is_ascii_digit());
    assert!(
        has_digit,
        "the next-steps text must contain a number: {notes}"
    );
}

#[test]
fn next_steps_note_singular_finding_noun_at_exactly_one_candidate() {
    let note = next_steps_note(ReviewState::Reviewed, 1, 1, 0, 0);
    assert!(
        note.contains("kept 1 of 1 candidate finding as"),
        "a single candidate must use the singular noun, not 'findings': {note}"
    );
}

// ── C5-6 (REG-6): fix-rate sentence must be true or absent ─────────────────────────

/// Draft export, nothing excluded yet, some rows held for review: no percentage must ever
/// appear, and the held count must be named so the backlog isn't invisible.
#[test]
fn next_steps_note_draft_with_held_rows_names_the_held_count_with_no_percentage() {
    let note = next_steps_note(ReviewState::Raw, 23, 0, 23, 0);
    assert!(
        !note.contains('%'),
        "a draft export must never print a fix-rate percentage: {note}"
    );
    assert!(
        !note.contains("real, actionable"),
        "held rows on a draft export must never be called real, actionable items: {note}"
    );
    assert!(
        note.contains("Fix rate not yet established"),
        "a draft export must say the fix rate is not yet established: {note}"
    );
    assert!(
        note.contains("23 of 23 candidates are held for a reviewer's decision"),
        "the held count must be named plainly: {note}"
    );
    assert!(
        note.contains("0 excluded so far"),
        "a draft export has excluded nothing yet, and must say so: {note}"
    );
}

/// Reviewed export, 17 curated and 3 excluded (0 held): the fix rate is curated / (curated
/// + excluded) = 85%, and the exclusion clause appears because something was excluded.
#[test]
fn next_steps_note_reviewed_with_exclusions_shows_percentage_and_exclusion_clause() {
    let note = next_steps_note(ReviewState::Reviewed, 20, 17, 0, 3);
    assert!(
        note.contains("17 of 20") && note.contains("85%"),
        "the fix rate must be curated/(curated+excluded) = 17/20 = 85%: {note}"
    );
    assert!(
        note.contains("excluding the rest as likely false positives"),
        "excluded > 0 must surface the exclusion clause: {note}"
    );
}

/// Reviewed export, 20 curated and 0 excluded: still a percentage (100%), but the
/// "excluding the rest" clause must not appear since nothing was excluded.
#[test]
fn next_steps_note_reviewed_with_no_exclusions_omits_the_exclusion_clause() {
    let note = next_steps_note(ReviewState::Reviewed, 20, 20, 0, 0);
    assert!(
        note.contains("20 of 20") && note.contains("100%"),
        "zero excluded still yields a (100%) fix rate over curated candidates: {note}"
    );
    assert!(
        !note.contains("excluding the rest"),
        "excluded == 0 must never print the exclusion clause: {note}"
    );
}

// ── W6: a failed/timed-out pass must be disclosed, never omitted silently ──────

#[test]
fn a_failed_pass_renders_the_not_computed_disclosure_in_both_methodology_and_summary() {
    let mut report = report_with(vec![], vec![]);
    report.failed_passes = vec![crate::ai_audit::FailedPass {
        repo: "acme/widgets".to_string(),
        pass: "rule-alternative recommendations".to_string(),
        reason: "Claude CLI timed out after 300s (no output).".to_string(),
        files_not_reviewed: 0,
        class: crate::ai_audit::FailureClass::Content,
        files: Vec::new(),
        attempts: Vec::new(),
        scope_note: false,
    }];
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &internal_opts(),
    );

    assert_eq!(json.methodology.failed_passes.len(), 1);
    assert!(
        json.methodology.failed_passes[0].contains("not computed"),
        "methodology must explicitly state the pass was not computed: {:?}",
        json.methodology.failed_passes
    );
    assert!(
        json.methodology.failed_passes[0].contains("acme/widgets"),
        "methodology disclosure must name the affected repo: {:?}",
        json.methodology.failed_passes
    );
    assert!(
        json.methodology.failed_passes[0].contains("Claude CLI timed out after 300s"),
        "methodology disclosure must carry the real reason verbatim: {:?}",
        json.methodology.failed_passes
    );

    // The SAME disclosure also renders in the executive summary — never JUST the
    // methodology (a reader of only the summary must not be left in the dark either).
    assert_eq!(
        json.executive_summary.failed_passes, json.methodology.failed_passes,
        "the summary and methodology must carry the identical disclosure text"
    );
}

#[test]
fn no_failed_pass_means_no_not_computed_disclosure_anywhere() {
    let report = report_with(vec![], vec![]);
    assert!(
        report.failed_passes.is_empty(),
        "sanity: the base fixture has no failed pass"
    );
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );

    assert!(
        json.methodology.failed_passes.is_empty(),
        "a clean run must not fabricate a not-computed disclosure: {:?}",
        json.methodology.failed_passes
    );
    assert!(
        json.executive_summary.failed_passes.is_empty(),
        "a clean run must not fabricate a not-computed disclosure: {:?}",
        json.executive_summary.failed_passes
    );
}

/// The Typst template only renders the disclosure block when the list is non-empty, and
/// renders it in BOTH sections when it is — pins the gating + duplication so a future
/// edit can't silently drop one of the two renders.
#[test]
fn shipped_template_gates_the_failed_pass_disclosure_on_both_sections() {
    let template = include_str!("../../../templates/audit_report.typ");
    assert_eq!(
        template
            .matches("d.executive_summary.failed_passes.len() > 0")
            .count(),
        1,
        "the summary section must gate the disclosure block on a non-empty list"
    );
    assert_eq!(
        template
            .matches("d.methodology.failed_passes.len() > 0")
            .count(),
        1,
        "the methodology section must gate the disclosure block on a non-empty list"
    );
}

// ── C3-1b: no field ever renders a literal "N/A" for a pipeline gap ─────────────

/// An unestimated `effort`/`confidence` is a PIPELINE gap (the calibration pass never
/// produced an opinion for this finding this run), not a legitimate "not applicable"
/// value — `or_na`'s "N/A" reads as the latter. Pins that the shipped template uses the
/// field-specific placeholders for both fields in the per-finding chip line, never `or_na`.
#[test]
fn shipped_template_never_renders_na_for_effort() {
    let template = include_str!("../../../templates/audit_report.typ");
    assert!(
        template.contains("or_not_estimated(item.effort)"),
        "an unestimated effort must render \"not estimated this run\", not \"N/A\""
    );
    assert!(
        !template.contains("or_na(item.effort)"),
        "an entry's effort must never fall back to the generic N/A placeholder"
    );
}

/// M1: a repo whose file list was too large to send in full is a disclosed coverage limit in
/// the client wording too, not a silent one, and it is not a failed part.
#[test]
fn a_file_tree_reduced_to_an_outline_is_disclosed_as_a_scope_limit() {
    let fallback = crate::ai_audit::FailedPass {
        repo: "acme/api".to_string(),
        pass: "full file list in model calls".to_string(),
        reason: "reduced to a per-directory outline: the repository has 30000 files".to_string(),
        files_not_reviewed: 0,
        class: crate::ai_audit::FailureClass::Content,
        files: Vec::new(),
        attempts: Vec::new(),
        scope_note: true,
    };
    for internal in [false, true] {
        let notes = failed_pass_disclosures(std::slice::from_ref(&fallback), internal);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("per-directory outline"), "{}", notes[0]);
        assert!(notes[0].contains("30000 files"), "{}", notes[0]);
        assert!(!notes[0].contains("did not complete"), "{}", notes[0]);
    }
}

/// C3-1b, W6 wiring: a run where `ai_audit::verify_findings` recorded a run-wide "hour
/// estimation" `FailedPass` must (a) disclose it in BOTH methodology and summary, exactly
/// like the pre-existing alternative-recommendation disclosure, and (b) still ship the
/// unestimated finding in the curated set / matrix — a failed estimation pass degrades
/// honestly, it never drops a row.
#[test]
fn a_failed_estimation_pass_is_disclosed_and_unestimated_rows_still_ship() {
    let f = finding("ARCH-1", "a.rs", 1, "high"); // effort: None (never estimated)
    assert_eq!(
        f.effort, None,
        "test fixture must simulate the unestimated state"
    );
    let mut report = report_with(vec![f], vec![]);
    report.failed_passes = vec![crate::ai_audit::FailedPass {
        repo: "owner/repo".to_string(),
        pass: "hour estimation".to_string(),
        reason: "the calibration pass returned no usable remediation-effort estimate for \
                     any finding this run"
            .to_string(),
        files_not_reviewed: 0,
        class: crate::ai_audit::FailureClass::Content,
        files: Vec::new(),
        attempts: Vec::new(),
        scope_note: false,
    }];
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &internal_opts(),
    );

    assert_eq!(json.methodology.failed_passes.len(), 1);
    assert!(
        json.methodology.failed_passes[0].contains("not computed"),
        "methodology must explicitly disclose the estimation pass was not computed: {:?}",
        json.methodology.failed_passes
    );
    assert!(
        json.methodology.failed_passes[0]
            .to_ascii_lowercase()
            .contains("hour estimation"),
        "the disclosure must name the failed pass: {:?}",
        json.methodology.failed_passes
    );
    assert_eq!(
        json.executive_summary.failed_passes, json.methodology.failed_passes,
        "the disclosure must appear identically in both sections"
    );

    // The row is NOT dropped — it still ships in the matrix and the curated findings,
    // carrying `effort: None` for the template to render honestly.
    assert_eq!(
        json.matrix.do_next.len() + json.matrix.do_now.len(),
        1,
        "an unestimated finding must still ship in an action bucket: {:?}",
        json.matrix
    );
    assert_eq!(json.curated_findings[0].sites[0].effort, None);
}

// ── Template regressions (2026-09-13 review) ────────────────────────────────
/// FIX 4 regression: the exec summary's own "Top priority items" bullet list (and its
/// `top_do_now` feed) must not come back — the "Three things this week" box is the one
/// place page 3 names the top do-now findings.
#[test]
fn shipped_template_has_no_top_priority_items_bullet_list() {
    let template = include_str!("../../../templates/audit_report.typ");
    assert!(!template.contains("Top priority items"));
    assert!(!template.contains("executive_summary.top_do_now"));
}

/// FIX 5 regression: the scorecard's "checked/clean" column must read as words, not a bare
/// numeric fraction that reads like a grade.
#[test]
fn shipped_template_scorecard_spells_out_checked_and_clean() {
    let template = include_str!("../../../templates/audit_report.typ");
    assert!(template.contains("Rules checked"));
    assert!(template.contains("Rules clean"));
    assert!(
        !template.contains("row.audited_rules)/#str(row.clean_rules"),
        "the scorecard must not render a bare N/M fraction"
    );
}

/// FIX 6 regression: the severity x effort section must render the real 2-D
/// `d.priority_grid` table, not the old 4-box bucket list keyed off `d.matrix`.
#[test]
fn shipped_template_renders_the_priority_grid_not_the_old_bucket_list() {
    let template = include_str!("../../../templates/audit_report.typ");
    assert!(template.contains("d.priority_grid"));
    assert!(
        !template.contains("d.matrix.do_now") && !template.contains("d.matrix.accepted"),
        "the severity x effort section must read from d.priority_grid, not d.matrix directly"
    );
}

// ── M8 item 3: the typst time budget scales with the document ──────────────────────

#[test]
fn the_typst_time_budget_is_a_base_plus_a_per_site_allowance_up_to_a_ceiling() {
    assert_eq!(typst_timeout(0), TYPST_TIMEOUT_BASE);
    assert_eq!(
        typst_timeout(1_000),
        TYPST_TIMEOUT_BASE + TYPST_TIMEOUT_PER_SITE * 1_000
    );
    assert!(
        typst_timeout(30_000) > Duration::from_secs(30),
        "a 30,000-site report must get more than the old fixed 30 seconds"
    );
    assert!(typst_timeout(2_000) > typst_timeout(1_000));
    assert_eq!(typst_timeout(usize::MAX / 2), TYPST_TIMEOUT_CEILING);
    assert!(TYPST_TIMEOUT_BASE <= TYPST_TIMEOUT_CEILING);
}

#[test]
fn the_site_count_is_the_places_the_report_covers() {
    let data = serde_json::json!({
        "site_total": 6,
        "cover": {"project_title": "x"}
    })
    .to_string();
    assert_eq!(pdf_site_count(&data), 6);
    assert_eq!(pdf_site_count("{}"), 0);
    assert_eq!(pdf_site_count("not json"), 0);
}

/// A typst that outlives its budget fails the export with a message that names the size and
/// the budget, and ships no PDF.
#[cfg(unix)]
#[tokio::test]
async fn a_typst_that_outlives_its_budget_fails_naming_the_size_and_ships_no_pdf() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("typst");
    // Writes a partial PDF, then keeps going past the budget.
    std::fs::write(
        &script,
        "#!/bin/sh\nprintf '%%PDF-partial' > report.pdf\nexec sleep 30\n",
    )
    .expect("write the fake typst");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let data = serde_json::json!({ "site_total": 5 }).to_string();
    let started = std::time::Instant::now();
    let result = TYPST_PROGRAM
        .scope(
            script,
            run_typst_within(&data, FontSources::BundledFirst, Duration::from_millis(300)),
        )
        .await;
    let error = result.expect_err("a typst past its budget must fail the export");
    let message = format!("{error:#}");
    assert!(message.contains("timed out"), "{message}");
    assert!(
        message.contains("5 sites"),
        "the error must name the size of the report: {message}"
    );
    assert!(
        message.contains("No PDF was produced"),
        "the error must say no partial PDF was kept: {message}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the compile must be stopped at the budget, not left to finish"
    );
}

#[test]
fn the_timeout_message_names_the_size_the_budget_and_whether_the_ceiling_was_hit() {
    let below = typst_timeout_message(5_000, typst_timeout(5_000));
    assert!(below.contains("5000 sites"), "{below}");
    assert!(below.contains("No PDF was produced"), "{below}");
    assert!(!below.contains("ceiling"), "{below}");
    let at_ceiling = typst_timeout_message(10_000_000, TYPST_TIMEOUT_CEILING);
    assert!(at_ceiling.contains("ceiling"), "{at_ceiling}");
    assert!(at_ceiling.contains("10000000 sites"), "{at_ceiling}");
}

/// M8 item 2: the PDF renders each site ONCE. The severity x effort grid used to print every
/// finding's headline and location a second time, beside the curated card that is the one
/// place a site is described in full. The grid now counts: a cell says how many findings it
/// holds and the cards list them. Pinned on the shipped template source (the grid cell must
/// not interpolate any per-finding field) and, with typst present, on the rendered text.
#[test]
fn shipped_template_priority_grid_counts_findings_instead_of_listing_them_again() {
    let template = include_str!("../../../templates/audit_report.typ");
    let start = template
        .find("#let grid_cell_content(cell)")
        .expect("the grid cell renderer");
    let end = template[start..]
        .find("\n}\n")
        .map(|i| start + i)
        .expect("the end of the grid cell renderer");
    let body = &template[start..end];
    for per_finding_field in ["f.headline", "f.path", "f.repo", "f.line", "f.rule_id"] {
        assert!(
            !body.contains(per_finding_field),
            "the grid cell must not print {per_finding_field}: the curated card is the one \
                 place a site is described. Cell body:\n{body}"
        );
    }
    assert!(
        body.contains("cell.findings.len()"),
        "the grid cell must count its findings"
    );
    assert!(
        template.contains("d.priority_grid.footnote"),
        "the grid must say where the problems are listed"
    );
    let footnote = build_priority_grid(&MatrixJson::default()).footnote;
    assert!(
        footnote.contains(crate::xlsx_export::vocabulary::ALL_PROBLEMS),
        "{footnote}"
    );
}

#[tokio::test]
async fn compile_pdf_prints_no_place_of_a_report_with_no_escalation_and_counts_them_all() {
    if which_typst().is_none() {
        eprintln!("skipping: typst not on PATH");
        return;
    }
    let findings: Vec<Finding> = (0..8)
        .map(|i| {
            finding(
                "SEC-NO-RAW-SQL-CONCAT-1",
                &format!("src/orders/handler_{i}.rs"),
                10 + i,
                "medium",
            )
        })
        .collect();
    let report = report_with(findings, vec!["SEC-NO-RAW-SQL-CONCAT-1"]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    let in_grid: usize = json
        .priority_grid
        .rows
        .iter()
        .flat_map(|r| r.cells.iter())
        .map(|c| c.findings.len())
        .sum();
    assert_eq!(in_grid, 8, "all eight reach the grid");

    let pdf = compile_pdf(&json).await.expect("the report must compile");
    let text = pdf_extract::extract_text_from_mem(&pdf).expect("text of the compiled PDF");
    assert_eq!(
        text.matches("handler_").count(),
        0,
        "the PDF is a summary: no place is listed unless it is escalated or in the top three: {text}"
    );
}

/// Branding regression: no hardcoded agency/"Camerata" cover title, no "Brownfield".
#[test]
fn shipped_template_has_no_hardcoded_cover_title() {
    let template = include_str!("../../../templates/audit_report.typ");
    assert!(!template.contains("Camerata Brownfield Audit Report"));
    assert!(!template.contains("Brownfield"));
    assert!(template.contains("brand_title"));
    assert!(template.contains(NEUTRAL_COVER_TITLE));
}

/// Branding regression: Audit model / Calibration model must not render on the cover table
/// anymore (they moved into Methodology, alongside the Camerata version line).
#[test]
fn shipped_template_cover_table_omits_audit_and_calibration_model() {
    let template = include_str!("../../../templates/audit_report.typ");
    let cover_start = template.find("── 1. Cover").expect("cover section marker");
    let cover_end = template
        .find("── 2. Contents")
        .expect("contents section marker");
    let cover_section = &template[cover_start..cover_end];
    assert!(!cover_section.contains("Audit model"));
    assert!(!cover_section.contains("Calibration model"));
    assert!(
        template.contains("Scanned with Camerata v"),
        "the model/version provenance line must still render, just in Methodology"
    );
}

// ── P5: review-state honesty + reconciling narrative ────────────────────────

/// Pure reconciliation check, independent of `build_report_json`'s much larger fixture
/// surface: every candidate finding a scan surfaces lands in exactly one of the four
/// top-level buckets (curated — itself do_now + do_next + plan + accepted — held for
/// review, excluded as a false positive, or its own dependency-advisory lane).
#[allow(clippy::too_many_arguments)]
fn reconciles(
    candidates: usize,
    curated_total: usize,
    do_now: usize,
    do_next: usize,
    plan: usize,
    accepted: usize,
    held_for_review: usize,
    excluded_false_positive: usize,
    dependency_advisories: usize,
) -> bool {
    curated_total == do_now + do_next + plan + accepted
        && candidates
            == curated_total + held_for_review + excluded_false_positive + dependency_advisories
}

#[test]
fn reconciliation_holds_across_fixtures_including_the_41_18_23_0_shape() {
    // (candidates, curated_total, do_now, do_next, plan, accepted, held_for_review,
    //  excluded_false_positive, dependency_advisories)
    #[allow(clippy::type_complexity)]
    let fixtures: &[(
        usize,
        usize,
        usize,
        usize,
        usize,
        usize,
        usize,
        usize,
        usize,
    )] = &[
        // The exact bug shape from the P5 writeup: 41 candidates, 18 curated, 23 held for
        // review (previously invisible), 0 excluded, no dependency advisories in play.
        (41, 18, 5, 6, 7, 0, 23, 0, 0),
        // A reviewed run: false positives excluded, one dependency advisory carved out.
        (10, 6, 2, 1, 2, 1, 0, 3, 1),
        // Zero-finding scan.
        (0, 0, 0, 0, 0, 0, 0, 0, 0),
        // Nothing curated yet, everything held for a human reviewer's judgment call.
        (5, 0, 0, 0, 0, 0, 5, 0, 0),
    ];
    for &(candidates, curated_total, do_now, do_next, plan, accepted, held, excluded, dep) in
        fixtures
    {
        assert!(
            reconciles(
                candidates,
                curated_total,
                do_now,
                do_next,
                plan,
                accepted,
                held,
                excluded,
                dep
            ),
            "fixture failed to reconcile: {:?}",
            (
                candidates,
                curated_total,
                do_now,
                do_next,
                plan,
                accepted,
                held,
                excluded,
                dep
            )
        );
    }
    // Deliberately broken shapes must NOT reconcile — proves the check discriminates
    // rather than vacuously passing everything.
    assert!(
        !reconciles(41, 18, 5, 6, 7, 0, 22, 0, 0),
        "an off-by-one held_for_review must fail to reconcile"
    );
    assert!(
        !reconciles(41, 19, 5, 6, 7, 0, 23, 0, 0),
        "a curated_total that doesn't match its own do_now+do_next+plan+accepted must fail"
    );
}

/// A finding the SCAN produced but the AUDITOR has never opened this session (no wire
/// dispositions at all) must export as `Raw` — the honest starting state.
#[test]
fn no_dispositions_at_all_is_raw_review_state() {
    let f = finding("SEC-1", "a.rs", 1, "critical");
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert_eq!(json.review_state, ReviewState::Raw);
    assert!(json.review_state.is_draft());
}

/// `BaselineAccepted` (a PRIOR run's suppression) and `WaivedInline` (an in-code waiver) are
/// both sourced from `Finding.status`, not this session's triage — see `ReviewState`'s doc
/// comment. Neither may flip the export to `Reviewed`; nobody looked at this finding THIS
/// session.
#[test]
fn baseline_or_inline_disposition_alone_does_not_flip_review_state_to_reviewed() {
    let mut baseline = finding("SEC-1", "a.rs", 1, "high");
    baseline.status = "suppressed-baseline".to_string();
    let mut inline = finding("SEC-2", "b.rs", 2, "high");
    inline.status = "suppressed-inline".to_string();
    let report = report_with(vec![baseline, inline], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert_eq!(
        json.review_state,
        ReviewState::Raw,
        "a prior-run baseline suppression or an in-code waiver is not THIS session's \
             auditor reviewing anything"
    );
}

/// An explicit wire disposition of any recognized kind (`Ignored` / `TechDebt` /
/// `FalsePositive`) on even ONE finding is enough to flip the whole export to `Reviewed`.
#[test]
fn a_single_auditor_disposition_flips_the_whole_export_to_reviewed() {
    let f1 = finding("SEC-1", "a.rs", 1, "critical");
    let f2 = finding("SEC-2", "b.rs", 2, "critical");
    let mut dispositions = HashMap::new();
    dispositions.insert(finding_key(&f1), wire("Ignored", "defense in depth", ""));
    let report = report_with(vec![f1, f2], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &dispositions),
        None,
        &empty_opts(),
    );
    assert_eq!(json.review_state, ReviewState::Reviewed);
    assert!(!json.review_state.is_draft());
}

/// req_pass2_10 (THE DEFECT, stated against the export): a finding the auditor READ AND
/// CONFIRMED must be distinguishable, in the exported artifacts, from one he never opened.
///
/// RED BEFORE THE FIX, on `1a61a7c`, as two separate failures:
///
/// 1. `review_state` came back `Raw`, so the PDF stamped "DRAFT: not yet reviewed" on
///    every page and the narrative spoke in the engine's voice — on a scan the auditor had
///    reviewed end to end. The product's first public promise is "one person reads every
///    finding and signs"; a confirmation that does not reach `review_state` leaves that
///    promise with no data behind it.
/// 2. `classify`'s catch-all mapped the unrecognized wire value `"Confirmed"` to
///    `Disposition::Unresolved`, so the confirmed row's own site label read "Open
///    (recommended: ...)" — byte-identical to the row nobody had looked at.
///
/// Together those are the "ships inert" shape: a disposition that exists only in the UI.
#[test]
fn req_pass2_10_a_confirmed_finding_is_distinguishable_from_an_unopened_one_in_the_export() {
    let confirmed = finding("SEC-1", "a.rs", 1, "critical");
    let never_opened = finding("SEC-2", "b.rs", 2, "critical");
    let mut dispositions = HashMap::new();
    dispositions.insert(finding_key(&confirmed), wire("Confirmed", "", ""));
    let report = report_with(vec![confirmed.clone(), never_opened.clone()], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &dispositions),
        None,
        &empty_opts(),
    );

    // (1) The sign-off promise, backed by data.
    assert_eq!(
        json.review_state,
        ReviewState::Reviewed,
        "confirming a finding IS this session's auditor dispositioning it"
    );
    assert!(!json.review_state.is_draft());

    // (2) The two rows do not read the same.
    let label_for = |rule_id: &str| -> String {
        json.curated_findings
            .iter()
            .chain(json.held_for_review_findings.iter())
            .flat_map(|g| g.sites.iter())
            .find(|s| s.path == if rule_id == "SEC-1" { "a.rs" } else { "b.rs" })
            .unwrap_or_else(|| panic!("{rule_id} must appear in the export"))
            .disposition
            .clone()
    };
    let confirmed_label = label_for("SEC-1");
    let unopened_label = label_for("SEC-2");
    assert_ne!(
        confirmed_label, unopened_label,
        "a reviewed-and-confirmed finding must not read identically to one nobody opened"
    );
    assert!(
        confirmed_label.to_lowercase().contains("confirmed"),
        "the confirmed row says so in words; got {confirmed_label:?}"
    );
    assert!(
        unopened_label.starts_with("Open ("),
        "and the unopened row still reads as open; got {unopened_label:?}"
    );
}

/// req_pass2_10: a confirmed finding is DECIDED, but it is not FIXED and it is not
/// accepted — so it keeps its severity-derived action bucket exactly as an open row would,
/// and it still counts as outstanding work for the client.
///
/// This is the half of the semantics that is easy to get wrong in the other direction:
/// `Ignored`/`TechDebt*` all move a row out of its computed quadrant because they record a
/// decision about WHEN or WHETHER to fix it. `Confirmed` records only that the finding is
/// REAL, which is precisely what the quadrant already assumed.
#[test]
fn req_pass2_10_confirmed_keeps_the_severity_derived_bucket_and_still_counts_as_open() {
    assert_eq!(
        matrix_bucket(Disposition::Confirmed, "critical", None),
        matrix_bucket(Disposition::Unresolved, "critical", None),
        "confirming does not reschedule the fix"
    );
    assert_eq!(
        matrix_bucket(Disposition::Confirmed, "high", Some("low")),
        "do_now"
    );
    assert_eq!(
        matrix_bucket(Disposition::Confirmed, "medium", None),
        "plan"
    );

    let confirmed = finding("SEC-1", "a.rs", 1, "critical");
    let mut dispositions = HashMap::new();
    dispositions.insert(finding_key(&confirmed), wire("Confirmed", "", ""));
    let report = report_with(vec![confirmed.clone()], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &dispositions),
        None,
        &empty_opts(),
    );
    assert_eq!(json.executive_summary.do_now, 1);
    assert_eq!(
        json.executive_summary.open, 1,
        "confirmed-and-unfixed is still open work for the client"
    );
    assert_eq!(json.executive_summary.excluded_false_positive, 0);
    assert_eq!(json.executive_summary.held_for_review, 0);
}

/// The core P5 raw-export contract: the JSON carries the draft flag, the narrative
/// reconciles with EVERY number shown (candidates == curated + held + excluded), and the
/// narrative never claims a human reviewed or dispositioned anything.
#[test]
fn raw_export_carries_the_draft_flag_and_a_fully_reconciling_engine_voiced_narrative() {
    let mut f1 = finding("SEC-1", "a.rs", 1, "critical"); // -> do_now, curated
    f1.detail = "The profiles table has no RLS.".to_string();
    let mut f2 = finding("SEC-2", "b.rs", 2, "medium"); // -> needs-review, held
    f2.confidence = Some("needs-review".to_string());
    let report = report_with(vec![f1, f2], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );

    // The flag.
    assert_eq!(json.review_state, ReviewState::Raw);
    assert!(json.review_state.is_draft());

    // The reconciling numbers, each present on the JSON.
    assert_eq!(json.executive_summary.candidates_reviewed, 2);
    assert_eq!(json.executive_summary.curated_total, 1);
    assert_eq!(json.executive_summary.held_for_review, 1);
    assert_eq!(json.executive_summary.excluded_false_positive, 0);
    assert!(reconciles(
        json.executive_summary.candidates_reviewed,
        json.executive_summary.curated_total,
        json.executive_summary.do_now,
        json.executive_summary.do_next,
        json.executive_summary.plan,
        json.executive_summary.accepted,
        json.executive_summary.held_for_review,
        json.executive_summary.excluded_false_positive,
        json.executive_summary.dependency_advisories,
    ));

    // Every one of those numbers is actually visible in the prose, not just the JSON.
    let n = &json.executive_summary.narrative;
    assert!(n.contains("2 candidate findings"), "{n}");
    assert!(n.contains('1'), "{n}"); // curated_total / held_for_review both literally "1"
    assert!(n.contains("0 were auto-excluded"), "{n}");

    // Never claims a human reviewed or dispositioned anything.
    assert!(!n.contains("reviewed by a human reviewer"), "{n}");
    assert!(!n.contains("dispositioned by a human reviewer"), "{n}");
    assert!(!n.contains("were reviewed"), "{n}");
    assert!(
        !json
            .methodology
            .ai_tier_note
            .contains("reviewed by a human reviewer"),
        "{}",
        json.methodology.ai_tier_note
    );
    assert!(
        !json
            .methodology
            .ai_tier_note
            .contains("dispositioned by a human reviewer"),
        "{}",
        json.methodology.ai_tier_note
    );
}

/// The P5 reviewed-export contract: reviewer dispositions actually appear, the narrative
/// reconciles, no draft flag, and reviewed-voice phrasing IS allowed (because it's true).
#[test]
fn reviewed_export_reconciles_in_auditor_voice_with_no_draft_flag() {
    let mut ignored = finding("SEC-1", "a.rs", 1, "critical"); // -> accepted, curated
    ignored.detail = "The profiles table has no RLS.".to_string();
    let mut held = finding("SEC-2", "b.rs", 2, "medium"); // -> needs-review, held
    held.confidence = Some("needs-review".to_string());
    let excluded = finding("SEC-3", "c.rs", 3, "critical"); // -> FalsePositive, excluded

    let mut dispositions = HashMap::new();
    dispositions.insert(
        finding_key(&ignored),
        wire("Ignored", "defense in depth only", ""),
    );
    dispositions.insert(
        finding_key(&excluded),
        wire("FalsePositive", "not exploitable", ""),
    );

    let report = report_with(vec![ignored, held, excluded], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &dispositions),
        None,
        &empty_opts(),
    );

    assert_eq!(json.review_state, ReviewState::Reviewed);
    assert!(!json.review_state.is_draft());

    assert_eq!(json.executive_summary.candidates_reviewed, 3);
    assert_eq!(json.executive_summary.excluded_false_positive, 1);
    assert_eq!(json.executive_summary.curated_total, 1);
    assert_eq!(json.executive_summary.held_for_review, 1);
    assert_eq!(json.executive_summary.accepted, 1);
    assert!(reconciles(
        json.executive_summary.candidates_reviewed,
        json.executive_summary.curated_total,
        json.executive_summary.do_now,
        json.executive_summary.do_next,
        json.executive_summary.plan,
        json.executive_summary.accepted,
        json.executive_summary.held_for_review,
        json.executive_summary.excluded_false_positive,
        json.executive_summary.dependency_advisories,
    ));

    let n = &json.executive_summary.narrative;
    assert!(
        n.contains("3 candidate findings were reviewed by a human reviewer"),
        "{n}"
    );
    assert!(n.contains("1 was dispositioned as false positives"), "{n}");
    assert!(n.contains("1 accepted as risk"), "{n}");
    assert!(n.contains("held for further review"), "{n}");
    assert!(
        json.methodology
            .ai_tier_note
            .contains("reviewed and dispositioned by a human reviewer"),
        "{}",
        json.methodology.ai_tier_note
    );
}

/// Zero-candidate raw scans keep the pre-P5 honest empty-state sentence — no draft-banner
/// noise added to a run that surfaced nothing at all.
#[test]
fn zero_candidates_raw_scan_keeps_the_empty_state_sentence() {
    let report = report_with(vec![], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert_eq!(json.review_state, ReviewState::Raw);
    assert_eq!(
        json.executive_summary.narrative,
        "The scan surfaced no candidate findings to review in this run."
    );
}

/// The Typst template's per-page draft banner must be gated on `d.review_state`, and the
/// literal banner text the design calls for must be present.
#[test]
fn shipped_template_gates_the_draft_banner_on_review_state() {
    let template = include_str!("../../../templates/audit_report.typ");
    assert!(
        template.contains("d.review_state == \"raw\""),
        "the draft banner must be gated on d.review_state"
    );
    assert!(
        template.contains("DRAFT: not yet reviewed by a human reviewer"),
        "the required banner text must be present verbatim"
    );
    // The gate must live inside `#set page(...)`'s `header:` so it renders on EVERY page,
    // not just the cover.
    let page_start = template.find("#set page(").expect("page setup block");
    let is_draft_def = template
        .find("#let is_draft")
        .expect("is_draft let-binding");
    assert!(
        is_draft_def < page_start,
        "is_draft must be defined before the page setup that reads it in header:"
    );
}

/// C4-R3 defect 1: the template used to ALSO echo a bare " · DRAFT" in the footer, on top
/// of the header's full banner — a standalone "DRAFT" immediately ahead of the next page's
/// full banner sentence in reading order, which collapsed into "DRAFTDRAFT: not yet
/// reviewed..." under a naive multi-page text read. The banner text must appear in exactly
/// ONE place in the template source (the header), never echoed a second time in the footer.
#[test]
fn shipped_template_does_not_echo_the_draft_banner_a_second_time_in_the_footer() {
    let template = include_str!("../../../templates/audit_report.typ");
    // The rendered banner CONTENT (not a comment mentioning it) is this exact bracketed
    // Typst markup span — it must appear exactly once (in the header), never a second time
    // anywhere else in the template.
    assert_eq!(
        template
            .matches("[DRAFT: not yet reviewed by a human reviewer]")
            .count(),
        1,
        "the rendered draft banner content must appear exactly once in the template"
    );
    let footer_start = template.find("footer: context [").expect("footer block");
    let footer_end = footer_start + template[footer_start..].find("],").expect("footer close");
    let footer_body = &template[footer_start..footer_end];
    assert!(
        !footer_body.contains("[ · DRAFT]") && !footer_body.contains("[DRAFT]"),
        "the footer must not render its own standalone DRAFT echo — the header banner \
             already marks every page, and a bare echo there used to sit immediately ahead of \
             the next page's full banner, concatenating into \"DRAFTDRAFT: not yet \
             reviewed...\" under a naive multi-page text read: {footer_body}"
    );
}

/// C4-R3 defect 2: an empty severity×effort matrix cell used to render the literal word
/// "None" — a stringified absent value, not an intentional empty state. The template must
/// render empty content for a zero-finding cell, never the word "None" (or "null").
#[test]
fn shipped_template_renders_an_empty_matrix_cell_as_blank_not_the_word_none() {
    let template = include_str!("../../../templates/audit_report.typ");
    let fn_start = template
        .find("#let grid_cell_content(cell) = {")
        .expect("grid_cell_content definition");
    let fn_end = fn_start + template[fn_start..].find("\n}\n").expect("function close");
    let body = &template[fn_start..fn_end];
    assert!(
        !body.contains("[None]") && !body.contains("[null]"),
        "an empty grid cell must never render the literal word None/null: {body}"
    );
}

/// The Methodology section's own reconciling line (separate literal text from the exec
/// summary) must ALSO be gated — it used to unconditionally say "... reviewed; ...
/// dispositioned as false positives by the auditor and excluded" no matter what.
#[test]
fn shipped_template_methodology_line_is_gated_on_review_state() {
    let template = include_str!("../../../templates/audit_report.typ");
    assert!(
        template.contains("if d.review_state == \"reviewed\""),
        "the methodology reconciling line must branch on review_state"
    );
    assert!(
        template.contains("has not yet had a human triage pass"),
        "the raw branch must say so explicitly"
    );
    assert!(
        template.contains("held for a human reviewer's judgment call"),
        "the raw branch must surface held_for_review, not just candidates/excluded"
    );
}

/// W5: a RAW (unreviewed) export must compile cleanly through the now-gated sign-off
/// block — the new `if is_draft [...] else [...]` branch in the signature block must not be
/// a Typst syntax trap on the raw path, the one that is actually exercised by default (no
/// disposition ever recorded).
#[tokio::test]
async fn compile_pdf_succeeds_for_an_unreviewed_draft_with_the_gated_signoff() {
    if which_typst().is_none() {
        eprintln!(
            "skipping compile_pdf_succeeds_for_an_unreviewed_draft_with_the_gated_signoff: \
                 typst not on PATH"
        );
        return;
    }
    let f = finding("SEC-NO-HARDCODED-SECRETS-1", "src/a.rs", 10, "critical");
    let report = report_with(vec![f], vec!["SEC-NO-HARDCODED-SECRETS-1"]);
    // No dispositions recorded at all -> Raw/unreviewed, same source of truth the draft
    // banner and the sign-off block both read.
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert_eq!(json.review_state, ReviewState::Raw);
    assert!(json.review_state.is_draft());

    let pdf = compile_pdf(&json)
        .await
        .expect("compile_pdf must succeed for an unreviewed draft export");
    assert!(pdf.starts_with(b"%PDF"));
}

/// Regression (cycle-3 scan): a finding with an EMPTY path must not crash the PDF compile.
/// `breakable("")` used to hit `().join(sep)`, which is `none` in Typst, and `#raw(none)`
/// fails the whole compile ("expected string, found none"). A real scan produced a top-3
/// do-now finding with an empty path and took the entire export down; the `breakable()` guard
/// (none/empty -> "") fixes it. Critical severity routes the finding into the "three things"
/// box that renders `#raw(breakable(item.path))`, exercising the crash path.
#[tokio::test]
async fn compile_pdf_succeeds_when_a_top_finding_has_an_empty_path() {
    if which_typst().is_none() {
        eprintln!(
            "skipping compile_pdf_succeeds_when_a_top_finding_has_an_empty_path: \
                 typst not on PATH"
        );
        return;
    }
    let f = finding("SEC-NO-HARDCODED-SECRETS-1", "", 0, "critical");
    let report = report_with(vec![f], vec!["SEC-NO-HARDCODED-SECRETS-1"]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    let pdf = compile_pdf(&json)
        .await
        .expect("compile_pdf must succeed even when a finding has an empty path");
    assert!(pdf.starts_with(b"%PDF"));
}

/// End-to-end smoke test (typst-present-only, mirrors `compile_pdf_produces_a_real_pdf_
/// when_typst_is_present`): a REVIEWED export (with a real disposition) must still compile
/// cleanly through the gated template — the conditional banner/methodology logic must not
/// be a Typst syntax trap that only happens to work on the (more commonly exercised) raw
/// path. W5 companion to the test above: the sign-off block's reviewed branch must also
/// still be valid Typst.
#[tokio::test]
async fn compile_pdf_succeeds_for_a_reviewed_export_with_the_gated_template() {
    if which_typst().is_none() {
        eprintln!(
            "skipping compile_pdf_succeeds_for_a_reviewed_export_with_the_gated_template: \
                 typst not on PATH"
        );
        return;
    }
    let f = finding("SEC-NO-HARDCODED-SECRETS-1", "src/a.rs", 10, "critical");
    let mut dispositions = HashMap::new();
    dispositions.insert(finding_key(&f), wire("Ignored", "test fixture only", ""));
    let report = report_with(vec![f], vec!["SEC-NO-HARDCODED-SECRETS-1"]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &dispositions),
        None,
        &empty_opts(),
    );
    assert_eq!(json.review_state, ReviewState::Reviewed);

    let pdf = compile_pdf(&json)
        .await
        .expect("compile_pdf must succeed for a reviewed export");
    assert!(pdf.starts_with(b"%PDF"));
}

// ── P6 (2026-09-29): cover, branding, product name ──────────────────────────────

/// The cover title: brand_title (both the branded and neutral forms) must say
/// "Codebase Inspection", never "Codebase Audit" — see `NEUTRAL_COVER_TITLE` and the
/// template's `brand_title` binding.
#[test]
fn shipped_template_brand_title_says_codebase_inspection_not_audit() {
    let template = include_str!("../../../templates/audit_report.typ");
    assert!(template.contains(NEUTRAL_COVER_TITLE));
    assert!(template.contains(" - Codebase Inspection"));
    assert!(!template.contains("Codebase Audit"));
}

/// The per-page footer keeps "(advisory, not a certification)" verbatim (explicitly
/// required to survive the rebrand) but the word in front of it is now "inspection".
#[test]
fn shipped_template_footer_says_inspection_report_and_keeps_the_advisory_caveat() {
    let template = include_str!("../../../templates/audit_report.typ");
    assert!(template.contains("inspection report (advisory, not a certification)"));
    assert!(!template.contains("audit report (advisory"));
}

/// The draft banner (raw-export only) no longer attributes the pending review to "the
/// auditor" — client-facing role language is "a human reviewer" throughout.
#[test]
fn shipped_template_draft_banner_says_reviewer_not_auditor() {
    let template = include_str!("../../../templates/audit_report.typ");
    assert!(template.contains("DRAFT: not yet reviewed by a human reviewer"));
    assert!(!template.contains("reviewed by the auditor"));
}

/// A signature block exists (promise 1: "one person reads every finding and signs the
/// report") and it renders the real, resolved preparer name.
#[test]
fn shipped_template_has_a_signature_block_naming_the_preparer() {
    let template = include_str!("../../../templates/audit_report.typ");
    assert!(template.contains("Prepared and signed off by"));
    assert!(template.contains("d.cover.prepared_by"));
}

/// W5: the sign-off block used to render "Prepared and signed off by: NAME / DATE"
/// unconditionally — including on a raw (nobody-has-looked-at-this-yet) export, directly
/// contradicting the per-page DRAFT banner above it. It must now be gated on the exact same
/// `is_draft` signal the draft banner reads (not a second, independently-derived flag), so
/// the two can never disagree again.
#[test]
fn shipped_template_signoff_is_gated_on_the_same_is_draft_signal_as_the_draft_banner() {
    let template = include_str!("../../../templates/audit_report.typ");
    let signoff_pos = template
        .find("Prepared and signed off by")
        .expect("signature block must exist");

    // The nearest `is_draft` check ABOVE the sign-off line must be the one that guards it
    // (`#if is_draft [ ... ] else [ ... Prepared and signed off by ... ]`) — found via
    // `rfind` on the (byte-boundary-safe, since `signoff_pos` came from `find`) prefix, so
    // this can't accidentally match the draft banner's own `is_draft` check way up in the
    // page header.
    let if_draft_pos = template[..signoff_pos]
        .rfind("if is_draft")
        .expect("an `if is_draft` branch must guard the sign-off block");
    let between = &template[if_draft_pos..signoff_pos];
    assert!(
        between.len() < 300,
        "the `if is_draft` guarding the sign-off block must sit directly above it, not be \
             some unrelated earlier use of the signal; gap was {} bytes: {between:?}",
        between.len()
    );
    assert!(
        between.contains("] else ["),
        "the sign-off line must be in the `else` (reviewed) branch of the is_draft check"
    );
    assert!(
        between.contains("unreviewed draft"),
        "the draft branch must explicitly say this is an unreviewed draft"
    );
    assert!(
        between.contains("not signed off"),
        "the draft branch must not claim a sign-off happened"
    );

    // The reviewed branch (only) still pairs the preparer name with a date.
    let reviewed_branch_end = template.len().min(signoff_pos + 200);
    let reviewed_branch = &template[signoff_pos..reviewed_branch_end];
    assert!(reviewed_branch.contains("d.cover.generated_at"));
}

/// Contract test: no CLIENT-FACING string in the shipped template says "audit"/"Audit".
/// Typst line comments (developer-facing design commentary, never rendered) are stripped
/// first; the three JSON field-accessor tokens that happen to spell "audit" as part of a
/// Rust struct field name (`d.cover.audited_refs`, `d.cover.audit_model`,
/// `row.audited_rules` — internal identifiers, exempt per the plan) are scrubbed before the
/// word-boundary scan so they don't produce a false positive.
#[test]
fn shipped_template_client_facing_text_has_no_audit_word() {
    let template = include_str!("../../../templates/audit_report.typ");
    let code_only: String = template
        .lines()
        .map(|line| line.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");
    let scrubbed = code_only
        .replace("d.cover.audited_refs", "")
        .replace("d.cover.audit_model", "")
        .replace("row.audited_rules", "");
    let re = regex::Regex::new(r"(?i)\baudit\w*\b").unwrap();
    let hits: Vec<&str> = re.find_iter(&scrubbed).map(|m| m.as_str()).collect();
    assert!(
        hits.is_empty(),
        "client-facing template text still says audit: {hits:?}"
    );
}

/// Contract test: none of the authored, client-facing prose this module PRODUCES (the
/// disclaimer, the next-steps note, both methodology AI-tier notes, both narrative voices,
/// and the disposition labels) says "audit"/"Audit". Internal identifiers/rule ids are a
/// separate, non-rendered surface and are exempt.
#[test]
fn no_client_facing_authored_string_contains_the_word_audit() {
    let candidates: Vec<String> = vec![
        AUDIT_REPORT_DISCLAIMER.to_string(),
        next_steps_note(ReviewState::Reviewed, 10, 7, 0, 3),
        next_steps_note(ReviewState::Raw, 0, 0, 0, 0),
        ai_tier_note_for(ReviewState::Raw, 0, 1),
        ai_tier_note_for(ReviewState::Reviewed, 0, 1),
        ai_tier_note_for(ReviewState::Raw, 1, 2),
        ai_tier_note_for(ReviewState::Reviewed, 2, 2),
        default_narrative(ReviewState::Raw, 10, 3, 1, 1, 1, 0, 1, 0, 0, 0),
        default_narrative(ReviewState::Reviewed, 10, 3, 1, 1, 1, 0, 1, 0, 0, 0),
        disposition_label(Disposition::Ignored, "some reason", "accepted", false),
        disposition_label(Disposition::Ignored, "some reason", "accepted", true),
        disposition_label(Disposition::Unresolved, "", "do_now", false),
    ];
    for s in &candidates {
        assert!(
            !s.to_lowercase().contains("audit"),
            "client-facing string still says audit: {s}"
        );
    }
}

/// "Prepared by" must never render blank/"N/A" with default (un-mutated) options — the
/// resolver's floor is a real name (promise 1).
#[test]
fn cover_prepared_by_is_never_blank_with_default_options() {
    let f = finding("SEC-1", "a.rs", 1, "low");
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert_eq!(json.cover.prepared_by, DEFAULT_PREPARED_BY);
    assert_ne!(json.cover.prepared_by, "");
}

/// Zero real code volume (no local file was ever read this run) must HIDE the cover's
/// code-volume field entirely — never render a zero.
#[test]
fn cover_code_volume_is_none_when_the_scan_has_no_real_line_count() {
    let f = finding("SEC-1", "a.rs", 1, "low");
    let mut report = report_with(vec![f], vec![]);
    report.code_lines = 0;
    report.code_lines_by_language = Vec::new();
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert!(
        json.cover.code_volume.is_none(),
        "zero code_lines must hide the row, not print a zero"
    );
}

/// A real line count produces `Some(CodeVolumeJson)` with the per-language breakdown
/// carried straight through from the scan (`report_with`'s fixture sets 400 lines split
/// TypeScript 300 / Rust 100).
#[test]
fn cover_code_volume_is_some_with_real_lines_and_language_breakdown_when_known() {
    let f = finding("SEC-1", "a.rs", 1, "low");
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    let cv = json
        .cover
        .code_volume
        .expect("a nonzero code_lines must produce Some");
    assert_eq!(cv.lines, 400);
    assert_eq!(cv.by_language.len(), 2);
    assert_eq!(cv.by_language[0].language, "TypeScript");
    assert_eq!(cv.by_language[0].lines, 300);
    assert_eq!(cv.by_language[1].language, "Rust");
    assert_eq!(cv.by_language[1].lines, 100);
}

/// The template never has an unconditional "characters"-suffixed render of the raw char
/// count any more (the bug: "0 characters" always rendered regardless of value) — the row
/// is now built from the gated `code_volume_rows` array.
#[test]
fn shipped_template_never_unconditionally_renders_code_chars_as_characters() {
    let template = include_str!("../../../templates/audit_report.typ");
    assert!(
        !template.contains("thousands(d.cover.code_chars) characters"),
        "the old unconditional zero-prone render must be gone"
    );
    assert!(template.contains("code_volume_rows"));
    assert!(template.contains("d.cover.code_volume"));
}

/// P6: extends the existing P1 e2e coverage
/// (`p1_e2e_cover_counts_and_security_finding_survive_post_merge`, which already pins
/// `cover.stats.critical`) to the high/medium buckets: two raw findings at the exact same
/// file+line must merge into ONE finding before the cover ever counts severities, so
/// `cover.stats.high` reads 1, not 2.
#[tokio::test]
async fn cover_stats_high_bucket_also_reflects_post_merge_counts() {
    let cors_code = "app.use(cors())";
    let mut det = finding("ARCH-MIDDLEWARE-FIRST-1", "middleware.ts", 12, "high");
    det.snippet = cors_code.to_string();
    let mut sibling = finding("SOME-OTHER-HIGH-RULE", "middleware.ts", 12, "high");
    sibling.snippet = cors_code.to_string();
    let files = vec![("middleware.ts".to_string(), cors_code.to_string())];
    let merged = crate::review_tree::test_support::cluster_heads(
        crate::review_tree::test_support::collapse_located(vec![det, sibling], &files),
        &files,
    );
    assert_eq!(
        merged.len(),
        1,
        "two raw findings at the same file+line must merge to one before counting"
    );
    let report = report_with(merged, vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert_eq!(
        json.cover.stats.high, 1,
        "the merged high finding must count once, not twice"
    );
    assert_eq!(json.cover.stats.critical, 0);
    assert_eq!(json.cover.stats.medium, 0);
}

/// End-to-end (typst-present-only): a scan with NO real code-volume figure must still
/// compile cleanly through the template — the `code_volume_rows` conditional is not a
/// Typst syntax trap on the empty-array branch.
#[tokio::test]
async fn compile_pdf_succeeds_with_zero_code_lines_hiding_the_code_volume_row() {
    if which_typst().is_none() {
        eprintln!(
            "skipping compile_pdf_succeeds_with_zero_code_lines_hiding_the_code_volume_row: \
                 typst not on PATH"
        );
        return;
    }
    let f = finding("SEC-NO-HARDCODED-SECRETS-1", "src/a.rs", 10, "critical");
    let mut report = report_with(vec![f], vec!["SEC-NO-HARDCODED-SECRETS-1"]);
    report.code_lines = 0;
    report.code_lines_by_language = Vec::new();
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert!(json.cover.code_volume.is_none());

    let pdf = compile_pdf(&json)
        .await
        .expect("compile_pdf must succeed with the code-volume row hidden");
    assert!(pdf.starts_with(b"%PDF"));
}

/// `sample-report/audit_report.typ` is a PREVIEW COPY of the shipped template (used to
/// regenerate the sample PDF/PNGs) and must stay byte-identical to it — a drifted copy
/// means the sample deliverable stops representing what the app actually ships. This
/// caught real drift as of this pass (the sample copy predated the P2/P6 template edits).
///
/// `sample-report/` is a LOCAL-ONLY, self-ignoring directory (its own `.gitignore` is `*`),
/// so it does not exist in a fresh clone or in CI. The copy is therefore read at RUNTIME and
/// the check is skipped when it is absent; an `include_str!` here made the whole lib-test
/// target fail to compile on any checkout without the preview directory.
#[test]
fn sample_report_template_copy_is_byte_identical_to_the_shipped_template() {
    let shipped = include_str!("../../../templates/audit_report.typ");
    let sample_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../sample-report/audit_report.typ");
    let sample = match std::fs::read_to_string(&sample_path) {
        Ok(sample) => sample,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!(
                "skipping drift check: {} is absent (sample-report/ is local-only)",
                sample_path.display()
            );
            return;
        }
        Err(e) => panic!("read {}: {e}", sample_path.display()),
    };
    assert_eq!(
        shipped, sample,
        "sample-report/audit_report.typ has drifted from the shipped template — copy the \
             shipped file over it"
    );
}
