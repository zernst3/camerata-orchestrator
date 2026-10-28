//! Tests written against the mutants `cargo mutants -p camerata-server --file
//! crates/server/src/xlsx_export.rs` left alive on 2026-10-28 (docs/MUTATION_TESTING.md). The
//! workbook, its CSVs and `findings.json` are client deliverables; each test pins a value a
//! client reads that no existing test held exactly.

use super::*;
use crate::onboard::{AuditedRef, Finding, ScanProvenance};

/// A minimal report fixture (this module's own copy, per the crate's convention that test
/// modules never import a sibling module's private helpers).
fn report_of(findings: Vec<Finding>) -> ScanReport {
    ScanReport {
        repos: vec!["owner/repo".to_string()],
        stacks: Vec::new(),
        files_scanned: 10,
        test_file_count: 0,
        files_excluded: 0,
        code_chars: 5000,
        code_lines: 400,
        code_lines_by_language: Vec::new(),
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
            audited_refs: vec![AuditedRef {
                repo: "owner/repo".to_string(),
                sha: Some("abcdef1234567890".to_string()),
                branch: Some("main".to_string()),
                dirty: false,
                ..AuditedRef::default()
            }],
            audit_model: Some("test-model".to_string()),
            calibration_model: None,
            clone_judge_model: None,
            mode: "parallel".to_string(),
            thorough: false,
            deep: false,
            rules_fingerprint: "fp".to_string(),
            audited_rule_ids: Vec::new(),
            camerata_version: "0.0.0-test".to_string(),
            osv_scanner_version: None,
            started_at: "2026-07-23T00:00:00Z".to_string(),
            finished_at: "2026-07-23T00:05:00Z".to_string(),
            model_sessions: Vec::new(),
        },
        recommendations: std::collections::HashMap::new(),
        failed_passes: Vec::new(),
        ledger: crate::scan_ledger::ScanLedger::new(),
    }
}

fn option(id: &str, label: &str) -> camerata_rules::RuleOption {
    camerata_rules::RuleOption {
        id: id.to_string(),
        label: label.to_string(),
        directive: "d".to_string(),
        why: "w".to_string(),
        remediation: None,
        finding_headline: None,
        finding_detail: None,
        escalation: None,
        effort: None,
        violates: false,
    }
}

fn recommendation(
    rule_id: &str,
    option_id: &str,
    applicable: bool,
) -> crate::ai_audit::RuleRecommendation {
    serde_json::from_value(serde_json::json!({
        "rule_id": rule_id,
        "recommended_option_id": option_id,
        "recommendation_reasoning": "",
        "evidence": "src/a.rs:3",
        "applicable": applicable,
    }))
    .expect("a minimal recommendation deserializes")
}

// ---- the Coverage sheet's "Rules applied" appendix -------------------------------------------

#[test]
fn rules_applied_names_the_chosen_option_by_label_and_marks_not_applicable_rules() {
    let mut rule = camerata_rules::bare_rule("PICK-1", "test-fixture");
    rule.options = vec![option("opt-a", "Option A"), option("opt-b", "Option B")];
    let mut corpus = camerata_rules::RuleSet::default();
    corpus.push(rule);
    let mut report = report_of(Vec::new());
    for rec in [
        recommendation("PICK-1", "opt-b", true),
        recommendation("UNKNOWN-2", "opt-x", true),
        recommendation("ABSENT-0", "opt-a", false),
    ] {
        report.recommendations.insert(rec.rule_id.clone(), rec);
    }
    let rows = build_rules_applied_rows(&report, Some(&corpus));
    let summary: Vec<(&str, &str, bool)> = rows
        .iter()
        .map(|r| (r.rule_id.as_str(), r.chosen.as_str(), r.applicable))
        .collect();
    assert_eq!(summary[1], ("PICK-1", "Option B", true));
    assert_eq!(
        summary[2],
        ("UNKNOWN-2", "opt-x", true),
        "no corpus rule: the bare id"
    );
    assert_eq!(summary[0].0, "ABSENT-0");
    assert!(
        summary[0].1.starts_with("Not applicable"),
        "{}",
        summary[0].1
    );
    assert!(!summary[0].2);
    assert!(rows.iter().all(|r| r.evidence == "src/a.rs:3"));
}

// ---- small workbook helpers -------------------------------------------------------------------

