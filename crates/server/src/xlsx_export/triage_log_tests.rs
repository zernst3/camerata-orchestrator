//! H4 item 3: the optional triage log, per rule, of what the AI pass proposed, what review kept
//! and what was thrown out, and why. Off by default.

use std::collections::HashMap;

use super::triage_log::{build_triage_log, TriageLogRow, TRIAGE_LOG_CSV, TRIAGE_LOG_SHEET};
use super::*;
use crate::onboard::Finding;
use crate::report_export::{finding_key, DispositionWire};

async fn corpus() -> camerata_rules::RuleSet {
    let (corpus, errors) =
        camerata_rules::load_corpus_lenient(&camerata_rules::corpus_path()).await;
    assert!(errors.is_empty(), "{errors:?}");
    corpus
}

fn ai_finding(rule_id: &str, line: usize, confidence: &str) -> Finding {
    Finding {
        repo: "owner/repo".to_string(),
        path: "src/a.rs".to_string(),
        line,
        rule_id: rule_id.to_string(),
        severity: "medium".to_string(),
        snippet: format!("snippet-{line}"),
        detail: format!("detail for {rule_id}"),
        confidence: Some(confidence.to_string()),
        needs_review: confidence == "needs-review",
        ..Finding::default()
    }
}

fn deterministic_finding(rule_id: &str, line: usize) -> Finding {
    Finding {
        repo: "owner/repo".to_string(),
        path: "src/b.rs".to_string(),
        line,
        rule_id: rule_id.to_string(),
        severity: "high".to_string(),
        snippet: format!("snippet-{line}"),
        detail: "floor".to_string(),
        ..Finding::default()
    }
}

fn false_positive(reason: &str) -> DispositionWire {
    DispositionWire {
        state: "FalsePositive".to_string(),
        reason: reason.to_string(),
        bucket: String::new(),
        confirmed_by_client: false,
    }
}

fn row<'a>(log: &'a [TriageLogRow], rule_id: &str) -> &'a TriageLogRow {
    log.iter()
        .find(|r| r.rule_id == rule_id)
        .unwrap_or_else(|| panic!("no triage row for {rule_id}: {log:#?}"))
}

/// Three AI proposals under one rule: one kept, one held by calibration, one thrown out by the
/// reviewer. A deterministic finding is not the AI pass's and never appears.
fn scenario() -> (
    crate::onboard::SealedReport,
    HashMap<String, DispositionWire>,
) {
    let (report, dispositions) = scenario_report();
    (report.seal(), dispositions)
}

fn scenario_report() -> (crate::onboard::ScanReport, HashMap<String, DispositionWire>) {
    let kept = ai_finding("SEC-NO-HARDCODED-SECRETS-1", 1, "high");
    let held = ai_finding("SEC-NO-HARDCODED-SECRETS-1", 2, "needs-review");
    let thrown = ai_finding("SEC-NO-HARDCODED-SECRETS-1", 3, "high");
    let floor = deterministic_finding("SEC-NO-RAW-SQL-CONCAT-1", 4);
    let mut dispositions = HashMap::new();
    dispositions.insert(
        finding_key(&thrown),
        false_positive("test fixture, not shipped"),
    );
    let report = tests::report_with(
        vec![kept, held, thrown, floor],
        vec!["SEC-NO-HARDCODED-SECRETS-1", "SEC-NO-RAW-SQL-CONCAT-1"],
    );
    (report, dispositions)
}

