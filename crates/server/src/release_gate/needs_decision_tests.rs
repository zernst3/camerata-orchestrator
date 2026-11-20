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

// ── a severity edit on the row is the reviewer's decision (Zach, 2026-11-20: "If I manually update
// the severity at all, that is my decision regardless.") ─────────────────────────────────────────

use camerata_api_types::edits::{Edit, EditField};
use camerata_api_types::review_view::{CorrectionRef, EngineCheckError, EngineCheckVerdict};

const EDIT_AT: &str = "2026-11-20T10:00:00Z";
const CHECK_RUN: &str = "insp-1";

fn edit_on(n: usize, f: &Finding, field: EditField, old: &str, new: &str) -> Edit {
    Edit {
        id: format!("edit-{n:06}"),
        key: f.site_key.clone(),
        field,
        old: old.to_string(),
        new: new.to_string(),
        reason: "the reviewer judged it".to_string(),
        by: "reviewer".to_string(),
        at: EDIT_AT.to_string(),
        run_id: CHECK_RUN.to_string(),
        fingerprint_at_edit: camerata_api_types::fingerprint::fingerprint(&f.rule_id, &f.snippet),
        stale: false,
        discarded_at: None,
        reapplied_from: None,
    }
}

fn sealed_row(sealed: &crate::onboard::SealedReport, path: &str) -> Finding {
    sealed
        .findings
        .iter()
        .find(|f| f.path == path)
        .unwrap_or_else(|| panic!("no row in {path}"))
        .clone()
}

fn package_of_reviewed(reviewed: &crate::report_export::ReviewedReport) -> ProductPackage {
    let options = client_options();
    let json = build_report_json(reviewed, None, &options);
    let data = render_data_json(&json).expect("data.json renders");
    assemble_package(
        reviewed,
        None,
        &options,
        "acme-inspection",
        (json, data, b"%PDF-1.7 stand-in for the gate tests".to_vec()),
    )
    .expect("the package assembles")
}

/// The base report's high row at src/db.rs, with an edit applied to it.
fn base_row_edited(
    field: EditField,
    old: &str,
    new: &str,
) -> (
    crate::onboard::SealedReport,
    crate::report_export::ReviewedReport,
    Edit,
) {
    let sealed = scan_report().seal();
    let row = sealed_row(&sealed, "src/db.rs");
    let edit = edit_on(1, &row, field, old, new);
    let reviewed =
        crate::report_export::apply_overlay(&sealed, &[], std::slice::from_ref(&edit), &[]);
    (sealed, reviewed, edit)
}

/// Review door: a high row the reviewer raised to critical in Review is decided by that edit.
#[test]
fn a_review_severity_edit_to_critical_releases_the_row() {
    let (_, reviewed, _) = base_row_edited(EditField::Severity, "high", "critical");
    let row = sealed_row(reviewed.report(), "src/db.rs");
    assert_eq!(row.severity, "critical", "the edit is applied");
    assert!(
        needs_decision::undecided(&reviewed.report().findings, &reviewed)
            .next()
            .is_none(),
        "the reviewer's own severity edit is the decision"
    );
    let package = package_of_reviewed(&reviewed);
    assert!(
        package.critical_or_unsure_without_decision.is_empty(),
        "{:?}",
        package.critical_or_unsure_without_decision
    );
    let released = release(package);
    assert!(released.is_ok(), "{:?}", released.err());
}

/// Whatever the direction: an unsure row the reviewer moved up or down is decided too.
#[test]
fn a_severity_edit_in_either_direction_decides_an_unsure_row() {
    for (old, new) in [("medium", "high"), ("medium", "low")] {
        let sealed = report_with(unsure_medium()).seal();
        let row = sealed_row(&sealed, "src/cache.rs");
        let edit = edit_on(1, &row, EditField::Severity, old, new);
        let reviewed =
            crate::report_export::apply_overlay(&sealed, &[], std::slice::from_ref(&edit), &[]);
        let package = package_of_reviewed(&reviewed);
        assert!(
            package.critical_or_unsure_without_decision.is_empty(),
            "{old} to {new}: {:?}",
            package.critical_or_unsure_without_decision
        );
    }
}

