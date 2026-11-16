//! The counts object (single counts, work order A): the place rule, the four identities and the
//! scale fixture. Synthetic rows only.

use std::collections::{HashMap, HashSet};

use camerata_api_types::audit::{
    ConventionObservedKind, FindingKind, NotAssessedKind, NotAssessedReason,
};

use super::counts::{counts_of, FixCounts, OtherCounts, ProblemCounts, ReportCounts};
use super::tests::{empty_opts, finding, report_with};
use super::*;
use crate::xlsx_export::{CountKind, CountRow, CountingView};

fn counts_for(findings: Vec<crate::onboard::Finding>) -> ReportCounts {
    let sealed = report_with(findings, vec!["SYN-A-1", "SYN-B-1"]).seal();
    report_counts(
        &ReviewedReport::unreviewed(&sealed, &HashMap::new()),
        None,
        &empty_opts(),
    )
}

fn held(mut f: crate::onboard::Finding) -> crate::onboard::Finding {
    f.needs_review = true;
    f
}

#[test]
fn a_place_with_a_held_row_needs_a_decision_whatever_heads_it() {
    // One row to act on and one held row, of equal severity, at one place. Whichever comes first
    // (and so heads the place on a tie), the place needs a decision.
    for held_first in [false, true] {
        let acting = finding("SYN-A-1", "src/a.rs", 5, "medium");
        let doubtful = held(finding("SYN-B-1", "src/a.rs", 5, "medium"));
        let rows = if held_first {
            vec![doubtful, acting]
        } else {
            vec![acting, doubtful]
        };
        let c = counts_for(rows);
        assert_eq!(c.problems.total, 1, "one place, held_first={held_first}");
        assert_eq!(c.problems.needs_decision, 1, "held_first={held_first}");
        assert_eq!(c.problems.for_action, 0, "held_first={held_first}");
        assert_eq!(c.rows.problem, 2);
        assert_eq!(c.rows.folded, 1);
    }
    // Anti-vacuity: the same acting row alone is for action.
    let alone = counts_for(vec![finding("SYN-A-1", "src/a.rs", 5, "medium")]);
    assert_eq!(alone.problems.for_action, 1);
    assert_eq!(alone.problems.needs_decision, 0);
}

#[test]
fn convention_rows_never_count_as_problems_needing_a_decision() {
    let mut conv = held(finding("SYN-A-1", "src/c.rs", 1, "info"));
    conv.kind = Some(FindingKind::ConventionObserved(ConventionObservedKind {
        convention_option_id: "x".to_string(),
        convention_label: "A convention".to_string(),
    }));
    let c = counts_for(vec![conv]);
    assert_eq!(c.rows.convention, 1);
    assert_eq!(c.rows.problem, 0);
    assert_eq!(c.problems.total, 0);
    assert_eq!(c.problems.needs_decision, 0);
}

#[test]
fn problems_plus_dependency_advisories_reconcile_to_source_rows() {
    let mut conv = finding("SYN-A-1", "src/c.rs", 1, "info");
    conv.kind = Some(FindingKind::ConventionObserved(ConventionObservedKind {
        convention_option_id: "x".to_string(),
        convention_label: "A convention".to_string(),
    }));
    let mut unchecked = finding("SYN-B-1", "src/u.rs", 0, "info");
    unchecked.kind = Some(FindingKind::NotAssessed(NotAssessedKind {
        reason: NotAssessedReason::NoFilesMatched,
        reason_detail: "none".to_string(),
    }));
    let dep = finding(crate::dep_audit::DEP_AUDIT_RULE_ID, "Cargo.lock", 0, "high");
    let c = counts_for(vec![
        finding("SYN-A-1", "src/a.rs", 5, "high"),
        finding("SYN-B-1", "src/a.rs", 5, "low"),
        finding("SYN-A-1", "src/b.rs", 9, "medium"),
        conv,
        unchecked,
        dep,
    ]);
    // Two places, three problem rows (one folded), one dependency advisory, one convention row
    // and one row for a rule that could not be completed: six source rows.
    assert_eq!(c.rows.total, 6);
    assert_eq!(c.rows.dependency, 1);
    assert_eq!(c.rows.problem, 3);
    assert_eq!(c.rows.folded, 1);
    assert_eq!(c.rows.convention, 1);
    assert_eq!(c.rows.not_checked, 1);
    assert_eq!(c.rows.ruled_out, 0);
    assert_eq!(c.problems.total, 2);
    assert_eq!(c.problems.by_severity.high, 1);
    assert_eq!(c.problems.by_severity.medium, 1);
    assert_eq!(
        c.problems.by_severity.low, 0,
        "a place counts at its worst row"
    );
    assert_eq!(c.identity_failures(), Vec::<&str>::new());
}

#[test]
fn the_identity_check_names_each_identity_that_does_not_hold() {
    let ok = counts_for(vec![finding("SYN-A-1", "src/a.rs", 5, "high")]);
    assert!(ok.identity_failures().is_empty());
    let mut bad = ok.clone();
    bad.rows.total += 1;
    assert_eq!(bad.identity_failures().len(), 1, "identity 1");
    let mut bad = ok.clone();
    bad.problems.accepted += 1;
    assert_eq!(bad.identity_failures().len(), 1, "identity 2");
    let mut bad = ok.clone();
    bad.rows.folded += 1;
    assert_eq!(bad.identity_failures().len(), 1, "identity 3");
    let mut bad = ok.clone();
    bad.problems.first += 1;
    assert_eq!(bad.identity_failures().len(), 1, "identity 4");
}

