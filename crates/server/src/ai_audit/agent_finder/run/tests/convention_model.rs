//! The convention model on the scan side (design 2026-11-20, section 11, tests 1 to 6): the
//! finder judges every rule against one directive, and a finder problem at a place that follows
//! the standard is not a departure.

use super::*;
use camerata_api_types::audit::{OptionBasis, PlaceStatus, ProposalBasis, StandardSource};
use camerata_rules::{RuleKind, RuleOption};

const PAR: &str = "ARCH-PARALLEL-INDEPENDENT-1";

fn option(id: &str, label: &str) -> RuleOption {
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
        violates: false,
    }
}

fn decision(id: &str, opts: &[(&str, &str)], default: &str) -> camerata_rules::Rule {
    let mut r = camerata_rules::bare_rule(id, "universal");
    r.options = opts.iter().map(|(i, l)| option(i, l)).collect();
    r.default_option = Some(default.into());
    r.kind = Some(RuleKind::Decision);
    r
}

fn hearth_corpus() -> camerata_rules::RuleSet {
    let mut set = camerata_rules::RuleSet::default();
    set.push(decision(
        PAR,
        &[
            ("par", "Run in parallel"),
            ("seq", "Sequential awaits by default"),
        ],
        "par",
    ));
    set.push(camerata_rules::bare_rule("SPIRIT-OPTIMIZE-1", "universal"));
    set.push(camerata_rules::bare_rule("SQL-DB-NPLUSONE-1", "universal"));
    set
}

fn hearth_rules() -> Vec<AuditRule> {
    vec![
        AuditRule::new(PAR, "Run independent awaits in parallel."),
        AuditRule::new("SPIRIT-OPTIMIZE-1", "Optimize by default."),
        AuditRule::new("SQL-DB-NPLUSONE-1", "No N+1 queries."),
    ]
}

const HEARTH_PATHS: [&str; 4] = [
    "app/listings/page.tsx",
    "app/admin/page.tsx",
    "app/bookings/[id]/page.tsx",
    "app/api/checkout/route.ts",
];

fn hearth_files() -> Vec<(String, String)> {
    HEARTH_PATHS
        .iter()
        .map(|p| ((*p).to_string(), rust_file(30)))
        .collect()
}

fn hearth_script(
    par_answer: Value,
    problems: Vec<Value>,
    citations: Vec<Value>,
    with_problems: bool,
) -> Vec<ScriptStep> {
    let paths: Vec<(&str, usize)> = HEARTH_PATHS.iter().map(|p| (*p, 30)).collect();
    full_script(
        &paths,
        problems,
        citations,
        vec![
            par_answer,
            if with_problems {
                json!({"rule": "SPIRIT-OPTIMIZE-1", "answer": "problems", "problems": [1, 2]})
            } else {
                answer_clean("SPIRIT-OPTIMIZE-1", "app/admin/page.tsx", 1)
            },
            if with_problems {
                json!({"rule": "SQL-DB-NPLUSONE-1", "answer": "problems", "problems": [1]})
            } else {
                answer_clean("SQL-DB-NPLUSONE-1", "app/admin/page.tsx", 1)
            },
        ],
    )
}

fn hearth_problems() -> (Vec<Value>, Vec<Value>) {
    let p1 = problem(
        1,
        "app/listings/page.tsx",
        17,
        "medium",
        "fn item_17() { run(17); }",
    );
    let mut p2 = problem(
        2,
        "app/admin/page.tsx",
        27,
        "medium",
        "fn item_27() { run(27); }",
    );
    p2["places"] = json!([
        {"path": "app/bookings/[id]/page.tsx", "line": 24},
        {"path": "app/api/checkout/route.ts", "line": 16}
    ]);
    let citations = vec![
        json!({"n": 1, "rule": PAR}),
        json!({"n": 1, "rule": "SPIRIT-OPTIMIZE-1"}),
        json!({"n": 1, "rule": "SQL-DB-NPLUSONE-1"}),
        json!({"n": 2, "rule": PAR}),
        json!({"n": 2, "rule": "SPIRIT-OPTIMIZE-1"}),
    ];
    (vec![p1, p2], citations)
}

