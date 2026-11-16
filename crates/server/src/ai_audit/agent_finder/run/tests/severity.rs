//! Build item 5: severity. The agent's rating goes through the code a calibration verdict goes
//! through, then the wording floors and ceilings, and the rule bounds at the call site.

use super::*;
use crate::onboard::Finding;

fn sure(mut p: Value, sure: &str) -> Value {
    p["sure"] = json!(sure);
    p
}

fn labelled(mut p: Value, label: &str) -> Value {
    p["label"] = json!(label);
    p
}

fn worded(mut p: Value, what: &str) -> Value {
    p["what"] = json!(what);
    p["why"] = json!("It matters.");
    p
}

fn bounded_corpus() -> camerata_rules::RuleSet {
    let mut set = camerata_rules::RuleSet::default();
    let mut floor = camerata_rules::bare_rule("ARCH-ONE-1", "universal");
    floor.min_severity = Some("high".into());
    set.push(floor);
    let mut cap = camerata_rules::bare_rule("SEC-TWO-1", "universal");
    cap.max_severity = Some("medium".into());
    set.push(cap);
    set
}

async fn rows_for(files: Files, problems: Vec<Value>, citations: Vec<Value>) -> Vec<Finding> {
    let paths: Vec<(String, usize)> = files
        .iter()
        .map(|(p, t)| (p.clone(), t.lines().count()))
        .collect();
    let refs: Vec<(&str, usize)> = paths.iter().map(|(p, n)| (p.as_str(), *n)).collect();
    let first = files[0].0.clone();
    let script = full_script(
        &refs,
        problems,
        citations,
        vec![
            answer_clean("ARCH-ONE-1", &first, 1),
            answer_clean("SEC-TWO-1", &first, 1),
        ],
    );
    let agent = ScriptedAgent::new(script);
    let (rows, _, _, failed, _, _) =
        run_with(&agent, &files, &rules(), &bounded_corpus(), &options()).await;
    assert!(failed.is_empty(), "{failed:?}");
    crate::engine_discard::split_discarded(rows).1
}

#[tokio::test]
async fn an_unsure_problem_goes_under_needs_a_decision_and_a_sure_one_does_not() {
    let rows = rows_for(
        repo_files(),
        vec![
            sure(
                problem(1, "src/a.rs", 2, "medium", "fn item_2() { run(2); }"),
                "unsure",
            ),
            problem(2, "src/a.rs", 5, "medium", "fn item_5() { run(5); }"),
        ],
        vec![],
    )
    .await;
    let unsure = rows.iter().find(|f| f.line == 2).unwrap();
    let sure = rows.iter().find(|f| f.line == 5).unwrap();
    assert!(unsure.needs_review, "unsure is held for a decision");
    assert_eq!(unsure.confidence.as_deref(), Some("needs-review"));
    assert!(!sure.needs_review);
    assert_eq!(sure.confidence.as_deref(), Some("high"));
    assert_eq!(
        sure.effort.as_deref(),
        Some("low"),
        "the agent's effort rides on the row"
    );
    let record = crate::ai_audit::stage1_of(sure).expect("the rating is recorded");
    assert!(
        record.reason.contains("no second reviewer"),
        "{}",
        record.reason
    );
}

#[tokio::test]
async fn a_category_named_by_the_label_is_kept_and_a_free_label_is_not_one() {
    let rows = rows_for(
        repo_files(),
        vec![
            labelled(
                problem(1, "src/a.rs", 2, "medium", "fn item_2() { run(2); }"),
                "authorization",
            ),
            labelled(
                problem(2, "src/a.rs", 5, "medium", "fn item_5() { run(5); }"),
                "odd shape",
            ),
        ],
        vec![],
    )
    .await;
    assert_eq!(
        rows.iter()
            .find(|f| f.line == 2)
            .unwrap()
            .category
            .as_deref(),
        Some("authorization")
    );
    assert_eq!(rows.iter().find(|f| f.line == 5).unwrap().category, None);
}

#[tokio::test]
async fn the_rule_bounds_raise_a_low_rating_and_lower_a_critical_one_after_the_unchanged_call() {
    let mut rows = rows_for(
        repo_files(),
        vec![
            problem(1, "src/a.rs", 2, "low", "fn item_2() { run(2); }"),
            problem(2, "src/a.rs", 5, "critical", "fn item_5() { run(5); }"),
        ],
        vec![
            json!({"n": 1, "rule": "ARCH-ONE-1"}),
            json!({"n": 2, "rule": "SEC-TWO-1"}),
        ],
    )
    .await;
    crate::severity_bounds::apply_scan_severity_bounds(&mut rows, Some(&bounded_corpus()));
    let raised = rows.iter().find(|f| f.line == 2).unwrap();
    assert_eq!(raised.rule_id, "ARCH-ONE-1");
    assert_eq!(
        raised.severity, "high",
        "a minimum of high raises a low rating"
    );
    assert!(
        !crate::severity_bounds::recorded_adjustments(raised).is_empty(),
        "the adjustment is noted"
    );
    let lowered = rows.iter().find(|f| f.line == 5).unwrap();
    assert_eq!(lowered.rule_id, "SEC-TWO-1");
    assert_eq!(
        lowered.severity, "medium",
        "a maximum of medium lowers a critical rating"
    );
    assert!(!crate::severity_bounds::recorded_adjustments(lowered).is_empty());
}

