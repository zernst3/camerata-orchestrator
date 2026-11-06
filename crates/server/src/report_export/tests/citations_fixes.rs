//! Suppression dispositions, citation joins, fix resolution, remediation
//! instantiation, evidence checks and the AI citation gate.

use super::*;

#[test]
fn suppressed_baseline_with_no_wire_disposition_is_baseline_accepted() {
    let mut f = finding("SEC-1", "a.rs", 1, "high");
    f.status = "suppressed-baseline".to_string();
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert_eq!(
        json.curated_findings[0].sites[0].disposition,
        "Pre-existing accepted debt (baseline suppression)"
    );
    assert_eq!(json.matrix.accepted.len(), 1);
}

/// Defect 2 (2026-10-07 grading pass): the SAME `status == "suppressed-baseline"` string
/// the test above correctly honors for a genuine floor finding must NEVER be honored for a
/// PREVIEW/external-tool row — no real baseline record can ever back one (see `classify`'s
/// doc comment: `classify_repo_findings`, the only legitimate producer of this status,
/// runs exclusively over the pre-preview finding set). Before this fix, every scan-time
/// preview finding (`scan_tools::preview_finding` defaults `status` to this exact string)
/// rendered "Pre-existing accepted debt (baseline suppression)" on a repo that had never
/// been triaged at all.
#[test]
fn a_preview_finding_with_suppressed_baseline_status_is_never_baseline_accepted() {
    let mut f = finding("SEC-1", "a.rs", 1, "high");
    f.status = "suppressed-baseline".to_string();
    f.preview = true;
    f.preview_tool = Some("semgrep".to_string());
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert_ne!(
        json.curated_findings[0].sites[0].disposition,
        "Pre-existing accepted debt (baseline suppression)",
        "a preview row must never be stamped as baseline-accepted"
    );
    assert_eq!(
        json.matrix.accepted.len(),
        0,
        "a preview row with no real baseline record must not land in Accepted"
    );
    // It still renders — never dropped — as an ordinary open row the severity×effort
    // quadrant can place normally.
    assert_ne!(json.curated_findings[0].sites[0].bucket, "accepted");
}

#[test]
fn suppressed_baseline_with_explicit_wire_disposition_defers_to_client() {
    // The auditor re-triaged a previously-suppressed finding this session — the
    // fresh, explicit decision wins over the durable baseline status.
    let mut f = finding("SEC-1", "a.rs", 1, "high");
    f.status = "suppressed-baseline".to_string();
    let mut dispositions = HashMap::new();
    dispositions.insert(finding_key(&f), wire("TechDebt", "", "Now"));
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &dispositions),
        None,
        &empty_opts(),
    );
    assert_eq!(json.matrix.do_now.len(), 1);
}

// ── Suppressed-inline -> waived in code ────────────────────────────────────

#[test]
fn suppressed_inline_with_no_wire_disposition_is_waived_in_code() {
    // An inline `camerata:allow` waiver must land in Accepted, not Unresolved/open —
    // the bug: an inline-suppressed finding was reported as still-open.
    let mut f = finding("SEC-1", "a.rs", 1, "high");
    f.status = "suppressed-inline".to_string();
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert_eq!(
        json.curated_findings[0].sites[0].disposition,
        "Waived in code (inline camerata:allow)"
    );
    assert_eq!(json.matrix.accepted.len(), 1);
    assert_eq!(json.matrix.do_now.len(), 0);
}

#[test]
fn suppressed_inline_with_explicit_wire_disposition_defers_to_client() {
    let mut f = finding("SEC-1", "a.rs", 1, "high");
    f.status = "suppressed-inline".to_string();
    let mut dispositions = HashMap::new();
    dispositions.insert(finding_key(&f), wire("TechDebt", "", "Now"));
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &dispositions),
        None,
        &empty_opts(),
    );
    assert_eq!(json.matrix.do_now.len(), 1);
}

// ── Citation join ──────────────────────────────────────────────────────────

#[tokio::test]
async fn citation_join_resolves_a_real_corpus_rule_id() {
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(
        errors.is_empty(),
        "corpus must load cleanly, got errors: {errors:?}"
    );
    let f = finding("SEC-NO-UNSAFE-DESERIALIZATION-1", "a.py", 1, "critical");
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );

    let group = &json.curated_findings[0];
    assert_eq!(group.citation.kind, "grounded");
    assert!(
        group
            .citation
            .sources
            .iter()
            .any(|s| s.url.contains("owasp.org")),
        "must cite OWASP for SEC-NO-UNSAFE-DESERIALIZATION-1, got: {:?}",
        group.citation.sources
    );
}

// ── Recommended-Fix: binds to authored `remediation`, NEVER `directive` (Fix 1) ─────

#[tokio::test]
async fn resolve_fix_binds_to_the_authored_remediation_and_substitutes_captures() {
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(
        errors.is_empty(),
        "corpus must load cleanly, got errors: {errors:?}"
    );
    let mut f = finding(
        "SUPABASE-RLS-ENABLED-1",
        "supabase/migrations/1.sql",
        1,
        "critical",
    );
    f.captures
        .insert("table".to_string(), "profiles".to_string());
    let fix = resolve_fix("SUPABASE-RLS-ENABLED-1", Some(&corpus), &f, None)
        .expect("SUPABASE-RLS-ENABLED-1 must have authored remediation");
    assert!(
        fix.contains("`profiles`") || fix.contains("profiles"),
        "expected the captured table name substituted into the authored remediation, got: {fix:?}"
    );
    assert!(
        !fix.to_lowercase().contains("timeline replay") && !fix.contains("emit a critical finding"),
        "must bind to the client-facing `remediation` text, never the detection-recipe \
             `directive` (which mentions the replay mechanism), got: {fix:?}"
    );
}

#[test]
fn resolve_fix_never_falls_back_to_directive_when_remediation_is_unauthored() {
    // Every real corpus rule's default option now has authored `remediation` (the corpus-wide
    // authoring passes closed that gap), so no REAL rule id can demonstrate the "unauthored"
    // path anymore — hardcoding one here would silently start asserting nothing the moment the
    // next rule got authored. Instead build a synthetic single-rule corpus via
    // `ruleset_with_unauthored_rule`: a real, resolvable default option with a real `directive`,
    // but `remediation: None`. This pins the fail-safe permanently, independent of authoring
    // state: absent remediation means an omitted Fix, never the directive text.
    let corpus = camerata_rules::ruleset_with_unauthored_rule("SEC-TEST-UNAUTHORED-1");
    let f = finding("SEC-TEST-UNAUTHORED-1", "a.py", 1, "critical");
    let fix = resolve_fix("SEC-TEST-UNAUTHORED-1", Some(&corpus), &f, None);
    assert_eq!(
        fix, None,
        "must omit the Fix block, not fall back to the rule's directive, when remediation is unauthored"
    );
}

#[test]
fn resolve_fix_is_none_not_fabricated_when_corpus_is_absent() {
    let f = finding("SEC-NO-UNSAFE-DESERIALIZATION-1", "a.py", 1, "critical");
    assert_eq!(
        resolve_fix("SEC-NO-UNSAFE-DESERIALIZATION-1", None, &f, None),
        None
    );
}

#[tokio::test]
async fn resolve_fix_is_none_when_rule_id_is_unknown_to_the_corpus() {
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(
        errors.is_empty(),
        "corpus must load cleanly, got errors: {errors:?}"
    );
    let f = finding("AI-CUSTOM-ARCH-RULE-1", "a.rs", 1, "medium");
    assert_eq!(
        resolve_fix("AI-CUSTOM-ARCH-RULE-1", Some(&corpus), &f, None),
        None
    );
}

// ── resolve_fix honors the evaluated/chosen option (2026-09-22 audit-integrated
// alternatives), not always the rule's default ──────────────────────────────────

/// `SUPABASE-RLS-ENABLED-1` has two options: the default
/// `enforce-rls-enabled-replayed-end-state` (authored remediation) and
/// `regex-grep-each-migration-independently` (NO authored remediation — a deliberately
/// rejected alternative in the corpus). This makes it a real fixture for proving
/// `evaluated_option_id` genuinely changes which option's remediation renders, not just
/// which rule's default happens to be picked.
#[tokio::test]
async fn resolve_fix_uses_the_finding_evaluated_option_id_over_the_project_chosen_option() {
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(
        errors.is_empty(),
        "corpus must load cleanly, got errors: {errors:?}"
    );
    let mut f = finding(
        "SUPABASE-RLS-ENABLED-1",
        "supabase/migrations/1.sql",
        1,
        "critical",
    );
    f.evaluated_option_id = Some("enforce-rls-enabled-replayed-end-state".to_string());
    // The chosen_option PARAMETER disagrees with the finding's evaluated_option_id — the
    // finding's tag must win (it's what this SPECIFIC finding was actually judged under).
    let fix = resolve_fix(
        "SUPABASE-RLS-ENABLED-1",
        Some(&corpus),
        &f,
        Some("regex-grep-each-migration-independently"),
    );
    assert!(
        fix.is_some(),
        "the evaluated option has authored remediation and must win over the chosen_option param"
    );
}

/// When the finding carries no `evaluated_option_id` (a legacy finding, or a rule the AI
/// tier never tagged), `resolve_fix` falls back to the project's `chosen_option` parameter
/// — and correctly OMITS the Fix block when that specific option has no authored
/// remediation, proving the per-option (not just per-rule) omit-if-unauthored contract.
#[tokio::test]
async fn resolve_fix_falls_back_to_the_chosen_option_param_when_the_finding_has_none() {
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(
        errors.is_empty(),
        "corpus must load cleanly, got errors: {errors:?}"
    );
    let f = finding(
        "SUPABASE-RLS-ENABLED-1",
        "supabase/migrations/1.sql",
        1,
        "critical",
    );
    assert!(f.evaluated_option_id.is_none());
    let fix = resolve_fix(
        "SUPABASE-RLS-ENABLED-1",
        Some(&corpus),
        &f,
        Some("regex-grep-each-migration-independently"),
    );
    assert_eq!(
        fix, None,
        "the chosen (non-default) option has no authored remediation — must omit, not \
             fall back to the default option's remediation or the directive"
    );
}

/// With neither `evaluated_option_id` nor a `chosen_option` param, resolution falls all the
/// way through to the corpus rule's own default — today's exact pre-feature behavior,
/// unchanged.
#[tokio::test]
async fn resolve_fix_falls_back_to_the_corpus_default_when_nothing_else_is_set() {
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(
        errors.is_empty(),
        "corpus must load cleanly, got errors: {errors:?}"
    );
    let f = finding(
        "SUPABASE-RLS-ENABLED-1",
        "supabase/migrations/1.sql",
        1,
        "critical",
    );
    let fix = resolve_fix("SUPABASE-RLS-ENABLED-1", Some(&corpus), &f, None);
    assert!(
        fix.is_some(),
        "the corpus default option has authored remediation"
    );
}