async fn hearth(par_answer: Value, with_problems: bool) -> FinderResult {
    let (problems, citations) = if with_problems {
        hearth_problems()
    } else {
        (vec![], vec![])
    };
    let agent = ScriptedAgent::new(hearth_script(
        par_answer,
        problems,
        citations,
        with_problems,
    ));
    run_with(
        &agent,
        &hearth_files(),
        &hearth_rules(),
        &hearth_corpus(),
        &options(),
    )
    .await
}

fn finder_rows(rows: &[crate::onboard::Finding]) -> Vec<&crate::onboard::Finding> {
    rows.iter()
        .filter(|f| {
            f.captures
                .contains_key(crate::ai_audit::agent_finder::reply::FINDER_PROBLEM_CAPTURE)
        })
        .collect()
}

/// Test 1: the finder judged "run in parallel" (the default); four places of "sequential" are
/// what it reported. They follow the observed convention, so none is a departure from it.
#[tokio::test]
async fn hearth_parallel_rule_reports_are_not_departures_from_the_observed_sequential_convention() {
    let (rows, _, recs, failed, _, extras) = hearth(
        json!({
            "rule": PAR, "answer": "problems", "problems": [1, 2],
            "options": [
                {"id": "seq", "count": 4, "places": []},
                {"id": "par", "count": 1, "places": []}
            ]
        }),
        true,
    )
    .await;
    assert!(failed.is_empty(), "{failed:?}");
    let finder = finder_rows(&rows);
    assert_eq!(finder.len(), 4, "{rows:?}");
    for f in &finder {
        assert!(
            !f.rule_id.eq_ignore_ascii_case(PAR),
            "keyed to the rule: {f:?}"
        );
        assert!(
            !matches!(f.kind, Some(FindingKind::Deviation(_))),
            "{:?}",
            f.kind
        );
        assert!(!f
            .detail
            .contains("deviate from \"Sequential awaits by default\""));
    }
    assert!(
        kinds_of(&rows, PAR).iter().all(|k| *k != "deviation"),
        "{rows:?}"
    );
    assert_eq!(recs[0].recommended_option_id, "seq");
    let c = extras
        .conventions
        .iter()
        .find(|c| c.rule_id == PAR)
        .unwrap();
    assert_eq!(c.observed_option_id.as_deref(), Some("seq"));
    assert_eq!(c.assessed_places, Some(5));
    assert_eq!(c.standard.source, StandardSource::Observed);
    assert_eq!(
        c.standard.differs_from_default.as_deref(),
        Some("Run in parallel")
    );
    assert_eq!(c.finder_rows.len(), 4, "{:?}", c.finder_rows);
    assert!(c
        .finder_rows
        .iter()
        .all(|r| r.option_basis == OptionBasis::OnlyOther && r.status == PlaceStatus::Conforms));
    // The ledger parts add up: departing, conforming and unknown.
    let departs = c
        .finder_rows
        .iter()
        .filter(|r| r.status == PlaceStatus::Departs)
        .count();
    let conforms = c
        .finder_rows
        .iter()
        .filter(|r| {
            matches!(
                r.status,
                PlaceStatus::Conforms | PlaceStatus::FollowsRuleDefault
            )
        })
        .count();
    assert_eq!(departs + conforms, 4);
    let block = crate::scan_ledger::ConventionsBlock::from_records(&extras.conventions, &rows);
    assert!(block.reconciles(), "{block:?}");
    let entry = block.rules.iter().find(|r| r.rule_id == PAR).unwrap();
    assert_eq!(
        (entry.finder_rows, entry.conforming, entry.departures),
        (4, 4, 0)
    );
}

