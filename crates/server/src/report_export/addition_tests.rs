//! Follow ups to single counts work order C (coordinator's list). Synthetic rows only.

use std::collections::HashMap;

use super::tests::{empty_opts, finding, report_with};
use super::*;

fn held(mut f: crate::onboard::Finding) -> crate::onboard::Finding {
    f.needs_review = true;
    f
}

#[test]
fn findings_json_summary_agrees_with_counts() {
    // Two rows at one place, one of them held: one problem, and it needs a decision.
    let rows = vec![
        finding("SYN-A-1", "src/a.rs", 5, "medium"),
        held(finding("SYN-B-1", "src/a.rs", 5, "medium")),
    ];
    let sealed = report_with(rows, vec!["SYN-A-1", "SYN-B-1"]).seal();
    let reviewed = ReviewedReport::unreviewed(&sealed, &HashMap::new());
    let opts = empty_opts();
    let json = build_report_json(&reviewed, None, &opts);
    let fe = crate::xlsx_export::build_findings_export(&reviewed, None, &json, &HashMap::new());
    let c = &fe.counts;
    assert_eq!(c.problems.needs_decision, 1, "anti-vacuity");
    assert_eq!(fe.summary.held_for_review, c.problems.needs_decision);
    assert_eq!(fe.summary.do_now, c.problems.first);
    assert_eq!(fe.summary.do_next, c.problems.next);
    assert_eq!(fe.summary.plan, c.problems.later);
    assert_eq!(fe.summary.accepted, c.problems.accepted);
    let printed = serde_json::to_value(&fe.summary).expect("json");
    for retired in ["curated_total", "open"] {
        assert!(printed.get(retired).is_none(), "{retired} is retired");
    }
}

/// A reviewed synthetic package (an accepted row, a ruled out row, a held row and two confirmed
/// rows) compiles to a PDF. Set `CAMERATA_RENDER_OUT` to a file path to keep the PDF for a look.
#[tokio::test]
async fn a_reviewed_synthetic_package_compiles_to_a_pdf() {
    if super::tests::which_typst().is_none() {
        eprintln!("skipping: typst not on PATH");
        return;
    }
    let rows = vec![
        finding("SYN-A-1", "src/a.rs", 5, "high"),
        finding("SYN-A-1", "src/b.rs", 9, "medium"),
        finding("SYN-B-1", "src/c.rs", 3, "medium"),
        held(finding("SYN-C-1", "src/d.rs", 7, "low")),
        finding("SYN-D-1", "src/e.rs", 2, "low"),
    ];
    let sealed = report_with(rows, vec!["SYN-A-1", "SYN-B-1", "SYN-C-1", "SYN-D-1"]).seal();
    let mut dispositions = HashMap::new();
    for (i, state, reason) in [
        (2usize, "Ignored", "accepted for now"),
        (4, "FalsePositive", "a constant"),
        (0, "Confirmed", ""),
    ] {
        dispositions.insert(
            finding_key(&sealed.findings[i]),
            DispositionWire {
                state: state.to_string(),
                reason: reason.to_string(),
                ..Default::default()
            },
        );
    }
    let reviewed = ReviewedReport::unreviewed(&sealed, &dispositions);
    let json = build_report_json(&reviewed, None, &empty_opts());
    assert_eq!(json.counts.problems.accepted, 1, "anti-vacuity");
    let pdf = compile_pdf(&json).await.expect("the PDF compiles");
    assert!(pdf.starts_with(b"%PDF"));
    if let Ok(out) = std::env::var("CAMERATA_RENDER_OUT") {
        std::fs::write(out, &pdf).expect("write the PDF");
    }
}
