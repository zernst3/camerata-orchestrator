//! Round 2, part 1: one real problem is one row, and every further place of it is a sibling row.
//! A rule answered `problems` with a cited problem number adds no engine row of its own; its
//! listed places become sibling rows of the cited problem (A11: no producer writes
//! `also_locations`).

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

use crate::onboard::Finding;

fn layer_rows(rows: &[Finding]) -> Vec<&Finding> {
    rows.iter()
        .filter(|f| f.rule_id == "ARCH-LAYER-1")
        .collect()
}

fn places_of(rows: &[&Finding]) -> Vec<(String, usize)> {
    let mut v: Vec<(String, usize)> = rows.iter().map(|f| (f.path.clone(), f.line)).collect();
    v.sort();
    v
}

/// The Fix ids the rows fall in, once the report builds its tree.
fn fix_ids_of(
    rows: Vec<Finding>,
    of: impl Fn(&Finding) -> bool,
) -> std::collections::BTreeSet<String> {
    let mut report = crate::onboard::build_report(vec![REPO.to_string()], Vec::new(), 2, rows);
    report.normalize_loaded();
    let index = crate::review_tree::FixIndex::new(report.tree.as_ref(), &report.findings);
    report
        .findings
        .iter()
        .filter(|f| of(f))
        .map(|f| index.fix_id_of(f))
        .collect()
}

#[tokio::test]
async fn a_cited_problems_answer_gives_sibling_rows_not_departure_rows() {
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
    let layer = layer_rows(&rows);
    assert_eq!(
        places_of(&layer),
        vec![
            ("src/a.rs".to_string(), 2),
            ("src/a.rs".to_string(), 4),
            ("src/b.rs".to_string(), 3)
        ],
        "the model's problem and one sibling for each listed place: {rows:?}"
    );
    assert!(
        layer
            .iter()
            .all(|f| !f.detail.contains("The rest of the codebase follows")),
        "no engine departure text"
    );
    assert!(rows.iter().all(|f| f.also_locations.is_empty()), "A11");
    let sibling = layer.iter().find(|f| f.path == "src/b.rs").unwrap();
    assert!(
        sibling.snippet.starts_with("fn item_3"),
        "{}",
        sibling.snippet
    );
    assert_eq!(sibling.severity, "medium", "the problem's own rating");
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
    let layer = layer_rows(&rows);
    assert_eq!(
        places_of(&layer),
        vec![
            ("src/a.rs".to_string(), 4),
            ("src/b.rs".to_string(), 3),
            ("src/b.rs".to_string(), 4)
        ],
        "{rows:?}"
    );
    assert!(layer.iter().all(|f| !f.detail.contains("judged to follow")));
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
    let at_problem: Vec<_> = rows
        .iter()
        .filter(|f| (f.path.as_str(), f.line) == ("src/a.rs", 4))
        .collect();
    assert_eq!(at_problem.len(), 1, "{rows:?}");
    let all: Vec<&str> = std::iter::once(at_problem[0].rule_id.as_str())
        .chain(at_problem[0].also_matches.iter().map(String::as_str))
        .collect();
    assert!(
        all.contains(&"ARCH-LAYER-1") && all.contains(&"ARCH-ONE-1"),
        "{all:?}"
    );
    assert!(
        rows.iter().all(|f| f.severity != "high"),
        "no engine-made high rows: {rows:?}"
    );
}

