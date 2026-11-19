//! Outside review item 2: every critical and every unsure finding needs a recorded decision
//! before a client package leaves. A real package is built through `assemble_package` around a
//! stand-in PDF, like the gate's other tests, so these run without Typst.

use std::collections::HashMap;

use super::tests::{client_options, scan_report};
use super::*;
use crate::onboard::{Finding, ScanReport};
use crate::report_export::{build_report_json, finding_key, render_data_json, DispositionWire};
use camerata_api_types::review::{EscalationReason, EscalationState};

fn package_with(
    report: ScanReport,
    dispositions: &HashMap<String, DispositionWire>,
) -> ProductPackage {
    let sealed = report.seal();
    let options = client_options();
    let reviewed = crate::report_export::ReviewedReport::unreviewed(&sealed, dispositions);
    let json = build_report_json(&reviewed, None, &options);
    let data = render_data_json(&json).expect("data.json renders");
    assemble_package(
        &reviewed,
        None,
        &options,
        "acme-inspection",
        (json, data, b"%PDF-1.7 stand-in for the gate tests".to_vec()),
    )
    .expect("the package assembles")
}

/// A critical the group reviewer wanted but the review budget left below the line, for a reason
/// that is not one of the severe ones.
fn critical_below_the_line() -> Finding {
    let mut f = Finding {
        repo: "acme/portal".to_string(),
        path: "src/admin.rs".to_string(),
        line: 40,
        rule_id: "SEC-NO-RAW-SQL-CONCAT-1".to_string(),
        severity: "critical".to_string(),
        snippet: "run(format!(\"drop {}\", name))".to_string(),
        detail: "A table name goes into SQL by string building.".to_string(),
        ..Finding::default()
    };
    f.review.escalation_state = EscalationState::BelowTheLine;
    f.review.escalation_reason = Some(EscalationReason::NeedsMoreCode);
    f
}

/// A medium the finder was unsure about.
fn unsure_medium() -> Finding {
    Finding {
        repo: "acme/portal".to_string(),
        path: "src/cache.rs".to_string(),
        line: 7,
        rule_id: "SEC-NO-RAW-SQL-CONCAT-1".to_string(),
        severity: "medium".to_string(),
        snippet: "cache.get(key)".to_string(),
        detail: "The key may come from a request.".to_string(),
        needs_review: true,
        confidence: Some("needs-review".to_string()),
        ..Finding::default()
    }
}

fn report_with(extra: Finding) -> ScanReport {
    let mut report = scan_report();
    report.findings.push(extra);
    report
}

fn decided(row: &Finding, state: &str) -> HashMap<String, DispositionWire> {
    HashMap::from([(
        finding_key(row),
        DispositionWire {
            state: state.to_string(),
            reason: "read it in person".to_string(),
            ..DispositionWire::default()
        },
    )])
}

fn decision_problems(failure: &GateFailure) -> Vec<&Problem> {
    failure
        .report
        .checks
        .iter()
        .filter(|o| o.check == CheckId::CriticalOrUnsureWithoutDecision)
        .flat_map(|o| o.problems.iter())
        .collect()
}

#[test]
fn a_critical_below_the_line_with_no_decision_is_refused_and_named() {
    let package = package_with(report_with(critical_below_the_line()), &HashMap::new());
    match release(package) {
        Err(ReleaseError::Blocked(failure)) => {
            let problems = decision_problems(&failure);
            assert_eq!(problems.len(), 1, "{:?}", failure.report);
            assert!(
                problems[0].location.contains("src/admin.rs line 40"),
                "{}",
                problems[0].location
            );
            assert!(failure.to_string().contains("src/admin.rs line 40"));
            assert!(
                problems[0].message.contains("critical"),
                "{}",
                problems[0].message
            );
        }
        other => panic!("an undecided critical must block the release: {other:?}"),
    }
}

