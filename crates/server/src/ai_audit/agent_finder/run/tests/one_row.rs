//! Round 2, part 1: one real problem is one row. A rule answered `problems` with a cited problem
//! number adds no engine row of its own; its listed places ride on the cited problem.

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

/// A review where the model reports one problem (src/a.rs:4) and answers the layering rule
/// `problems: [1]` with `options`; the other rule is answered `other_answer`.
fn cited_script(options: Value, other_answer: Value, citations: Vec<Value>) -> Vec<ScriptStep> {
    full_script(
        &[("src/a.rs", 6), ("src/b.rs", 4)],
        vec![problem(
            1,
            "src/a.rs",
            4,
            "medium",
            "fn item_4() { run(4); }",
        )],
        citations,
        vec![
            other_answer,
            json!({
                "rule": "ARCH-LAYER-1", "answer": "problems", "problems": [1],
                "options": options
            }),
        ],
    )
}

async fn run_cited(script: Vec<ScriptStep>, second_violates: bool) -> FinderResult {
    let agent = ScriptedAgent::new(script);
    run_with(
        &agent,
        &repo_files(),
        &layer_rules(),
        &corpus_with_layer(second_violates),
        &options(),
    )
    .await
}

fn site_places(f: &crate::onboard::Finding) -> Vec<(String, usize)> {
    let mut v: Vec<(String, usize)> = f
        .also_locations
        .iter()
        .map(|s| (s.path.clone(), s.line))
        .collect();
    v.sort();
    v
}

#[tokio::test]
async fn a_cited_problems_answer_gives_no_departure_rows_and_the_places_ride_on_the_problem() {
    let (rows, _, _, failed, _, _) = run_cited(
        cited_script(
            json!([
                {"id": "repo", "count": 8, "places": ["src/a.rs:1"]},
                {"id": "direct", "count": 2, "places": ["src/a.rs:2", "src/b.rs:3"]}
            ]),
            answer_clean("ARCH-ONE-1", "src/a.rs", 1),
            vec![json!({"n": 1, "rule": "ARCH-LAYER-1"})],
        ),
        false,
    )
    .await;
    assert!(failed.is_empty(), "{failed:?}");
    let layer: Vec<_> = rows
        .iter()
        .filter(|f| f.rule_id == "ARCH-LAYER-1")
        .collect();
    assert_eq!(
        layer.len(),
        1,
        "the model's one problem is the only row: {rows:?}"
    );
    assert_eq!(
        site_places(layer[0]),
        vec![("src/a.rs".to_string(), 2), ("src/b.rs".to_string(), 3)]
    );
}

#[tokio::test]
async fn a_cited_problems_answer_on_a_violates_option_gives_no_judged_to_follow_row() {
    let (rows, _, recs, failed, _, _) = run_cited(
        cited_script(
            json!([
                {"id": "repo", "count": 1, "places": ["src/a.rs:1"]},
                {"id": "direct", "count": 9, "places": ["src/b.rs:3", "src/b.rs:4"]}
            ]),
            answer_clean("ARCH-ONE-1", "src/a.rs", 1),
            vec![json!({"n": 1, "rule": "ARCH-LAYER-1"})],
        ),
        true,
    )
    .await;
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(recs[0].recommended_option_id, "direct", "the record stays");
    let layer: Vec<_> = rows
        .iter()
        .filter(|f| f.rule_id == "ARCH-LAYER-1")
        .collect();
    assert_eq!(layer.len(), 1, "{rows:?}");
    assert_eq!((layer[0].path.as_str(), layer[0].line), ("src/a.rs", 4));
    assert_eq!(
        site_places(layer[0]),
        vec![("src/b.rs".to_string(), 3), ("src/b.rs".to_string(), 4)]
    );
}

#[tokio::test]
async fn one_problem_cited_to_two_rules_stays_one_row_with_the_other_rule_in_also_matches() {
    let (rows, _, _, failed, _, _) = run_cited(
        full_script(
            &[("src/a.rs", 6), ("src/b.rs", 4)],
            vec![problem(
                1,
                "src/a.rs",
                4,
                "medium",
                "fn item_4() { run(4); }",
            )],
            vec![
                json!({"n": 1, "rule": "ARCH-LAYER-1"}),
                json!({"n": 1, "rule": "ARCH-ONE-1"}),
            ],
            vec![
                json!({"rule": "ARCH-ONE-1", "answer": "problems", "problems": [1]}),
                json!({
                    "rule": "ARCH-LAYER-1", "answer": "problems", "problems": [1],
                    "options": [
                        {"id": "repo", "count": 8, "places": ["src/a.rs:1"]},
                        {"id": "direct", "count": 2, "places": ["src/b.rs:3"]}
                    ]
                }),
            ],
        ),
        false,
    )
    .await;
    assert!(failed.is_empty(), "{failed:?}");
    let defects: Vec<_> = rows
        .iter()
        .filter(|f| f.rule_id == "ARCH-LAYER-1" || f.rule_id == "ARCH-ONE-1")
        .collect();
    assert_eq!(defects.len(), 1, "{rows:?}");
    let all: Vec<&str> = std::iter::once(defects[0].rule_id.as_str())
        .chain(defects[0].also_matches.iter().map(String::as_str))
        .collect();
    assert!(
        all.contains(&"ARCH-LAYER-1") && all.contains(&"ARCH-ONE-1"),
        "{all:?}"
    );
}

#[tokio::test]
async fn a_problem_listing_other_places_carries_them_as_extra_places() {
    let mut p = problem(1, "src/a.rs", 4, "medium", "fn item_4() { run(4); }");
    p["places"] = json!([
        {"path": "src/a.rs", "line": 2},
        {"path": "src/b.rs", "line": 3, "end_line": 4}
    ]);
    let agent = ScriptedAgent::new(full_script(
        &[("src/a.rs", 6), ("src/b.rs", 4)],
        vec![p],
        vec![json!({"n": 1, "rule": "ARCH-ONE-1"})],
        vec![
            json!({"rule": "ARCH-ONE-1", "answer": "problems", "problems": [1]}),
            answer_clean("SEC-TWO-1", "src/b.rs", 1),
        ],
    ));
    let (rows, _, _, failed, _, _) = run(&agent).await;
    assert!(failed.is_empty(), "{failed:?}");
    let row = rows
        .iter()
        .find(|f| f.rule_id == "ARCH-ONE-1")
        .unwrap_or_else(|| panic!("{rows:?}"));
    assert_eq!(
        site_places(row),
        vec![("src/a.rs".to_string(), 2), ("src/b.rs".to_string(), 3)]
    );
    assert!(
        row.also_locations
            .iter()
            .all(|s| s.snippet.starts_with("fn item_")),
        "the line of each place: {:?}",
        row.also_locations
    );
}