/// An edit to wording or location is not a severity decision.
#[test]
fn an_edit_of_another_field_is_not_a_decision() {
    for (field, old, new) in [
        (EditField::Headline, "", "A new headline"),
        (EditField::Detail, "", "A new explanation"),
        (EditField::Fix, "", "A new fix"),
    ] {
        let sealed = report_with(critical_below_the_line()).seal();
        let row = sealed_row(&sealed, "src/admin.rs");
        let edit = edit_on(1, &row, field, old, new);
        let reviewed =
            crate::report_export::apply_overlay(&sealed, &[], std::slice::from_ref(&edit), &[]);
        let package = package_of_reviewed(&reviewed);
        assert_eq!(
            package.critical_or_unsure_without_decision.len(),
            1,
            "{field:?} edit must not decide the critical"
        );
    }
}

/// A discarded severity edit is not applied, so it decides nothing.
#[test]
fn a_discarded_severity_edit_is_not_a_decision() {
    let sealed = scan_report().seal();
    let row = sealed_row(&sealed, "src/db.rs");
    let mut edit = edit_on(1, &row, EditField::Severity, "high", "critical");
    edit.discarded_at = Some(EDIT_AT.to_string());
    let reviewed = crate::report_export::apply_overlay(&sealed, &[], &[edit], &[]);
    // The row is still high, and still not forced.
    assert!(
        needs_decision::undecided(&reviewed.report().findings, &reviewed)
            .next()
            .is_none()
    );
    assert_eq!(sealed_row(reviewed.report(), "src/db.rs").severity, "high");
}

/// Engine Check door: the correction the verdict names is a severity edit, and it decides the row
/// as well as answering the No.
#[test]
fn an_engine_check_severity_correction_to_critical_releases_the_row() {
    let (sealed, _, edit) = base_row_edited(EditField::Severity, "high", "critical");
    let row = sealed_row(&sealed, "src/db.rs");
    let verdict = EngineCheckVerdict {
        key: row.site_key.clone(),
        run_id: CHECK_RUN.to_string(),
        right: false,
        wrong: Some(EngineCheckError::WrongSeverity),
        correction: Some(CorrectionRef::Edit(edit.id.clone())),
        by: "reviewer".to_string(),
        at: "2026-11-20T09:00:00Z".to_string(),
        carried_from: None,
    };
    let reviewed =
        crate::report_export::apply_overlay(&sealed, &[], std::slice::from_ref(&edit), &[])
            .with_engine_check(crate::engine_check::EngineCheckEvidence::Recorded {
                run_id: CHECK_RUN.to_string(),
                verdicts: vec![verdict],
                edits: vec![edit],
            });
    assert!(
        crate::release_gate::holds::gate_holds(&reviewed)
            .held
            .is_empty(),
        "{:?}",
        crate::release_gate::holds::gate_holds(&reviewed).held
    );
    let released = release(package_of_reviewed(&reviewed));
    assert!(released.is_ok(), "{:?}", released.err());
}

/// A group decision on the row is not the reviewer's own; their own severity edit on the same row is.
#[test]
fn an_own_severity_edit_counts_beside_an_inherited_group_record() {
    use camerata_api_types::decisions::{DecisionState, Scope};
    let sealed = report_with(unsure_medium()).seal();
    let row = sealed_row(&sealed, "src/cache.rs");
    let key = row.site_key.clone();
    let mut node = node_record("", DecisionState::Confirmed, "dec-0", None);
    node.scope = Scope::Node("n".to_string());
    node.applies_to = vec![key.clone()];
    let group = node_record(&key, DecisionState::Confirmed, "dec-2", Some("dec-0"));
    let records = [node, group];
    let without_edit = crate::report_export::apply_overlay(&sealed, &records, &[], &[]);
    assert_eq!(
        needs_decision::undecided(&without_edit.report().findings, &without_edit).count(),
        1,
        "a group record alone is not a decision in person"
    );
    let edit = edit_on(1, &row, EditField::Severity, "medium", "low");
    let with_edit = crate::report_export::apply_overlay(&sealed, &records, &[edit], &[]);
    assert_eq!(
        needs_decision::undecided(&with_edit.report().findings, &with_edit).count(),
        0
    );
}

/// The gate's text must be true for every hold it is shown on: it names only what releases the row.
#[test]
fn the_problem_text_names_only_ways_that_release_the_row() {
    for row in [critical_below_the_line(), unsure_medium()] {
        let package = package_with(report_with(row), &HashMap::new());
        let message = package.critical_or_unsure_without_decision[0]
            .message
            .clone();
        assert!(
            message.contains("confirm it, mark it a false positive or set its severity"),
            "{message}"
        );
        assert!(
            !message.contains("correct it"),
            "an edit of the wording does not release the row: {message}"
        );
    }
}