#[test]
fn a_workbook_snippet_is_capped_only_past_its_limit() {
    assert_eq!(cap_snippet_for_workbook("short"), "short");
    let exact = "x".repeat(WORKBOOK_SNIPPET_MAX_CHARS);
    assert_eq!(cap_snippet_for_workbook(&exact), exact);
    let over = "x".repeat(WORKBOOK_SNIPPET_MAX_CHARS + 1);
    assert_eq!(
        cap_snippet_for_workbook(&over),
        format!("{exact}\n... (truncated)")
    );
}

#[test]
fn still_open_matches_the_pdfs_definition_variant_for_variant() {
    for (d, open) in [
        (Disposition::Unresolved, true),
        (Disposition::Confirmed, true),
        (Disposition::TechDebtNow, true),
        (Disposition::TechDebtLater, true),
        (Disposition::Ignored, false),
        (Disposition::BaselineAccepted, false),
        (Disposition::WaivedInline, false),
    ] {
        assert_eq!(is_still_open(d), open, "{d:?}");
    }
}

#[test]
fn severity_labels_are_title_cased_and_the_worst_wins() {
    assert_eq!(title_case("high"), "High");
    assert_eq!(title_case(""), "");
    let finding = |line: usize, severity: &str| Finding {
        repo: "owner/repo".to_string(),
        path: "src/a.rs".to_string(),
        line,
        rule_id: format!("SEV-{line}"),
        severity: severity.to_string(),
        snippet: format!("snippet-{line}"),
        detail: format!("Detail {line}."),
        ..Finding::default()
    };
    let label_for = |severities: &[&str]| {
        let report = report_of(
            severities
                .iter()
                .enumerate()
                .map(|(i, s)| finding(i + 1, s))
                .collect(),
        );
        let (rows, _) = partition_rows(&report, &HashMap::new(), None, &HashMap::new());
        let refs: Vec<&FindingRow> = rows.iter().collect();
        worst_severity_label(&refs)
    };
    assert_eq!(label_for(&[]), "Clean");
    assert_eq!(label_for(&["low", "medium"]), "Medium");
    assert_eq!(label_for(&["medium", "high", "low"]), "High");
    assert_eq!(label_for(&["low", "critical"]), "Critical");
    assert_eq!(label_for(&["low"]), "Low");
}

#[test]
fn the_dependency_sheet_label_is_its_worst_advisory() {
    let advisory = |line: usize, severity: &str| Finding {
        repo: "owner/repo".to_string(),
        path: "package-lock.json".to_string(),
        line,
        rule_id: DEP_AUDIT_RULE_ID.to_string(),
        severity: severity.to_string(),
        snippet: format!("pkg-{line}"),
        detail: format!("Advisory {line}."),
        ..Finding::default()
    };
    let label_for = |severities: &[&str]| {
        let report = report_of(
            severities
                .iter()
                .enumerate()
                .map(|(i, s)| advisory(i + 1, s))
                .collect(),
        );
        let (_, deps) = partition_rows(&report, &HashMap::new(), None, &HashMap::new());
        worst_dep_severity_label(&deps)
    };
    assert_eq!(label_for(&[]), "Clean");
    assert_eq!(label_for(&["low", "high"]), "High");
    assert_eq!(label_for(&["critical"]), "Critical");
    assert_eq!(label_for(&["medium", "low"]), "Medium");
    assert_eq!(label_for(&["low"]), "Low");
}

// ---- category tab status and sheet names ----------------------------------------------------

#[test]
fn a_category_tab_takes_the_colour_of_its_worst_still_open_row() {
    let finding = |line: usize, severity: &str| Finding {
        repo: "owner/repo".to_string(),
        path: "src/a.rs".to_string(),
        line,
        rule_id: format!("CAT-{line}"),
        severity: severity.to_string(),
        snippet: format!("snippet-{line}"),
        detail: format!("Detail {line}."),
        ..Finding::default()
    };
    let status_for = |severities: &[&str], dispositions: &HashMap<String, DispositionWire>| {
        let report = report_of(
            severities
                .iter()
                .enumerate()
                .map(|(i, s)| finding(i + 1, s))
                .collect(),
        );
        let (rows, _) = partition_rows(&report, dispositions, None, &HashMap::new());
        let refs: Vec<&FindingRow> = rows.iter().collect();
        category_status(&refs)
    };
    let open = HashMap::new();
    assert_eq!(status_for(&["low", "critical"], &open), (0, "#C0392B"));
    assert_eq!(status_for(&["low", "high"], &open), (1, "#EA580C"));
    assert_eq!(status_for(&["low", "medium"], &open), (2, "#E0B400"));
    assert_eq!(status_for(&["low"], &open), (3, "#27AE60"));
    // An accepted-risk critical is not still open.
    let report = report_of(vec![finding(1, "critical")]);
    let key = crate::report_export::finding_key(&report.findings[0]);
    let mut ignored = HashMap::new();
    ignored.insert(
        key,
        DispositionWire {
            state: "Ignored".to_string(),
            reason: "accepted".to_string(),
            bucket: String::new(),
            confirmed_by_client: true,
        },
    );
    assert_eq!(status_for(&["critical"], &ignored), (3, "#27AE60"));
}