#[tokio::test]
async fn curated_finding_site_carries_the_fix_line_populated_from_authored_remediation() {
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(
        errors.is_empty(),
        "corpus must load cleanly, got errors: {errors:?}"
    );
    let mut f = finding(
        "SUPABASE-RLS-ENABLED-1",
        "supabase/migrations/1.sql",
        1,
        "critical",
    );
    f.captures
        .insert("table".to_string(), "profiles".to_string());
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );
    let fix = json.curated_findings[0].sites[0]
        .fix
        .as_deref()
        .expect("curated site's fix line must be populated from authored remediation");
    assert!(fix.contains("profiles"));
}

#[test]
fn curated_finding_site_fix_is_none_not_fabricated_without_a_corpus() {
    // Deliberately NOT an `AI-`-prefixed rule id: this test is about `resolve_fix`
    // (the "Fix:" line), not the P3 citation gate — an `AI-` id with no corpus would
    // also be held out of `curated_findings` entirely as uncited (see
    // `citation_gate_holds_an_unclassifiable_ai_finding_out_of_curated_as_uncited`),
    // which would make `json.curated_findings[0]` panic here for an unrelated reason.
    let f = finding("CUSTOM-ARCH-RULE-1", "a.rs", 1, "medium");
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert_eq!(json.curated_findings[0].sites[0].fix, None);
}

#[test]
fn curated_finding_site_fix_is_none_when_remediation_is_unauthored() {
    // Same rationale as `resolve_fix_never_falls_back_to_directive_when_remediation_is_unauthored`
    // above: every real corpus rule is now authored, so a synthetic single-rule corpus (via
    // `ruleset_with_unauthored_rule`) is the only way to permanently pin the "unauthored
    // remediation omits the Fix line" fail-safe, independent of corpus authoring state.
    //
    // C5-8: a row with NEITHER a rule-level fix NOR a codebase-specific one is now re-routed
    // to `held` entirely (see `curated_row_with_neither_fix_half_and_a_known_unauthored_rule_
    // is_held_not_curated` below) — that is a DIFFERENT, newer invariant than the one this
    // test exists to pin (that `resolve_fix`/`CuratedSiteJson::fix` itself never fabricates a
    // directive-sourced fallback). Giving the finding a `fix_specific` here keeps it curated
    // so this test still exercises exactly what it always did, undisturbed by C5-8.
    let corpus = camerata_rules::ruleset_with_unauthored_rule("SEC-TEST-UNAUTHORED-1");
    let mut f = finding("SEC-TEST-UNAUTHORED-1", "a.py", 1, "critical");
    f.fix_specific = Some(
        "A codebase-specific fix, independent of the rule's own (absent) \
                                authored remediation."
            .to_string(),
    );
    // C6-B4: this test is about the FIX half only — give it its own calibrated effort so
    // `is_action_row_missing_an_estimate` (a separate, newer gate for the OTHER half)
    // doesn't also re-route it to held and mask the assertion below.
    f.effort = Some("medium".to_string());
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );
    assert_eq!(
        json.curated_findings[0].sites[0].fix, None,
        "an unauthored rule's Fix block must be omitted (None), never backfilled from directive"
    );
}

/// C5-8 (R7 amended): the actual target scenario `curated_finding_site_fix_is_none_when_
/// remediation_is_unauthored` above used to (incorrectly, pre-C5-8) leave curated — a KNOWN
/// corpus rule with no authored remediation, paired with a finding that also has no
/// codebase-specific fix. Both fix halves are empty, so this row must never ship as a
/// do_now item with a blank Fix block: it is re-routed to `held`, fully visible, with the
/// reason disclosed in its own headline.
#[test]
fn curated_row_with_neither_fix_half_and_a_known_unauthored_rule_is_held_not_curated() {
    let corpus = camerata_rules::ruleset_with_unauthored_rule("SEC-TEST-UNAUTHORED-1");
    let f = finding("SEC-TEST-UNAUTHORED-1", "a.py", 1, "critical");
    assert_eq!(
        f.fix_specific, None,
        "sanity check: the shared `finding()` helper must not default to a fix_specific"
    );
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );

    assert!(
        json.curated_findings.is_empty(),
        "a row with no fix at all must never ship as a curated action item: {:?}",
        json.curated_findings
    );
    assert_eq!(
        json.matrix.held.len(),
        1,
        "it must be re-routed to held, not dropped: {:?}",
        json.matrix.held
    );
    assert_eq!(
        json.matrix.held[0].severity, "critical",
        "held never downgrades severity"
    );
    assert!(
        json.matrix.held[0]
            .headline
            .to_lowercase()
            .contains("fix not generated"),
        "the held row's own headline must disclose WHY: {}",
        json.matrix.held[0].headline
    );
    // It is still fully present in the held-for-review appendix group too, never dropped.
    let group = json
        .held_for_review_findings
        .iter()
        .find(|g| g.rule_id == "SEC-TEST-UNAUTHORED-1")
        .expect("the row must still be present in held_for_review_findings");
    assert_eq!(group.sites[0].bucket, "held");
    assert_eq!(group.sites[0].fix, None);
    assert_eq!(group.sites[0].fix_for_this_finding, None);
}

/// C5-8: a curated row WITH a fix (either half) is entirely unaffected — this is the
/// companion "never over-applies" half of the gate's test matrix.
#[tokio::test]
async fn curated_row_with_a_fix_stays_in_its_action_bucket() {
    let corpus = camerata_rules::ruleset_with_unauthored_rule("SEC-TEST-UNAUTHORED-1");

    // Half A: no rule-level remediation, but a codebase-specific fix WAS generated.
    let mut with_specific = finding("SEC-TEST-UNAUTHORED-1", "a.py", 1, "critical");
    with_specific.fix_specific =
        Some("Rotate the credential and remove it from source.".to_string());
    // C6-B4: give it its own calibrated effort so the separate estimate-availability gate
    // (`is_action_row_missing_an_estimate`) doesn't also re-route it to held — this test
    // is isolating the FIX half only, exactly like the test above.
    with_specific.effort = Some("low".to_string());
    let report = report_with(vec![with_specific], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );
    assert_eq!(json.matrix.held.len(), 0);
    assert_eq!(json.matrix.do_now.len(), 1);
    assert_eq!(json.curated_findings.len(), 1);

    // Half B: no codebase-specific fix, but the rule itself DOES carry authored remediation.
    let corpus_path = camerata_rules::corpus_path();
    let (real_corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");
    let mut rls = finding(
        "SUPABASE-RLS-ENABLED-1",
        "supabase/migrations/1.sql",
        1,
        "critical",
    );
    rls.captures
        .insert("table".to_string(), "profiles".to_string());
    let report2 = report_with(vec![rls], vec!["SUPABASE-RLS-ENABLED-1"]);
    let json2 = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report2.clone().seal(), &HashMap::new()),
        Some(&real_corpus),
        &empty_opts(),
    );
    assert_eq!(json2.matrix.held.len(), 0);
    assert_eq!(json2.matrix.do_now.len(), 1);
    assert_eq!(json2.curated_findings.len(), 1);
    assert!(json2.curated_findings[0].sites[0].fix.is_some());
}

// ── P4: floor findings get finding-level treatment ──────────────────────────────────

/// Every AUDIT_RULES floor rule fired against a synthetic (never a fixture/benchmark)
/// snippet, run through the REAL bundled corpus — no client-facing string (a curated
/// site's `headline`, its `detail`, or any matrix `FindingRefJson::headline`) may ever
/// start with "Deny": that word is the gate's own internal enforcement directive
/// (`camerata_gateway::RULE_REGISTRY`), never authored client prose. General over the
/// whole floor, not one rule — this is the P4 contract test the plan calls for.
#[tokio::test]
async fn no_client_facing_string_begins_with_deny_for_any_floor_rule_that_fires() {
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(
        errors.is_empty(),
        "corpus must load cleanly, got errors: {errors:?}"
    );

    let files = vec![
        (".env".to_string(), "SERVICE_ROLE_KEY=abc\n".to_string()),
        (
            "a.py".to_string(),
            "API_KEY = \"hardcoded-not-a-real-secret-abcdefgh\"\n".to_string(),
        ),
        (
            "b.py".to_string(),
            concat!("token = \"sk_li", "ve_abcdefghijklmnopqrstuvwx\"\n").to_string(),
        ),
        (
            "c.pem".to_string(),
            concat!(
                "-----BEGIN RSA PRIV",
                "ATE KEY-----\nMIIBFAKEFAKEFAKE\n-----END RSA PRIV",
                "ATE KEY-----\n"
            )
            .to_string(),
        ),
        (
            "d.py".to_string(),
            "q = \"SELECT * FROM users WHERE id = \" + user_id\n".to_string(),
        ),
        (
            "e.py".to_string(),
            "url = f\"https://api.example.com/x?api_key={key}\"\n".to_string(),
        ),
        (
            "f.py".to_string(),
            "requests.get(url, verify=False)\n".to_string(),
        ),
        ("g.py".to_string(), "yaml.load(data)\n".to_string()),
    ];
    let findings = crate::onboard::audit_files("owner/repo", &files);
    assert!(
        findings.len() >= 6,
        "fixture must actually trip most of the floor rules under test, got: {findings:?}"
    );

    let report = report_with(findings, Vec::new());
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );

    let mut offenders = Vec::new();
    for group in &json.curated_findings {
        for site in &group.sites {
            if site.headline.starts_with("Deny") {
                offenders.push(format!("{} headline: {:?}", group.rule_id, site.headline));
            }
            if site.detail.starts_with("Deny") {
                offenders.push(format!("{} detail: {:?}", group.rule_id, site.detail));
            }
        }
    }
    let matrix_buckets: [&Vec<FindingRefJson>; 6] = [
        &json.matrix.do_now,
        &json.matrix.do_next,
        &json.matrix.plan,
        &json.matrix.accepted,
        &json.matrix.informational,
        &json.matrix.held,
    ];
    for bucket in matrix_buckets {
        for f in bucket {
            if f.headline.starts_with("Deny") {
                offenders.push(format!("matrix headline: {:?}", f.headline));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "client-facing strings must never start with \"Deny\" (the gate's own internal \
             directive): {offenders:#?}"
    );
}

/// Every curated finding (floor or AI-tier) must carry a non-empty `est_hours` label — a
/// deterministic floor finding must never render "not yet estimated" (see
/// `default_effort_for` in `onboard::audit` and `effort_hours_bounds` above).
#[tokio::test]
async fn every_curated_finding_has_a_non_empty_est_hours() {
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(
        errors.is_empty(),
        "corpus must load cleanly, got errors: {errors:?}"
    );
    let files = vec![(".env".to_string(), "SERVICE_ROLE_KEY=abc\n".to_string())];
    let findings = crate::onboard::audit_files("owner/repo", &files);
    assert!(!findings.is_empty());
    let report = report_with(findings, Vec::new());
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );
    for group in &json.curated_findings {
        for site in &group.sites {
            let (_, hours_label) = effort_hours_bounds(site.effort.as_deref());
            assert_ne!(
                hours_label, "not yet estimated",
                "{}: curated finding must carry a real estimate, not the honest-gap label",
                group.rule_id
            );
        }
    }
}

