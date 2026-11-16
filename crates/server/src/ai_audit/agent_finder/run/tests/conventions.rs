//! Build item 4: conventions from the checklist answers.

use super::*;
use camerata_rules::{RuleKind, RuleOption};

fn option(id: &str, label: &str, violates: bool) -> RuleOption {
    RuleOption {
        id: id.into(),
        label: label.into(),
        directive: format!("do it the {label} way"),
        why: "w".into(),
        remediation: None,
        finding_headline: None,
        finding_detail: None,
        escalation: None,
        effort: None,
        violates,
    }
}

fn corpus_with_layer(second_violates: bool) -> camerata_rules::RuleSet {
    let mut set = camerata_rules::RuleSet::default();
    set.push(camerata_rules::bare_rule("ARCH-ONE-1", "universal"));
    let mut layer = camerata_rules::bare_rule("ARCH-LAYER-1", "universal");
    layer.options = vec![
        option("repo", "Repository layer", false),
        option("direct", "Direct queries", second_violates),
    ];
    layer.default_option = Some("repo".into());
    layer.kind = Some(RuleKind::Decision);
    set.push(layer);
    set
}

fn layer_rules() -> Vec<AuditRule> {
    vec![
        AuditRule::new("ARCH-ONE-1", "Keep one thing in one place."),
        AuditRule::new("ARCH-LAYER-1", "Use the repository layer."),
    ]
}

/// A good review of both files whose answer for the layering rule carries `options`.
fn script_with(options: Value, problems: Vec<Value>, citations: Vec<Value>) -> Vec<ScriptStep> {
    full_script(
        &[("src/a.rs", 6), ("src/b.rs", 4)],
        problems,
        citations,
        vec![
            answer_clean("ARCH-ONE-1", "src/a.rs", 1),
            json!({
                "rule": "ARCH-LAYER-1", "answer": "clean",
                "places": [{"path": "src/a.rs", "line": 1}],
                "options": options
            }),
        ],
    )
}

async fn run_layer(script: Vec<ScriptStep>) -> FinderResult {
    let agent = ScriptedAgent::new(script);
    run_with(
        &agent,
        &repo_files(),
        &layer_rules(),
        &corpus_with_layer(false),
        &options(),
    )
    .await
}

#[tokio::test]
async fn every_place_following_one_good_option_gives_a_convention_followed_row() {
    let (rows, _, recs, failed, samples, _) = run_layer(script_with(
        json!([{"id": "repo", "count": 5, "places": ["src/a.rs:1", "src/b.rs:2"]}]),
        vec![],
        vec![],
    ))
    .await;
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].rule_id, "ARCH-LAYER-1");
    assert_eq!(recs[0].recommended_option_id, "repo");
    assert!(!recs[0].no_convention);
    assert_eq!(recs[0].currently_followed, Some(true));
    assert_eq!(recs[0].evidence.as_deref(), Some("src/a.rs:1"));
    assert_eq!(
        kinds_of(&rows, "ARCH-LAYER-1"),
        vec!["convention_observed"],
        "{rows:?}"
    );
    assert!(samples
        .iter()
        .any(|s| s.stage == "convention-observed-emission"));
}

#[tokio::test]
async fn a_split_with_no_two_thirds_majority_gives_one_no_convention_row_and_no_departures() {
    // Two and two: no option holds two thirds. An agent problem under the rule is kept, re-keyed to its label.
    let (rows, _, recs, failed, _, _) = run_layer(script_with(
        json!([
            {"id": "repo", "count": 2, "places": ["src/a.rs:1"]},
            {"id": "direct", "count": 2, "places": ["src/a.rs:2", "src/b.rs:3"]}
        ]),
        vec![problem(
            1,
            "src/a.rs",
            2,
            "medium",
            "fn item_2() { run(2); }",
        )],
        vec![json!({"n": 1, "rule": "ARCH-LAYER-1"})],
    ))
    .await;
    assert!(failed.is_empty(), "{failed:?}");
    let (thrown_out, rows) = crate::engine_discard::split_discarded(rows);
    assert_eq!(
        thrown_out.len(),
        0,
        "the problem is kept under its label, never thrown out"
    );
    assert_eq!(recs.len(), 1);
    assert!(recs[0].no_convention);
    assert_eq!(recs[0].observed_options.len(), 2);
    let under_rule: Vec<_> = rows
        .iter()
        .filter(|f| f.rule_id == "ARCH-LAYER-1")
        .collect();
    assert_eq!(under_rule.len(), 1, "one row stands for the rule: {rows:?}");
    assert_eq!(kinds_of(&rows, "ARCH-LAYER-1"), vec!["no_convention"]);
    assert!(
        rows.iter()
            .all(|f| f.kind != Some(FindingKind::Defect) || f.rule_id != "ARCH-LAYER-1"),
        "no departures"
    );
}