/// The exports record the row as decided by the severity edit.
#[test]
fn the_exports_record_a_severity_edit_as_the_decision() {
    let (_, reviewed, edit) = base_row_edited(EditField::Severity, "high", "critical");
    let package = package_of_reviewed(&reviewed);
    let decisions: serde_json::Value = serde_json::from_slice(
        &crate::report_export::decisions_file::decisions_json(&reviewed).unwrap(),
    )
    .unwrap();
    let rows = decisions["decisions"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["id"], "severity-set-000001");
    assert!(
        !rows[0].to_string().contains(&edit.id),
        "edit ids stay out of decisions.json"
    );
    assert_eq!(rows[0]["scope"], "site");
    assert_eq!(rows[0]["state"], "severity_set");
    assert_eq!(rows[0]["has_reason"], true);
    assert_eq!(rows[0]["reason"], "", "reviewer text stays in the store");
    let label = "Severity set by the reviewer";
    // The report's rows (the PDF's summary reads their counts, and the CSV and the workbook print
    // them) say who decided; the PDF's own review line counts the place as read in person.
    let report_text = serde_json::to_string(&package.report_json).unwrap();
    assert!(
        report_text.contains(label),
        "the report rows say who decided"
    );
    assert!(
        package
            .report_json
            .escalations
            .review_line
            .starts_with("1 place was reviewed in person."),
        "{}",
        package.report_json.escalations.review_line
    );
    let cells = xlsx_strings(&package.xlsx).expect("the workbook strings");
    assert!(
        cells.iter().any(|c| c.contains(label)),
        "the workbook says who decided"
    );
    // No em or en dashes, no serial commas in the words.
    assert!(!label.contains('\u{2014}') && !label.contains('\u{2013}'));
}

// ── round 1 review: the released package, forged marks, stale edits, counts ─────────────────

/// A row the finder was unsure of, with no model confidence (a row with one is an AI-tier row and
/// is held again for having no published citation, which a severity edit does not lift).
fn doubted_row() -> Finding {
    let mut f = unsure_medium();
    f.confidence = None;
    f
}

fn all_problems_cell(zip: &[u8], path: &str, header: &str) -> String {
    let entries = read_zip_entries(zip).unwrap();
    let xlsx = entries
        .iter()
        .find(|(name, _)| name.ends_with(".xlsx"))
        .map(|(_, bytes)| bytes.clone())
        .unwrap();
    let grid = xlsx_sheet_grid(&xlsx, "All problems").expect("All problems sheet");
    let col = grid[0]
        .iter()
        .position(|h| h == header)
        .unwrap_or_else(|| panic!("no {header} column in {:?}", grid[0]));
    let row = grid
        .iter()
        .skip(1)
        .find(|r| r.iter().any(|c| c.contains(path)))
        .unwrap_or_else(|| panic!("no row for {path}"));
    row[col].clone()
}

fn findings_row(zip: &[u8], path: &str) -> serde_json::Value {
    let entries = read_zip_entries(zip).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&entries["findings.json"]).unwrap();
    v["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["path"] == path)
        .unwrap_or_else(|| panic!("no findings.json row for {path}"))
        .clone()
}

/// Review round 1, item 1: an unsure row the reviewer released by a severity edit ships as a
/// decided row, not as "Needs a decision". Ruling: a severity edit is the reviewer's decision.
#[test]
fn a_released_unsure_row_reads_as_decided_not_as_needs_a_decision() {
    // The doubt is the engine's statement and exports as it is, as on a Confirmed unsure row;
    // the reviewer's ruling is the label, the Decision so far cell and the severity_set record.
    for row in [doubted_row(), unsure_medium()] {
        let sealed = report_with(row.clone()).seal();
        let sealed_row = sealed_row(&sealed, "src/cache.rs");
        let edit = edit_on(1, &sealed_row, EditField::Severity, "medium", "high");
        let reviewed =
            crate::report_export::apply_overlay(&sealed, &[], std::slice::from_ref(&edit), &[]);
        let released = release(package_of_reviewed(&reviewed)).expect("released");
        let json = findings_row(&released.bytes, "src/cache.rs");
        let label = json["disposition_label"].as_str().unwrap_or_default();
        assert!(label.starts_with("Severity set by the reviewer"), "{json}");
        assert!(!label.starts_with("Needs a decision"), "{json}");
        assert_eq!(
            all_problems_cell(&released.bytes, "src/cache.rs", "Decision so far"),
            "Severity set by the reviewer",
            "{json}"
        );
        // The same hedge values a Confirmed unsure row exports.
        let confirmed = package_with(report_with(row.clone()), &decided(&row, "Confirmed"));
        let confirmed_zip = release(confirmed).expect("released");
        let c = findings_row(&confirmed_zip.bytes, "src/cache.rs");
        assert_eq!(json["needs_review"], c["needs_review"], "{json} {c}");
        assert_eq!(json["confidence"], c["confidence"], "{json} {c}");
        assert_eq!(json["needs_review"], true, "{json}");
        assert_eq!(json["confidence"], "needs-review", "{json}");
    }
}