// ── C4-P3: deterministic confidence-by-construction + estimate fallback ────────────────
//
// Deterministic-floor and RLS-replay rows never go through AI calibration, so `confidence`
// and `effort` are `None` on them forever unless something else fills them in. Before this
// pass, a PROVEN critical (a live secret, an unprotected table, a definer function with no
// pinned search_path) rendered "Confidence: not evaluated this run" / "Effort: not
// estimated this run" beside it — which reads as doubt next to exactly the findings a
// client is paying to act on first.

/// A deterministic finding (a realistic architectural-checker shape: `arch_violation_to_
/// finding` never sets `confidence` or `effort`) with a REAL location, against a rule whose
/// corpus TOML carries an authored effort band (`SUPABASE-FUNC-SEARCH-PATH-1`, authored
/// `effort = "low"` on its default option), must export `confidence == "high"` AND a
/// non-empty estimate — in `findings.json` (`CuratedSiteJson`), the xlsx `FindingRow`, AND
/// the compiled PDF's rendered text. Never "not evaluated this run" / "not estimated this
/// run" next to a proven defect.
#[tokio::test]
async fn deterministic_finding_with_real_location_exports_high_confidence_and_an_estimate() {
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(
        errors.is_empty(),
        "corpus must load cleanly, got errors: {errors:?}"
    );

    // Exactly the shape `onboard::architectural::arch_violation_to_finding` produces: no
    // calibration ever touches this finding, so `confidence`/`effort` start (and, absent
    // this fix, would stay) `None`.
    let f = finding(
        "SUPABASE-FUNC-SEARCH-PATH-1",
        "supabase/migrations/1.sql",
        8,
        "high",
    );
    assert_eq!(f.confidence, None, "fixture must simulate never-calibrated");
    assert_eq!(f.effort, None, "fixture must simulate never-calibrated");

    let report = report_with(vec![f.clone()], vec!["SUPABASE-FUNC-SEARCH-PATH-1"]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );

    // ── findings.json (CuratedSiteJson) ──
    let site = &json
        .curated_findings
        .iter()
        .find(|g| g.rule_id == "SUPABASE-FUNC-SEARCH-PATH-1")
        .expect("must be curated, not held out")
        .sites[0];
    assert_eq!(
        site.confidence,
        Some("high".to_string()),
        "a deterministic finding with a real location exports confidence 'high' by \
             construction: {:?}",
        site.confidence
    );
    assert_eq!(
        site.effort,
        Some("low".to_string()),
        "must fall back to the rule's authored effort band: {:?}",
        site.effort
    );

    // ── xlsx FindingRow (via the same findings.json export the product route ships) ──
    // `FindingRow`'s fields are private to `xlsx_export` (only the struct is `pub`), so a
    // sibling module inspects it the same way an external consumer of `findings.json`
    // would: through its serialized shape.
    let findings_export = crate::xlsx_export::build_findings_export(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        Some(&corpus),
        &json,
        &HashMap::new(),
    );
    let export_value =
        serde_json::to_value(&findings_export).expect("FindingsExport must serialize");
    let row = export_value["findings"]
        .as_array()
        .expect("findings must be an array")
        .iter()
        .find(|r| r["rule_id"] == "SUPABASE-FUNC-SEARCH-PATH-1")
        .expect("row must be present in the xlsx/findings.json export");
    assert_eq!(
        row["confidence"], "high",
        "xlsx confidence must match the PDF/json"
    );
    assert_eq!(
        row["effort"], "low",
        "xlsx effort must fall back to the authored band"
    );

    // ── PDF source ──
    if which_typst().is_none() {
        eprintln!(
            "skipping the PDF leg of \
                 deterministic_finding_with_real_location_exports_high_confidence_and_an_estimate: \
                 typst not on PATH"
        );
        return;
    }
    let pdf = compile_pdf(&json).await.expect("compile_pdf must succeed");
    let text = pdf_extract::extract_text_from_mem(&pdf)
        .expect("must be able to extract text from the compiled PDF");
    assert!(
        text.contains("Confidence: high"),
        "the PDF must render the deterministic-by-construction confidence: {text:?}"
    );
    assert!(
        !text.contains("Confidence: not evaluated this run"),
        "a proven deterministic finding must never render as unevaluated: {text:?}"
    );
    assert!(
        !text.contains("Effort: not estimated this run"),
        "a deterministic finding with an authored effort band must never render \
             unestimated: {text:?}"
    );
}

/// Sequencing proof with R1 (`downgrade_unlocated_above_informational`): a deterministic
/// finding downgraded to informational because it carries NO real location (empty path,
/// `line == 0` — the absence-only / platform-owned shape) must NOT get the "high by
/// construction" treatment. It stays hedged (`"needs-review"`), exactly like any other
/// informational row — proving the deterministic-confidence rule runs AFTER R1's downgrade
/// has already had its say, never before it.
#[test]
fn deterministic_finding_downgraded_to_informational_is_not_high() {
    let unlocated = finding("SUPABASE-RLS-POLICY-DISABLED-1", "", 0, "critical");
    assert_eq!(unlocated.confidence, None, "never calibrated");
    let report = report_with(vec![unlocated], vec!["SUPABASE-RLS-POLICY-DISABLED-1"]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );

    assert_eq!(
        json.matrix.informational.len(),
        1,
        "R1 must still downgrade the unlocated finding to informational: {:?}",
        json.matrix
    );
    // C4-P4 (residual defect 5): this finding's whole group is entirely informational (its
    // only site), so it lives in `held_for_review_findings`, not `curated_findings` — see
    // `AuditReportJson::curated_findings`'s doc comment.
    assert!(json.curated_findings.is_empty());
    let site = &json.held_for_review_findings[0].sites[0];
    assert_eq!(
        site.confidence,
        Some("needs-review".to_string()),
        "an informational deterministic row must stay hedged, never 'high': {:?}",
        site.confidence
    );
}

/// An AI-tier finding the calibrator genuinely marked doubtful keeps its calibrated
/// `"needs-review"` confidence (never overwritten to `"high"` — `is_ai_tier` excludes it from
/// the deterministic-by-construction path), and its calibrated effort still renders even
/// though the row is hedged — a needs-review verdict is a confidence judgement, not an
/// estimate failure. C5-4: a `high`-severity hedge like this one routes to `held` (never an
/// action bucket, regardless of severity) — the site lives in `held_for_review_findings`,
/// not `curated_findings`.
#[tokio::test]
async fn ai_tier_needs_review_row_keeps_its_confidence_and_still_carries_an_estimate() {
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(
        errors.is_empty(),
        "corpus must load cleanly, got errors: {errors:?}"
    );
    let mut f = finding(
        "SUPABASE-RLS-ENABLED-1",
        "supabase/migrations/1.sql",
        1,
        "high",
    );
    f.confidence = Some("needs-review".to_string());
    f.needs_review = true;
    f.effort = Some("medium".to_string());
    let report = report_with(vec![f], vec!["SUPABASE-RLS-ENABLED-1"]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );
    assert!(
        json.curated_findings.is_empty(),
        "a hedged row — even high severity — must never render under Curated findings: {:?}",
        json.curated_findings
    );
    let site = &json.held_for_review_findings[0].sites[0];
    assert_eq!(
        site.bucket, "held",
        "a high hedge routes to held, not an action bucket"
    );
    assert_eq!(
        site.severity, "high",
        "held never downgrades severity, unlike the informational appendix"
    );
    assert_eq!(
        site.confidence,
        Some("needs-review".to_string()),
        "a genuine calibration doubt verdict must be preserved, never promoted to high"
    );
    assert_eq!(
        site.effort,
        Some("medium".to_string()),
        "a hedged row still carries its own calibrated estimate — doubt about the \
             finding is not the same thing as an unestimated fix"
    );
}

/// The C4-P3 guardrail: nothing downstream may ever refuse or drop a row because it has no
/// calibration and no corpus to fall back on — the honest degrade is "not yet estimated",
/// never an omitted row. A bare deterministic finding with NO corpus at all (so neither a
/// calibrated effort NOR an authored band is available) still exports, still lands in its
/// severity-driven bucket, and still carries the deterministic-by-construction confidence.
#[test]
fn a_row_with_no_calibration_and_no_corpus_still_exports_unrefused() {
    let f = finding("ARCH-NEVER-CALIBRATED-1", "a.rs", 1, "high");
    assert_eq!(f.confidence, None);
    assert_eq!(f.effort, None);
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );

    assert_eq!(
        json.curated_findings.len(),
        1,
        "a finding with no calibration and no corpus must still be curated, never dropped"
    );
    let site = &json.curated_findings[0].sites[0];
    assert_eq!(
        site.confidence,
        Some("high".to_string()),
        "still gets the deterministic-by-construction confidence even with no corpus"
    );
    assert_eq!(
        site.effort, None,
        "with neither a calibrated effort nor a corpus to fall back on, the row \
             honestly reports no estimate rather than fabricating one"
    );
    // Bucket placement is untouched by any of this — a high-severity finding still lands
    // somewhere actionable, never silently excluded.
    let in_some_action_bucket =
        json.matrix.do_now.len() + json.matrix.do_next.len() + json.matrix.plan.len() == 1;
    assert!(
        in_some_action_bucket,
        "the row must land in exactly one action bucket, never vanish: {:?}",
        json.matrix
    );
}

/// A committed-secret floor finding (`SEC-NO-SECRET-FILE-1` on a real `.env`) renders a
/// headline naming the actual file, PLUS a `.gitignore`-coverage context fact in its
/// detail — never the gate's own "Deny writing a file whose path marks it as
/// secret-bearing…" directive.
#[tokio::test]
async fn committed_secret_finding_renders_a_specific_headline_and_a_context_fact() {
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(
        errors.is_empty(),
        "corpus must load cleanly, got errors: {errors:?}"
    );
    let files = vec![
        (".gitignore".to_string(), "node_modules/\n".to_string()),
        (".env".to_string(), "SERVICE_ROLE_KEY=abc\n".to_string()),
    ];
    let findings = crate::onboard::audit_files("owner/repo", &files);
    let report = report_with(findings, Vec::new());
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );
    let group = json
        .curated_findings
        .iter()
        .find(|g| g.rule_id == "SEC-NO-SECRET-FILE-1")
        .expect("SEC-NO-SECRET-FILE-1 must be curated for a real committed .env");
    let site = &group.sites[0];
    assert!(
        site.headline.contains(".env"),
        "headline must name the actual file, got: {:?}",
        site.headline
    );
    assert!(
        !site.headline.starts_with("Deny") && !site.detail.starts_with("Deny"),
        "must never render the gate directive: headline={:?} detail={:?}",
        site.headline,
        site.detail
    );
    assert!(
        site.detail.contains("gitignore") || site.detail.contains(".gitignore"),
        "detail must carry the gitignore-coverage context fact, got: {:?}",
        site.detail
    );
}

