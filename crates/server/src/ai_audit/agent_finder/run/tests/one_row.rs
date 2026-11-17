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
async fn a_cited_rules_option_places_are_census_only_and_make_no_rows() {
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
        vec![("src/a.rs".to_string(), 4)],
        "only the model's own problem; option places are census: {rows:?}"
    );
    assert!(
        layer
            .iter()
            .all(|f| !f.detail.contains("The rest of the codebase follows")),
        "no engine departure text"
    );
    assert!(rows.iter().all(|f| f.also_locations.is_empty()), "A11");
}

#[tokio::test]
async fn a_cited_problems_own_places_still_become_sibling_rows_beside_the_census() {
    let mut p = problem(1, "src/a.rs", 4, "medium", "fn item_4() { run(4); }");
    p["places"] = json!([{"path": "src/b.rs", "line": 3}]);
    let script = full_script(
        &[("src/a.rs", 6), ("src/b.rs", 4)],
        vec![p],
        vec![json!({"n": 1, "rule": "ARCH-LAYER-1"})],
        vec![
            answer_clean("ARCH-ONE-1", "src/a.rs", 1),
            json!({
                "rule": "ARCH-LAYER-1", "answer": "problems", "problems": [1],
                "options": [
                    {"id": "repo", "count": 8, "places": ["src/a.rs:1"]},
                    {"id": "direct", "count": 2, "places": ["src/a.rs:2"]}
                ]
            }),
        ],
    );
    let (rows, _, _, failed, _, _) = run_cited(script, false).await;
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(
        places_of(&layer_rows(&rows)),
        vec![("src/a.rs".to_string(), 4), ("src/b.rs".to_string(), 3)],
        "{rows:?}"
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
    let layer = layer_rows(&rows);
    assert_eq!(
        places_of(&layer),
        vec![("src/a.rs".to_string(), 4)],
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
async fn a_cited_rules_unchecked_option_place_makes_no_row() {
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
    assert!(
        extras.notes.iter().any(|n| n.contains("src/missing.rs")),
        "{:?}",
        extras.notes
    );
    assert!(
        extras.notes.iter().any(|n| n.contains("src/a.rs:2")),
        "{:?}",
        extras.notes
    );
    assert!(
        rows.iter().all(|f| f.path != "src/missing.rs"),
        "census only, no row: {rows:?}"
    );
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
    let present = run(&[(0, 7)]);
    assert!(present.departures.is_empty());
    assert!(missing.notes.is_empty(), "{:?}", missing.notes);
    for place in ["src/a.rs:2", "src/b.rs:3"] {
        assert!(
            present
                .notes
                .iter()
                .any(|n| n.contains(place) && n.contains("direct")),
            "{place}: {:?}",
            present.notes
        );
    }
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

/// The rows through the real grouping and the real json/all-findings.json export: for each row,
/// (path, line, rule id, fix id).
fn exported_fix_ids(
    rows: Vec<Finding>,
    files: &[(String, String)],
) -> Vec<(String, u64, String, String)> {
    use crate::review_tree::{collapse_identical, group_semantic, SubgroupAllocator, ToolMap};
    let tools = ToolMap::builtin();
    let collapsed = collapse_identical(rows, &tools);
    let (grouped, nodes) =
        group_semantic(collapsed.rows, files, &tools, &mut SubgroupAllocator::new());
    let mut report = crate::onboard::build_report(vec![REPO.to_string()], Vec::new(), 4, grouped);
    report.tree = Some(crate::review_tree::ReviewTree {
        nodes,
        ..crate::review_tree::ReviewTree::default()
    });
    report.attach_tree();
    let sealed = report.seal();
    let reviewed = crate::report_export::ReviewedReport::unreviewed(&sealed, &Default::default());
    let exports = crate::xlsx_export::build_json_exports(
        &reviewed,
        None,
        &crate::report_export::ReportOptions::default(),
    )
    .unwrap();
    let bytes = &exports
        .iter()
        .find(|(n, _)| n == "json/all-findings.json")
        .unwrap()
        .1;
    let all: Value = serde_json::from_slice(bytes).unwrap();
    all.as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["path"].as_str().unwrap().to_string(),
                r["line"].as_u64().unwrap(),
                r["rule_id"].as_str().unwrap().to_string(),
                r["fix_id"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

#[tokio::test]
async fn a_problem_with_places_in_three_files_is_one_fix_in_the_real_export() {
    let files: Vec<(String, String)> = ["a", "b", "c", "d"]
        .iter()
        .map(|n| (format!("src/{n}.rs"), rust_file(40)))
        .collect();
    let mut first = problem(1, "src/a.rs", 4, "medium", "fn item_4() { run(4); }");
    first["places"] = json!([
        {"path": "src/a.rs", "line": 30},
        {"path": "src/b.rs", "line": 2},
        {"path": "src/c.rs", "line": 3}
    ]);
    let mut second = problem(2, "src/d.rs", 5, "medium", "fn item_5() { run(5); }");
    second["title"] = json!("Another lookup skips the layer");
    second["what"] = json!("This lookup goes round the repository layer.");
    second["fix"] = json!("Move the lookup behind the repository.");
    let agent = ScriptedAgent::new(full_script(
        &[
            ("src/a.rs", 6),
            ("src/b.rs", 6),
            ("src/c.rs", 6),
            ("src/d.rs", 6),
        ],
        vec![first, second],
        vec![
            json!({"n": 1, "rule": "ARCH-ONE-1"}),
            json!({"n": 2, "rule": "ARCH-ONE-1"}),
        ],
        vec![
            json!({"rule": "ARCH-ONE-1", "answer": "problems", "problems": [1, 2]}),
            answer_clean("SEC-TWO-1", "src/b.rs", 1),
        ],
    ));
    let (rows, _, _, failed, _, _) =
        run_with(&agent, &files, &rules(), &corpus(), &options()).await;
    assert!(failed.is_empty(), "{failed:?}");
    let exported = exported_fix_ids(rows, &files);
    let fix_of = |path: &str, line: u64| {
        exported
            .iter()
            .find(|(p, l, r, _)| p == path && *l == line && r == "ARCH-ONE-1")
            .unwrap_or_else(|| panic!("{path}:{line} in {exported:?}"))
            .3
            .clone()
    };
    let one = fix_of("src/a.rs", 4);
    assert!(!one.is_empty(), "{exported:?}");
    assert_eq!(fix_of("src/a.rs", 30), one, "{exported:?}");
    assert_eq!(fix_of("src/b.rs", 2), one, "{exported:?}");
    assert_eq!(fix_of("src/c.rs", 3), one, "{exported:?}");
    assert_ne!(
        fix_of("src/d.rs", 5),
        one,
        "another problem of the same rule in another file keeps its own Fix"
    );
}

/// A problem at src/a.rs:4 listing `places`, with the rule answered `problems` for it.
async fn run_with_places(files: &[(String, String)], places: Value) -> Vec<Finding> {
    let mut p = problem(1, "src/a.rs", 4, "medium", "fn item_4() { run(4); }");
    p["places"] = places;
    let line_counts: Vec<(&str, usize)> = files
        .iter()
        .map(|(path, text)| (path.as_str(), text.lines().count()))
        .collect();
    let agent = ScriptedAgent::new(full_script(
        &line_counts,
        vec![p],
        vec![json!({"n": 1, "rule": "ARCH-ONE-1"})],
        vec![
            json!({"rule": "ARCH-ONE-1", "answer": "problems", "problems": [1]}),
            answer_clean("SEC-TWO-1", "src/b.rs", 1),
        ],
    ));
    let (rows, _, _, failed, _, _) = run_with(&agent, files, &rules(), &corpus(), &options()).await;
    assert!(failed.is_empty(), "{failed:?}");
    rows
}

#[tokio::test]
async fn a_model_place_on_a_line_that_does_not_exist_is_a_held_row_not_an_empty_sibling() {
    let rows = run_with_places(&repo_files(), json!([{"path": "src/b.rs", "line": 9999}])).await;
    let held = rows
        .iter()
        .find(|f| f.path == "src/b.rs" && f.line == 9999)
        .unwrap_or_else(|| panic!("the place is a row: {rows:?}"));
    assert!(held.needs_review && !held.located, "{held:?}");
    assert_eq!(held.rule_id, "AI-OPEN-QUESTION");
    assert!(held.detail.contains("needs a decision"));
    assert!(
        rows.iter().all(|f| !f.snippet.is_empty()),
        "no row with an empty snippet: {rows:?}"
    );
    assert!(rows
        .iter()
        .all(|f| f.rule_id != "ARCH-ONE-1" || f.line == 4));
}

#[tokio::test]
async fn a_text_place_makes_a_sibling_row_and_a_path_only_place_one_held_row() {
    let rows = run_with_places(&repo_files(), json!(["src/b.rs:3"])).await;
    let sibling = rows
        .iter()
        .find(|f| f.path == "src/b.rs" && f.line == 3)
        .unwrap_or_else(|| panic!("the text place is a row: {rows:?}"));
    assert!(sibling.located && !sibling.needs_review, "{sibling:?}");
    let rows = run_with_places(&repo_files(), json!(["src/b.rs"])).await;
    let held: Vec<_> = rows
        .iter()
        .filter(|f| f.path == "src/b.rs" && f.rule_id == "AI-OPEN-QUESTION")
        .collect();
    assert_eq!(held.len(), 1, "{rows:?}");
    assert!(held[0].needs_review && !held[0].located, "{:?}", held[0]);
    assert!(held[0].detail.contains("needs a decision"));
}

#[tokio::test]
async fn a_place_with_no_path_makes_no_row() {
    let before = run_with_places(&repo_files(), json!([])).await;
    let rows = run_with_places(&repo_files(), json!([7, {"line": 3}, ""])).await;
    assert_eq!(rows.len(), before.len(), "{rows:?}");
}

#[tokio::test]
async fn a_model_place_on_a_blank_line_is_a_held_row() {
    let files = vec![
        ("src/a.rs".to_string(), rust_file(6)),
        (
            "src/b.rs".to_string(),
            "fn x() {}\n\nfn y() {}\n".to_string(),
        ),
    ];
    let rows = run_with_places(&files, json!([{"path": "src/b.rs", "line": 2}])).await;
    let held = rows
        .iter()
        .find(|f| f.path == "src/b.rs" && f.line == 2)
        .unwrap_or_else(|| panic!("{rows:?}"));
    assert!(held.needs_review && !held.located, "{held:?}");
    assert_eq!(held.rule_id, "AI-OPEN-QUESTION");
}

#[test]
fn when_findings_and_problems_do_not_line_up_each_listed_place_is_a_held_row() {
    use super::super::super::reply::{Problem, ProblemPlace};
    let place = |p: &str, l: usize| ProblemPlace {
        path: p.into(),
        line: l,
        end_line: None,
    };
    let problem_with = |n: u32, places: Vec<ProblemPlace>| Problem {
        n,
        path: "src/a.rs".into(),
        line: 4,
        end_line: None,
        severity: "medium".into(),
        title: "A title".into(),
        code: "c".into(),
        what: "w".into(),
        why: "y".into(),
        fix: "f".into(),
        sure: true,
        effort: "low".into(),
        label: "l".into(),
        places,
    };
    let problems = vec![
        problem_with(1, vec![place("src/a.rs", 2), place("src/b.rs", 3)]),
        problem_with(2, vec![place("src/b.rs", 1)]),
    ];
    let mut out = Vec::new();
    // No findings at all for two problems: the lengths differ.
    super::super::places_as_rows(REPO, &[], &problems, &repo_files(), &mut out);
    assert_eq!(out.len(), 3, "one held row for each listed place: {out:?}");
    assert!(out
        .iter()
        .all(|f| f.needs_review && !f.located && f.rule_id == "AI-OPEN-QUESTION"));
}

#[tokio::test]
async fn a_cited_placeholder_problem_does_not_carry_the_rules_places() {
    // The model's problem 1 is a placeholder the parser throws out. It has a row (marked), but it
    // must not stand for the rule, so the engine's own rows keep the listed places.
    let mut p = problem(1, "src/a.rs", 4, "medium", "x");
    p["title"] = json!("placeholder");
    p["what"] = json!("x");
    p["why"] = json!("");
    let agent = ScriptedAgent::new(full_script(
        &[("src/a.rs", 6), ("src/b.rs", 4)],
        vec![p],
        vec![json!({"n": 1, "rule": "ARCH-LAYER-1"})],
        vec![
            answer_clean("ARCH-ONE-1", "src/a.rs", 1),
            json!({
                "rule": "ARCH-LAYER-1", "answer": "problems", "problems": [1],
                "options": [
                    {"id": "repo", "count": 8, "places": ["src/a.rs:1"]},
                    {"id": "direct", "count": 2, "places": ["src/a.rs:2", "src/b.rs:3"]}
                ]
            }),
        ],
    ));
    let (rows, _, _, failed, _, _) = run_with(
        &agent,
        &repo_files(),
        &layer_rules(),
        &corpus_with_layer(false),
        &options(),
    )
    .await;
    assert!(failed.is_empty(), "{failed:?}");
    let live: Vec<_> = layer_rows(&rows)
        .into_iter()
        .filter(|f| !crate::engine_discard::is_discarded(f))
        .collect();
    assert_eq!(
        places_of(&live),
        vec![("src/a.rs".to_string(), 2), ("src/b.rs".to_string(), 3)],
        "the engine's departure rows stand: {rows:?}"
    );
    assert!(live
        .iter()
        .all(|f| f.detail.contains("The rest of the codebase follows")));
}