/// The decided count agrees too: on the real unsure row, an internal export counts the flagged
/// row as decided once the reviewer set its severity.
#[test]
fn the_decided_count_includes_the_real_unsure_row_with_a_severity_edit() {
    let sealed = report_with(unsure_medium()).seal();
    let row = sealed_row(&sealed, "src/cache.rs");
    let edit = edit_on(1, &row, EditField::Severity, "medium", "high");
    let reviewed =
        crate::report_export::apply_overlay(&sealed, &[], std::slice::from_ref(&edit), &[]);
    let options = ReportOptions {
        internal: true,
        ..client_options()
    };
    let json = build_report_json(&reviewed, None, &options);
    let data = render_data_json(&json).unwrap();
    let package = assemble_package(
        &reviewed,
        None,
        &options,
        "acme-inspection",
        (json, data, b"%PDF-1.7 stand-in".to_vec()),
    )
    .unwrap();
    let strings = xlsx_strings(&package.xlsx).unwrap();
    assert!(
        strings
            .iter()
            .any(|c| c == "1 row flagged as a possible false positive, 1 decided"),
        "{:?}",
        strings
            .iter()
            .filter(|c| c.contains("flagged"))
            .collect::<Vec<_>>()
    );
}

/// The severity edit lifts the reviewer-doubt hedge only: unverified evidence, junk text and a
/// put-back row stay held.
#[test]
fn a_severity_edit_does_not_lift_the_other_holds() {
    use crate::report_export::{is_held_for_review, Disposition};
    for capture in [
        crate::onboard::evidence_check::EVIDENCE_UNVERIFIED_CAPTURE,
        crate::junk_finding::JUNK_CONTENT_CAPTURE,
        crate::engine_discard::ENGINE_RESTORED_CAPTURE,
    ] {
        let mut f = unsure_medium();
        f.captures.insert(capture.to_string(), "x".to_string());
        f.captures.insert(
            crate::report_export::overlay::REVIEWER_FIELDS_CAPTURE.to_string(),
            "severity".to_string(),
        );
        assert!(is_held_for_review(&f, Disposition::Unresolved), "{capture}");
    }
    let mut plain = unsure_medium();
    plain.captures.insert(
        crate::report_export::overlay::REVIEWER_FIELDS_CAPTURE.to_string(),
        "severity".to_string(),
    );
    assert!(!is_held_for_review(&plain, Disposition::Unresolved));
    let mut headline_only = unsure_medium();
    headline_only.captures.insert(
        crate::report_export::overlay::REVIEWER_FIELDS_CAPTURE.to_string(),
        "headline".to_string(),
    );
    assert!(is_held_for_review(&headline_only, Disposition::Unresolved));
}

/// Item 5: only a real stored edit counts. A sealed row that already carries the reviewer-edit
/// marks (forged or carried over by a merge) is still held.
#[test]
fn a_sealed_row_with_forged_edit_marks_is_still_held() {
    let mut row = critical_below_the_line();
    row.captures.insert(
        crate::report_export::overlay::REVIEWER_FIELDS_CAPTURE.to_string(),
        "severity".to_string(),
    );
    row.captures.insert(
        crate::report_export::overlay::REVIEWER_EDITED_CAPTURE.to_string(),
        "edit-999999".to_string(),
    );
    let sealed = report_with(row).seal();
    let reviewed = crate::report_export::apply_overlay(&sealed, &[], &[], &[]);
    assert_eq!(
        needs_decision::undecided(&reviewed.report().findings, &reviewed).count(),
        1,
        "a forged mark is not a decision"
    );
    assert_eq!(
        package_of_reviewed(&reviewed)
            .critical_or_unsure_without_decision
            .len(),
        1
    );
}

