//! The triage log, per rule, of what the AI pass proposed, what review kept and what was thrown
//! out, and why. It is the operator's file: the client package never carries it.

use std::collections::HashMap;

use super::triage_log::{
    build_triage_log, operator_csv, TriageLogRow, INTERNAL_MARK, TRIAGE_LOG_CSV, TRIAGE_LOG_SHEET,
};
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
    let log = build_triage_log(
        &crate::report_export::ReviewedReport::unreviewed(&report, &dispositions),
        Some(&corpus),
        &HashMap::new(),
    );
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

/// Every option a client export can be given, the old `triage_log` key a stale client may still
/// send included.
fn every_option_on() -> ReportOptions {
    let mut options: ReportOptions = serde_json::from_str(
        r#"{"triage_log": true, "project_title": "Acme", "prepared_by": "A. Person",
            "executive_summary_override": "A summary.", "brand": "Acme Co"}"#,
    )
    .expect("a stale client's body still parses");
    options.client_name = "Example Client Ltd".to_string();
    options
}

#[tokio::test]
async fn the_client_workbook_never_has_a_triage_log_sheet() {
    let corpus = corpus().await;
    let (report, dispositions) = scenario();
    let xlsx = build_workbook(
        &crate::report_export::ReviewedReport::unreviewed(&report, &dispositions),
        Some(&corpus),
        &every_option_on(),
    )
    .unwrap();
    let sheets = crate::release_gate::xlsx_sheet_names(&xlsx).unwrap();
    assert!(
        !sheets.iter().any(|s| s == TRIAGE_LOG_SHEET),
        "no Triage Log sheet, whatever is set: {sheets:?}"
    );
    assert!(
        sheets.iter().any(|s| s == vocabulary::ALL_PROBLEMS),
        "anti-vacuity: the workbook has its real sheets: {sheets:?}"
    );
}

/// `CAMERATA_EXPORT_TRIAGE_LOG` no longer adds anything to the client package: the command line
/// writes the operator's file beside the zip instead.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn the_environment_switch_adds_nothing_to_the_client_package() {
    let _env = camerata_test_env::EnvGuard::set(crate::report_export::EXPORT_TRIAGE_LOG_ENV, "1");
    let mut options = ReportOptions::default();
    options.apply_env_defaults();
    let corpus = corpus().await;
    let (report, dispositions) = scenario();
    let reviewed = crate::report_export::ReviewedReport::unreviewed(&report, &dispositions);
    let xlsx = build_workbook(&reviewed, Some(&corpus), &options).unwrap();
    let sheets = crate::release_gate::xlsx_sheet_names(&xlsx).unwrap();
    assert!(!sheets.iter().any(|s| s == TRIAGE_LOG_SHEET), "{sheets:?}");
    let csv = build_csv_exports(&reviewed, Some(&corpus), &options).unwrap();
    assert!(csv.iter().all(|(path, _)| path != TRIAGE_LOG_CSV));
}

#[tokio::test]
async fn the_client_csv_set_never_has_the_triage_log() {
    let corpus = corpus().await;
    let (report, dispositions) = scenario();
    let csv = build_csv_exports(
        &crate::report_export::ReviewedReport::unreviewed(&report, &dispositions),
        Some(&corpus),
        &every_option_on(),
    )
    .unwrap();
    assert!(
        csv.iter().all(|(path, _)| path != TRIAGE_LOG_CSV),
        "no csv/triage-log.csv, whatever is set"
    );
    assert!(
        csv.iter().any(|(path, _)| path == tickets::TICKETS_CSV),
        "anti-vacuity: the other CSV files are there"
    );
}