#[test]
fn sheet_names_are_cleaned_truncated_and_deduplicated() {
    let mut used = HashSet::new();
    assert_eq!(
        sanitize_sheet_name("Auth: [Login]/Tokens?", &mut used),
        "Auth LoginTokens"
    );
    assert_eq!(sanitize_sheet_name("[]", &mut used), "Sheet");
    let long = "A very long category name that overflows";
    let first = sanitize_sheet_name(long, &mut used);
    assert_eq!(first, "A very long category name that ");
    assert_eq!(first.chars().count(), 31);
    assert_eq!(
        sanitize_sheet_name(long, &mut used),
        "A very long category name t (2)"
    );
    assert_eq!(
        sanitize_sheet_name(long, &mut used),
        "A very long category name t (3)"
    );
    assert_eq!(sanitize_sheet_name("Sheet", &mut used), "Sheet (2)");
}

#[test]
fn a_rule_title_comes_from_the_corpus_or_falls_back_to_the_id() {
    let mut rule = camerata_rules::bare_rule("TITLED-1", "test-fixture");
    rule.title = "Validate input at the boundary".to_string();
    let mut corpus = camerata_rules::RuleSet::default();
    corpus.push(rule);
    assert_eq!(
        rule_title("TITLED-1", Some(&corpus)),
        "Validate input at the boundary"
    );
    assert_eq!(rule_title("AI-UNTITLED-1", Some(&corpus)), "AI-UNTITLED-1");
    assert_eq!(rule_title("TITLED-1", None), "TITLED-1");
}

#[test]
fn how_checked_speaks_the_index_sheets_vocabulary() {
    assert_eq!(how_checked_label("grounded"), "Deterministic");
    assert_eq!(how_checked_label("preview"), "Preview");
    assert_eq!(how_checked_label("advisory"), "AI-advisory");
}

#[test]
fn each_severity_cell_has_its_own_colour() {
    let formats = Formats::new();
    let cells: Vec<Format> = ["critical", "high", "medium", "low"]
        .iter()
        .map(|s| formats.severity_cell(s))
        .collect();
    for (i, a) in cells.iter().enumerate() {
        for b in &cells[i + 1..] {
            assert_ne!(a, b, "two severities share a cell colour");
        }
    }
    assert_eq!(formats.severity_cell("info"), formats.severity_cell("low"));
}

// ---- the Dependencies sheet -------------------------------------------------------------------

fn dep(package: &str, severity: &str, fixed: Option<&str>, action: &str) -> DepRow {
    DepRow {
        package: package.to_string(),
        advisory: format!("Advisory for {package}"),
        severity: severity.to_string(),
        repo: "owner/repo".to_string(),
        fixed_version: fixed.map(str::to_string),
        ownership: "client_owned".to_string(),
        action: action.to_string(),
    }
}

#[test]
fn the_dependencies_sheet_lists_worst_first_then_its_coverage_notes_one_per_row() {
    let mut wb = Workbook::new();
    let deps = vec![
        dep("left-pad", "low", Some("1.2.0"), ""),
        dep("zlib", "high", None, "Monitor upstream."),
        dep("minimist", "medium", Some("1.2.6"), ""),
    ];
    let notes = vec![
        "osv-scanner timed out on repo b".to_string(),
        "no lockfile in repo c".to_string(),
    ];
    write_dependencies_sheet(&mut wb, "Dependencies", &deps, &notes, &Formats::new()).unwrap();
    let bytes = wb.save_to_buffer().unwrap();
    let grid = crate::release_gate::xlsx_sheet_grid(&bytes, "Dependencies").unwrap();
    let col = |r: usize, c: usize| grid[r].get(c).map(String::as_str).unwrap_or("");
    assert_eq!(
        (col(1, 0), col(1, 2), col(1, 4), col(1, 5)),
        ("zlib", "High", "(no patch yet)", "Monitor upstream.")
    );
    assert_eq!(col(2, 0), "minimist");
    assert_eq!(
        (col(3, 0), col(3, 2), col(3, 4), col(3, 5)),
        ("left-pad", "Low", "1.2.0", "Upgrade to >= 1.2.0.")
    );
    // Three rows, so "two rows after the last" (5) differs from "twice the last" (6).
    assert_eq!(col(4, 0), "", "one blank row after the table");
    assert_eq!(col(5, 0), "Dependency-audit coverage notes");
    assert_eq!(col(6, 0), "osv-scanner timed out on repo b");
    assert_eq!(col(7, 0), "no lockfile in repo c");
}