/// `SEC-NO-RAW-SQL-CONCAT-1` had NO corpus entry at all before P4 (flagged by P3's own
/// doc comment on `is_ai_tier`) — it must now resolve to a GROUNDED citation (CWE-89 +
/// OWASP), never "AI-advisory, model-inferred." (it isn't even AI-tier — it's a
/// deterministic floor rule, which makes the pre-P4 "advisory" label doubly wrong).
#[tokio::test]
async fn sec_no_raw_sql_concat_1_resolves_to_a_grounded_citation() {
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(
        errors.is_empty(),
        "corpus must load cleanly, got errors: {errors:?}"
    );
    let citation = resolve_citation("SEC-NO-RAW-SQL-CONCAT-1", None, Some(&corpus));
    assert_eq!(
        citation.kind, "grounded",
        "SEC-NO-RAW-SQL-CONCAT-1 must resolve to a grounded citation, got: {citation:?}"
    );
    assert!(
        citation
            .sources
            .iter()
            .any(|s| s.url.contains("cwe.mitre.org/data/definitions/89")),
        "must cite CWE-89 (SQL Injection), got: {:?}",
        citation.sources
    );
    assert!(
        citation.sources.iter().any(|s| s.url.contains("owasp.org")),
        "must also cite OWASP, got: {:?}",
        citation.sources
    );

    // End-to-end: a real finding of this rule, run through the curated-set builder,
    // must carry that same grounded citation — not fall through to "advisory".
    let f = finding("SEC-NO-RAW-SQL-CONCAT-1", "a.py", 1, "critical");
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );
    let group = json
        .curated_findings
        .iter()
        .find(|g| g.rule_id == "SEC-NO-RAW-SQL-CONCAT-1")
        .expect("SEC-NO-RAW-SQL-CONCAT-1 must be curated, not held out as uncited");
    assert_eq!(group.citation.kind, "grounded");
}

/// W3 (commodity-class taint layer) grounded-mapping test: a SYNTHETIC external-tool
/// finding whose OWN rule id is the semgrep taint rule's id (not a corpus id) still
/// resolves to OUR grounded citation (CWE-89/OWASP), OUR authored remediation, and never
/// loses its external-tool provenance (`preview = true`, `preview_tool = Some("semgrep")`)
/// — it must never render as our own deterministic tier, and never borrow a citation from
/// an unrelated rule.
#[tokio::test]
async fn taint_sqli_tool_rule_id_resolves_through_the_grounding_map() {
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(
        errors.is_empty(),
        "corpus must load cleanly, got errors: {errors:?}"
    );

    let mut f = finding(
        "camerata.security.taint-sql-injection-go",
        "internal/db.go",
        88,
        "high",
    );
    f.preview = true;
    f.preview_tool = Some("semgrep".to_string());

    let citation = resolve_citation(&f.rule_id, f.preview_tool.as_deref(), Some(&corpus));
    assert_eq!(
        citation.kind, "grounded",
        "a taint rule's OWN (non-corpus) rule id must still resolve to a grounded citation \
             via the grounding map, got: {citation:?}"
    );
    assert!(
        citation
            .sources
            .iter()
            .any(|s| s.url.contains("cwe.mitre.org/data/definitions/89")),
        "must cite CWE-89 (the SAME citation SEC-NO-RAW-SQL-CONCAT-1 cites), got: {:?}",
        citation.sources
    );

    let fix = resolve_fix(&f.rule_id, Some(&corpus), &f, None);
    assert!(
        fix.is_some_and(|s| !s.trim().is_empty()),
        "must resolve OUR authored remediation via the grounding map"
    );

    // Provenance stays honest: never promoted to our own deterministic tier.
    assert!(
        f.preview,
        "grounding a citation must never flip preview to false"
    );
    assert_eq!(
        f.preview_tool.as_deref(),
        Some("semgrep"),
        "grounding a citation must never clear preview_tool (never 'our own tier')"
    );

    // End-to-end: the curated-set builder renders it grounded too, not advisory.
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );
    let group = json
        .curated_findings
        .iter()
        .find(|g| g.rule_id == "camerata.security.taint-sql-injection-go")
        .expect("must be curated, not held out as uncited");
    assert_eq!(group.citation.kind, "grounded");
}

/// Sensitive-data-in-logs taint class grounded-mapping test: SYNTHETIC external-tool
/// findings for BOTH the secret/credential family (`...taint-log-secret-python`, "high")
/// and the PII family (`...taint-log-pii-python`, "medium") resolve to the SAME grounded
/// corpus rule (SEC-NO-SENSITIVE-DATA-IN-LOGS-1 / CWE-532) with our citation and authored
/// remediation, while each KEEPS its own distinct severity — confirming the grounding join
/// (`corpus_rule_for`) never touches a finding's own severity (see that function's doc
/// comment), which is what makes the two-severity design in the YAML's section comment
/// actually hold end to end rather than being silently flattened once grounded.
#[tokio::test]
async fn taint_log_sink_tool_rule_ids_resolve_through_the_grounding_map_with_distinct_severities() {
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(
        errors.is_empty(),
        "corpus must load cleanly, got errors: {errors:?}"
    );

    for (rule_id, severity) in [
        ("camerata.security.taint-log-secret-python", "high"),
        ("camerata.security.taint-log-pii-python", "medium"),
    ] {
        let mut f = finding(rule_id, "app/handlers.py", 23, severity);
        f.preview = true;
        f.preview_tool = Some("semgrep".to_string());

        let citation = resolve_citation(&f.rule_id, f.preview_tool.as_deref(), Some(&corpus));
        assert_eq!(
            citation.kind, "grounded",
            "{rule_id} must resolve to a grounded citation via the grounding map, got: {citation:?}"
        );
        assert!(
            citation
                .sources
                .iter()
                .any(|s| s.url.contains("cwe.mitre.org/data/definitions/532")),
            "{rule_id} must cite CWE-532 (Insertion of Sensitive Information into Log File), got: {:?}",
            citation.sources
        );

        let fix = resolve_fix(&f.rule_id, Some(&corpus), &f, None);
        assert!(
            fix.is_some_and(|s| !s.trim().is_empty()),
            "{rule_id} must resolve OUR authored remediation via the grounding map"
        );

        assert!(
            f.preview,
            "grounding a citation must never flip preview to false"
        );
        assert_eq!(f.preview_tool.as_deref(), Some("semgrep"));

        // End-to-end: the curated-set builder renders it grounded, AND preserves this
        // finding's own distinct severity — never flattened to one shared value.
        let report = report_with(vec![f], vec![]);
        let json = build_report_json(
            &crate::report_export::ReviewedReport::unreviewed(
                &report.clone().seal(),
                &HashMap::new(),
            ),
            Some(&corpus),
            &empty_opts(),
        );
        let group = json
            .curated_findings
            .iter()
            .find(|g| g.rule_id == rule_id)
            .unwrap_or_else(|| panic!("{rule_id} must be curated, not held out as uncited"));
        assert_eq!(group.citation.kind, "grounded");
        assert_eq!(
            group.sites.first().map(|s| s.severity.as_str()),
            Some(severity),
            "{rule_id} must keep its own severity ({severity}) through the curated-set builder"
        );
    }
}

/// ESLint wiring (JAVASCRIPT-REACT-EXHAUSTIVE-DEPS-1 / RULES-OF-HOOKS-1): a SYNTHETIC
/// eslint finding whose OWN rule id is the real ESLint SARIF `ruleId`
/// (`react-hooks/exhaustive-deps`, not a corpus id) still resolves to OUR grounded
/// citation (the React docs lint-rule page), OUR authored remediation, and never loses
/// its external-tool provenance (`preview = true`, `preview_tool = Some("eslint")`) —
/// it must never render as our own deterministic tier. Mirrors
/// `taint_sqli_tool_rule_id_resolves_through_the_grounding_map` above for Semgrep.
#[tokio::test]
async fn react_hooks_exhaustive_deps_tool_rule_id_resolves_through_the_grounding_map() {
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(
        errors.is_empty(),
        "corpus must load cleanly, got errors: {errors:?}"
    );

    let mut f = finding(
        "react-hooks/exhaustive-deps",
        "src/UseThing.tsx",
        42,
        "medium",
    );
    f.preview = true;
    f.preview_tool = Some("eslint".to_string());

    let citation = resolve_citation(&f.rule_id, f.preview_tool.as_deref(), Some(&corpus));
    assert_eq!(
        citation.kind, "grounded",
        "the eslint-native rule id must still resolve to a grounded citation via the \
             grounding map, got: {citation:?}"
    );
    assert!(
        citation.sources.iter().any(|s| s
            .url
            .contains("react.dev/reference/eslint-plugin-react-hooks")),
        "must cite the SAME React docs source JAVASCRIPT-REACT-EXHAUSTIVE-DEPS-1 cites, \
             got: {:?}",
        citation.sources
    );

    let fix = resolve_fix(&f.rule_id, Some(&corpus), &f, None);
    assert!(
        fix.is_some_and(|s| !s.trim().is_empty()),
        "must resolve OUR authored remediation via the grounding map"
    );

    // Provenance stays honest: never promoted to our own deterministic tier.
    assert!(
        f.preview,
        "grounding a citation must never flip preview to false"
    );
    assert_eq!(
        f.preview_tool.as_deref(),
        Some("eslint"),
        "grounding a citation must never clear preview_tool (never 'our own tier')"
    );

    // End-to-end: the curated-set builder renders it grounded too, not advisory.
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );
    let group = json
        .curated_findings
        .iter()
        .find(|g| g.rule_id == "react-hooks/exhaustive-deps")
        .expect("must be curated, not held out as uncited");
    assert_eq!(group.citation.kind, "grounded");
}

/// A tool rule id with NO grounding mapping at all (an unrecognized id) must still fall
/// back to the honest generic "preview" label — the grounding map is additive, it never
/// makes an ungrounded id LOOK grounded.
#[test]
fn ungrounded_tool_rule_id_still_falls_back_to_preview_label() {
    let citation = resolve_citation("camerata.security.some-future-rule", Some("semgrep"), None);
    assert_eq!(citation.kind, "preview");
    assert!(citation.label.contains("semgrep"));
}