#[tokio::test]
async fn the_operator_file_is_marked_internal_then_has_the_header_and_the_rows() {
    let corpus = corpus().await;
    let (report, dispositions) = scenario();
    let bytes = operator_csv(
        &crate::report_export::ReviewedReport::unreviewed(&report, &dispositions),
        Some(&corpus),
        &HashMap::new(),
    )
    .unwrap();
    let text = String::from_utf8(bytes).unwrap();
    let mut lines = text.lines();
    let first = lines.next().unwrap();
    assert!(first.starts_with(INTERNAL_MARK), "{first}");
    assert!(
        first.contains("Rows that reached review; 0 rows merged into another"),
        "{first}"
    );
    assert_eq!(
        lines.next(),
        Some("Rule ID,Rule,Proposed by the AI pass,Kept,Held for review,Information only,Thrown out,Why")
    );
    let data: Vec<&str> = lines.collect();
    assert_eq!(data.len(), 1, "{text}");
    assert!(data[0].starts_with("SEC-NO-HARDCODED-SECRETS-1,"), "{text}");
    assert!(data[0].contains("test fixture, not shipped"), "{text}");
}

/// A reviewer's reason is free text; a credential pasted into it must not reach even the internal
/// file.
#[tokio::test]
async fn a_credential_in_a_reviewers_reason_is_redacted_in_the_operator_file() {
    use camerata_secrets::fixtures::planted;
    use camerata_secrets::SecretShape;
    let corpus = corpus().await;
    let (report, _) = scenario();
    let secret = planted(SecretShape::StripeLiveSecretKey);
    let thrown = ai_finding("SEC-NO-HARDCODED-SECRETS-1", 3, "high");
    let mut dispositions = HashMap::new();
    dispositions.insert(finding_key(&thrown), false_positive(&secret.text));
    let bytes = operator_csv(
        &crate::report_export::ReviewedReport::unreviewed(&report, &dispositions),
        Some(&corpus),
        &HashMap::new(),
    )
    .unwrap();
    let text = String::from_utf8(bytes).unwrap();
    assert!(
        text.contains("thrown out: marked a false positive"),
        "anti-vacuity: the reason is in the file: {text}"
    );
    assert!(
        !text.contains(&secret.value),
        "the credential leaked: {text}"
    );
}

#[test]
fn an_empty_operator_file_still_has_its_mark_and_header() {
    let report = tests::report_with(Vec::new(), Vec::new()).seal();
    let bytes = operator_csv(
        &crate::report_export::ReviewedReport::unreviewed(&report, &HashMap::new()),
        None,
        &HashMap::new(),
    )
    .unwrap();
    let text = String::from_utf8(bytes).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert!(lines[0].starts_with(INTERNAL_MARK), "{text}");
    assert!(lines[1].starts_with("Rule ID,"), "{text}");
}

/// A stale client still sends the old key: it parses and changes nothing.
#[test]
fn the_old_triage_log_key_is_ignored_by_the_export_options() {
    let on: ReportOptions = serde_json::from_str(r#"{"triage_log": true}"#).unwrap();
    let absent: ReportOptions = serde_json::from_str("{}").unwrap();
    assert_eq!(
        format!("{on:?}"),
        format!("{absent:?}"),
        "the old key leaves no trace in the options"
    );
}

#[test]
fn the_env_switch_asks_for_the_operator_file_only_for_one_or_true() {
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
async fn the_operator_files_first_line_carries_the_ledgers_deduped_count() {
    let corpus = corpus().await;
    let (_, dispositions) = scenario();
    let report = sealed_with_repeats_dropped(4);
    assert_eq!(report.ledger.deduped_total(), 4);
    let bytes = operator_csv(
        &crate::report_export::ReviewedReport::unreviewed(&report, &dispositions),
        Some(&corpus),
        &HashMap::new(),
    )
    .unwrap();
    let text = String::from_utf8(bytes).unwrap();
    let first = text.lines().next().unwrap();
    assert!(
        first.contains(
            "Rows that reached review; 4 rows merged into another at the same location are not listed"
        ),
        "{first}"
    );
}

/// The operator file's text follows the package's words rule: no dashes and no serial commas.
#[test]
fn the_mark_has_no_dashes() {
    assert!(!INTERNAL_MARK.contains(['\u{2013}', '\u{2014}']));
}