#[test]
fn the_counts_ride_in_the_pdf_data_and_in_findings_json_whole() {
    let sealed = report_with(
        vec![finding("SYN-A-1", "src/a.rs", 5, "high")],
        vec!["SYN-A-1"],
    )
    .seal();
    let reviewed = ReviewedReport::unreviewed(&sealed, &HashMap::new());
    let json = build_report_json(&reviewed, None, &empty_opts());
    assert_eq!(json.counts.problems.total, 1, "anti-vacuity");
    let data: serde_json::Value = serde_json::from_str(&render_data_json(&json).unwrap()).unwrap();
    assert_eq!(data["counts"], serde_json::to_value(&json.counts).unwrap());
    let export = crate::xlsx_export::build_findings_export(
        &reviewed,
        None,
        &json,
        &empty_opts().chosen_options,
    );
    let findings = serde_json::to_value(&export).unwrap();
    assert_eq!(findings["counts"], data["counts"]);
}

// ── The scale fixture (test 13) ───────────────────────────────────────────────────────────

const TOTAL_PLACES: usize = 5_264;
const NEEDS: usize = 406;
const INFO: usize = 137;
const FIRST: usize = 222;
const NEXT: usize = 38;
const FOLDED: usize = 327;
const DEPS: usize = 2_276;
const CONVENTIONS: usize = 108;
const NOT_CHECKED: usize = 6;
const TOOL_ROWS: usize = 3_018;

fn row(i: usize, bucket: &'static str, severity: &str, kind: CountKind) -> CountRow {
    CountRow {
        repo: "acme/portal".to_string(),
        path: format!("src/f{i}.rs"),
        line: i + 1,
        severity: severity.to_string(),
        bucket,
        ruled_out: false,
        kind,
        tool_preview: false,
        escalated: false,
        rule_id: format!("SYN-{}", i % 161),
    }
}

/// A view at the size of the Greenstand run: 5,264 places over 5,591 problem rows, where the
/// number of DEFECT ROWS equals the number of PLACES by coincidence of the old counting (5,264
/// rows of one kind), so a unit mix cannot hide.
fn scale_view() -> CountingView {
    let mut rows = Vec::new();
    for i in 0..TOTAL_PLACES {
        let bucket = if i < NEEDS {
            "held"
        } else if i < NEEDS + INFO {
            "informational"
        } else if i < NEEDS + INFO + FIRST {
            "do_now"
        } else if i < NEEDS + INFO + FIRST + NEXT {
            "do_next"
        } else {
            "plan"
        };
        // Severity runs the other way round, so class and severity are independent.
        let j = TOTAL_PLACES - 1 - i;
        let severity = if j < 173 {
            "critical"
        } else if j < 173 + 117 {
            "high"
        } else if j < 173 + 117 + 3_597 {
            "medium"
        } else {
            "low"
        };
        rows.push(row(i, bucket, severity, CountKind::Problem));
        if i < FOLDED {
            // A second row at the place, same severity and bucket.
            let mut again = row(i, bucket, severity, CountKind::Problem);
            again.rule_id = "SYN-FOLDED".to_string();
            rows.push(again);
        }
    }
    for r in rows.iter_mut().take(TOOL_ROWS) {
        r.tool_preview = true;
    }
    for i in 0..CONVENTIONS {
        rows.push(row(
            10_000 + i,
            "informational",
            "info",
            CountKind::Convention,
        ));
    }
    for i in 0..NOT_CHECKED {
        rows.push(row(
            20_000 + i,
            "informational",
            "info",
            CountKind::NotChecked,
        ));
    }
    CountingView {
        rows,
        dependency_rows: DEPS,
        fix_ids_to_do: HashSet::new(),
    }
}

#[test]
fn the_counts_at_the_size_of_the_scale_run_hold_the_identities() {
    let c = counts_of(
        &scale_view(),
        FixCounts {
            total: 651,
            to_do: 618,
        },
        OtherCounts::default(),
    );
    assert_eq!(c.rows.total, 7_981);
    assert_eq!(c.rows.dependency, 2_276);
    assert_eq!(c.rows.problem, 5_591);
    assert_eq!(c.rows.folded, 327);
    assert_eq!(c.rows.convention, 108);
    assert_eq!(c.rows.not_checked, 6);
    assert_eq!(c.rows.ruled_out, 0);
    assert_eq!(c.rows.tool_preview, 3_018);
    let p: &ProblemCounts = &c.problems;
    assert_eq!(p.total, 5_264);
    assert_eq!(
        (
            p.by_severity.critical,
            p.by_severity.high,
            p.by_severity.medium,
            p.by_severity.low
        ),
        (173, 117, 3_597, 1_377)
    );
    assert_eq!(p.needs_decision, 406);
    assert_eq!(p.information_only, 137);
    assert_eq!(p.accepted, 0);
    assert_eq!(p.for_action, 4_721);
    assert_eq!((p.first, p.next, p.later), (222, 38, 4_461));
    // The two readings that were mixed: problem rows are NOT problems.
    assert_ne!(c.rows.problem, p.total);
    assert_eq!(c.fixes.to_do, 618);
    assert_eq!(c.identity_failures(), Vec::<&str>::new());
}