#[test]
fn the_same_critical_passes_once_it_carries_a_decision() {
    for state in ["Confirmed", "Ignored", "TechDebt", "FalsePositive"] {
        let row = critical_below_the_line();
        let package = package_with(report_with(row.clone()), &decided(&row, state));
        let released = release(package);
        assert!(released.is_ok(), "state {state}: {:?}", released.err());
    }
}

#[test]
fn an_unsure_medium_with_no_decision_is_refused_and_named() {
    let package = package_with(report_with(unsure_medium()), &HashMap::new());
    match release(package) {
        Err(ReleaseError::Blocked(failure)) => {
            let problems = decision_problems(&failure);
            assert_eq!(problems.len(), 1, "{:?}", failure.report);
            assert!(
                problems[0].location.contains("src/cache.rs line 7"),
                "{}",
                problems[0].location
            );
            assert!(
                problems[0].message.contains("unsure"),
                "{}",
                problems[0].message
            );
        }
        other => panic!("an undecided unsure row must block the release: {other:?}"),
    }
}

#[test]
fn an_unsure_medium_passes_once_it_carries_a_decision() {
    let row = unsure_medium();
    let package = package_with(report_with(row.clone()), &decided(&row, "Confirmed"));
    let released = release(package);
    assert!(released.is_ok(), "{:?}", released.err());
}

#[test]
fn a_high_or_a_sure_medium_with_no_decision_is_not_forced() {
    // The base report already holds an undecided high; add a sure medium beside it.
    let mut sure_medium = unsure_medium();
    sure_medium.needs_review = false;
    sure_medium.confidence = Some("high".to_string());
    let package = package_with(report_with(sure_medium), &HashMap::new());
    assert!(package.critical_or_unsure_without_decision.is_empty());
    let released = release(package);
    assert!(released.is_ok(), "{:?}", released.err());
}

#[test]
fn an_internal_package_may_carry_the_undecided_rows() {
    let sealed = report_with(critical_below_the_line()).seal();
    let options = ReportOptions {
        internal: true,
        ..client_options()
    };
    let reviewed = crate::report_export::ReviewedReport::unreviewed(&sealed, &HashMap::new());
    let json = build_report_json(&reviewed, None, &options);
    let data = render_data_json(&json).expect("data.json renders");
    let package = assemble_package(
        &reviewed,
        None,
        &options,
        "acme-inspection",
        (json, data, b"%PDF-1.7 stand-in for the gate tests".to_vec()),
    )
    .expect("the package assembles");
    let report = gate_report(
        &package,
        &zip_package(&package, &package.pdf).unwrap(),
        true,
    );
    assert!(
        !report
            .checks
            .iter()
            .any(|o| o.check == CheckId::CriticalOrUnsureWithoutDecision && !o.problems.is_empty()),
        "{report:?}"
    );
}

#[test]
fn the_export_panel_lists_the_rows_the_gate_refuses() {
    use camerata_api_types::review_view::HoldReason;
    let mut report = report_with(critical_below_the_line());
    report.findings.push(unsure_medium());
    let sealed = report.seal();
    let reviewed = crate::report_export::ReviewedReport::unreviewed(&sealed, &HashMap::new());
    let view = holds::gate_holds(&reviewed);
    let listed: Vec<(HoldReason, String)> = view
        .held
        .iter()
        .map(|h| (h.reason, h.path.clone()))
        .collect();
    assert!(
        listed.contains(&(
            HoldReason::CriticalWithoutDecision,
            "src/admin.rs".to_string()
        )),
        "{listed:?}"
    );
    assert!(
        listed.contains(&(
            HoldReason::UnsureWithoutDecision,
            "src/cache.rs".to_string()
        )),
        "{listed:?}"
    );
    // The same rows are kept out of a batch decision, so each is decided in person.
    let held = holds::hold_set(&reviewed);
    assert_eq!(held.awaiting_a_ruling.len(), 2, "{held:?}");
}