// ---- the Coverage sheet layout ----------------------------------------------------------------

fn coverage_row(rule_id: &str, findings_count: usize) -> CoverageRow {
    CoverageRow {
        rule_id: rule_id.to_string(),
        title: format!("Title of {rule_id}"),
        category: "Security".to_string(),
        citation_kind: "grounded".to_string(),
        citation_label: "CWE-89".to_string(),
        findings_count,
    }
}

#[test]
fn the_coverage_sheet_places_every_section_after_the_last_without_overlap() {
    let mut wb = Workbook::new();
    let coverage = vec![
        coverage_row("CLEAN-1", 0),
        coverage_row("TWO-1", 2),
        coverage_row("ONE-1", 1),
    ];
    let applied = vec![RulesAppliedRow {
        rule_id: "PICK-1".to_string(),
        chosen: "Option B".to_string(),
        evidence: "src/a.rs:3".to_string(),
        applicable: true,
    }];
    let notes = vec![
        "semgrep timed out".to_string(),
        "eslint not installed".to_string(),
    ];
    let not_run = vec![("SKIP-1".to_string(), "no wired detector".to_string())];
    let not_assessed = vec![NotAssessedRow {
        rule_title: "Title of NA-1".to_string(),
        rule_id: "NA-1".to_string(),
        why: "the file was too large".to_string(),
        headline: "Not assessed".to_string(),
        detail: "Skipped.".to_string(),
    }];
    write_coverage_sheet(
        &mut wb,
        "Coverage",
        &coverage,
        &["MECH-1".to_string()],
        &applied,
        &notes,
        &not_run,
        &not_assessed,
        &Formats::new(),
    )
    .unwrap();
    let bytes = wb.save_to_buffer().unwrap();
    let grid = crate::release_gate::xlsx_sheet_grid(&bytes, "Coverage").unwrap();
    let col = |r: usize, c: usize| {
        grid.get(r)
            .and_then(|row| row.get(c))
            .map(String::as_str)
            .unwrap_or("")
    };
    assert_eq!(col(3, 0), "Area");
    assert!(col(4, 2).starts_with("Clean"), "{}", col(4, 2));
    assert_eq!(col(5, 2), "2 sites found");
    assert_eq!(col(6, 2), "1 site found");
    assert_eq!(col(6, 3), "Deterministic");
    assert_eq!(col(6, 5), "ONE-1");
    let first_column: Vec<&str> = (7..=25).map(|r| col(r, 0)).collect();
    assert_eq!(
        first_column,
        vec![
            "",
            "Excluded from this code-only audit (mechanical, CI-enforced instead)",
            "MECH-1",
            "",
            "Rules applied (alternative selection)",
            "Rule ID",
            "PICK-1",
            "",
            "Tool coverage notes",
            "semgrep timed out",
            "eslint not installed",
            "",
            "Checks we could not complete this scan",
            "What was checked",
            "Title of NA-1",
            "",
            "Rules not run this scan",
            "Rule ID",
            "SKIP-1",
        ]
    );
    assert_eq!((col(13, 1), col(13, 3)), ("Option B", "Yes"));
    assert_eq!(col(25, 1), "no wired detector");
}

// ---- the Index sheet ----------------------------------------------------------------------------