#[tokio::test]
async fn the_log_counts_what_the_ai_pass_proposed_kept_and_threw_out_per_rule_and_says_why() {
    let corpus = corpus().await;
    let (report, dispositions) = scenario();
    let log = build_triage_log(&report, &dispositions, Some(&corpus), &HashMap::new());
    assert_eq!(log.len(), 1, "only the AI pass's rule: {log:#?}");
    let r = row(&log, "SEC-NO-HARDCODED-SECRETS-1");
    assert_eq!(r.proposed, 3);
    assert_eq!(r.kept, 1);
    assert_eq!(r.held_for_review, 1);
    assert_eq!(r.information_only, 0);
    assert_eq!(r.thrown_out, 1);
    assert_eq!(
        r.proposed,
        r.kept + r.held_for_review + r.information_only + r.thrown_out,
        "every proposal is accounted for"
    );
    assert!(
        r.why.contains(
            "1 thrown out: marked a false positive by the reviewer (test fixture, not shipped)"
        ),
        "{}",
        r.why
    );
    assert!(
        r.why
            .contains("1 held for review: calibration flagged it as needs review"),
        "{}",
        r.why
    );
    assert!(!r.rule.is_empty());
}

#[tokio::test]
async fn the_triage_log_is_off_by_default() {
    let corpus = corpus().await;
    let (report, dispositions) = scenario();
    let options = ReportOptions::default();
    assert!(!options.triage_log);
    let xlsx = build_workbook(&report, &dispositions, Some(&corpus), &options).unwrap();
    let strings = crate::release_gate::xlsx_strings(&xlsx).unwrap().join("\n");
    assert!(
        !strings.contains(TRIAGE_LOG_SHEET),
        "no triage sheet by default"
    );
    let csv = build_csv_exports(&report, &dispositions, Some(&corpus), &options).unwrap();
    assert!(csv.iter().all(|(path, _)| path != TRIAGE_LOG_CSV));
}

#[tokio::test]
async fn when_asked_for_the_triage_log_rides_as_a_workbook_sheet_and_a_csv() {
    let corpus = corpus().await;
    let (report, dispositions) = scenario();
    let options = ReportOptions {
        triage_log: true,
        ..ReportOptions::default()
    };
    let xlsx = build_workbook(&report, &dispositions, Some(&corpus), &options).unwrap();
    let grid = crate::release_gate::xlsx_sheet_grid(&xlsx, TRIAGE_LOG_SHEET)
        .expect("the workbook has a Triage Log sheet");
    let flat: Vec<String> = grid.iter().flatten().cloned().collect();
    assert!(
        flat.iter().any(|c| c == "SEC-NO-HARDCODED-SECRETS-1"),
        "{flat:?}"
    );
    assert!(
        flat.iter().any(|c| c.contains("test fixture, not shipped")),
        "{flat:?}"
    );

    let csv = build_csv_exports(&report, &dispositions, Some(&corpus), &options).unwrap();
    let (_, bytes) = csv
        .iter()
        .find(|(path, _)| path == TRIAGE_LOG_CSV)
        .expect("the CSV is exported");
    let text = String::from_utf8(bytes.clone()).unwrap();
    let mut lines = text.lines();
    assert_eq!(
        lines.next(),
        Some("Rule ID,Rule,Proposed by the AI pass,Kept,Held for review,Information only,Thrown out,Why")
    );
    let data: Vec<&str> = lines.collect();
    assert_eq!(data.len(), 1, "{text}");
    assert!(data[0].starts_with("SEC-NO-HARDCODED-SECRETS-1,"), "{text}");
}

#[test]
fn an_empty_triage_log_still_has_its_header() {
    let report = tests::report_with(Vec::new(), Vec::new()).seal();
    let options = ReportOptions {
        triage_log: true,
        ..ReportOptions::default()
    };
    let csv = build_csv_exports(&report, &HashMap::new(), None, &options).unwrap();
    let (_, bytes) = csv
        .iter()
        .find(|(path, _)| path == TRIAGE_LOG_CSV)
        .expect("the CSV is exported even when empty");
    assert!(String::from_utf8_lossy(bytes).starts_with("Rule ID,"));
}