/// Test 2: a clean answer with listed places. The one place of the rule's default is not a
/// departure while the standard is only observed (A2).
#[tokio::test]
async fn a_place_of_the_rule_default_is_not_a_departure_from_an_observed_standard() {
    let (rows, _, _, failed, _, extras) = hearth(
        json!({
            "rule": PAR, "answer": "clean",
            "places": [{"path": "app/admin/page.tsx", "line": 1}],
            "options": [
                {"id": "seq", "count": 4, "places": [
                    "app/admin/page.tsx:2", "app/admin/page.tsx:3",
                    "app/listings/page.tsx:4", "app/listings/page.tsx:5"]},
                {"id": "par", "count": 1, "places": ["app/api/checkout/route.ts:6"]}
            ]
        }),
        false,
    )
    .await;
    assert!(failed.is_empty(), "{failed:?}");
    assert!(
        rows.iter().all(|f| f.path != "app/api/checkout/route.ts"),
        "{rows:?}"
    );
    assert_eq!(
        kinds_of(&rows, PAR),
        vec!["convention_observed"],
        "{rows:?}"
    );
    let c = extras
        .conventions
        .iter()
        .find(|c| c.rule_id == PAR)
        .unwrap();
    let par = c.options.iter().find(|o| o.option_id == "par").unwrap();
    assert_eq!(par.places[0].status, PlaceStatus::FollowsRuleDefault);
    assert!(par.places[0].checked && !par.places[0].text.is_empty());
}

fn env_corpus() -> camerata_rules::RuleSet {
    let mut set = camerata_rules::RuleSet::default();
    set.push(decision(
        "ARCH-TRIGGER-ENV-1",
        &[("git-ref", "Git ref"), ("manual", "Manual input")],
        "git-ref",
    ));
    set
}

async fn env_run(chosen: Option<(&str, &str)>) -> FinderResult {
    let mut opts = options();
    if let Some((rule, option)) = chosen {
        opts.chosen_options.insert(rule.into(), option.into());
    }
    let agent = ScriptedAgent::new(full_script(
        &[("src/a.rs", 6)],
        vec![],
        vec![],
        vec![json!({
            "rule": "ARCH-TRIGGER-ENV-1", "answer": "clean",
            "places": [{"path": "src/a.rs", "line": 1}],
            "options": [
                {"id": "git-ref", "count": 1, "places": ["src/a.rs:2"]},
                {"id": "manual", "count": 1, "places": ["src/a.rs:3"]}
            ]
        })],
    ));
    run_with(
        &agent,
        &[("src/a.rs".to_string(), rust_file(6))],
        &[AuditRule::new("ARCH-TRIGGER-ENV-1", "Trigger from a ref.")],
        &env_corpus(),
        &opts,
    )
    .await
}

/// Test 3: a split decision rule has no standard, one No convention row and a proposal labelled
/// with its origin. The recommendation names no option.
#[tokio::test]
async fn a_split_decision_rule_has_no_standard_and_proposes_the_rule_default() {
    let (rows, _, recs, failed, _, extras) = env_run(None).await;
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(
        kinds_of(&rows, "ARCH-TRIGGER-ENV-1"),
        vec!["no_convention"],
        "{rows:?}"
    );
    assert_eq!(recs[0].recommended_option_id, "");
    let c = &extras.conventions[0];
    assert_eq!(c.standard.option_id, None);
    let p = c.standard.proposal.as_ref().expect("a proposal");
    assert_eq!(
        (p.option_id.as_str(), p.basis),
        ("git-ref", ProposalBasis::RuleDefault)
    );
}

/// Test 4: the project's earlier choice is the proposal when it is not the rule's default.
#[tokio::test]
async fn the_projects_earlier_choice_is_the_proposal_when_it_is_not_the_default() {
    let (_, _, _, failed, _, extras) = env_run(Some(("ARCH-TRIGGER-ENV-1", "manual"))).await;
    assert!(failed.is_empty(), "{failed:?}");
    let c = &extras.conventions[0];
    assert_eq!(c.standard.option_id, None);
    let p = c.standard.proposal.as_ref().expect("a proposal");
    assert_eq!(
        (p.option_id.as_str(), p.basis),
        ("manual", ProposalBasis::EarlierChoice)
    );
}