/// Defect 1/3 (2026-10-07 grading pass): the preview citation label must never say
/// "Deterministic" (reserved for Camerata's own proven floor checks) or use internal
/// "wired into the CI gate" phrasing — both leaked into the client PDF/xlsx via this
/// exact label before the fix.
#[test]
fn preview_citation_label_has_no_deterministic_or_ci_gate_phrasing() {
    let citation = resolve_citation("camerata.security.some-future-rule", Some("semgrep"), None);
    assert_eq!(citation.kind, "preview");
    for banned in ["Deterministic", "wired into the CI gate"] {
        assert!(
            !citation.label.contains(banned),
            "preview citation label must never contain {banned:?}: {}",
            citation.label
        );
    }
}

/// A clean repo (no floor findings at all) must still render without panicking, with an
/// empty curated set and empty matrix — the P4 machinery must never assume at least one
/// floor finding exists.
#[tokio::test]
async fn a_clean_repo_with_no_floor_findings_still_renders_fine() {
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(
        errors.is_empty(),
        "corpus must load cleanly, got errors: {errors:?}"
    );
    let files = vec![("a.py".to_string(), "print('hello world')\n".to_string())];
    let findings = crate::onboard::audit_files("owner/repo", &files);
    assert!(findings.is_empty(), "fixture must genuinely be clean");
    let report = report_with(findings, Vec::new());
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );
    assert!(json.curated_findings.is_empty());
    assert_eq!(json.matrix.do_now.len(), 0);
    assert_eq!(json.matrix.do_next.len(), 0);
    assert_eq!(json.matrix.plan.len(), 0);
    assert_eq!(json.matrix.informational.len(), 0);
}

// ── P2: fix_specific shown as the PRIMARY fix, generic `fix` secondary ──────────────

#[test]
fn curated_finding_site_shows_fix_specific_as_fix_for_this_finding() {
    let mut f = finding("ARCH-1", "app/auth/signout/route.ts", 12, "high");
    f.fix_specific = Some(
        "Use `safeInternalPath` from lib/redirect.ts, as app/auth/signin/route.ts does."
            .to_string(),
    );
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert_eq!(
        json.curated_findings[0].sites[0]
            .fix_for_this_finding
            .as_deref(),
        Some("Use `safeInternalPath` from lib/redirect.ts, as app/auth/signin/route.ts does.")
    );
}

#[test]
fn curated_finding_site_fix_for_this_finding_is_none_when_never_generated() {
    // Back-compat / deterministic-only-scan case: `fix_specific` was never set.
    let f = finding("ARCH-1", "a.rs", 1, "high");
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert_eq!(json.curated_findings[0].sites[0].fix_for_this_finding, None);
}

// ── C3-1b: fix-generation failure is a PIPELINE gap, never a confidence hedge ───────

/// The deterministic finding from the C3-1b spec: its specific fix failed to generate
/// (`fix_specific: None`, exactly `generate_fix_specifics`' new no-hedge failure state —
/// `needs_review: false`, `confidence: None`, `detail` carrying no pipeline-state tag).
/// It must export UNHEDGED (disposition is never demoted), it must NOT be
/// informational/out-of-bucket, it must still land in `do_now` on its own severity/effort
/// merits, and it must carry the RULE-level fix (`resolve_fix`'s authored remediation) so
/// the row still ships a usable fix even with no codebase-specific one.
///
/// C4-P3: `confidence` is NOT `None` here — this finding never went through AI calibration
/// (`f.confidence` starts `None` and stays that way), so by `hedge_confidence`'s
/// deterministic-by-construction rule it exports `"high"`. This is the exact fix C4-P3
/// targets: before it, this SUPABASE-RLS-ENABLED-1 do_now critical rendered "Confidence: not
/// evaluated this run" next to a proven defect. The "ONLY hedge source is calibration"
/// invariant this test originally asserted is still true in spirit — a fix-generation
/// failure still never HEDGES this row (never flips it to `"needs-review"` or
/// informational) — it just no longer leaves a non-hedged, never-calibrated row blank.
#[tokio::test]
async fn fix_generation_failure_exports_unhedged_with_the_rule_level_fix_in_do_now() {
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(
        errors.is_empty(),
        "corpus must load cleanly, got errors: {errors:?}"
    );
    let mut f = finding(
        "SUPABASE-RLS-ENABLED-1",
        "supabase/migrations/1.sql",
        1,
        "critical",
    );
    f.captures
        .insert("table".to_string(), "profiles".to_string());
    // The exact post-failure state `generate_fix_specifics` now leaves behind: no
    // fix_specific, no hedge, no tag — see that function's tests in ai_audit.rs.
    f.fix_specific = None;
    f.needs_review = false;
    f.confidence = None;

    let report = report_with(vec![f], vec!["SUPABASE-RLS-ENABLED-1"]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );

    assert_eq!(
        json.matrix.do_now.len(),
        1,
        "a critical finding must stay in do_now even when its specific fix failed to \
             generate: {:?}",
        json.matrix
    );
    assert!(
        json.matrix.informational.is_empty(),
        "a fix-generation failure must never route a finding to the informational \
             appendix: {:?}",
        json.matrix.informational
    );
    let site = &json.curated_findings[0].sites[0];
    assert_eq!(
        site.confidence,
        Some("high".to_string()),
        "C4-P3: a never-calibrated, non-hedged deterministic finding exports confidence \
             'high' by construction, instead of blank — a fix-generation failure must not \
             change that"
    );
    assert_eq!(
        site.fix_for_this_finding, None,
        "no codebase-specific fix was generated"
    );
    assert!(
        site.fix.is_some(),
        "the row must still carry the rule's own authored remediation as its fix: {:?}",
        site.fix
    );
    assert!(
        !site.detail.contains('['),
        "detail must carry no bracketed pipeline-state text: {:?}",
        site.detail
    );
    assert!(
        site.disposition.to_ascii_lowercase().contains("do now"),
        "the site's own disposition label must not be demoted either: {:?}",
        site.disposition
    );
}

/// The other half of the C3-1b contract: a finding the CALIBRATOR itself explicitly
/// flagged doubtful (`confidence: Some("needs-review")`) still exports hedged — proving
/// confidence hedging is driven by calibration doubt, not by whether a separate AI pass
/// (fix-generation) happened to succeed. C5-4: this `high`-severity hedge lives in
/// `held_for_review_findings` (the `held` bucket), not `curated_findings`.
#[tokio::test]
async fn calibrator_flagged_doubt_still_exports_hedged() {
    // Setting `confidence` marks a finding AI-tier (`is_ai_tier`), which routes an
    // uncited AI finding to the informational appendix and OUT of `curated_findings`
    // entirely (P3's citation gate) — a real corpus rule keeps this finding grounded so
    // the test exercises the hedge itself, not that unrelated gate.
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(
        errors.is_empty(),
        "corpus must load cleanly, got errors: {errors:?}"
    );
    let mut f = finding(
        "SUPABASE-RLS-ENABLED-1",
        "supabase/migrations/1.sql",
        1,
        "high",
    );
    f.confidence = Some("needs-review".to_string());
    f.needs_review = true;
    let report = report_with(vec![f], vec!["SUPABASE-RLS-ENABLED-1"]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );
    assert!(
        json.curated_findings.is_empty(),
        "a hedged row, even high severity, must never render under Curated findings: {:?}",
        json.curated_findings
    );
    assert_eq!(
        json.held_for_review_findings[0].sites[0].confidence,
        Some("needs-review".to_string()),
        "a genuine calibration doubt verdict must still render as hedged"
    );
}

#[test]
fn a_critical_finding_with_a_valid_fix_still_lands_in_do_now() {
    // A critical finding whose fix-generation ran and succeeded behaves exactly as one
    // that never went through fix-generation at all (e.g. a deterministic-only scan) —
    // bucket placement never depended on `fix_specific` being present.
    let f = finding("ARCH-1", "a.rs", 1, "critical");
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert_eq!(json.matrix.do_now.len(), 1);
}

// ── Placeholder substitution helper ─────────────────────────────────────────

#[test]
fn instantiate_remediation_fills_known_tokens_from_captures() {
    let mut captures = std::collections::BTreeMap::new();
    captures.insert("table".to_string(), "profiles".to_string());
    captures.insert("bucket".to_string(), "avatars".to_string());
    let text = instantiate_remediation(
        "Enable RLS on `<table>` and lock down the `<bucket>` bucket.",
        "supabase/migrations/1.sql",
        &captures,
    );
    assert_eq!(
        text,
        "Enable RLS on `profiles` and lock down the `avatars` bucket."
    );
}

#[test]
fn instantiate_remediation_fills_path_and_file_tokens_from_the_finding_path() {
    let captures = std::collections::BTreeMap::new();
    let text = instantiate_remediation(
        "Fix the issue in <path> (also see <file>).",
        "apps/web/lib/analytics.ts",
        &captures,
    );
    assert_eq!(
        text,
        "Fix the issue in apps/web/lib/analytics.ts (also see apps/web/lib/analytics.ts)."
    );
}

#[test]
fn instantiate_remediation_falls_back_to_a_readable_generic_for_unfilled_known_tokens() {
    let captures = std::collections::BTreeMap::new();
    let text = instantiate_remediation(
        "Re-enable verify_jwt for <function-name>.",
        "supabase/config.toml",
        &captures,
    );
    assert_eq!(text, "Re-enable verify_jwt for the affected function.");
}

#[test]
fn instantiate_remediation_never_emits_a_raw_token_for_an_unknown_placeholder() {
    let captures = std::collections::BTreeMap::new();
    let text = instantiate_remediation(
        "Check <some-totally-unmapped-token> before shipping.",
        "a.rs",
        &captures,
    );
    assert!(
        !text.contains('<') && !text.contains('>'),
        "raw token escaped: {text:?}"
    );
    assert_eq!(text, "Check the affected resource before shipping.");
}

// ── C4-R3 defect 4: a literal angle-bracketed EXAMPLE in authored prose is not a token ──

/// General regression for the "a payload like the affected resource renders as inert text"
/// bug: `instantiate_remediation` used to treat EVERY `<...>` span as a placeholder to
/// fill, so an authored sentence showing a literal HTML/markup example got its own example
/// swallowed by the generic filler. A real placeholder token is always a bare
/// lowercase-and-hyphens identifier (`<path>`, `<function-name>`, `<a:secret-kind>`, …); a
/// span containing spaces, `=`, or uppercase letters is prose, not a token, and must render
/// completely unchanged.
#[test]
fn instantiate_remediation_treats_a_literal_bracketed_example_as_prose_not_a_placeholder() {
    let captures = std::collections::BTreeMap::new();
    let text = instantiate_remediation(
        "Verify by confirming a payload like <img src=x onerror=alert(1)> renders as \
             inert text or is stripped, never executes.",
        "a.tsx",
        &captures,
    );
    assert_eq!(
        text,
        "Verify by confirming a payload like <img src=x onerror=alert(1)> renders as \
             inert text or is stripped, never executes."
    );
    assert!(
        !text.contains("the affected resource"),
        "a literal example must never be replaced by the generic placeholder filler: {text:?}"
    );
}