/// The key the export dialog sends (`triage_log`) is the one `ReportOptions` reads.
#[test]
fn the_export_dialogs_options_key_turns_the_triage_log_on() {
    let on: ReportOptions = serde_json::from_str(r#"{"triage_log": true}"#).unwrap();
    assert!(on.triage_log);
    let off: ReportOptions = serde_json::from_str(r#"{"triage_log": false}"#).unwrap();
    assert!(!off.triage_log);
    let absent: ReportOptions = serde_json::from_str("{}").unwrap();
    assert!(!absent.triage_log);
}

#[test]
fn the_env_switch_turns_the_triage_log_on_only_for_one_or_true() {
    use crate::report_export::triage_log_env_on;
    assert!(triage_log_env_on(Some("1")));
    assert!(triage_log_env_on(Some("true")));
    assert!(triage_log_env_on(Some(" TRUE ")));
    assert!(!triage_log_env_on(None));
    assert!(!triage_log_env_on(Some("")));
    assert!(!triage_log_env_on(Some("0")));
    assert!(!triage_log_env_on(Some("yes")));
}

// ── The caption: the log counts rows that reached review, and says how many did not ──────────

fn sealed_with_repeats_dropped(deduped: usize) -> crate::onboard::SealedReport {
    let (mut report, _) = scenario_report();
    report.ledger.record_stage(
        "ai-location-merge",
        10,
        10 - deduped,
        crate::scan_ledger::StageAccounting {
            deduped,
            ..Default::default()
        },
    );
    report.seal()
}

#[test]
fn the_caption_states_that_the_counts_are_rows_that_reached_review() {
    assert_eq!(
        triage_log::caption(7),
        "Rows that reached review; 7 rows merged into another at the same location are not listed"
    );
    assert_eq!(
        triage_log::caption(1),
        "Rows that reached review; 1 row merged into another at the same location is not listed"
    );
    assert_eq!(
        triage_log::caption(0),
        "Rows that reached review; 0 rows merged into another at the same location are not listed"
    );
}

#[tokio::test]
async fn the_sheet_carries_the_caption_with_the_ledgers_deduped_count() {
    let corpus = corpus().await;
    let (_, dispositions) = scenario();
    let report = sealed_with_repeats_dropped(4);
    assert_eq!(report.ledger.deduped_total(), 4);
    let options = ReportOptions {
        triage_log: true,
        ..ReportOptions::default()
    };
    let xlsx = build_workbook(&report, &dispositions, Some(&corpus), &options).unwrap();
    let grid = crate::release_gate::xlsx_sheet_grid(&xlsx, TRIAGE_LOG_SHEET).expect("the sheet");
    let flat: Vec<String> = grid.iter().flatten().cloned().collect();
    assert!(
        flat.iter().any(|c| c
            == "Rows that reached review; 4 rows merged into another at the same location are not listed"),
        "{flat:?}"
    );
}

/// Every sheet, the Triage Log included, carries the confidentiality footer: the sheet is added
/// before the footer loop runs.
#[tokio::test]
async fn every_sheet_including_the_triage_log_carries_the_footer() {
    let corpus = corpus().await;
    let (report, dispositions) = scenario();
    let options = ReportOptions {
        triage_log: true,
        client_name: "Example Client Ltd".to_string(),
        ..ReportOptions::default()
    };
    let xlsx = build_workbook(&report, &dispositions, Some(&corpus), &options).unwrap();
    let sheets = crate::release_gate::xlsx_sheet_names(&xlsx).unwrap();
    assert!(
        sheets.iter().any(|s| s == TRIAGE_LOG_SHEET),
        "anti-vacuity: the Triage Log sheet is present: {sheets:?}"
    );
    let footers = crate::release_gate::xlsx_header_footers(&xlsx).unwrap();
    for sheet in &sheets {
        let footer = footers
            .iter()
            .find(|f| f.part == "footer" && &f.sheet == sheet)
            .unwrap_or_else(|| panic!("sheet {sheet:?} has no footer"));
        assert!(
            footer.left.contains("Example Client Ltd"),
            "sheet {sheet:?}: {:?}",
            footer.left
        );
    }
}
