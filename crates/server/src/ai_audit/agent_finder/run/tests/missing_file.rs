//! #546: a finder row on a file the repo does not have never reaches the report unexamined.
//!
//! By design the agent finder cannot emit a row of its own on a file outside the files it was
//! given: a problem on one is a lead, and a lead nothing settles becomes an `AI-OPEN-QUESTION` row
//! with `needs_review`, which the release gate's needs-decision check holds. This proves that
//! path for a planted missing file, and that the evidence check now also marks the row, so the
//! two holds agree.

use super::joints::{parts, two_part_run};
use super::*;

#[tokio::test]
async fn a_planted_missing_file_problem_becomes_a_held_open_question_row() {
    // Part two writes a problem about src/gone.rs, which no piece owns and the repo does not have.
    let (a, b) = parts(
        vec![],
        vec![problem(
            1,
            "src/gone.rs",
            3,
            "high",
            "fn item_3() { run(3); }",
        )],
    );
    let joints = ScriptedAgent::new(vec![out("problems-1.json", &json!([]))]);
    let (_, mut rows, failed, _) = two_part_run(a, b, joints).await;
    assert!(failed.is_empty(), "{failed:?}");
    let held = rows
        .iter()
        .find(|f| f.path == "src/gone.rs")
        .unwrap_or_else(|| panic!("the lead was kept as a row: {rows:?}"));
    assert_eq!(held.rule_id, "AI-OPEN-QUESTION");
    assert!(
        held.needs_review,
        "held for a decision by the needs-decision check"
    );
    // Then the scan's evidence check: the same row is marked, with the missing file named.
    crate::onboard::evidence_check::verify_evidence(&mut rows, &repo_files(), None);
    let held = rows.iter().find(|f| f.path == "src/gone.rs").unwrap();
    let why = held
        .captures
        .get(crate::onboard::evidence_check::EVIDENCE_UNVERIFIED_CAPTURE)
        .unwrap_or_else(|| panic!("marked: {held:?}"));
    assert!(why.contains("src/gone.rs"), "{why}");
}