#[tokio::test]
async fn a_problem_listing_six_places_is_six_rows_in_one_fix() {
    let mut p = problem(1, "src/a.rs", 4, "medium", "fn item_4() { run(4); }");
    p["places"] = json!([
        {"path": "src/a.rs", "line": 1},
        {"path": "src/a.rs", "line": 2},
        {"path": "src/a.rs", "line": 3},
        {"path": "src/a.rs", "line": 4},
        {"path": "src/b.rs", "line": 1},
        {"path": "src/b.rs", "line": 2},
        {"path": "src/b.rs", "line": 2},
        {"path": "src/a.rs", "line": 6}
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
    let own: Vec<Finding> = rows
        .iter()
        .filter(|f| f.rule_id == "ARCH-ONE-1")
        .cloned()
        .collect();
    assert_eq!(
        own.len(),
        7,
        "the problem and six places (its own place skipped, a repeat once): {rows:?}"
    );
    assert!(own.iter().all(|f| f.also_locations.is_empty()), "A11");
    let ids = fix_ids_of(rows.clone(), |f| f.rule_id == "ARCH-ONE-1");
    assert_eq!(ids.len(), 1, "one Fix, never six: {ids:?}");
}

#[tokio::test]
async fn a_cited_rules_unchecked_place_is_a_held_row_not_only_a_note() {
    let (rows, _, _, failed, _, extras) = run_cited(
        cited_script(
            json!([
                {"id": "repo", "count": 8, "places": ["src/a.rs:1"]},
                {"id": "direct", "count": 2, "places": ["src/a.rs:2", "src/missing.rs:3"]}
            ]),
            answer_clean("ARCH-ONE-1", "src/a.rs", 1),
            vec![json!({"n": 1, "rule": "ARCH-LAYER-1"})],
        ),
        false,
    )
    .await;
    assert!(failed.is_empty(), "{failed:?}");
    assert!(extras.notes.iter().any(|n| n.contains("src/missing.rs")));
    let held = rows
        .iter()
        .find(|f| f.path == "src/missing.rs")
        .unwrap_or_else(|| panic!("the unchecked place is a row: {rows:?}"));
    assert!(held.needs_review && !held.located);
    assert!(held.detail.contains("needs a decision"), "{}", held.detail);
}

#[test]
fn a_problems_answer_whose_cited_problem_has_no_row_keeps_its_places_as_engine_rows() {
    use super::super::super::checklist::{Effective, EffectiveAnswer, OptionCount, Place};
    let files = repo_files();
    let corpus = corpus_with_layer(false);
    let rules = layer_rules();
    let alternatives = crate::onboard::build_rule_alternatives(
        &corpus,
        rules.iter().map(|r| r.id.as_str()),
        &std::collections::HashMap::new(),
    );
    let place = |p: &str, l: usize| Place {
        path: p.into(),
        line: Some(l),
    };
    let answers = vec![vec![EffectiveAnswer {
        rule: "ARCH-LAYER-1".into(),
        state: Effective::Problems(vec![7]),
        options: vec![
            OptionCount {
                id: "repo".into(),
                count: 8,
                places: vec![place("src/a.rs", 1)],
            },
            OptionCount {
                id: "direct".into(),
                count: 2,
                places: vec![place("src/a.rs", 2), place("src/b.rs", 3)],
            },
        ],
    }]];
    let run = |rows: &[(usize, u32)]| {
        let have: std::collections::HashSet<(usize, u32)> = rows.iter().copied().collect();
        super::super::conventions::conventions_from_answers(
            REPO,
            &alternatives,
            &answers,
            &files,
            &rules,
            &corpus,
            &have,
        )
    };
    let missing = run(&[]);
    assert_eq!(
        missing.departures.len(),
        2,
        "no row to carry the places, so the engine rows stand"
    );
    assert!(missing.attached.is_empty());
    let present = run(&[(0, 7)]);
    assert!(present.departures.is_empty());
    assert_eq!(present.attached.len(), 1);
    assert_eq!(present.attached[0].sites.len(), 2);
}

#[tokio::test]
async fn a_stored_report_of_sibling_rows_reloads_with_the_same_rows() {
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
    let mut report = crate::onboard::build_report(vec![REPO.to_string()], Vec::new(), 2, rows);
    report.normalize_loaded();
    let once = report.findings.clone();
    let stored = serde_json::to_string(&report.findings).unwrap();
    let mut loaded = crate::onboard::build_report(
        vec![REPO.to_string()],
        Vec::new(),
        2,
        serde_json::from_str(&stored).unwrap(),
    );
    loaded.normalize_loaded();
    assert_eq!(loaded.findings.len(), once.len());
    let key = |f: &Finding| (f.path.clone(), f.line, f.rule_id.clone());
    let mut a: Vec<_> = once.iter().map(key).collect();
    let mut b: Vec<_> = loaded.findings.iter().map(key).collect();
    a.sort();
    b.sort();
    assert_eq!(a, b);
}