/// The review budget and the reviewed count are printed in the PDF's data and in the workbook, in
/// the same words, so a blank "Reviewed in person" cell is explained.
#[test]
fn the_budget_and_the_reviewed_count_are_printed_in_the_pdf_and_the_workbook() {
    let row = critical_below_the_line();
    // A real run carries its review tree, which holds the budget.
    let mut report = report_with(row.clone());
    report.attach_tree();
    let package = package_with(report, &decided(&row, "Confirmed"));
    let budget = package.report_json.escalations.review_line.clone();
    assert!(
        budget.starts_with("1 place was reviewed in person."),
        "{budget}"
    );
    let tree_budget = package
        .report_json
        .counts
        .review
        .review_budget
        .expect("the review tree holds the budget");
    assert!(tree_budget > 0);
    assert!(
        budget.contains(&format!(
            "review budget for this inspection is {tree_budget} places"
        )),
        "{budget}"
    );
    assert!(package.data_json.contains(&budget), "the PDF data");
    let cells = xlsx_strings(&package.xlsx).expect("the workbook strings");
    assert!(cells.contains(&budget), "the workbook: {cells:?}");
    // And the template prints it.
    let template = include_str!("../../templates/audit_report.typ");
    assert!(template.contains("d.escalations.review_line"));
}

/// The PDF's reviewed count is the workbook's count of "Reviewed in person" cells on All problems.
#[test]
fn the_reviewed_count_equals_the_workbooks_reviewed_in_person_cells() {
    let row = critical_below_the_line();
    let package = package_with(report_with(row.clone()), &decided(&row, "Confirmed"));
    let grid = xlsx_sheet_grid(&package.xlsx, crate::xlsx_export::vocabulary::ALL_PROBLEMS)
        .expect("All problems");
    let header_at = grid
        .iter()
        .position(|r| {
            r.iter()
                .any(|c| c == crate::xlsx_export::vocabulary::H_REVIEWED_IN_PERSON)
        })
        .expect("the header row");
    let col = grid[header_at]
        .iter()
        .position(|c| c == crate::xlsx_export::vocabulary::H_REVIEWED_IN_PERSON)
        .unwrap();
    let yes = grid[header_at + 1..]
        .iter()
        .filter(|r| r.get(col).is_some_and(|c| !c.is_empty()))
        .count();
    assert_eq!(yes, 1);
    assert_eq!(package.report_json.counts.review.reviewed_in_person, yes);
}

/// The compiled PDF itself prints the line (skipped where Typst is not installed).
#[tokio::test]
async fn the_compiled_pdf_prints_the_review_line() {
    if !camerata_test_env::typst_on_path() {
        eprintln!("skipped: typst is not on PATH");
        return;
    }
    let row = critical_below_the_line();
    let mut report = report_with(row.clone());
    report.attach_tree();
    let package = package_with(report, &decided(&row, "Confirmed"));
    let pdf = crate::report_export::compile_pdf_from_data(&package.data_json)
        .await
        .expect("the PDF compiles");
    let text = pdf_extract::extract_text_from_mem(&pdf).unwrap_or_default();
    let squeeze = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    let line = squeeze(&package.report_json.escalations.review_line);
    assert!(squeeze(&text).contains(&line), "{text}");
}