/// A real, known placeholder token sitting right next to a literal bracketed example in the
/// SAME sentence must still be filled normally — the token-shape check must not become
/// overly broad and start treating legitimate tokens as prose too.
#[test]
fn instantiate_remediation_still_fills_a_real_token_alongside_a_literal_example() {
    let mut captures = std::collections::BTreeMap::new();
    captures.insert("table".to_string(), "profiles".to_string());
    let text = instantiate_remediation(
        "Enable RLS on <table>; an example payload like <SELECT 1> must then fail.",
        "a.sql",
        &captures,
    );
    assert_eq!(
        text,
        "Enable RLS on profiles; an example payload like <SELECT 1> must then fail."
    );
}

#[test]
fn is_placeholder_token_shape_accepts_only_lowercase_hyphenated_identifiers() {
    assert!(is_placeholder_token_shape("path"));
    assert!(is_placeholder_token_shape("function-name"));
    assert!(is_placeholder_token_shape("secret-kind"));
    assert!(!is_placeholder_token_shape(""));
    assert!(!is_placeholder_token_shape("img src=x onerror=alert(1)"));
    assert!(!is_placeholder_token_shape("SELECT 1"));
    assert!(!is_placeholder_token_shape("Entity"));
    assert!(!is_placeholder_token_shape("T, E"));
}

#[test]
fn instantiate_remediation_fills_p4_context_fact_tokens_from_captures_and_generic_fallback() {
    let mut captures = std::collections::BTreeMap::new();
    captures.insert(
        "secret-kind".to_string(),
        "a live Stripe secret key".to_string(),
    );
    let filled = instantiate_remediation(
        "Found <secret-kind> in <path>, which is <gitignore-status>.",
        ".env",
        &captures,
    );
    assert_eq!(
        filled,
        "Found a live Stripe secret key in .env, which is not confirmed against this \
             repository's `.gitignore`."
    );
}

#[test]
fn instantiate_remediation_prefers_a_capture_over_the_generic_even_when_both_available() {
    let mut captures = std::collections::BTreeMap::new();
    captures.insert("table".to_string(), "orders".to_string());
    let text = instantiate_remediation("Review <table> now.", "a.sql", &captures);
    assert_eq!(text, "Review orders now.");
}

// ── W4 (2026-09-30): headline-rewriter grammar bugs ─────────────────────────────
//
// Doubled article ("a live a <vendor> secret key") and a self-referential substitution
// ("a live .env file that is committed to this repository" — the file described as
// containing itself). Both were template/variable-substitution bugs: every value
// `classify_secret_kind` (onboard::audit) returns is now a BARE noun phrase, and
// `instantiate_remediation`'s `<a:token>` syntax computes the correct article at
// substitution time via `indefinite_article` rather than a template hardcoding one that
// can collide with a value that already carries its own.

#[test]
fn indefinite_article_uses_the_vowel_letter_for_an_ordinary_word() {
    assert_eq!(indefinite_article("vendor credential token"), "a");
    assert_eq!(indefinite_article("Anthropic API key"), "an");
    assert_eq!(indefinite_article("Java key store"), "a");
}

#[test]
fn indefinite_article_uses_the_vowel_sound_for_an_acronym_read_letter_by_letter() {
    // "SSH" is pronounced "ess-ess-aitch" — a vowel SOUND despite "S" being a consonant
    // LETTER. A naive first-letter check would wrongly pick "a".
    assert_eq!(indefinite_article("SSH private key"), "an");
    // "AWS" ("ay-double-u-ess") already starts on a vowel LETTER too, so both checks agree.
    assert_eq!(indefinite_article("AWS access key"), "an");
    // "PKCS#12 key store" — acronym, but "P" is not in the vowel-sound exception set.
    assert_eq!(indefinite_article("PKCS#12 key store"), "a");
}

#[test]
fn instantiate_remediation_a_token_prepends_the_correct_article_without_doubling() {
    let mut captures = std::collections::BTreeMap::new();
    captures.insert("secret-kind".to_string(), "live `.env` file".to_string());
    let text = instantiate_remediation(
        "Your `<path>` file is <a:secret-kind> committed to this repository.",
        ".env",
        &captures,
    );
    assert_eq!(
        text,
        "Your `.env` file is a live `.env` file committed to this repository."
    );
    assert!(!text.contains("a a") && !text.contains("an a") && !text.contains("a an"));
}

#[test]
fn instantiate_remediation_a_token_falls_back_to_the_generic_bare_noun() {
    // No "secret-kind" capture at all — `<a:secret-kind>` must still resolve through the
    // generic fallback (also a bare noun, per its own doc comment) and prepend one article,
    // never leave a raw token or double up.
    let captures = std::collections::BTreeMap::new();
    let text = instantiate_remediation("Found <a:secret-kind> here.", "a.py", &captures);
    assert_eq!(text, "Found a hardcoded credential here.");
}

/// Golden-string, secret-FILE case (the self-referential bug's original shape): a real
/// `.env` committed to the repo. The old template read "…contains a live `.env` file that
/// is committed to this repository" — describing the file as containing itself. Pinned to
/// the exact rewritten sentence so a future edit can't reintroduce either bug silently.
#[tokio::test]
async fn w4_golden_secret_file_headline_has_no_doubled_article_and_is_not_self_referential() {
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");
    let files = vec![(".env".to_string(), "SERVICE_ROLE_KEY=abc\n".to_string())];
    let findings = crate::onboard::audit_files("owner/repo", &files);
    let f = findings
        .iter()
        .find(|f| f.rule_id == "SEC-NO-SECRET-FILE-1")
        .expect("a real committed .env must fire SEC-NO-SECRET-FILE-1")
        .clone();
    let (headline, _detail) =
        resolve_floor_finding_text("SEC-NO-SECRET-FILE-1", Some(&corpus), &f, None)
            .expect("SEC-NO-SECRET-FILE-1 must have an authored floor template");
    assert_eq!(
        headline,
        "Your `.env` file is a live `.env` file committed to this repository."
    );
    for doubled in ["a a ", "an a ", "a an ", "a live a ", "an an "] {
        assert!(
            !headline.contains(doubled),
            "doubled article {doubled:?} in {headline:?}"
        );
    }
    assert!(
        !headline.contains("contains") || !headline.contains("that is committed"),
        "must not describe the file as containing a committed copy of itself: {headline:?}"
    );
}

/// Golden-string, vendor-TOKEN case (the doubled-article bug's original shape): a
/// hardcoded AWS access key. The old template read "…contains a live an AWS access key"
/// (`"a live <secret-kind>"` with a secret-kind value that already carried its own
/// article) — pinned to the exact rewritten sentence.
#[tokio::test]
async fn w4_golden_vendor_token_headline_has_no_doubled_article() {
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");
    let files = vec![(
        "config.py".to_string(),
        concat!("aws_key = \"AK", "IAABCDEFGHIJKLMNOP\"\n").to_string(),
    )];
    let findings = crate::onboard::audit_files("owner/repo", &files);
    let f = findings
        .iter()
        .find(|f| f.rule_id == "SEC-NO-VENDOR-TOKEN-1")
        .expect("a hardcoded AWS key must fire SEC-NO-VENDOR-TOKEN-1")
        .clone();
    let (headline, _detail) =
        resolve_floor_finding_text("SEC-NO-VENDOR-TOKEN-1", Some(&corpus), &f, None)
            .expect("SEC-NO-VENDOR-TOKEN-1 must have an authored floor template");
    assert_eq!(
        headline,
        "Your `config.py` file contains a live AWS access key."
    );
    for doubled in ["a a ", "an a ", "a an ", "a live a ", "a live an "] {
        assert!(
            !headline.contains(doubled),
            "doubled article {doubled:?} in {headline:?}"
        );
    }
}

// ── Item 3: what's-healthy / curated-finding citations are EXTERNAL authorities only ──

#[tokio::test]
async fn citation_join_drops_internal_enforcement_md_citation_and_keeps_external_ones() {
    // SEC-NO-UNSAFE-DESERIALIZATION-1's corpus entry carries an internal
    // `docs/ENFORCEMENT.md` source (Camerata's own scaffolding, meaningless in an
    // arbitrary client repo) ALONGSIDE two real external OWASP sources. The report must
    // cite the OWASP sources and drop the internal one entirely, not just visually
    // de-emphasize it.
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(
        errors.is_empty(),
        "corpus must load cleanly, got errors: {errors:?}"
    );
    let citation = resolve_citation("SEC-NO-UNSAFE-DESERIALIZATION-1", None, Some(&corpus));
    assert_eq!(citation.kind, "grounded");
    assert!(
        !citation
            .sources
            .iter()
            .any(|s| !is_external_source_url(&s.url)),
        "an internal (non http/https) source leaked into the report: {:?}",
        citation.sources
    );
    assert!(
        !citation.label.contains("ENFORCEMENT.md") && !citation.label.contains("RULE_REGISTRY"),
        "the citation label must not mention Camerata's own internal scaffolding docs: {}",
        citation.label
    );
    assert!(
        citation.sources.iter().any(|s| s.url.contains("owasp.org")),
        "the real external OWASP sources must still be cited: {:?}",
        citation.sources
    );
}

#[test]
fn is_external_source_url_accepts_only_real_http_urls() {
    assert!(is_external_source_url("https://owasp.org/foo"));
    assert!(is_external_source_url("http://example.com"));
    assert!(!is_external_source_url("docs/ENFORCEMENT.md"));
    assert!(!is_external_source_url(""));
}

// ── Defect 4: evidence_negates_requirement / parse_evidence_citation ──────────────

#[test]
fn evidence_negates_requirement_detects_the_three_real_world_examples() {
    assert!(evidence_negates_requirement(
        "three sequential writes with no transaction"
    ));
    assert!(evidence_negates_requirement(
        "an external chargeable call with no idempotency-key handling"
    ));
    assert!(evidence_negates_requirement(
        "authorization done as ad hoc checks inside business logic"
    ));
}

#[test]
fn evidence_negates_requirement_is_false_for_a_positive_confirmation() {
    // The positive twin: evidence that CONFIRMS compliance, with no negation phrase, must
    // never trip the gate.
    assert!(!evidence_negates_requirement(
        "opens a transaction and passes the handle to each repository call"
    ));
    assert!(!evidence_negates_requirement(
        "every external chargeable call is issued with a client-generated idempotency key"
    ));
    assert!(!evidence_negates_requirement(
        "authorization is centralized in a single policy-enforcement middleware"
    ));
}

