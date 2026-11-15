//! Danger before doubt on every export surface: a site escalated only for a doubt is on the
//! "Check: possible false alarm" list, not the urgent one, and the PDF's escalation summary, the
//! ledger sentence, the workbook and the release gate's checks all count the two lists alike.

use camerata_api_types::review::{EscalationCandidate, EscalationLane, EscalationReason};

use super::support::*;
use crate::report_export::{apply_overlay, build_report_json, ReportOptions, ReviewedReport};
use crate::xlsx_export::vocabulary as v;

fn candidate(key: &str, reason: EscalationReason, rank: u32) -> EscalationCandidate {
    EscalationCandidate {
        key: key.to_string(),
        node_id: "rule:X".to_string(),
        reason,
        severity: "high".to_string(),
        stage2_severity: None,
        stage2_rank: rank,
        global_rank: rank,
        slot: rank,
        escalated: false,
        over_budget_reason: None,
        why: String::new(),
        held: false,
        lane: EscalationLane::Urgent,
    }
}

/// The mixed report with two sites nominated: `src/a.rs` for a danger (secret exposure) and
/// `src/d.rs` only for a doubt (conflicting verdicts). The line is marked by the real formula,
/// with room in the budget for both.
fn two_lists() -> crate::onboard::SealedReport {
    let mut report = mixed_sealed().into_inner();
    let a = report
        .findings
        .iter()
        .find(|f| f.path == "src/a.rs")
        .unwrap()
        .site_key
        .clone();
    let d = report
        .findings
        .iter()
        .find(|f| f.path == "src/d.rs")
        .unwrap()
        .site_key
        .clone();
    let rows_before = report.findings.len();
    let tree = report.tree.as_mut().expect("a tree");
    tree.candidates = vec![
        candidate(&d, EscalationReason::ConflictingVerdicts, 1),
        candidate(&a, EscalationReason::SecretExposure, 2),
    ];
    tree.budget = camerata_api_types::review::EscalationBudget::compute(1.0, 20, 400);
    tree.re_mark(
        &crate::review_tree::RankSettings {
            floor: 20,
            counted_lines: 400,
            ..crate::review_tree::RankSettings::default()
        },
        &mut report.findings,
    );
    assert_eq!(report.findings.len(), rows_before, "marking drops no row");
    assert_eq!(
        report.tree.as_ref().unwrap().candidates.len(),
        2,
        "nor a candidate"
    );
    report
        .ledger
        .record_escalation(crate::scan_ledger::EscalationBlock {
            model: Some("a-model".to_string()),
            rate_per_kloc: 1.0,
            floor: 20,
            counted_lines: 400,
            ..Default::default()
        });
    report.seal()
}

fn reviewed() -> ReviewedReport {
    apply_overlay(&two_lists(), &[], &[], &[])
}

fn paths(items: &[crate::report_export::EscalatedSiteJson]) -> Vec<&str> {
    items.iter().map(|i| i.path.as_str()).collect()
}

#[test]
fn the_pdf_lists_the_danger_and_counts_the_doubt_on_the_check_list() {
    let json = build_report_json(&reviewed(), None, &client_options());
    let e = &json.escalations;
    assert_eq!(
        paths(&e.items),
        vec!["src/a.rs"],
        "the urgent list holds the danger alone"
    );
    assert_eq!(
        paths(&e.check_items),
        vec!["src/d.rs"],
        "the doubt is on the check list"
    );
    assert!(
        e.check_line.starts_with(v::CHECK_POSSIBLE_FALSE_ALARM),
        "{:?}",
        e.check_line
    );
    assert!(e.check_line.contains("1 place"), "{:?}", e.check_line);
    assert!(!e.check_line.contains(['\u{2014}', '\u{2013}']));
}

#[test]
fn the_ledger_sentence_says_how_many_are_on_the_check_list() {
    let reviewed = reviewed();
    let block = reviewed.report().ledger.escalation().unwrap().clone();
    assert_eq!((block.escalated, block.check, block.check_rows), (2, 1, 1));
    assert_eq!(
        block.sentence(),
        format!(
            "Escalated 2 of a budget of 20 (none over), 1 of them on the {} list",
            v::CHECK_POSSIBLE_FALSE_ALARM
        )
    );
}

#[test]
fn the_internal_workbook_marks_the_check_list_beside_escalated() {
    let options = ReportOptions {
        internal: true,
        ..client_options()
    };
    let xlsx = crate::xlsx_export::build_workbook(&reviewed(), None, &options).unwrap();
    let grid = crate::release_gate::xlsx_sheet_grid(&xlsx, v::ALL_PROBLEMS).unwrap();
    assert!(
        grid[0].iter().any(|h| h == v::CHECK_POSSIBLE_FALSE_ALARM),
        "the internal All problems sheet has a {:?} column: {:?}",
        v::CHECK_POSSIBLE_FALSE_ALARM,
        grid[0]
    );
    let col = |h: &str| {
        grid[0]
            .iter()
            .position(|c| c == h)
            .unwrap_or_else(|| panic!("no {h:?} column in {:?}", grid[0]))
    };
    let (path, escalated, check) = (
        col(v::H_FILE),
        col(v::H_ESCALATED),
        col(v::CHECK_POSSIBLE_FALSE_ALARM),
    );
    let marked: Vec<(&str, &str, &str)> = grid[1..]
        .iter()
        .filter(|r| !r[escalated].is_empty() || !r[check].is_empty())
        .map(|r| (r[path].as_str(), r[escalated].as_str(), r[check].as_str()))
        .collect();
    assert_eq!(
        marked,
        vec![("src/a.rs", v::YES, ""), ("src/d.rs", v::YES, v::YES)],
        "both are escalated; the doubt alone is on the check list"
    );
    // A client workbook carries neither column.
    let client = crate::xlsx_export::build_workbook(&reviewed(), None, &client_options()).unwrap();
    let grid = crate::release_gate::xlsx_sheet_grid(&client, v::ALL_PROBLEMS).unwrap();
    assert!(!grid[0].iter().any(|h| h == v::CHECK_POSSIBLE_FALSE_ALARM));
}

