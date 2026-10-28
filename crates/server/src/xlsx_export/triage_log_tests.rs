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
    (report.seal(), dispositions)
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