/// Item 6: a stale severity edit (the row changed since) is not applied and decides nothing.
#[test]
fn a_stale_severity_edit_does_not_decide_the_row() {
    let sealed = report_with(critical_below_the_line()).seal();
    let row = sealed_row(&sealed, "src/admin.rs");
    let mut edit = edit_on(1, &row, EditField::Severity, "critical", "high");
    edit.fingerprint_at_edit = "not-the-rows-fingerprint".to_string();
    let reviewed = crate::report_export::apply_overlay(&sealed, &[], &[edit], &[]);
    assert_eq!(reviewed.stale_edits().len(), 1);
    assert_eq!(
        needs_decision::undecided(&reviewed.report().findings, &reviewed).count(),
        1
    );
}

/// Item 7: the counts agree with the Decision so far cell. A flagged row decided only by a
/// severity edit counts as decided in the internal workbook's possible-false-positive tally.
#[test]
fn the_counts_agree_with_decision_so_far_for_a_severity_edit() {
    // The calibration doubt flags the row as a possible false positive.
    let row = doubted_row();
    let sealed = report_with(row).seal();
    let row = sealed_row(&sealed, "src/cache.rs");
    let edit = edit_on(1, &row, EditField::Severity, "medium", "high");
    let reviewed =
        crate::report_export::apply_overlay(&sealed, &[], std::slice::from_ref(&edit), &[]);
    let options = ReportOptions {
        internal: true,
        ..client_options()
    };
    let json = build_report_json(&reviewed, None, &options);
    let data = render_data_json(&json).unwrap();
    let package = assemble_package(
        &reviewed,
        None,
        &options,
        "acme-inspection",
        (json, data, b"%PDF-1.7 stand-in".to_vec()),
    )
    .unwrap();
    let strings = xlsx_strings(&package.xlsx).unwrap();
    assert!(
        strings
            .iter()
            .any(|c| c == "1 row flagged as a possible false positive, 1 decided"),
        "a severity edit decides the flagged row: {:?}",
        strings
            .iter()
            .filter(|c| c.contains("flagged"))
            .collect::<Vec<_>>()
    );
}

/// Item 7, medium row with no record: the cell, the label and the open count agree.
#[test]
fn a_sure_medium_with_a_severity_edit_reads_decided_everywhere() {
    let mut row = doubted_row();
    row.needs_review = false;
    let sealed = report_with(row).seal();
    let row = sealed_row(&sealed, "src/cache.rs");
    let edit = edit_on(1, &row, EditField::Severity, "medium", "high");
    let reviewed =
        crate::report_export::apply_overlay(&sealed, &[], std::slice::from_ref(&edit), &[]);
    let released = release(package_of_reviewed(&reviewed)).expect("released");
    let json = findings_row(&released.bytes, "src/cache.rs");
    assert!(
        json["disposition_label"]
            .as_str()
            .or_else(|| json["disposition"].as_str())
            .unwrap_or_default()
            .starts_with("Severity set by the reviewer"),
        "{json}"
    );
    assert_eq!(
        all_problems_cell(&released.bytes, "src/cache.rs", "Decision so far"),
        "Severity set by the reviewer"
    );
}

/// Item 8: the actual strings the exports print carry no em or en dash and no serial comma.
#[test]
fn the_strings_the_exports_print_for_a_severity_edit_follow_the_text_rules() {
    let (_, reviewed, _) = base_row_edited(EditField::Severity, "high", "critical");
    let package = package_of_reviewed(&reviewed);
    let decisions =
        String::from_utf8(crate::report_export::decisions_file::decisions_json(&reviewed).unwrap())
            .unwrap();
    let report_text = serde_json::to_string(&package.report_json).unwrap();
    let sealed = report_with(critical_below_the_line()).seal();
    let undecided =
        package_of_reviewed(&crate::report_export::apply_overlay(&sealed, &[], &[], &[]));
    let problem = undecided.critical_or_unsure_without_decision[0]
        .message
        .clone();
    for text in [&decisions, &report_text, &problem] {
        assert!(!text.contains('\u{2014}') && !text.contains('\u{2013}'));
    }
    let strings = xlsx_strings(&package.xlsx).unwrap();
    for s in strings
        .iter()
        .filter(|s| s.contains("Severity set by the reviewer"))
    {
        assert!(!s.contains('\u{2014}') && !s.contains('\u{2013}'), "{s}");
        assert!(!s.contains(", and ") && !s.contains(", or "), "{s}");
    }
    assert!(
        problem.contains("confirm it, mark it a false positive or set its severity"),
        "{problem}"
    );
    assert!(!problem.contains("positive, or"), "{problem}");
    assert!(!crate::xlsx_export::vocabulary::SEVERITY_SET_BY_REVIEWER.contains(','));
}
