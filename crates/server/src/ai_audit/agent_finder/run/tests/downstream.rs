//! Build item 9 (server side): what the agent's rows are to the code that runs after the finder.

use super::*;
use crate::onboard::MergedLocation;

#[tokio::test]
async fn an_agent_problem_ruled_out_with_carried_places_goes_through_the_same_unfold_as_today() {
    let agent = ScriptedAgent::new(good_script(
        vec![problem(1, "src/a.rs", 2, "high", "fn item_2() { run(2); }")],
        vec![json!({"n": 1, "rule": "ARCH-ONE-1"})],
    ));
    let (mut rows, _, _, failed, _, _) = run(&agent).await;
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(rows.len(), 1);
    // The row carries two places absorbed by an old merge, as a stored report can.
    rows[0].also_locations = vec![
        MergedLocation::site(
            REPO,
            "src/a.rs",
            4,
            "ARCH-ONE-1",
            "fn item_4() { run(4); }",
            false,
        ),
        MergedLocation::site(
            REPO,
            "src/b.rs",
            3,
            "ARCH-ONE-1",
            "fn item_3() { run(3); }",
            false,
        ),
    ];
    let mut report = crate::onboard::build_report(vec![REPO.to_string()], Vec::new(), 2, rows);
    report.normalize_loaded();
    assert_eq!(
        report.findings.len(),
        3,
        "the carried places become problems of their own: {:?}",
        report.findings
    );
    assert!(report.findings.iter().all(|f| f.also_locations.is_empty()));
    let stage: Vec<_> = report
        .ledger
        .stages()
        .iter()
        .filter(|s| s.stage == crate::review_tree::UNFOLD_STAGE)
        .collect();
    assert_eq!(stage.len(), 1);
    assert_eq!(
        (stage[0].rows_in, stage[0].rows_out, stage[0].unaccounted),
        (1, 3, 0)
    );
}

#[tokio::test]
async fn the_rows_are_ordinary_rows_with_their_captures_and_no_second_rating_is_needed() {
    let agent = ScriptedAgent::new(good_script(
        vec![problem(
            1,
            "src/a.rs",
            2,
            "medium",
            "fn item_2() { run(2); }",
        )],
        vec![],
    ));
    let (rows, _, _, _, _, _) = run(&agent).await;
    let f = &rows[0];
    // The agent's own rating stands in for stage 1's record.
    let record = crate::ai_audit::stage1_of(f).expect("a stage 1 style record");
    assert_eq!(record.real, crate::ai_audit::Real::Yes);
    assert_eq!(record.severity.as_deref(), Some("medium"));
    // The operator-only captures are bookkeeping keys: none names an object.
    for key in [
        super::super::super::reply::FINDER_PIECE_CAPTURE,
        super::super::super::reply::FINDER_FIX_CAPTURE,
        super::super::super::reply::FINDER_SURE_CAPTURE,
    ] {
        assert!(f.captures.contains_key(key), "{key}");
        assert!(
            crate::ai_audit::BOOKKEEPING_CAPTURE_KEYS.contains(&key),
            "{key}"
        );
    }
}
