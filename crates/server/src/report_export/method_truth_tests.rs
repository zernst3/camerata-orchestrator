//! Outside review items 2b and 5b: the signed PDF says only what happened. A reviewed report
//! never claims every finding was reviewed in person, and its method text describes the finder
//! that actually ran. Synthetic rows only.

use std::collections::HashMap;

use super::tests::{empty_opts, finding, report_with};
use super::*;
use crate::release_gate::xlsx_sheet_grid;
use crate::xlsx_export::vocabulary as v;

const NOT_ASSESSED_LINE: &str =
    "Team, process and roadmap were not assessed; this inspection reads the code.";

/// Three places under one rule: a critical the reviewer decided, a high and a medium nobody
/// decided (below the review budget line).
fn mixed_report() -> crate::onboard::ScanReport {
    report_with(
        vec![
            finding("SEC-NO-HARDCODED-SECRETS-1", "src/a.rs", 1, "critical"),
            finding("SEC-NO-HARDCODED-SECRETS-1", "src/b.rs", 2, "high"),
            finding("SEC-NO-HARDCODED-SECRETS-1", "src/c.rs", 3, "medium"),
        ],
        vec!["SEC-NO-HARDCODED-SECRETS-1"],
    )
}

fn decided(report: &crate::onboard::ScanReport, line: usize) -> HashMap<String, DispositionWire> {
    let row = report.findings.iter().find(|f| f.line == line).unwrap();
    HashMap::from([(
        finding_key(row),
        DispositionWire {
            state: "Confirmed".to_string(),
            reason: "read it in person".to_string(),
            ..DispositionWire::default()
        },
    )])
}

fn reviewed_cells(xlsx: &[u8]) -> usize {
    let grid = xlsx_sheet_grid(xlsx, v::ALL_PROBLEMS).expect("All problems");
    let at = grid
        .iter()
        .position(|r| r.iter().any(|c| c == v::H_REVIEWED_IN_PERSON))
        .expect("the header row");
    let col = grid[at]
        .iter()
        .position(|c| c == v::H_REVIEWED_IN_PERSON)
        .unwrap();
    grid[at + 1..]
        .iter()
        .filter(|r| r.get(col).is_some_and(|c| !c.is_empty()))
        .count()
}

#[test]
fn a_reviewed_report_with_places_below_the_line_never_says_every_advisory_finding_was_reviewed() {
    let report = mixed_report();
    let dispositions = decided(&report, 1);
    let sealed = report.seal();
    let reviewed = ReviewedReport::unreviewed(&sealed, &dispositions);
    let json = build_report_json(&reviewed, None, &empty_opts());
    assert_eq!(json.review_state, ReviewState::Reviewed);
    let note = &json.methodology.ai_tier_note;
    let lower = note.to_ascii_lowercase();
    assert!(!lower.contains("every advisory finding"), "{note}");
    assert!(!lower.contains("dispositioned"), "{note}");
    // The count is the workbook's: one place reviewed in person, two left to the engine.
    let xlsx = crate::xlsx_export::build_workbook(&reviewed, None, &empty_opts()).unwrap();
    let in_person = reviewed_cells(&xlsx);
    assert_eq!(in_person, 1);
    assert!(
        note.contains(&format!("I reviewed {in_person} place in person")),
        "{note}"
    );
    assert!(
        note.contains(
            "including every critical problem and every problem the review marked unsure"
        ),
        "{note}"
    );
    assert!(
        note.contains("checked by the engine against the code"),
        "{note}"
    );
    assert!(note.contains(v::ALL_PROBLEMS), "{note}");
    assert!(note.contains(v::H_REVIEWED_IN_PERSON), "{note}");
}

#[test]
fn the_every_critical_clause_is_printed_only_when_the_gate_guarantees_it() {
    // The reviewer decided the high; the critical is still undecided, so the gate would refuse
    // and the sentence must not claim it.
    let report = mixed_report();
    let dispositions = decided(&report, 2);
    let reviewed = ReviewedReport::unreviewed(&report.seal(), &dispositions);
    let json = build_report_json(&reviewed, None, &empty_opts());
    let note = &json.methodology.ai_tier_note;
    assert!(note.contains("I reviewed 1 place in person"), "{note}");
    assert!(!note.contains("including every critical"), "{note}");
}

#[test]
fn a_reviewed_report_with_no_place_reviewed_says_so_in_words() {
    // Only a ruled-out row was decided, so no place was reviewed in person.
    let mut report = mixed_report();
    report
        .findings
        .push(finding("SEC-NO-HARDCODED-SECRETS-1", "src/d.rs", 4, "low"));
    let row = report.findings.last().unwrap().clone();
    let dispositions = HashMap::from([(
        finding_key(&row),
        DispositionWire {
            state: "FalsePositive".to_string(),
            reason: "test data".to_string(),
            ..DispositionWire::default()
        },
    )]);
    let reviewed = ReviewedReport::unreviewed(&report.seal(), &dispositions);
    let json = build_report_json(&reviewed, None, &empty_opts());
    let note = &json.methodology.ai_tier_note;
    assert!(
        !note.to_ascii_lowercase().contains("every advisory finding"),
        "{note}"
    );
    assert!(!note.contains("I reviewed 0"), "{note}");
    assert!(
        note.contains("I did not review any place in person"),
        "{note}"
    );
}

fn agent_run() -> crate::onboard::ScanReport {
    let mut report = mixed_report();
    report.ledger.record_finder_coverage("owner/repo", []);
    report
}