#[test]
fn evidence_negates_requirement_is_case_insensitive() {
    assert!(evidence_negates_requirement(
        "THREE SEQUENTIAL WRITES WITH NO TRANSACTION"
    ));
}

#[test]
fn parse_evidence_citation_splits_path_and_line_on_em_dash() {
    assert_eq!(
        parse_evidence_citation("src/orders/service.rs:142 — three sequential writes"),
        ("src/orders/service.rs".to_string(), 142)
    );
}

#[test]
fn parse_evidence_citation_handles_a_bare_path_colon_line_with_no_note() {
    assert_eq!(
        parse_evidence_citation("src/billing/charge.rs:58"),
        ("src/billing/charge.rs".to_string(), 58)
    );
}

#[test]
fn parse_evidence_citation_never_mistakes_a_hyphenated_path_for_a_note_separator() {
    // A bare hyphen inside the path (no surrounding spaces) must NOT be treated as a
    // "path - note" separator — only " — " (em dash) or " - " (spaced hyphen) count.
    assert_eq!(
        parse_evidence_citation("src/my-service/orders.rs:142"),
        ("src/my-service/orders.rs".to_string(), 142)
    );
}

/// W21 — an unparseable citation yields NO path, rather than a sentence masquerading as
/// one. This test previously asserted the opposite (`path == the whole evidence text`),
/// on the reasoning that the fallback should "never drop the evidence text". The text is
/// still never dropped: every caller puts it in `snippet`, and the negated-requirement
/// gate also quotes it in `detail`. What changed is that it no longer arrives in `path`,
/// a client-facing file location, where it shipped ten rows on hearth with sentences up
/// to 180 characters long rendered as file paths in the PDF and the workbook.
#[test]
fn parse_evidence_citation_yields_no_path_when_unparseable() {
    let (path, line) = parse_evidence_citation("this evidence has no file:line citation at all");
    assert_eq!(line, 0);
    assert_eq!(path, "", "prose must never be returned as a file path");
}

/// The real shape that leaked, taken verbatim from the 2026-10-18 hearth export: a
/// citation whose line part is a RANGE (`15-39`), not a single number, followed by a
/// prose note. `rsplit_once(':')` leaves `"15-39"`, which does not parse as a `usize`, so
/// this falls to the unparseable branch — which is why the whole sentence used to become
/// the path.
#[test]
fn parse_evidence_citation_rejects_a_line_range_rather_than_pathifying_the_note() {
    let (path, line) = parse_evidence_citation(
        "app/api/bookings/route.ts:15-39 — booking insert, payment insert, and slot \
             update execute as three separate awaits with no transaction boundary",
    );
    assert_eq!(line, 0);
    assert_eq!(
        path, "",
        "a line RANGE is not a parseable citation, and the note must not become the path"
    );
}

#[test]
fn citation_join_still_labels_a_non_ai_tier_unknown_rule_id_as_ai_advisory() {
    // P3 scopes the citation gate to AI-TIER findings only (`is_ai_tier`) — a
    // non-AI-tier finding (no `AI-` rule id, no calibration `confidence`) whose rule id
    // has no corpus entry is a DIFFERENT, pre-existing gap (e.g. `SEC-NO-RAW-SQL-CONCAT-1`
    // has no TOML entry at all) that this pass does not touch: it stays exactly as
    // before, curated with the honest "AI-advisory, model-inferred." label.
    let f = finding("CUSTOM-ARCH-RULE-1", "a.rs", 1, "medium");
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert_eq!(json.curated_findings[0].citation.kind, "advisory");
    assert_eq!(
        json.curated_findings[0].citation.label,
        "AI-advisory, model-inferred."
    );
}

// ── P3 (2026-09-29): the citation gate ──────────────────────────────────────────────
// the codebase-inspection hardening notes, P3.

/// An AI-tier finding (`AI-`-prefixed rule id) whose defect does not classify into any
/// of the P3 closed taxonomy classes must be held OUT of `curated_findings` entirely —
/// never rendered with "AI-advisory, model-inferred." on a curated row — and instead
/// surfaces in the informational/held-for-review appendix with an honest "uncited"
/// headline, counted in the reconciling totals.
#[test]
fn citation_gate_holds_an_unclassifiable_ai_finding_out_of_curated_as_uncited() {
    let f = finding("AI-CUSTOM-ARCH-RULE-1", "a.rs", 1, "medium");
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert!(
        json.curated_findings.is_empty(),
        "an uncited AI finding must never appear in curated_findings: {:?}",
        json.curated_findings
    );
    assert_eq!(
        json.matrix.informational.len(),
        1,
        "the uncited finding must still surface (over-tell), just outside the action tiers"
    );
    assert_eq!(
        json.matrix.informational[0].rule_id,
        "AI-CUSTOM-ARCH-RULE-1"
    );
    assert!(
        json.matrix.informational[0]
            .headline
            .to_lowercase()
            .contains("uncited"),
        "the appendix row must say WHY it's held out: {}",
        json.matrix.informational[0].headline
    );
    assert_eq!(json.executive_summary.held_for_review, 1);
    assert_eq!(json.executive_summary.curated_total, 0);
    // Reconciliation still holds: nothing is silently dropped.
    assert_eq!(
        json.executive_summary.candidates_reviewed,
        json.executive_summary.curated_total
            + json.executive_summary.held_for_review
            + json.executive_summary.excluded_false_positive
            + json.executive_summary.dependency_advisories
    );
}

/// The gate applies REGARDLESS of severity — a critical, uncited AI finding (exactly
/// the "MOST important findings" the plan's problem statement calls out) must still be
/// excluded from curated, unlike `is_informational`'s other signals which deliberately
/// never touch critical/high.
///
/// C5-7: a critical/high uncited finding used to land in `matrix.informational` — the SAME
/// bucket `is_informational`'s own hard invariant caps at `<= medium` everywhere else,
/// which this row silently violated (the gate's own `C5-5`-flagged open test). It now
/// routes to `matrix.held` instead, at its real (uncited) severity — never informational,
/// never an action bucket, never dropped.
#[test]
fn citation_gate_routes_a_critical_uncited_ai_finding_to_held_not_informational() {
    let f = finding("AI-SOME-NOVEL-DEFECT", "a.rs", 1, "critical");
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert!(json.curated_findings.is_empty());
    assert_eq!(json.matrix.do_now.len(), 0);
    assert_eq!(
        json.matrix.informational.len(),
        0,
        "a critical uncited finding must never sit in the severity-capped informational \
             appendix: {:?}",
        json.matrix.informational
    );
    assert_eq!(
        json.matrix.held.len(),
        1,
        "a critical uncited finding must route to held, the severity-unbounded needs-\
             review bucket: {:?}",
        json.matrix.held
    );
    assert_eq!(
        json.matrix.held[0].severity, "critical",
        "held never downgrades severity, same as the calibration-hedge case"
    );
    assert_eq!(json.matrix.held[0].rule_id, "AI-SOME-NOVEL-DEFECT");
    // The severity histogram (cover stats) still honestly reflects it — over-tell, not
    // hidden from the reader entirely, just excluded from the curated action tiers.
    assert_eq!(json.cover.stats.critical, 1);
}

/// C5-7: provenance/citation are ALWAYS the tier/citation of the row's OWN rule id, never
/// a sibling finding's — a held, uncited AI-tier row must never display "Deterministic"
/// provenance or another rule's citation, even when a genuinely grounded deterministic
/// finding is exported right alongside it in the same report. Exercised through
/// `findings.json` (`FindingRow`), the one exported shape that resolves `provenance` and
/// `citation_urls` independently of the PDF's curated/held-for-review grouping, so this
/// stays a meaningful cross-check of `FindingRow`'s own resolution regardless of which
/// `CuratedGroupJson` bucket (C6-B1) the same row also lands in.
#[tokio::test]
async fn uncited_ai_row_provenance_and_citation_are_never_borrowed_from_a_sibling() {
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");

    let grounded = finding("SEC-NO-HARDCODED-SECRETS-1", "a.rs", 1, "critical");
    let uncited_ai = finding("AI-SOME-NOVEL-DEFECT", "b.rs", 2, "critical");
    let report = report_with(vec![grounded, uncited_ai], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );
    let findings_export = crate::xlsx_export::build_findings_export(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        Some(&corpus),
        &json,
        &HashMap::new(),
    );
    // `FindingRow`'s fields are private to `xlsx_export` (only `findings.json`'s serialized
    // shape is the stable public contract) — go through `serde_json::Value`, same as the
    // `export_invariants_gate` module's own fixture crawlers do.
    let export_value =
        serde_json::to_value(&findings_export).expect("FindingsExport must serialize");
    let rows = export_value["findings"]
        .as_array()
        .expect("findings must be an array");

    let ai_row = rows
        .iter()
        .find(|r| r["rule_id"].as_str() == Some("AI-SOME-NOVEL-DEFECT"))
        .expect("the uncited AI row must still be exported in findings.json");
    assert_eq!(
        ai_row["provenance"].as_str(),
        Some("AI-advisory"),
        "an uncited AI-tier row must never display borrowed \"Deterministic\" provenance: \
             {ai_row:?}"
    );
    assert!(
        ai_row["citation_urls"].as_str().unwrap_or("").is_empty(),
        "an uncited AI-tier row's citation must be its own model-inferred label, not a \
             sibling's URLs: {:?}",
        ai_row["citation_urls"]
    );

    let secret_row = rows
        .iter()
        .find(|r| r["rule_id"].as_str() == Some("SEC-NO-HARDCODED-SECRETS-1"))
        .expect("the grounded deterministic row must still be exported");
    assert_eq!(secret_row["provenance"].as_str(), Some("Deterministic"));

    // And the bucket split itself: the uncited critical AI row is held, out of every
    // action bucket and out of informational; the grounded deterministic row stays curated.
    assert_eq!(json.matrix.held.len(), 1);
    assert_eq!(json.matrix.held[0].rule_id, "AI-SOME-NOVEL-DEFECT");
    assert_eq!(json.matrix.do_now.len(), 1);
    assert_eq!(json.matrix.do_now[0].rule_id, "SEC-NO-HARDCODED-SECRETS-1");
}