#[tokio::test]
async fn a_majority_good_option_with_two_listed_places_of_another_gives_two_departures() {
    let (rows, _, recs, failed, _, _) = run_layer(script_with(
        json!([
            {"id": "repo", "count": 8, "places": ["src/a.rs:1"]},
            {"id": "direct", "count": 2, "places": ["src/a.rs:2", "src/b.rs:3"]}
        ]),
        vec![],
        vec![],
    ))
    .await;
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(recs[0].recommended_option_id, "repo");
    assert!(!recs[0].no_convention);
    assert_eq!(
        kinds_of(&rows, "ARCH-LAYER-1"),
        vec!["deviation", "deviation"],
        "{rows:?}"
    );
    let mut places: Vec<(String, usize)> = rows
        .iter()
        .filter(|f| f.rule_id == "ARCH-LAYER-1")
        .map(|f| (f.path.clone(), f.line))
        .collect();
    places.sort();
    assert_eq!(
        places,
        vec![("src/a.rs".to_string(), 2), ("src/b.rs".to_string(), 3)]
    );
    for f in rows.iter().filter(|f| f.rule_id == "ARCH-LAYER-1") {
        assert!(
            f.snippet.starts_with("fn item_"),
            "the line of the place: {}",
            f.snippet
        );
        assert_eq!(f.evaluated_option_id.as_deref(), Some("repo"));
        assert!(f.detail.contains("Direct queries") && f.detail.contains("Repository layer"));
    }
    // The rule had departures, so it is not also reported as followed.
    assert!(!kinds_of(&rows, "ARCH-LAYER-1").contains(&"convention_observed"));
}

#[tokio::test]
async fn a_listed_place_that_does_not_exist_is_not_a_departure_and_is_noted() {
    let (rows, _, _, _, _, extras) = run_layer(script_with(
        json!([
            {"id": "repo", "count": 8, "places": ["src/a.rs:1"]},
            {"id": "direct", "count": 2, "places": ["src/a.rs:2", "src/missing.rs:3"]}
        ]),
        vec![],
        vec![],
    ))
    .await;
    assert_eq!(kinds_of(&rows, "ARCH-LAYER-1"), vec!["deviation"]);
    assert!(
        extras.notes.iter().any(|n| n.contains("src/missing.rs")),
        "{:?}",
        extras.notes
    );
}

#[tokio::test]
async fn a_listed_place_that_cannot_be_checked_becomes_a_problem_needing_a_decision() {
    let (rows, _, _, _, _, _) = run_layer(script_with(
        json!([
            {"id": "repo", "count": 8, "places": ["src/a.rs:1"]},
            {"id": "direct", "count": 2, "places": ["src/a.rs:2", "src/missing.rs:3"]}
        ]),
        vec![],
        vec![],
    ))
    .await;
    let held = rows
        .iter()
        .find(|f| f.path == "src/missing.rs")
        .unwrap_or_else(|| panic!("the unchecked place was only a note: {rows:?}"));
    assert!(held.needs_review);
    assert!(held.detail.contains("needs a decision"), "{}", held.detail);
    assert!(held.detail.contains("ARCH-LAYER-1"), "{}", held.detail);
}

#[tokio::test]
async fn a_majority_option_marked_violates_gives_its_defect() {
    let agent = ScriptedAgent::new(script_with(
        json!([
            {"id": "repo", "count": 1, "places": ["src/a.rs:1"]},
            {"id": "direct", "count": 9, "places": ["src/b.rs:3"]}
        ]),
        vec![],
        vec![],
    ));
    let (rows, _, recs, failed, samples, _) = run_with(
        &agent,
        &repo_files(),
        &layer_rules(),
        &corpus_with_layer(true),
        &options(),
    )
    .await;
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(recs[0].recommended_option_id, "direct");
    assert_eq!(kinds_of(&rows, "ARCH-LAYER-1"), vec!["defect"], "{rows:?}");
    let defect = rows.iter().find(|f| f.rule_id == "ARCH-LAYER-1").unwrap();
    assert_eq!(defect.severity, "high");
    assert_eq!((defect.path.as_str(), defect.line), ("src/b.rs", 3));
    assert!(samples
        .iter()
        .any(|s| s.stage == "violates-option-emission"));
}

#[tokio::test]
async fn options_are_added_across_the_pieces_of_a_repository() {
    // Piece one sees four places of "repo", piece two one place of "direct": together four of five
    // is under two thirds, but with piece one's other answer it is a majority: 5 of 6.
    let mut opts = options();
    opts.force_pieces = Some(2);
    let a = ScriptedAgent::new(full_script(
        &[("src/a.rs", 6)],
        vec![],
        vec![],
        vec![
            answer_clean("ARCH-ONE-1", "src/a.rs", 1),
            json!({"rule": "ARCH-LAYER-1", "answer": "clean",
                   "places": [{"path": "src/a.rs", "line": 1}],
                   "options": [{"id": "repo", "count": 5, "places": ["src/a.rs:1"]}]}),
        ],
    ));
    let b = ScriptedAgent::new(full_script(
        &[("src/b.rs", 4)],
        vec![],
        vec![],
        vec![
            answer_clean("ARCH-ONE-1", "src/b.rs", 1),
            json!({"rule": "ARCH-LAYER-1", "answer": "clean",
                   "places": [{"path": "src/b.rs", "line": 1}],
                   "options": [{"id": "direct", "count": 1, "places": ["src/b.rs:3"]}]}),
        ],
    ));
    let joints = ScriptedAgent::new(vec![
        out("problems-1.json", &json!([])),
        out("leads-answers.json", &json!([])),
    ]);
    let agent = ByPiece::new(vec![("src/a.rs", a), ("src/b.rs", b), ("joints", joints)]);
    let (rows, _, recs, failed, _, _) = run_with(
        &agent,
        &repo_files(),
        &layer_rules(),
        &corpus_with_layer(false),
        &opts,
    )
    .await;
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(
        recs.len(),
        1,
        "one record for the repository, not one for each piece"
    );
    assert_eq!(recs[0].recommended_option_id, "repo");
    assert_eq!(recs[0].assessed_sites, Some(6));
    assert_eq!(kinds_of(&rows, "ARCH-LAYER-1"), vec!["deviation"]);
}
