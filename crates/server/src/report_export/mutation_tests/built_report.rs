//! Whole-report scenarios through `build_report_json` and the PDF path: curated order, the
//! integrity rows, the ledger stage, the scorecard, disclosures and the typst invocation.

use super::*;

// ---- curated-section order in a built report -----------------------------------------------

/// The ids sort the other way alphabetically, and the medium finding comes first in the scan,
/// so neither id order nor input order can produce the expected order by accident.
#[test]
fn curated_findings_lead_with_the_most_severe_group_whatever_the_input_order() {
    let mut corpus = camerata_rules::RuleSet::default();
    corpus.push(fixable_rule("SORT-A-MEDIUM-1"));
    corpus.push(fixable_rule("SORT-Z-CRITICAL-1"));
    let finding = |rule_id: &str, line: usize, severity: &str| Finding {
        repo: "owner/repo".to_string(),
        path: "src/a.rs".to_string(),
        line,
        rule_id: rule_id.to_string(),
        severity: severity.to_string(),
        snippet: format!("snippet-{line}"),
        detail: format!("Detail for {rule_id}."),
        ..Finding::default()
    };
    let report = report_of(vec![
        finding("SORT-A-MEDIUM-1", 1, "medium"),
        finding("SORT-Z-CRITICAL-1", 2, "critical"),
    ]);
    let json = build_report_json(
        &report.seal(),
        &HashMap::new(),
        Some(&corpus),
        &ReportOptions::default(),
    );
    let order: Vec<&str> = json
        .curated_findings
        .iter()
        .map(|g| g.rule_id.as_str())
        .collect();
    assert_eq!(order, vec!["SORT-Z-CRITICAL-1", "SORT-A-MEDIUM-1"]);
}

// ---- the sites-vs-rows integrity row -------------------------------------------------------

#[test]
fn a_site_shortfall_is_disclosed_as_one_held_row_anchored_on_the_rule() {
    use camerata_api_types::audit::{DeviationKind, FindingKind};
    let deviation = FindingKind::Deviation(DeviationKind {
        deviating_sites: 5,
        assessed_sites: Some(9),
        convention_option_id: None,
        convention_label: "the house convention".to_string(),
    });
    let row = |line: usize| Finding {
        repo: "owner/repo".to_string(),
        path: "src/handlers.rs".to_string(),
        line,
        rule_id: "DEV-RULE-1".to_string(),
        severity: "low".to_string(),
        located: true,
        kind: Some(deviation.clone()),
        ..Finding::default()
    };
    let mut report = report_of(vec![row(10), row(20)]);
    apply_sites_vs_rows_check(&mut report);
    assert_eq!(report.findings.len(), 3, "one shortfall row is added");
    let shortfall = &report.findings[2];
    assert_eq!(
        (
            shortfall.repo.as_str(),
            shortfall.path.as_str(),
            shortfall.line
        ),
        ("owner/repo", "src/handlers.rs", 0)
    );
    assert_eq!(shortfall.severity, "medium");
    assert!(!shortfall.located);
    assert!(shortfall.needs_review);
    assert_eq!(
        shortfall.snippet,
        "5 deviating site(s) established; 2 represented in this report"
    );
    assert!(
        shortfall.detail.contains("only 2 of them are represented")
            && shortfall
                .detail
                .contains("The remaining 3 sites are not individually listed"),
        "{}",
        shortfall.detail
    );
    // Idempotent: a second pass finds the disclosure and adds nothing.
    apply_sites_vs_rows_check(&mut report);
    assert_eq!(report.findings.len(), 3);
}

// ---- the zero-finding evidence row ---------------------------------------------------------

/// Built through serde so every defaulted field takes its wire default.
fn recommendation(rule_id: &str, evidence: &str) -> crate::ai_audit::RuleRecommendation {
    serde_json::from_value(serde_json::json!({
        "rule_id": rule_id,
        "recommended_option_id": "default",
        "recommendation_reasoning": "",
        "evidence": evidence,
    }))
    .expect("a minimal recommendation deserializes")
}