/// A report touching every Index section: four severities across two categories, every kind
/// (and an unkinded row), every disposition bucket, an accepted-risk row, a false positive, a
/// dependency advisory and a clean audited rule.
fn index_fixture() -> (
    crate::onboard::SealedReport,
    HashMap<String, DispositionWire>,
    camerata_rules::RuleSet,
    ReportOptions,
) {
    use camerata_api_types::audit::{DeviationKind, FindingKind};
    let finding = |rule_id: &str, line: usize, severity: &str, kind: Option<FindingKind>| Finding {
        repo: "owner/repo".to_string(),
        path: "src/a.rs".to_string(),
        line,
        rule_id: rule_id.to_string(),
        severity: severity.to_string(),
        snippet: format!("snippet-{line}"),
        detail: format!("Detail {line}."),
        kind,
        ..Finding::default()
    };
    let deviation = FindingKind::Deviation(DeviationKind {
        deviating_sites: 1,
        assessed_sites: Some(4),
        convention_option_id: None,
        convention_label: "the house convention".to_string(),
    });
    let mut held = finding("IDX-HELD-1", 8, "medium", Some(FindingKind::Defect));
    held.needs_review = true;
    let mut report = report_of(vec![
        finding("IDX-CRIT-1", 1, "critical", Some(FindingKind::Defect)),
        finding("IDX-HIGH-1", 2, "high", Some(FindingKind::Defect)),
        finding("IDX-MED-1", 3, "medium", None),
        finding("IDX-LOW-1", 4, "low", Some(deviation)),
        finding("IDX-ACCEPTED-1", 5, "medium", Some(FindingKind::Defect)),
        finding("IDX-FP-1", 6, "high", Some(FindingKind::Defect)),
        finding(DEP_AUDIT_RULE_ID, 7, "high", None),
        held,
        finding("IDX-INFO-1", 9, "info", Some(FindingKind::Defect)),
    ]);
    report.provenance.audited_rule_ids = vec!["IDX-CLEAN-1".to_string()];
    let wire = |state: &str| DispositionWire {
        state: state.to_string(),
        reason: "reviewed".to_string(),
        bucket: String::new(),
        confirmed_by_client: true,
    };
    let mut dispositions = HashMap::new();
    dispositions.insert(
        crate::report_export::finding_key(&report.findings[4]),
        wire("Ignored"),
    );
    dispositions.insert(
        crate::report_export::finding_key(&report.findings[5]),
        wire("FalsePositive"),
    );
    // Two rules in a "security" domain (one fixable finding rule, one clean audited rule) so
    // the category table has two rows; every other rule is outside the corpus ("Other").
    let mut crit = camerata_rules::bare_rule("IDX-CRIT-1", "security");
    crit.default_option = Some("default".to_string());
    crit.options = vec![option("default", "Default")];
    crit.options[0].remediation = Some("Fix it.".to_string());
    crit.options[0].effort = Some("low".to_string());
    let mut corpus = camerata_rules::RuleSet::default();
    corpus.push(crit);
    corpus.push(camerata_rules::bare_rule("IDX-CLEAN-1", "security"));
    let opts = ReportOptions {
        project_title: "Acme storefront".to_string(),
        client_name: "Acme".to_string(),
        prepared_by: "An Auditor".to_string(),
        ..ReportOptions::default()
    };
    (report.seal(), dispositions, corpus, opts)
}

fn index_rows() -> Vec<String> {
    let (report, dispositions, corpus, opts) = index_fixture();
    let bytes = build_workbook(&report, &dispositions, Some(&corpus), &opts).unwrap();
    crate::release_gate::xlsx_sheet_grid(&bytes, "Index")
        .unwrap()
        .iter()
        .map(|row| {
            let cells: Vec<&str> = row.iter().map(String::as_str).collect();
            cells.join(" | ").trim_end_matches([' ', '|']).to_string()
        })
        .collect()
}