fn three_corpus() -> camerata_rules::RuleSet {
    let mut set = camerata_rules::RuleSet::default();
    set.push(decision(
        "ARCH-LAYER-1",
        &[("a", "Option A"), ("b", "Option B"), ("c", "Option C")],
        "a",
    ));
    set
}

async fn three_run(judged_in_two_thirds: bool) -> FinderResult {
    let counts = if judged_in_two_thirds {
        json!([
            {"id": "a", "count": 8, "places": ["src/a.rs:1"]},
            {"id": "b", "count": 1, "places": ["src/b.rs:1"]},
            {"id": "c", "count": 1, "places": ["src/b.rs:2"]}
        ])
    } else {
        json!([
            {"id": "a", "count": 1, "places": ["src/a.rs:1"]},
            {"id": "b", "count": 8, "places": ["src/b.rs:1"]},
            {"id": "c", "count": 1, "places": ["src/b.rs:2"]}
        ])
    };
    let agent = ScriptedAgent::new(full_script(
        &[("src/a.rs", 6)],
        vec![problem(
            1,
            "src/a.rs",
            5,
            "medium",
            "fn item_5() { run(5); }",
        )],
        vec![json!({"n": 1, "rule": "ARCH-LAYER-1"})],
        vec![json!({
            "rule": "ARCH-LAYER-1", "answer": "problems", "problems": [1],
            "options": counts
        })],
    ));
    run_with(
        &agent,
        &[
            ("src/a.rs".to_string(), rust_file(6)),
            ("src/b.rs".to_string(), rust_file(4)),
        ],
        &[AuditRule::new("ARCH-LAYER-1", "Layer it.")],
        &three_corpus(),
        &options(),
    )
    .await
}

/// Test 5: the observed convention is the judged option and a finder row cites the rule: the row
/// is keyed to the rule and is a deviation from it (the case that was always right).
#[tokio::test]
async fn a_finder_row_against_the_judged_and_observed_option_is_a_deviation() {
    let (rows, _, _, failed, _, extras) = three_run(true).await;
    assert!(failed.is_empty(), "{failed:?}");
    let row = rows.iter().find(|f| f.line == 5).unwrap();
    assert_eq!(row.rule_id, "ARCH-LAYER-1");
    assert!(
        matches!(row.kind, Some(FindingKind::Deviation(_))),
        "{:?}",
        row.kind
    );
    assert_eq!(
        extras.conventions[0].finder_rows[0].status,
        PlaceStatus::Departs
    );
}

/// Test 6: three options in use, and a finder row at no listed place: its option is unknown, it
/// is not a departure, and it stays in the Queue as the finder's own problem.
#[tokio::test]
async fn a_finder_row_whose_option_is_unknown_is_not_a_departure_and_stays_in_the_queue() {
    let (rows, _, _, failed, _, extras) = three_run(false).await;
    assert!(failed.is_empty(), "{failed:?}");
    let row = rows
        .iter()
        .find(|f| f.line == 5)
        .unwrap_or_else(|| panic!("the problem left the Queue: {rows:?}"));
    assert_ne!(row.rule_id, "ARCH-LAYER-1");
    assert!(!matches!(row.kind, Some(FindingKind::Deviation(_))));
    let r = &extras.conventions[0].finder_rows[0];
    assert_eq!(r.option_basis, OptionBasis::Unknown);
    assert_ne!(r.status, PlaceStatus::Departs);
}

fn dwe_corpus() -> camerata_rules::RuleSet {
    let mut set = camerata_rules::RuleSet::default();
    let mut r = decision("ARCH-LAYER-2", &[("a", "Option A"), ("b", "Option B")], "a");
    r.kind = Some(RuleKind::DefaultWithExceptions);
    set.push(r);
    set
}