#[test]
fn negating_evidence_on_a_clean_rule_becomes_one_held_defect_row() {
    let mut report = report_of(Vec::new());
    report.provenance.audited_rule_ids = vec!["HEALTHY-1".to_string(), "UNCITED-1".to_string()];
    let cited = "src/auth.rs:42 - handlers run without any auth check";
    let uncited = "handlers run without any auth check";
    report
        .recommendations
        .insert("HEALTHY-1".to_string(), recommendation("HEALTHY-1", cited));
    report.recommendations.insert(
        "UNCITED-1".to_string(),
        recommendation("UNCITED-1", uncited),
    );
    apply_zero_finding_evidence_check(&mut report);
    report.findings.sort_by(|a, b| a.rule_id.cmp(&b.rule_id));
    assert_eq!(report.findings.len(), 2);

    let row = &report.findings[0];
    assert_eq!(row.rule_id, "HEALTHY-1");
    assert_eq!(
        (row.repo.as_str(), row.path.as_str(), row.line),
        ("owner/repo", "src/auth.rs", 42)
    );
    assert_eq!(row.severity, "medium");
    assert_eq!(row.snippet, cited);
    assert!(
        row.detail.starts_with(&format!(
            "This rule's own evaluation evidence reads \"{cited}\""
        )),
        "{}",
        row.detail
    );
    assert!(row.needs_review);
    assert_eq!(row.confidence.as_deref(), Some("needs-review"));
    assert!(row.located, "the evidence named a file");
    assert_eq!(
        row.kind,
        Some(camerata_api_types::audit::FindingKind::Defect)
    );
    assert!(
        !report.findings[1].located,
        "evidence with no parseable file is not a location"
    );
}

// ---- the report-build ledger stage ---------------------------------------------------------

#[test]
fn the_report_partition_accounts_for_every_row_so_no_integrity_gap_is_disclosed() {
    let corpus = curated_corpus();
    let json = build_report_json(
        &partition_report().seal(),
        &HashMap::new(),
        Some(&corpus),
        &ReportOptions::default(),
    );
    assert_eq!(
        (
            json.matrix.do_now.len(),
            json.matrix.plan.len(),
            json.matrix.held.len(),
            json.matrix.informational.len()
        ),
        (1, 1, 1, 1)
    );
    assert!(
        json.methodology
            .failed_passes
            .iter()
            .all(|note| !note.to_ascii_lowercase().contains("pipeline integrity")),
        "{:?}",
        json.methodology.failed_passes
    );
    // Four open rows, two of them held or informational: two still-open ACTION items.
    assert_eq!(json.executive_summary.open, 2);
}

#[test]
fn neither_a_held_row_nor_a_curated_row_is_labelled_unestimated() {
    let corpus = curated_corpus();
    let mut report = partition_report();
    // No effort anywhere for the curated row: the rule's band is removed and the finding has
    // none. A missing estimate never re-labels a curated row (C6-B4 amended).
    let mut no_band = fixable_rule("PART-NOW-1");
    no_band.options[0].effort = None;
    let mut corpus_without_band = camerata_rules::RuleSet::default();
    corpus_without_band.push(no_band);
    for rule in corpus.iter().filter(|r| r.id_str() != "PART-NOW-1") {
        corpus_without_band.push(rule.clone());
    }
    report.findings[0].effort = None;
    let json = build_report_json(
        &report.seal(),
        &HashMap::new(),
        Some(&corpus_without_band),
        &ReportOptions::default(),
    );
    let headlines: Vec<&str> = json
        .matrix
        .do_now
        .iter()
        .chain(json.matrix.held.iter())
        .chain(json.matrix.informational.iter())
        .map(|f| f.headline.as_str())
        .chain(
            json.curated_findings
                .iter()
                .chain(json.held_for_review_findings.iter())
                .flat_map(|g| g.sites.iter().map(|s| s.headline.as_str())),
        )
        .collect();
    assert!(headlines.len() >= 3, "{headlines:?}");
    for headline in headlines {
        assert!(!headline.contains("effort not estimated"), "{headline}");
    }
}

