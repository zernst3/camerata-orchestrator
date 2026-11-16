//! Defect 3: an agent problem cited to a rule with no convention is kept, never thrown out.

use super::*;

fn layer_corpus() -> camerata_rules::RuleSet {
    let mut set = camerata_rules::RuleSet::default();
    let mut layer = camerata_rules::bare_rule("ARCH-LAYER-1", "universal");
    for (id, label) in [("repo", "Repository layer"), ("direct", "Direct queries")] {
        layer.options.push(camerata_rules::RuleOption {
            id: id.into(),
            label: label.into(),
            directive: "d".into(),
            why: "w".into(),
            remediation: None,
            finding_headline: None,
            finding_detail: None,
            escalation: None,
            effort: None,
            violates: false,
        });
    }
    layer.default_option = Some("repo".into());
    layer.kind = Some(camerata_rules::RuleKind::Decision);
    set.push(layer);
    set.push(camerata_rules::bare_rule("SEC-TWO-1", "universal"));
    set
}

fn layer_rules() -> Vec<AuditRule> {
    vec![
        AuditRule::new("ARCH-LAYER-1", "Use the repository layer."),
        AuditRule::new("SEC-TWO-1", "Check the caller."),
    ]
}

/// Two pieces split five places to five on the decision rule, so it has no convention. Part one
/// also writes problem 1, cited to `cited`.
async fn split_run(cited: &[&str]) -> Vec<crate::onboard::Finding> {
    let mut opts = options();
    opts.force_pieces = Some(2);
    let citations: Vec<Value> = cited.iter().map(|r| json!({"n": 1, "rule": r})).collect();
    let a = ScriptedAgent::new(full_script(
        &[("src/a.rs", 6)],
        vec![problem(1, "src/a.rs", 5, "high", "fn item_5() { run(5); }")],
        citations,
        vec![
            json!({
                "rule": "ARCH-LAYER-1", "answer": "problems", "problems": [1],
                "options": [{"id": "repo", "count": 5, "places": ["src/a.rs:1"]}]
            }),
            answer_clean("SEC-TWO-1", "src/a.rs", 2),
        ],
    ));
    let b = ScriptedAgent::new(full_script(
        &[("src/b.rs", 4)],
        vec![],
        vec![],
        vec![
            json!({
                "rule": "ARCH-LAYER-1", "answer": "clean",
                "places": [{"path": "src/b.rs", "line": 1}],
                "options": [{"id": "direct", "count": 5, "places": ["src/b.rs:1"]}]
            }),
            answer_clean("SEC-TWO-1", "src/b.rs", 2),
        ],
    ));
    let agent = ByPiece::new(vec![
        ("src/a.rs", a),
        ("src/b.rs", b),
        (
            "joints",
            ScriptedAgent::new(vec![
                out("problems-1.json", &json!([])),
                out("leads-answers.json", &json!([])),
            ]),
        ),
    ]);
    let (rows, _, recs, failed, _, _) = run_with(
        &agent,
        &repo_files(),
        &layer_rules(),
        &layer_corpus(),
        &opts,
    )
    .await;
    assert!(failed.is_empty(), "{failed:?}");
    assert!(
        recs.iter()
            .any(|r| r.rule_id == "ARCH-LAYER-1" && r.no_convention),
        "the rule has no convention: {recs:?}"
    );
    crate::engine_discard::split_discarded(rows).1
}

#[tokio::test]
async fn a_problem_cited_only_to_a_rule_with_no_convention_is_kept_under_its_label() {
    let rows = split_run(&["ARCH-LAYER-1"]).await;
    let kept = rows
        .iter()
        .find(|f| f.path == "src/a.rs" && f.line == 5)
        .unwrap_or_else(|| panic!("the problem was thrown out: {rows:?}"));
    assert_eq!(kept.rule_id, "AI-MISSING-CHECK");
    assert_eq!(
        kinds_of(&rows, "ARCH-LAYER-1")
            .iter()
            .filter(|k| **k == "no_convention")
            .count(),
        1,
        "{rows:?}"
    );
}

#[tokio::test]
async fn a_problem_cited_to_a_no_convention_rule_moves_to_the_next_cited_rule_with_a_convention() {
    let rows = split_run(&["ARCH-LAYER-1", "SEC-TWO-1"]).await;
    let kept = rows
        .iter()
        .find(|f| f.path == "src/a.rs" && f.line == 5)
        .unwrap_or_else(|| panic!("the problem was thrown out: {rows:?}"));
    assert_eq!(kept.rule_id, "SEC-TWO-1");
    assert_eq!(
        kinds_of(&rows, "ARCH-LAYER-1")
            .iter()
            .filter(|k| **k == "no_convention")
            .count(),
        1
    );
}