#[test]
fn an_agent_finder_run_describes_what_the_agent_finder_did() {
    let report = agent_run();
    let dispositions = decided(&report, 1);
    let reviewed = ReviewedReport::unreviewed(&report.seal(), &dispositions);
    let json = build_report_json(&reviewed, None, &empty_opts());
    let m = &json.methodology;
    let all = format!(
        "{}\n{}\n{}",
        m.deterministic_note,
        m.ai_tier_note,
        m.not_done.join("\n")
    );
    for banned in [
        "two-tier",
        "advisory tier",
        "advisory language-model",
        "calibrated language-model review",
        "SAST",
    ] {
        assert!(!all.contains(banned), "{banned:?} in {all}");
    }
    assert!(
        m.deterministic_note.contains("deterministic"),
        "{}",
        m.deterministic_note
    );
    assert!(
        m.ai_tier_note
            .contains("reads every in-scope file against the rules that apply"),
        "{}",
        m.ai_tier_note
    );
    assert!(
        m.ai_tier_note.contains("record of the files it opened"),
        "{}",
        m.ai_tier_note
    );
    assert!(
        m.ai_tier_note.contains("group review"),
        "{}",
        m.ai_tier_note
    );
    assert!(
        m.ai_tier_note.contains("I reviewed 1 place in person"),
        "{}",
        m.ai_tier_note
    );
    assert!(
        m.not_done.iter().any(|l| l == NOT_ASSESSED_LINE),
        "{:?}",
        m.not_done
    );
}

#[test]
fn an_engine_finder_run_keeps_its_method_text_and_gains_only_the_not_assessed_line() {
    let report = mixed_report();
    assert!(report.ledger.finder_coverage().is_empty());
    let json = build_report_json(
        &ReviewedReport::unreviewed(&report.seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    let m = &json.methodology;
    assert!(
        m.deterministic_note
            .starts_with("Camerata runs a two-tier engine."),
        "{}",
        m.deterministic_note
    );
    assert!(m.deterministic_note.contains("SAST"));
    assert!(
        m.ai_tier_note.contains("calibrated language-model review"),
        "{}",
        m.ai_tier_note
    );
    assert!(
        m.not_done.iter().any(|l| l == NOT_ASSESSED_LINE),
        "{:?}",
        m.not_done
    );
}

#[test]
fn an_agent_finder_run_says_the_model_did_not_read_a_repo_it_did_not_read() {
    let mut report = agent_run();
    report.provenance.audit_model = None;
    let json = build_report_json(
        &ReviewedReport::unreviewed(&report.seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    let note = &json.methodology.ai_tier_note;
    assert!(!note.contains("tier"), "{note}");
    assert!(note.contains("did not read the code of 1 repo"), "{note}");
}

#[test]
fn the_reconciling_line_does_not_say_the_whole_export_was_reviewed() {
    let report = mixed_report();
    let dispositions = decided(&report, 1);
    let json = build_report_json(
        &ReviewedReport::unreviewed(&report.seal(), &dispositions),
        None,
        &empty_opts(),
    );
    let line = &json.methodology.reconciliation;
    assert!(!line.contains("Reviewed by a human reviewer"), "{line}");
    assert!(
        line.contains("A person has ruled on part of this export"),
        "{line}"
    );
}

#[test]
fn the_reconciling_line_branches_on_the_reviewed_count() {
    let report = mixed_report();
    // A decision on a ruled-out row only: reviewed state, no place read in person.
    let mut r = report.clone();
    r.findings
        .push(finding("SEC-NO-HARDCODED-SECRETS-1", "src/d.rs", 4, "low"));
    let fp = r.findings.last().unwrap().clone();
    let none = HashMap::from([(
        finding_key(&fp),
        DispositionWire {
            state: "FalsePositive".to_string(),
            reason: "x".to_string(),
            ..DispositionWire::default()
        },
    )]);
    let json = build_report_json(
        &ReviewedReport::unreviewed(&r.seal(), &none),
        None,
        &empty_opts(),
    );
    let line = &json.methodology.reconciliation;
    assert!(line.contains("No place was reviewed in person"), "{line}");
    assert!(!line.contains("A person has ruled"), "{line}");
    let some = build_report_json(
        &ReviewedReport::unreviewed(&report.clone().seal(), &decided(&report, 1)),
        None,
        &empty_opts(),
    );
    assert!(some
        .methodology
        .reconciliation
        .contains("A person has ruled on part"));
}

#[test]
fn the_confidence_glossary_entry_names_every_cause_of_low() {
    let e = crate::xlsx_export::vocabulary::GLOSSARY_ENTRIES
        .iter()
        .find(|e| e.term == "Confidence")
        .unwrap();
    for cause in [
        "marked the problem unsure",
        "no published standard backs it",
        "convention to consider",
    ] {
        assert!(e.meaning.contains(cause), "{}", e.meaning);
    }
}

#[test]
fn the_severity_paragraph_follows_the_finder_that_ran() {
    let engine = build_report_json(
        &ReviewedReport::unreviewed(&mixed_report().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert!(engine
        .methodology
        .severity_scale_note
        .contains("advisory findings"));
    let agent = build_report_json(
        &ReviewedReport::unreviewed(&agent_run().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    let note = &agent.methodology.severity_scale_note;
    assert!(
        !note.contains("advisory") && !note.contains("calibrated"),
        "{note}"
    );
}