// ---- the scorecard and what's healthy ------------------------------------------------------

#[test]
fn the_scorecard_rates_each_category_and_leads_with_the_most_urgent() {
    let mut corpus = camerata_rules::RuleSet::default();
    corpus.push(fixable_rule_in("SC-MID-1", "mid"));
    corpus.push(fixable_rule_in("SC-ZETA-1", "zeta"));
    corpus.push(fixable_rule_in("SC-ALPHA-1", "alpha"));
    corpus.push(fixable_rule_in("SC-ZETA-CLEAN-1", "zeta"));
    let finding = |rule_id: &str, line: usize, severity: &str| Finding {
        repo: "owner/repo".to_string(),
        path: "src/a.rs".to_string(),
        line,
        rule_id: rule_id.to_string(),
        severity: severity.to_string(),
        snippet: format!("snippet-{line}"),
        detail: format!("Detail for {rule_id}."),
        ..Finding::default()
    };
    let mut report = report_of(vec![
        finding("SC-ALPHA-1", 1, "low"),
        finding("SC-ZETA-1", 2, "medium"),
        finding("SC-MID-1", 3, "high"),
    ]);
    report.provenance.audited_rule_ids = vec![
        "SC-MID-1".to_string(),
        "SC-ZETA-1".to_string(),
        "SC-ALPHA-1".to_string(),
        "SC-ZETA-CLEAN-1".to_string(),
    ];
    let json = build_report_json(
        &report.seal(),
        &HashMap::new(),
        Some(&corpus),
        &ReportOptions::default(),
    );
    let rows: Vec<(&str, &str, usize, usize)> = json
        .scorecard
        .rows
        .iter()
        .map(|r| {
            (
                r.category.as_str(),
                r.status.as_str(),
                r.audited_rules,
                r.clean_rules,
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            ("Mid", "Action-needed", 1, 0),
            ("Zeta", "Attention", 2, 1),
            ("Alpha", "Clean", 1, 0),
        ]
    );
}

#[test]
fn whats_healthy_lists_grounded_rules_first() {
    let mut grounded = fixable_rule_in("Z-GROUNDED-1", "test-fixture");
    grounded.sources = vec![camerata_rules::RuleSource {
        url: "https://cwe.mitre.org/data/definitions/89.html".to_string(),
        title: "CWE-89".to_string(),
        linter: None,
        relevance: None,
    }];
    grounded.verification = camerata_rules::Verification::Grounded;
    let mut corpus = camerata_rules::RuleSet::default();
    corpus.push(grounded);
    corpus.push(fixable_rule_in("A-PLAIN-1", "test-fixture"));
    let mut report = report_of(Vec::new());
    report.provenance.audited_rule_ids = vec!["A-PLAIN-1".to_string(), "Z-GROUNDED-1".to_string()];
    let json = build_report_json(
        &report.seal(),
        &HashMap::new(),
        Some(&corpus),
        &ReportOptions::default(),
    );
    let order: Vec<&str> = json
        .whats_healthy
        .rules
        .iter()
        .map(|r| r.rule_id.as_str())
        .collect();
    assert_eq!(order, vec!["Z-GROUNDED-1", "A-PLAIN-1"]);
}

// ---- disclosures, the summary override and the PDF timestamp -------------------------------

#[test]
fn only_a_rule_with_no_wired_detector_is_disclosed_as_unchecked() {
    use crate::scan_ledger::{RuleTier, NO_WIRED_DETECTOR_REASON};
    let mut report = report_of(Vec::new());
    report.ledger.record_rule(
        "OFF-STACK-1",
        RuleTier::Deterministic,
        false,
        Some("not applicable to this stack".to_string()),
        0,
        0,
    );
    report
        .ledger
        .record_rule("RAN-1", RuleTier::Deterministic, true, None, 3, 0);
    let unchecked_note = |report: &crate::onboard::ScanReport| {
        build_report_json(
            &report.clone().seal(),
            &HashMap::new(),
            None,
            &ReportOptions::default(),
        )
        .methodology
        .failed_passes
        .into_iter()
        .find(|note| note.contains("could not be mechanically checked"))
    };
    assert_eq!(unchecked_note(&report), None);
    report.ledger.record_rule(
        "NO-DETECTOR-1",
        RuleTier::Deterministic,
        false,
        Some(NO_WIRED_DETECTOR_REASON.to_string()),
        0,
        0,
    );
    assert_eq!(
        unchecked_note(&report).as_deref(),
        Some("1 rule could not be mechanically checked this run and was reviewed by the model instead.")
    );
}

#[test]
fn a_blank_summary_override_is_ignored() {
    let opts = ReportOptions {
        executive_summary_override: Some("   ".to_string()),
        ..ReportOptions::default()
    };
    let json = build_report_json(
        &partition_report().seal(),
        &HashMap::new(),
        Some(&curated_corpus()),
        &opts,
    );
    assert!(!json.executive_summary.is_override);
    assert!(json
        .executive_summary
        .narrative
        .starts_with("This is a draft export"));
}

#[test]
fn the_pdf_creation_time_is_read_back_from_the_cover() {
    // `compile_pdf` stamps the PDF with this epoch; its own test needs `typst` on PATH and
    // skips without it, so the round trip is pinned here without a compile.
    let json = build_report_json(
        &partition_report().seal(),
        &HashMap::new(),
        Some(&curated_corpus()),
        &ReportOptions::default(),
    );
    let data = render_data_json(&json).expect("serializes");
    // The fixture's scan finished at 2026-07-23T00:05:00Z.
    assert_eq!(creation_epoch(&data), Some(1_784_765_100));
    assert_eq!(creation_epoch("{}"), None);
}

// ---- the typst invocation (no real typst needed) -------------------------------------------

/// Write an executable `/bin/sh` script standing in for `typst` and return its path.
fn fake_typst(dir: &std::path::Path, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("typst");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write fake typst");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

async fn compile_with(program: std::path::PathBuf, data: &str) -> anyhow::Result<Vec<u8>> {
    TYPST_PROGRAM
        .scope(program, compile_pdf_from_data(data))
        .await
}

#[tokio::test]
async fn compile_pdf_returns_the_bytes_typst_wrote_stamped_with_the_scan_time() {
    let dir = tempfile::tempdir().unwrap();
    // `typst compile report.typ report.pdf --root <dir>`: $3 is the output file.
    let typst = fake_typst(
        dir.path(),
        r#"printf '%%PDF-fake %s' "$SOURCE_DATE_EPOCH" > "$3""#,
    );
    let json = build_report_json(
        &partition_report().seal(),
        &HashMap::new(),
        Some(&curated_corpus()),
        &ReportOptions::default(),
    );
    let bytes = TYPST_PROGRAM
        .scope(typst, compile_pdf(&json))
        .await
        .unwrap();
    assert_eq!(bytes, b"%PDF-fake 1784765100");
}

#[tokio::test]
async fn a_failing_compile_reports_typsts_own_error() {
    let dir = tempfile::tempdir().unwrap();
    let typst = fake_typst(dir.path(), "echo 'bad template' >&2; exit 3");
    let err = compile_with(typst, "{}").await.unwrap_err().to_string();
    assert!(
        err.starts_with("typst compile failed: bad template"),
        "{err}"
    );
}

#[tokio::test]
async fn a_missing_typst_says_how_to_install_it_and_another_spawn_error_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let missing = compile_with(dir.path().join("no-such-typst"), "{}")
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(missing, "Install Typst: brew install typst");
    // A directory cannot be executed: a spawn error that is not "not found".
    let other = compile_with(dir.path().to_path_buf(), "{}")
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(other, "could not run typst");
}