#[tokio::test]
async fn the_rule_the_bounds_read_is_the_strictest_cited_one_in_either_citation_order() {
    for citations in [
        vec![
            json!({"n": 1, "rule": "SEC-TWO-1"}),
            json!({"n": 1, "rule": "ARCH-ONE-1"}),
        ],
        vec![
            json!({"n": 1, "rule": "ARCH-ONE-1"}),
            json!({"n": 1, "rule": "SEC-TWO-1"}),
        ],
    ] {
        let rows = rows_for(
            repo_files(),
            vec![problem(1, "src/a.rs", 2, "low", "fn item_2() { run(2); }")],
            citations,
        )
        .await;
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].rule_id, "ARCH-ONE-1",
            "the rule with a minimum comes first"
        );
    }
}

// ── The three A1 fixes act on agent problems ───────────────────────────────────────────────

#[tokio::test]
async fn a_problem_in_a_test_only_place_is_held_at_low_and_a_secret_keeps_its_rating() {
    let files = vec![
        ("src/a.rs".to_string(), rust_file(6)),
        ("tests/auth_test.rs".to_string(), rust_file(6)),
    ];
    let rows = rows_for(
        files,
        vec![
            problem(
                1,
                "tests/auth_test.rs",
                2,
                "high",
                "fn item_2() { run(2); }",
            ),
            labelled(
                problem(
                    2,
                    "tests/auth_test.rs",
                    4,
                    "high",
                    "fn item_4() { run(4); }",
                ),
                "secret exposure",
            ),
            problem(3, "src/a.rs", 3, "high", "fn item_3() { run(3); }"),
        ],
        vec![],
    )
    .await;
    let plain = rows.iter().find(|f| f.line == 2).unwrap();
    assert_eq!(plain.severity, "low");
    assert!(
        plain.in_test && plain.needs_review,
        "held, marked as test scope"
    );
    let secret = rows.iter().find(|f| f.line == 4).unwrap();
    assert_eq!(secret.severity, "high", "the secret class is exempt");
    assert!(!secret.in_test);
    let production = rows.iter().find(|f| f.line == 3).unwrap();
    assert_eq!(production.severity, "high");
}

#[tokio::test]
async fn a_security_fault_under_a_departure_is_a_defect_not_a_deviation() {
    // The layering rule has a convention with one listed departure; a separate problem under the
    // same rule states a cross-tenant read, an objective fault.
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
    let rules = vec![AuditRule::new("ARCH-LAYER-1", "Use the repository layer.")];
    let script = full_script(
        &[("src/a.rs", 6), ("src/b.rs", 4)],
        vec![worded(
            problem(1, "src/a.rs", 5, "high", "fn item_5() { run(5); }"),
            "This route lets one tenant read another tenant's records, a cross-tenant read.",
        )],
        vec![json!({"n": 1, "rule": "ARCH-LAYER-1"})],
        vec![json!({
            "rule": "ARCH-LAYER-1", "answer": "problems", "problems": [1],
            "options": [
                {"id": "repo", "count": 9, "places": ["src/a.rs:1"]},
                {"id": "direct", "count": 1, "places": ["src/b.rs:3"]}
            ]
        })],
    );
    let agent = ScriptedAgent::new(script);
    let (rows, _, _, failed, _, _) =
        run_with(&agent, &repo_files(), &rules, &set, &options()).await;
    assert!(failed.is_empty(), "{failed:?}");
    let fault = rows.iter().find(|f| f.line == 5).unwrap();
    assert!(
        !matches!(fault.kind, Some(FindingKind::Deviation(_))),
        "an objective fault stays a defect: {:?}",
        fault.kind
    );
    let departure = rows
        .iter()
        .find(|f| f.path == "src/b.rs" && f.line == 3)
        .unwrap();
    assert!(matches!(departure.kind, Some(FindingKind::Deviation(_))));
}

#[tokio::test]
async fn a_medium_uncited_agent_problem_is_held_and_a_low_one_is_informational() {
    let rows = rows_for(
        repo_files(),
        vec![
            problem(1, "src/a.rs", 2, "medium", "fn item_2() { run(2); }"),
            problem(2, "src/a.rs", 5, "low", "fn item_5() { run(5); }"),
        ],
        vec![],
    )
    .await;
    let bucket = |f: &Finding| {
        crate::report_export::effective_bucket(
            f,
            crate::report_export::Disposition::Unresolved,
            &f.severity,
            None,
            0,
            None,
        )
    };
    assert_eq!(bucket(rows.iter().find(|f| f.line == 2).unwrap()), "held");
    assert_eq!(
        bucket(rows.iter().find(|f| f.line == 5).unwrap()),
        "informational"
    );
}

#[tokio::test]
async fn pin_what_the_code_does_today_with_an_unauthenticated_access_problem_ruling_r8() {
    // R-8 says unauthenticated access is high by default and critical only when it reaches
    // sensitive data or allows a write. The wording floor in `severity_rules.rs` still reads
    // "always Critical", so today an agent problem rated high whose text says the route needs no
    // authentication is raised to critical. This pins that, and changes nothing.
    let rows = rows_for(
        repo_files(),
        vec![worded(
            problem(1, "src/a.rs", 2, "high", "fn item_2() { run(2); }"),
            "The route is reachable without authentication and returns a public page.",
        )],
        vec![],
    )
    .await;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].severity, "critical",
        "today's code: the wording floor raises it (R-8 is not applied here)"
    );
}