#[test]
fn the_counts_of_the_two_lists_reconcile_on_every_export_surface() {
    let reviewed = reviewed();
    let block = reviewed.report().ledger.escalation().unwrap().clone();
    let package = package_of(
        &reviewed,
        &ReportOptions {
            internal: true,
            ..client_options()
        },
    );
    let e = &package.report_json.escalations;
    assert_eq!(e.items.len() + e.check_items.len(), block.escalated);
    assert_eq!(e.check_items.len(), block.check);
    let tree = reviewed.report().tree.as_ref().unwrap();
    assert_eq!(tree.budget.escalated, block.escalated);
    assert_eq!(tree.budget.check, block.check);
    let check_rows = package
        .findings_export
        .findings
        .iter()
        .filter_map(|r| serde_json::to_value(r).ok())
        .filter(|r| r["check_list"] == true)
        .count();
    assert_eq!(check_rows, block.check_rows);
    let clean =
        crate::release_gate::fix_report_problems(&package.report_json, &package.findings_export);
    assert!(clean.is_empty(), "{clean:?}");
}

#[test]
fn the_release_gate_refuses_a_check_entry_on_the_urgent_list_or_a_lost_one() {
    let package = package_of(&reviewed(), &client_options());
    let text = |r: &crate::report_export::AuditReportJson| {
        crate::release_gate::fix_report_problems(r, &package.findings_export)
            .iter()
            .map(|p| format!("{p:?}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(
        package.report_json.escalations.check_items.len(),
        1,
        "the doubt is on the check list"
    );
    // The doubt moved onto the urgent list.
    let mut moved = package.report_json.clone();
    let entry = moved.escalations.check_items.pop().unwrap();
    moved.escalations.items.push(entry);
    assert!(
        text(&moved).contains(v::CHECK_POSSIBLE_FALSE_ALARM),
        "{}",
        text(&moved)
    );
    // The doubt dropped from both lists.
    let mut lost = package.report_json.clone();
    lost.escalations.check_items.clear();
    assert!(text(&lost).contains("escalated places"), "{}", text(&lost));
}

/// The PDF prints both lists: the dangerous place under the Escalations intro, then the check
/// line and the doubtful place after it. One entry ("Rules: ...") for each place above the line.
#[tokio::test]
async fn the_rendered_pdf_prints_the_urgent_entries_then_the_check_list() {
    if !camerata_test_env::typst_on_path() {
        eprintln!("skipping: typst not on PATH");
        return;
    }
    let json = build_report_json(&reviewed(), None, &client_options());
    let pdf = crate::report_export::compile_pdf(&json)
        .await
        .expect("the PDF compiles");
    let text = pdf_extract::extract_text_from_mem(&pdf)
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let check_at = text
        .find(&format!("{}: 1 place", v::CHECK_POSSIBLE_FALSE_ALARM))
        .unwrap_or_else(|| panic!("the check line is printed: {text}"));
    let entries: Vec<usize> = text.match_indices("Rules: ").map(|(i, _)| i).collect();
    assert_eq!(
        entries.len(),
        2,
        "one entry for each place above the line: {text}"
    );
    assert!(
        entries[0] < check_at && check_at < entries[1],
        "urgent first, then the check list"
    );
}

/// The seam for a row-level doubt (the possible false positive flag another branch adds): the
/// list is decided once, where the line is marked, and written on the candidate. Every surface
/// reads that field, never the reasons, so a site moved onto the check list by anything that
/// decides the list reaches every surface with no change to them. Here the danger site is put on
/// the check list by hand, the way a future flag would put it, and every surface follows.
#[test]
fn every_surface_reads_the_list_from_the_candidate_and_not_from_its_reason() {
    let mut report = two_lists().into_inner();
    let tree = report.tree.as_mut().unwrap();
    for c in &mut tree.candidates {
        c.lane = EscalationLane::Check;
    }
    tree.budget.check = 2;
    let tree = report.tree.clone().unwrap();
    tree.sync_rows(&mut report.findings);
    let reviewed = apply_overlay(&report.seal(), &[], &[], &[]);
    let block = reviewed.report().ledger.escalation().unwrap().clone();
    assert_eq!((block.escalated, block.check), (2, 2));
    let package = package_of(&reviewed, &client_options());
    let e = &package.report_json.escalations;
    assert!(
        e.items.is_empty(),
        "the secret exposure follows its list, not its reason"
    );
    assert_eq!(paths(&e.check_items), vec!["src/a.rs", "src/d.rs"]);
    let problems =
        crate::release_gate::fix_report_problems(&package.report_json, &package.findings_export);
    assert!(problems.is_empty(), "{problems:?}");
}