/// The gate reads the severity the package SHOWS: a rule's minimum severity is applied again at
/// export, so a row stored below critical can print as critical. It needs a decision too.
#[test]
fn a_row_the_exports_show_at_critical_through_a_rule_floor_needs_a_decision() {
    let mut rule = camerata_rules::bare_rule("SYN-FLOOR-1", "security");
    rule.min_severity = Some("critical".to_string());
    let mut set = camerata_rules::RuleSet::default();
    set.push(rule);
    let row = Finding {
        repo: "acme/portal".to_string(),
        path: "src/floor.rs".to_string(),
        line: 3,
        rule_id: "SYN-FLOOR-1".to_string(),
        severity: "low".to_string(),
        snippet: "let x = 1;".to_string(),
        detail: "A synthetic problem under a rule with a critical floor.".to_string(),
        kind: Some(camerata_api_types::audit::FindingKind::Defect),
        ..Finding::default()
    };
    let sealed = report_with(row).seal();
    let options = client_options();
    let reviewed = crate::report_export::ReviewedReport::unreviewed(&sealed, &HashMap::new());
    let json = build_report_json(&reviewed, Some(&set), &options);
    let data = render_data_json(&json).expect("data.json renders");
    let package = assemble_package(
        &reviewed,
        Some(&set),
        &options,
        "acme-inspection",
        (json, data, b"%PDF-1.7 stand-in for the gate tests".to_vec()),
    )
    .expect("the package assembles");
    assert_eq!(
        package.findings_export.shown_critical_site_ids().len(),
        1,
        "the rule floor shows the row at critical"
    );
    assert_eq!(package.critical_or_unsure_without_decision.len(), 1);
    assert!(
        package.critical_or_unsure_without_decision[0]
            .location
            .contains("src/floor.rs line 3"),
        "{:?}",
        package.critical_or_unsure_without_decision
    );
}

fn node_record(
    key: &str,
    state: camerata_api_types::decisions::DecisionState,
    id: &str,
    parent: Option<&str>,
) -> camerata_api_types::decisions::Decision {
    use camerata_api_types::decisions::{Decision, Scope};
    Decision {
        id: id.to_string(),
        key: key.to_string(),
        state,
        reason: "r".to_string(),
        scope: Scope::Site,
        applies_to: Vec::new(),
        except: Vec::new(),
        inherited_from: parent.map(str::to_string),
        decided_by: "reviewer".to_string(),
        decided_at: "2026-11-04T09:00:00Z".to_string(),
        run_id: "insp-1".to_string(),
        engine_version: "0.0.0-test".to_string(),
        needed_me: None,
        sample_verdict: None,
        confirmed_by_client: None,
        revoked_at: None,
    }
}

/// A group record that comes after the reviewer's own decision overwrites it: the critical is
/// undecided again, and the gate refuses.
#[test]
fn a_group_record_after_a_personal_decision_does_not_decide_a_critical() {
    use camerata_api_types::decisions::DecisionState;
    // An escalated row: a group record on it reads as PersonallyReviewed in the overlay.
    let mut row = critical_below_the_line();
    row.review.escalation_state = EscalationState::Escalated;
    let sealed = report_with(row).seal();
    let key = sealed
        .findings
        .iter()
        .find(|f| f.path == "src/admin.rs")
        .unwrap()
        .site_key
        .clone();
    let personal = node_record(&key, DecisionState::Confirmed, "dec-1", None);
    let mut node = node_record("", DecisionState::FalsePositive, "dec-0", None);
    node.scope = camerata_api_types::decisions::Scope::Node("n".to_string());
    node.applies_to = vec![key.clone()];
    let group = node_record(&key, DecisionState::FalsePositive, "dec-2", Some("dec-0"));
    let own =
        crate::report_export::apply_overlay(&sealed, std::slice::from_ref(&personal), &[], &[]);
    assert!(needs_decision::undecided(&own.report().findings, &own)
        .next()
        .is_none());
    let overwritten =
        crate::report_export::apply_overlay(&sealed, &[personal, node, group], &[], &[]);
    let rows: Vec<_> =
        needs_decision::undecided(&overwritten.report().findings, &overwritten).collect();
    assert_eq!(
        rows.len(),
        1,
        "an inherited record is not a decision in person"
    );
    // And a decided critical is still kept out of a batch.
    assert!(holds::hold_set(&own).kept_out_of_a_batch.contains(&key));
}

/// A row the workbook prints as Confidence Low through a needs-review confidence needs a decision.
#[test]
fn a_needs_review_confidence_alone_makes_a_row_unsure() {
    let mut row = unsure_medium();
    row.needs_review = false;
    row.confidence = Some("needs-review".to_string());
    let package = package_with(report_with(row), &HashMap::new());
    assert_eq!(package.critical_or_unsure_without_decision.len(), 1);
}