/// C5-7: a grounded AI-tier row (its class fallback resolves to a real published standard
/// — here, RLS) is UNAFFECTED by the held-routing fix: it is not uncited, so
/// `is_uncited_ai_finding` never fires for it, and a critical grounded AI finding stays
/// exactly where its severity×effort quadrant puts it (`do_now`), never diverted to `held`.
#[tokio::test]
async fn a_grounded_ai_row_with_a_real_citation_is_unaffected_by_the_held_route() {
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(errors.is_empty(), "corpus must load cleanly: {errors:?}");

    let mut rls = finding(
        "AI-CROSS-TENANT-RLS-1",
        "supabase/migrations/1.sql",
        1,
        "critical",
    );
    rls.detail = "Row Level Security is disabled on the payments table.".to_string();
    let report = report_with(vec![rls], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );

    assert!(is_ai_tier(&finding(
        "AI-CROSS-TENANT-RLS-1",
        "x",
        1,
        "critical"
    )));
    assert_eq!(
        json.matrix.held.len(),
        0,
        "a grounded AI finding must never be routed to held: {:?}",
        json.matrix.held
    );
    assert_eq!(json.matrix.do_now.len(), 1);
    assert_eq!(json.matrix.do_now[0].rule_id, "AI-CROSS-TENANT-RLS-1");
    assert_eq!(json.curated_findings.len(), 1);
    assert_ne!(json.curated_findings[0].citation.kind, "advisory");
}

/// A non-AI-tier finding's own advisory citation is untouched by the gate (see
/// `citation_join_still_labels_a_non_ai_tier_unknown_rule_id_as_ai_advisory` above) —
/// this is the `is_uncited_ai_finding` unit-level lock-in of that same scoping decision.
#[test]
fn is_uncited_ai_finding_is_false_for_a_non_ai_tier_finding_even_when_advisory() {
    let f = finding("CUSTOM-ARCH-RULE-1", "a.rs", 1, "medium");
    assert!(!is_ai_tier(&f));
    assert!(!is_uncited_ai_finding(&f, None));
}

#[test]
fn is_ai_tier_detects_the_ai_prefixed_rule_id_or_a_calibration_confidence() {
    let invented = finding("AI-SOME-DEFECT", "a.rs", 1, "medium");
    assert!(is_ai_tier(&invented));

    let mut adopted = finding("SUPABASE-RLS-ENABLED-1", "a.rs", 1, "critical");
    adopted.confidence = Some("high".to_string());
    assert!(is_ai_tier(&adopted));

    let deterministic = finding("SEC-NO-RAW-SQL-CONCAT-1", "a.rs", 1, "critical");
    assert!(!is_ai_tier(&deterministic));
}

// ── classify_ai_finding: pure, unit-testable, no model calls ────────────────────────

#[test]
fn classify_ai_finding_maps_each_required_class_by_general_keywords() {
    // Every signal is a defect-family vocabulary word (an OWASP/CWE term or the real
    // framework API a fix touches), never a fixture-specific rule id or identifier —
    // these rule ids are deliberately generic/invented, distinct from any one benchmark.
    assert_eq!(
        classify_ai_finding(
            "AI-DEFECT-1",
            Some("rls-policy"),
            "Row Level Security is disabled on the orders table, allowing cross-tenant reads."
        ),
        Some(AiFindingClass::Rls)
    );
    assert_eq!(
        classify_ai_finding(
            "AI-DEFECT-2",
            None,
            "User-supplied comment text is passed to dangerouslySetInnerHTML without sanitization, a stored XSS sink."
        ),
        Some(AiFindingClass::Xss)
    );
    assert_eq!(
        classify_ai_finding(
            "AI-DEFECT-3",
            None,
            "The `next` query parameter is redirected to without validation, an open redirect to an attacker-controlled host."
        ),
        Some(AiFindingClass::OpenRedirect)
    );
    assert_eq!(
        classify_ai_finding(
            "AI-DEFECT-4",
            None,
            "The password-reset token is generated with Math.random(), an insecure randomness source for a security token."
        ),
        Some(AiFindingClass::WeakTokenRandomness)
    );
    assert_eq!(
        classify_ai_finding(
            "AI-DEFECT-5",
            None,
            "User input is concatenated directly into a PostgREST .or() filter grammar expression, a query-grammar injection."
        ),
        Some(AiFindingClass::QueryGrammarInjection)
    );
    assert_eq!(
        classify_ai_finding(
            "AI-DEFECT-6",
            None,
            "The CORS middleware reflects the request's Origin header while Access-Control-Allow-Credentials is true."
        ),
        Some(AiFindingClass::PermissiveCors)
    );
}

#[test]
fn classify_ai_finding_also_matches_on_rule_id_tokens_alone() {
    assert_eq!(
        classify_ai_finding("AI-XSS-COMMENT-RENDER-1", None, "unrelated detail text"),
        Some(AiFindingClass::Xss)
    );
    assert_eq!(
        classify_ai_finding("AI-CORS-REFLECTED-ORIGIN-CREDENTIALS", None, "detail"),
        Some(AiFindingClass::PermissiveCors)
    );
}

#[test]
fn classify_ai_finding_returns_none_for_an_unmapped_defect() {
    assert_eq!(
        classify_ai_finding(
            "AI-SOME-NOVEL-ARCHITECTURAL-OBSERVATION",
            None,
            "The service layer bypasses the repository abstraction in a way nothing above classifies."
        ),
        None
    );
}

// ── The class -> corpus-rule mapping is grounded in the REAL bundled corpus ─────────

/// A conservative allowlist of real, well-known standards/linter-doc hosts a P3 grounded
/// citation is permitted to cite — a Camerata-internal doc citing itself would fail this,
/// which is the whole point (see `resolve_citation`'s `is_external_source_url`, which this
/// complements with a stronger "is it actually a recognized authority" check).
fn is_known_authority_url(url: &str) -> bool {
    const KNOWN_AUTHORITY_HOSTS: &[&str] = &[
        "cwe.mitre.org",
        "owasp.org",
        "cheatsheetseries.owasp.org",
        "supabase.com",
        "react.dev",
        "developer.mozilla.org",
        "postgrest.org",
    ];
    is_external_source_url(url)
        && KNOWN_AUTHORITY_HOSTS.iter().any(|host| {
            url.starts_with(&format!("https://{host}/"))
                || url.starts_with(&format!("http://{host}/"))
        })
}

#[tokio::test]
async fn each_required_class_resolves_to_a_grounded_citation_from_a_known_authority() {
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(
        errors.is_empty(),
        "corpus must load cleanly, got errors: {errors:?}"
    );

    for class in [
        AiFindingClass::Rls,
        AiFindingClass::Xss,
        AiFindingClass::OpenRedirect,
        AiFindingClass::WeakTokenRandomness,
        AiFindingClass::QueryGrammarInjection,
        AiFindingClass::PermissiveCors,
    ] {
        let rule_id = class.grounding_rule_id();
        let citation = resolve_citation(rule_id, None, Some(&corpus));
        assert_eq!(
            citation.kind, "grounded",
            "{rule_id} (class {class:?}) must resolve to a grounded citation"
        );
        assert!(
            !citation.label.trim().is_empty(),
            "{rule_id}'s citation label must not be empty"
        );
        assert_ne!(
            citation.label, "AI-advisory, model-inferred.",
            "{rule_id} must never render the AI-advisory fallback label"
        );
        assert!(
            !citation.sources.is_empty(),
            "{rule_id} must carry at least one real source"
        );
        for source in &citation.sources {
            assert!(
                is_known_authority_url(&source.url),
                "{rule_id}'s source {:?} is not a well-formed, known-authority URL",
                source
            );
        }
    }
}

/// Full-report contract test (EXTENSIVE per the plan): a synthetic fixture carries one
/// AI-tier finding per required class plus one unmappable AI finding and one
/// non-AI-tier finding with no corpus entry. After `build_report_json`, ZERO curated
/// findings may carry a model-inferred or empty citation, and the unmapped finding must
/// be the only one held out as uncited.
#[tokio::test]
async fn zero_curated_findings_carry_a_model_inferred_or_empty_citation() {
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(
        errors.is_empty(),
        "corpus must load cleanly, got errors: {errors:?}"
    );

    let mut rls = finding(
        "AI-CROSS-TENANT-RLS-1",
        "supabase/migrations/1.sql",
        1,
        "critical",
    );
    rls.detail = "Row Level Security is disabled on the payments table.".to_string();

    let mut xss = finding("AI-STORED-XSS-1", "web/comment.tsx", 12, "high");
    xss.detail =
        "A stored comment body is rendered via dangerouslySetInnerHTML without sanitization."
            .to_string();

    let mut redirect = finding("AI-LOGIN-REDIRECT-1", "web/login.ts", 40, "medium");
    redirect.detail =
        "The `next` redirect target is not validated against an allowlist, an open redirect."
            .to_string();

    let mut token = finding("AI-RESET-TOKEN-1", "api/reset.ts", 8, "high");
    token.detail =
        "The password-reset token uses Math.random(), insecure randomness for a security token."
            .to_string();

    let mut grammar = finding("AI-FILTER-INJECTION-1", "api/search.ts", 22, "high");
    grammar.detail =
        "A search term is concatenated into a PostgREST .or() filter grammar expression."
            .to_string();

    let mut cors = finding("AI-CORS-CREDS-1", "middleware.ts", 5, "medium");
    cors.detail =
        "CORS reflects the request Origin while Access-Control-Allow-Credentials is true."
            .to_string();

    let unmapped = finding("AI-SOME-NOVEL-DEFECT", "a.rs", 1, "medium");

    let report = report_with(
        vec![rls, xss, redirect, token, grammar, cors, unmapped],
        vec![],
    );
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );

    assert_eq!(
        json.curated_findings.len(),
        6,
        "the six mapped AI-tier findings must all survive as curated, distinct groups: {:?}",
        json.curated_findings
            .iter()
            .map(|g| &g.rule_id)
            .collect::<Vec<_>>()
    );
    for group in &json.curated_findings {
        assert_ne!(
            group.citation.kind, "advisory",
            "curated rule {} carries an advisory (uncited) citation",
            group.rule_id
        );
        assert!(
            !group.citation.label.trim().is_empty(),
            "curated rule {} carries an empty citation label",
            group.rule_id
        );
        assert_ne!(
            group.citation.label, "AI-advisory, model-inferred.",
            "curated rule {} renders the model-inferred fallback label",
            group.rule_id
        );
    }
    assert!(
        !json
            .curated_findings
            .iter()
            .any(|g| g.rule_id == "AI-SOME-NOVEL-DEFECT"),
        "the unmapped AI finding must never appear in curated_findings"
    );
    assert_eq!(
        json.matrix.informational.len(),
        1,
        "exactly the one unmapped AI finding is held for review"
    );
    assert_eq!(json.matrix.informational[0].rule_id, "AI-SOME-NOVEL-DEFECT");
}

#[test]
fn citation_join_labels_preview_finding_by_its_real_tool() {
    let mut f = finding("PREVIEW-RULE-1", "a.py", 1, "medium");
    f.preview = true;
    f.preview_tool = Some("ruff".to_string());
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert_eq!(json.curated_findings[0].citation.kind, "preview");
    assert!(json.curated_findings[0].citation.label.contains("ruff"));
}

// ── What's-healthy derivation ──────────────────────────────────────────────