/// A characterization of the whole Index sheet, row by row: every count, label and row offset
/// a client reads on the workbook's first sheet. Update it deliberately when the Index changes.
#[test]
fn the_index_sheet_reads_exactly_this() {
    assert_eq!(
        index_rows(),
        vec![
            "Camerata Codebase Inspection — Product Export Index",
            "",
            "Acme storefront",
            "Prepared for Acme",
            "Prepared by An Auditor",
            "",
            "Provenance",
            "Repos inspected | owner/repo",
            "owner/repo | abcdef1 on main",
            "Inspection model | test-model",
            "Calibration model | n/a",
            "Camerata version | 0.0.0-test",
            "Generated | 2026-07-23 00:05 UTC",
            "Files scanned | 10",
            "Files excluded as noise | 0",
            "Code volume (characters) | 5000",
            "",
            "Findings by category",
            "Category | Critical | High | Medium | Low | Total",
            "Other | 0 | 1 | 3 | 2 | 6",
            "Security | 1 | 0 | 0 | 0 | 1",
            "Total | 1 | 1 | 3 | 2 | 7",
            "",
            "Statement types",
            "Defects | 5 | All Findings",
            "Deviations from your own conventions | 1 | Deviations (one row per convention), All Findings (one row per site)",
            "Conventions followed consistently | 0 | Conventions",
            "No convention adopted | 0 | Conventions",
            "Not assessed this scan | 0 | Coverage",
            "Unclassified | 1 | All Findings",
            "",
            "Disposition summary: 4 open, 1 do now, 1 do next, 2 planned, 1 accepted, 2 held for review, 1 excluded as false positives, 1 dependency advisory.",
            "",
            "Legend",
            "Severity",
            "Critical",
            "High",
            "Medium",
            "Low",
            "",
            "Disposition vocabulary",
            "Open (recommended: ...) — no triage decision yet this session",
            "Confirmed by the reviewer (recommended: ...) — the auditor read this finding and judged it real; still open work, the recommendation is unchanged",
            "Accepted risk / Needs client confirmation — an Ignored disposition (see confirmed_by_client)",
            "Tech debt, resolve now / planned (resolve later) — a TechDebt disposition",
            "Pre-existing accepted debt (baseline suppression) — accepted in a PRIOR run",
            "False positive — auditor-dispositioned, excluded from every working sheet (see the False Positives sheet)",
            "",
            "Provenance vocabulary",
            "Deterministic — corpus-grounded, cites an authoritative external source",
            "Preview: {tool} — scan-time deterministic tool pass, not yet wired into the gate",
            "AI-advisory — model-inferred, no corpus citation",
            "",
            "Confidence",
            "high — a clear, concrete violation",
            "needs-review — the calibration pass flagged it as debatable/theoretical/under-evidenced",
            "",
            "",
            "Contents",
            "Sheet | Findings | Worst severity",
            "All Findings | 7 | Critical",
            "Deviations | 1 | Low",
            "Conventions | 0",
            "Security | 1 | Critical",
            "Other | 6 | High",
            "Dependencies | 1 | High",
            "False Positives | 1 | High",
            "Coverage | 1",
        ]
    );
}

// ---- the json/ datasets partition rows by kind ----------------------------------------------

#[test]
fn each_json_dataset_takes_only_its_own_kind() {
    use camerata_api_types::audit::{
        ConventionObservedKind, DeviationKind, FindingKind, NotAssessedKind, NotAssessedReason,
    };
    let finding = |rule_id: &str, line: usize, kind: FindingKind| Finding {
        repo: "owner/repo".to_string(),
        path: "src/a.rs".to_string(),
        line,
        rule_id: rule_id.to_string(),
        severity: "low".to_string(),
        snippet: format!("snippet-{line}"),
        detail: format!("Detail {line}."),
        kind: Some(kind),
        ..Finding::default()
    };
    let report = report_of(vec![
        finding("KIND-DEFECT-1", 1, FindingKind::Defect),
        finding(
            "KIND-DEV-1",
            2,
            FindingKind::Deviation(DeviationKind {
                deviating_sites: 1,
                assessed_sites: Some(3),
                convention_option_id: None,
                convention_label: "the house convention".to_string(),
            }),
        ),
        finding(
            "KIND-CONV-1",
            3,
            FindingKind::ConventionObserved(ConventionObservedKind {
                convention_option_id: "opt-a".to_string(),
                convention_label: "Option A".to_string(),
            }),
        ),
        finding(
            "KIND-NA-1",
            4,
            FindingKind::NotAssessed(NotAssessedKind {
                reason: NotAssessedReason::NoFilesMatched,
                reason_detail: "no matching files".to_string(),
            }),
        ),
    ]);
    let files = build_json_exports(
        &report.seal(),
        &HashMap::new(),
        None,
        &ReportOptions::default(),
    )
    .unwrap();
    let rule_ids = |name: &str| -> Vec<String> {
        let (_, bytes) = files.iter().find(|(n, _)| n == name).unwrap();
        let rows: Vec<serde_json::Value> = serde_json::from_slice(bytes).unwrap();
        rows.iter()
            .map(|r| r["rule_id"].as_str().unwrap_or_default().to_string())
            .collect()
    };
    assert_eq!(rule_ids("json/deviations.json"), vec!["KIND-DEV-1"]);
    assert_eq!(rule_ids("json/conventions.json"), vec!["KIND-CONV-1"]);
    assert_eq!(rule_ids("json/not-assessed.json"), vec!["KIND-NA-1"]);
    assert_eq!(rule_ids("json/all-findings.json").len(), 4);
}