/// A rule with a default that is not a pure decision: two thirds of the code follow option B, the
/// finder was judged against the default A. The record says so, so a finder row at a place of B is
/// not a departure from B (review item 3: it was recorded as judged against the observed option,
/// which made every finder row a departure from the code's own convention).
#[tokio::test]
async fn a_default_with_exceptions_rule_is_judged_against_its_default_not_the_observed_option() {
    let agent = ScriptedAgent::new(full_script(
        &[("src/a.rs", 6)],
        vec![problem(
            1,
            "src/a.rs",
            5,
            "medium",
            "fn item_5() { run(5); }",
        )],
        vec![json!({"n": 1, "rule": "ARCH-LAYER-2"})],
        vec![json!({
            "rule": "ARCH-LAYER-2", "answer": "problems", "problems": [1],
            "options": [
                {"id": "a", "count": 1, "places": ["src/a.rs:1"]},
                {"id": "b", "count": 8, "places": ["src/a.rs:5"]}
            ]
        })],
    ));
    let (rows, _, _, failed, _, extras) = run_with(
        &agent,
        &[("src/a.rs".to_string(), rust_file(6))],
        &[AuditRule::new("ARCH-LAYER-2", "Layer it.")],
        &dwe_corpus(),
        &options(),
    )
    .await;
    assert!(failed.is_empty(), "{failed:?}");
    let c = &extras.conventions[0];
    assert_eq!(c.judged_option_id.as_deref(), Some("a"), "the default");
    assert_eq!(c.standard.option_id.as_deref(), Some("b"), "anti-vacuity");
    assert_eq!(c.finder_rows.len(), 1);
    assert_ne!(
        c.finder_rows[0].status,
        PlaceStatus::Departs,
        "the row sits at a place of the observed option: {:?}",
        c.finder_rows
    );
    let row = rows.iter().find(|f| f.line == 5).unwrap();
    assert!(
        !matches!(row.kind, Some(FindingKind::Deviation(_))),
        "{row:?}"
    );
}

/// Review item 7: the ledger's convention block guards something. A finder row dropped after it was
/// tagged (a merge or a dedup that lost it) is found by the independent check; the three-part sum
/// alone stays true.
#[tokio::test]
async fn the_convention_ledger_finds_a_finder_row_lost_after_tagging() {
    let (rows, _, _, failed, _, extras) = hearth(
        json!({
            "rule": PAR, "answer": "problems", "problems": [1, 2],
            "options": [
                {"id": "seq", "count": 4, "places": []},
                {"id": "par", "count": 1, "places": []}
            ]
        }),
        true,
    )
    .await;
    assert!(failed.is_empty(), "{failed:?}");
    let mut block = crate::scan_ledger::ConventionsBlock::from_records(&extras.conventions, &rows);
    assert!(
        block
            .check_finder_rows_kept(&extras.conventions, &rows, &[])
            .is_empty(),
        "nothing is lost in a real run"
    );
    assert!(block.reconciles());
    // One finder row goes missing (and is not among the rows set aside).
    let victim = rows
        .iter()
        .find(|f| f.line == 17)
        .expect("the listings row")
        .clone();
    let kept: Vec<_> = rows
        .iter()
        .filter(|f| !(f.path == victim.path && f.line == victim.line))
        .cloned()
        .collect();
    let lost = block.check_finder_rows_kept(&extras.conventions, &kept, &[]);
    assert_eq!(lost.len(), 1, "{lost:?}");
    assert!(!block.reconciles(), "{block:?}");
    // The same row, set aside, is accounted for.
    let mut block = crate::scan_ledger::ConventionsBlock::from_records(&extras.conventions, &rows);
    assert!(block
        .check_finder_rows_kept(&extras.conventions, &kept, &[victim])
        .is_empty());
}
