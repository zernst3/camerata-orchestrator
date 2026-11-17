//! The checklist, its checks and the unit conventions (design section 3).

use super::checklist::{
    build, combine_for_unit, AnswerKind, Checklist, ChecklistFile, Citation, Effective,
    EffectiveAnswer, OptionCount, Place, RuleAnswer,
};
use super::reply::Problem;
use super::ToolEvent;
use crate::scan_routing::AuditRule;

fn rust_file() -> (String, String) {
    ("src/lib.rs".to_string(), "pub fn a() {}\n".to_string())
}

fn py_file() -> (String, String) {
    ("app/main.py".to_string(), "def a(): pass\n".to_string())
}

fn corpus() -> camerata_rules::RuleSet {
    let mut set = camerata_rules::RuleSet::default();
    let mut a = camerata_rules::bare_rule("RUST-ZED-1", "rust");
    a.min_severity = Some("medium".to_string());
    a.max_severity = Some("high".to_string());
    set.push(a);
    let mut b = camerata_rules::bare_rule("ARCH-ALPHA-1", "universal");
    b.options = vec![
        camerata_rules::RuleOption {
            id: "repo".into(),
            label: "Repository layer".into(),
            directive: "use repos".into(),
            why: "w".into(),
            remediation: None,
            finding_headline: None,
            finding_detail: None,
            escalation: None,
            effort: None,
            violates: false,
        },
        camerata_rules::RuleOption {
            id: "direct".into(),
            label: "Direct queries".into(),
            directive: "query in handlers".into(),
            why: "w".into(),
            remediation: None,
            finding_headline: None,
            finding_detail: None,
            escalation: None,
            effort: None,
            violates: false,
        },
    ];
    set.push(b);
    set.push(camerata_rules::bare_rule("PY-ONLY-1", "python"));
    set
}

fn rules() -> Vec<AuditRule> {
    vec![
        AuditRule::new("RUST-ZED-1", "Do the zed thing."),
        AuditRule::new("PY-ONLY-1", "Python only."),
        AuditRule::new("ARCH-ALPHA-1", "Use the repository layer."),
    ]
}

fn problem(n: u32, path: &str) -> Problem {
    Problem {
        n,
        path: path.into(),
        line: 1,
        end_line: None,
        severity: "high".into(),
        title: "t".into(),
        code: "c".into(),
        what: "w".into(),
        why: "y".into(),
        fix: "f".into(),
        sure: true,
        effort: "low".into(),
        label: "some label".into(),
        places: Vec::new(),
    }
}

fn read(path: &str) -> ToolEvent {
    ToolEvent::Read {
        path: path.into(),
        first_line: 1,
        line_count: 100,
        lines_returned: 10,
    }
}

/// The piece's files with their line counts: `src/lib.rs` has 10 lines.
fn piece() -> Vec<(String, usize)> {
    vec![("src/lib.rs".to_string(), 10)]
}

fn checklist() -> Checklist {
    build(&rules(), &corpus(), &[rust_file()], true)
}

fn answer(rule: &str, kind: AnswerKind) -> RuleAnswer {
    RuleAnswer {
        rule: rule.into(),
        kind,
        problems: vec![],
        places: vec![],
        reason: None,
        options: vec![],
    }
}

#[test]
fn only_rules_with_a_file_in_scope_are_listed_sorted_by_id() {
    let list = checklist();
    let ids: Vec<&str> = list.rules.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["ARCH-ALPHA-1", "RUST-ZED-1"],
        "the python rule has no python file in the piece"
    );
    let both = build(&rules(), &corpus(), &[rust_file(), py_file()], true);
    let ids: Vec<&str> = both.rules.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, vec!["ARCH-ALPHA-1", "PY-ONLY-1", "RUST-ZED-1"]);
}

#[test]
fn a_line_holds_title_directive_severity_range_and_options() {
    let list = checklist();
    let zed = list
        .rules
        .iter()
        .find(|r| r.id == "RUST-ZED-1")
        .expect("listed");
    assert_eq!(zed.directive, "Do the zed thing.");
    assert_eq!(zed.min_severity.as_deref(), Some("medium"));
    assert_eq!(zed.max_severity.as_deref(), Some("high"));
    assert!(zed.options.is_empty());
    let alpha = list
        .rules
        .iter()
        .find(|r| r.id == "ARCH-ALPHA-1")
        .expect("listed");
    assert_eq!(alpha.directive, "Use the repository layer.");
    let option_ids: Vec<&str> = alpha.options.iter().map(|o| o.id.as_str()).collect();
    assert_eq!(option_ids, vec!["repo", "direct"]);
    assert_eq!(alpha.options[0].label, "Repository layer");
}

#[test]
fn render_gives_each_rule_and_the_three_instructions() {
    let text = checklist().render();
    for needle in [
        "ARCH-ALPHA-1",
        "RUST-ZED-1",
        "Do the zed thing.",
        "medium",
        "high",
        "Repository layer",
    ] {
        assert!(text.contains(needle), "{needle} missing from:\n{text}");
    }
    for needle in [
        "Cite",
        "Answer every rule",
        "Add",
        "checklist.json",
        "not_applicable",
        "clean",
        "problems",
    ] {
        assert!(
            text.contains(needle),
            "instruction {needle} missing from:\n{text}"
        );
    }
    assert!(!text.contains('\u{2014}') && !text.contains('\u{2013}'));
    // Same stack, same text: the checklist is cacheable.
    assert_eq!(text, checklist().render());
}

#[test]
fn a_rule_with_no_answer_is_missing() {
    let list = checklist();
    let file = ChecklistFile {
        citations: vec![],
        answers: vec![answer("RUST-ZED-1", AnswerKind::NotApplicable)],
    };
    let out = list.check(&file, &[], &[], &piece());
    assert_eq!(out.missing, vec!["ARCH-ALPHA-1".to_string()]);
}

#[test]
fn a_problems_answer_must_name_a_problem_that_exists() {
    let list = checklist();
    let mut a = answer("RUST-ZED-1", AnswerKind::Problems);
    a.problems = vec![1, 9];
    let file = ChecklistFile {
        citations: vec![],
        answers: vec![a, answer("ARCH-ALPHA-1", AnswerKind::NotApplicable)],
    };
    let out = list.check(&file, &[problem(1, "src/lib.rs")], &[], &piece());
    assert_eq!(out.bad_problem_refs.len(), 1);
    assert_eq!(out.bad_problem_refs[0].problem, 9);
    assert_eq!(out.bad_problem_refs[0].rule, "RUST-ZED-1");
    let eff = out
        .effective
        .iter()
        .find(|e| e.rule == "RUST-ZED-1")
        .expect("answered");
    assert_eq!(
        eff.state,
        Effective::Problems(vec![1]),
        "the real problem number stays"
    );
}

#[test]
fn a_problems_answer_with_no_real_problem_is_not_completed_never_not_applicable() {
    let list = checklist();
    let mut a = answer("RUST-ZED-1", AnswerKind::Problems);
    a.problems = vec![7];
    let file = ChecklistFile {
        citations: vec![],
        answers: vec![a, answer("ARCH-ALPHA-1", AnswerKind::NotApplicable)],
    };
    let out = list.check(&file, &[], &[], &piece());
    let eff = out
        .effective
        .iter()
        .find(|e| e.rule == "RUST-ZED-1")
        .expect("answered");
    assert!(
        matches!(eff.state, Effective::NotCompleted(_)),
        "{:?}",
        eff.state
    );
    assert_eq!(out.not_completed, vec!["RUST-ZED-1".to_string()]);
}

#[test]
fn a_clean_answer_with_no_valid_checked_place_is_not_completed_never_clean() {
    let list = checklist();
    let mut no_places = answer("RUST-ZED-1", AnswerKind::Clean);
    no_places.places = vec![];
    let mut ghost = answer("ARCH-ALPHA-1", AnswerKind::Clean);
    ghost.places = vec![Place {
        path: "src/missing.rs".into(),
        line: Some(3),
    }];
    let file = ChecklistFile {
        citations: vec![],
        answers: vec![no_places, ghost],
    };
    let out = list.check(&file, &[], &[read("src/lib.rs")], &piece());
    assert_eq!(
        out.downgraded_clean,
        vec!["ARCH-ALPHA-1".to_string(), "RUST-ZED-1".to_string()]
    );
    for e in &out.effective {
        match &e.state {
            Effective::NotCompleted(reason) => {
                assert!(reason.contains("no place to check was named"), "{reason}");
            }
            other => panic!("{} must not be clean: {other:?}", e.rule),
        }
    }
}

#[test]
fn a_clean_answer_naming_a_file_the_trace_never_read_is_not_clean() {
    let list = checklist();
    let mut a = answer("RUST-ZED-1", AnswerKind::Clean);
    a.places = vec![Place {
        path: "src/lib.rs".into(),
        line: Some(1),
    }];
    let file = ChecklistFile {
        citations: vec![],
        answers: vec![a, answer("ARCH-ALPHA-1", AnswerKind::NotApplicable)],
    };
    // The file exists in the piece but the trace shows only a search, never a read.
    let trace = [ToolEvent::Grep {
        pattern: "pub fn".into(),
    }];
    let out = list.check(&file, &[], &trace, &piece());
    assert_eq!(out.downgraded_clean, vec!["RUST-ZED-1".to_string()]);
    // A read that returned no lines is no read either.
    let empty = [ToolEvent::Read {
        path: "src/lib.rs".into(),
        first_line: 9,
        line_count: 5,
        lines_returned: 0,
    }];
    assert_eq!(
        list.check(&file, &[], &empty, &piece()).downgraded_clean,
        vec!["RUST-ZED-1".to_string()]
    );
}

#[test]
fn a_clean_answer_with_one_valid_place_stays_clean() {
    let list = checklist();
    let mut a = answer("RUST-ZED-1", AnswerKind::Clean);
    a.places = vec![
        Place {
            path: "src/gone.rs".into(),
            line: None,
        },
        Place {
            path: "./src/lib.rs".into(),
            line: Some(1),
        },
    ];
    let file = ChecklistFile {
        citations: vec![],
        answers: vec![a, answer("ARCH-ALPHA-1", AnswerKind::NotApplicable)],
    };
    let out = list.check(&file, &[], &[read("src/lib.rs")], &piece());
    assert!(out.downgraded_clean.is_empty());
    let eff = out
        .effective
        .iter()
        .find(|e| e.rule == "RUST-ZED-1")
        .expect("answered");
    assert_eq!(
        eff.state,
        Effective::Clean(vec![Place {
            path: "./src/lib.rs".into(),
            line: Some(1)
        }])
    );
}

#[test]
fn a_citation_to_a_rule_not_on_the_list_is_foreign_and_a_listed_one_is_valid() {
    let list = checklist();
    let file = ChecklistFile {
        citations: vec![
            Citation {
                problem: 1,
                rule: "RUST-ZED-1".into(),
            },
            Citation {
                problem: 2,
                rule: "PY-ONLY-1".into(),
            },
            Citation {
                problem: 3,
                rule: "RUST-ZED-1".into(),
            },
        ],
        answers: vec![
            answer("RUST-ZED-1", AnswerKind::NotApplicable),
            answer("ARCH-ALPHA-1", AnswerKind::NotApplicable),
        ],
    };
    let out = list.check(
        &file,
        &[problem(1, "src/lib.rs"), problem(2, "src/lib.rs")],
        &[],
        &piece(),
    );
    assert_eq!(
        out.foreign_citations,
        vec![Citation {
            problem: 2,
            rule: "PY-ONLY-1".into()
        }]
    );
    assert_eq!(
        out.valid_citations,
        vec![Citation {
            problem: 1,
            rule: "RUST-ZED-1".into()
        }],
        "problem 3 does not exist"
    );
}

fn counts(rule: &str, items: &[(&str, usize)]) -> EffectiveAnswer {
    EffectiveAnswer {
        rule: rule.into(),
        state: Effective::Clean(vec![]),
        options: items
            .iter()
            .map(|(id, n)| OptionCount {
                id: (*id).into(),
                count: *n,
                places: vec![],
            })
            .collect(),
    }
}

#[test]
fn unit_conventions_add_option_counts_and_need_two_thirds() {
    let pieces = vec![
        vec![counts("ARCH-ALPHA-1", &[("repo", 5), ("direct", 1)])],
        vec![counts("ARCH-ALPHA-1", &[("repo", 4), ("direct", 2)])],
    ];
    let out = combine_for_unit(&pieces);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].counts["repo"], 9);
    assert_eq!(out[0].counts["direct"], 3);
    assert_eq!(out[0].total, 12);
    assert_eq!(
        out[0].convention.as_deref(),
        Some("repo"),
        "9 of 12 is three quarters"
    );
}

#[test]
fn exactly_two_thirds_is_a_convention_and_less_is_not() {
    let two_thirds = combine_for_unit(&[vec![counts("R-1", &[("a", 4), ("b", 2)])]]);
    assert_eq!(two_thirds[0].convention.as_deref(), Some("a"));
    let under = combine_for_unit(&[vec![counts("R-1", &[("a", 6), ("b", 4)])]]);
    assert_eq!(under[0].convention, None);
    let nothing = combine_for_unit(&[vec![counts("R-1", &[("a", 0), ("b", 0)])]]);
    assert_eq!(nothing[0].convention, None);
    assert_eq!(nothing[0].total, 0);
}

#[test]
fn a_rule_with_no_options_has_no_unit_convention_row() {
    let out = combine_for_unit(&[vec![counts("RUST-ZED-1", &[])]]);
    assert!(out.is_empty());
}

fn clean_at(rule: &str, path: &str, line: Option<usize>) -> RuleAnswer {
    let mut a = answer(rule, AnswerKind::Clean);
    a.places = vec![Place {
        path: path.into(),
        line,
    }];
    a
}

fn file_of(answers: Vec<RuleAnswer>) -> ChecklistFile {
    ChecklistFile {
        citations: vec![],
        answers,
    }
}

fn state_of(out: &super::checklist::CheckOutcome, rule: &str) -> Effective {
    out.effective
        .iter()
        .find(|e| e.rule == rule)
        .unwrap_or_else(|| panic!("{rule} not answered"))
        .state
        .clone()
}

#[test]
fn a_clean_answer_citing_the_checklist_or_a_context_file_is_not_completed() {
    let list = checklist();
    // The trace shows the checklist and a context file were read; neither is a file of the piece.
    let trace = [read("_camerata/checklist.md"), read("src/ctx.rs")];
    let file = file_of(vec![
        clean_at("RUST-ZED-1", "_camerata/checklist.md", Some(1)),
        clean_at("ARCH-ALPHA-1", "src/ctx.rs", Some(1)),
    ]);
    let out = list.check(&file, &[], &trace, &piece());
    assert_eq!(
        out.downgraded_clean,
        vec!["ARCH-ALPHA-1".to_string(), "RUST-ZED-1".to_string()]
    );
    assert!(matches!(
        state_of(&out, "RUST-ZED-1"),
        Effective::NotCompleted(_)
    ));
    assert!(matches!(
        state_of(&out, "ARCH-ALPHA-1"),
        Effective::NotCompleted(_)
    ));
}

#[test]
fn a_clean_answer_at_a_line_that_does_not_exist_or_was_not_returned_is_not_completed() {
    let list = checklist();
    // src/lib.rs has 10 lines; the read returned lines 1 to 5 only.
    let trace = [ToolEvent::Read {
        path: "src/lib.rs".into(),
        first_line: 1,
        line_count: 5,
        lines_returned: 5,
    }];
    for (line, ok) in [
        (Some(9999), false),
        (Some(0), false),
        (Some(8), false),
        (None, false),
        (Some(5), true),
        (Some(1), true),
    ] {
        let file = file_of(vec![
            clean_at("RUST-ZED-1", "src/lib.rs", line),
            answer("ARCH-ALPHA-1", AnswerKind::NotApplicable),
        ]);
        let out = list.check(&file, &[], &trace, &piece());
        assert_eq!(
            out.downgraded_clean.is_empty(),
            ok,
            "line {line:?}: {:?}",
            state_of(&out, "RUST-ZED-1")
        );
    }
}

#[test]
fn a_problem_moved_to_the_leads_leaves_its_rule_not_completed_never_not_applicable() {
    // The agent answered `problems` with problem 1, but problem 1 was written for a path outside
    // the piece, so `parse_outputs` moved it to the leads and the problem list is empty.
    let json = serde_json::json!([{
        "n": 1, "path": "src/outside.rs", "line": 2, "severity": "high", "title": "t",
        "code": "c", "what": "w", "why": "y", "fix": "f", "sure": "yes", "effort": "low",
        "label": "l"
    }])
    .to_string();
    let outputs: std::collections::BTreeMap<String, String> =
        [("_camerata/out/problems-1.json".to_string(), json)].into();
    let parsed = super::reply::parse_outputs(&outputs, &["src/lib.rs".to_string()]);
    assert!(parsed.problems.is_empty());
    assert_eq!(parsed.leads.len(), 1);
    let mut a = answer("RUST-ZED-1", AnswerKind::Problems);
    a.problems = vec![1];
    let list = checklist();
    let file = file_of(vec![a, answer("ARCH-ALPHA-1", AnswerKind::NotApplicable)]);
    let out = list.check(&file, &parsed.problems, &[], &piece());
    assert!(matches!(
        state_of(&out, "RUST-ZED-1"),
        Effective::NotCompleted(_)
    ));
    assert_eq!(out.not_completed, vec!["RUST-ZED-1".to_string()]);
    assert_eq!(out.bad_problem_refs.len(), 1);
}

#[test]
fn on_duplicate_answers_for_one_rule_the_worst_stands() {
    let list = checklist();
    let trace = [read("src/lib.rs")];
    let problem_answer = {
        let mut a = answer("RUST-ZED-1", AnswerKind::Problems);
        a.problems = vec![1];
        a
    };
    let lost_problem = {
        let mut a = answer("RUST-ZED-1", AnswerKind::Problems);
        a.problems = vec![9];
        a
    };
    let clean = clean_at("RUST-ZED-1", "src/lib.rs", Some(1));
    let na = answer("RUST-ZED-1", AnswerKind::NotApplicable);
    let other = answer("ARCH-ALPHA-1", AnswerKind::NotApplicable);
    let problems = [problem(1, "src/lib.rs")];

    let run = |first: &RuleAnswer, second: &RuleAnswer| {
        let file = file_of(vec![first.clone(), second.clone(), other.clone()]);
        state_of(
            &list.check(&file, &problems, &trace, &piece()),
            "RUST-ZED-1",
        )
    };
    // Problems over everything, whichever came first.
    assert_eq!(run(&na, &problem_answer), Effective::Problems(vec![1]));
    assert_eq!(run(&problem_answer, &clean), Effective::Problems(vec![1]));
    assert_eq!(
        run(&lost_problem, &problem_answer),
        Effective::Problems(vec![1])
    );
    // Not completed over clean and over not applicable.
    assert!(matches!(
        run(&clean, &lost_problem),
        Effective::NotCompleted(_)
    ));
    assert!(matches!(
        run(&na, &lost_problem),
        Effective::NotCompleted(_)
    ));
    // Clean over not applicable.
    assert!(matches!(run(&na, &clean), Effective::Clean(_)));
    assert!(matches!(run(&clean, &na), Effective::Clean(_)));
    assert_eq!(run(&na, &na), Effective::NotApplicable);
}

#[test]
fn a_downgraded_clean_that_loses_to_a_problems_answer_is_not_a_downgrade() {
    let list = checklist();
    let ghost = clean_at("RUST-ZED-1", "src/ghost.rs", Some(1));
    let mut with_problem = answer("RUST-ZED-1", AnswerKind::Problems);
    with_problem.problems = vec![1];
    let file = file_of(vec![
        ghost,
        with_problem,
        answer("ARCH-ALPHA-1", AnswerKind::NotApplicable),
    ]);
    let out = list.check(&file, &[problem(1, "src/lib.rs")], &[], &piece());
    assert_eq!(state_of(&out, "RUST-ZED-1"), Effective::Problems(vec![1]));
    assert!(out.downgraded_clean.is_empty());
    assert!(out.not_completed.is_empty());
}

fn not_a_problem_at(rule: &str, path: &str, line: usize, reason: &str) -> RuleAnswer {
    let mut a = answer(rule, AnswerKind::NotAProblemHere);
    a.places = vec![Place {
        path: path.into(),
        line: Some(line),
    }];
    a.reason = Some(reason.into());
    a
}

#[test]
fn render_offers_the_fourth_answer_and_leaves_adding_to_the_models_judgment() {
    let text = checklist().render();
    assert!(text.contains("not_a_problem_here"), "{text}");
    assert!(
        text.contains("and one sentence as `reason`, no more"),
        "{text}"
    );
    assert!(
        !text.contains("Give one place and one sentence of reason, no more."),
        "the limit lives inside the answer's own clause: {text}"
    );
    assert!(
        text.contains("never for something this code needs and does not have"),
        "{text}"
    );
    assert!(text.contains("Answer every rule"), "{text}");
    assert!(text.contains("on your own judgment"), "{text}");
    assert!(text.contains("Rate it yourself"), "{text}");
    assert!(
        text.contains("this code needs and does not have, that is a problem"),
        "{text}"
    );
}

#[test]
fn render_shows_severity_as_guidance_never_as_a_minimum() {
    let text = checklist().render();
    assert!(
        text.contains("Suggested severity: medium, at most high"),
        "{text}"
    );
    assert!(!text.contains("at least"), "{text}");
    let mut list = checklist();
    list.rules[0].directive =
        "Report at high severity when the key is shared. Flag as high severity: 'Your key'. \
         Emit a medium-severity finding. Severity is High."
            .to_string();
    let text = list.render();
    assert!(
        text.contains("Usually high when the key is shared."),
        "{text}"
    );
    assert!(text.contains("Usually high: 'Your key'."), "{text}");
    assert!(text.contains("a finding (usually medium)"), "{text}");
    assert!(text.contains("Usually High."), "{text}");
    assert!(!text.contains("Report at high severity"), "{text}");
}

#[test]
fn a_not_a_problem_here_answer_makes_no_row_and_is_noted_for_the_reviewer() {
    let list = checklist();
    let file = file_of(vec![
        not_a_problem_at("RUST-ZED-1", "src/lib.rs", 3, "The cast is checked above."),
        answer("ARCH-ALPHA-1", AnswerKind::NotApplicable),
    ]);
    let out = list.check(&file, &[], &[read("src/lib.rs")], &piece());
    assert_eq!(state_of(&out, "RUST-ZED-1"), Effective::NotAProblemHere);
    assert!(out.not_completed.is_empty() && out.missing.is_empty());
    assert!(out.valid_citations.is_empty());
    assert_eq!(out.not_a_problem_notes.len(), 1);
    let n = &out.not_a_problem_notes[0];
    assert_eq!(n.rule, "RUST-ZED-1");
    assert_eq!(n.place.path, "src/lib.rs");
    assert_eq!(n.place.line, Some(3));
    assert_eq!(n.reason, "The cast is checked above.");
}

#[test]
fn not_a_problem_here_ranks_between_clean_and_not_applicable() {
    let list = checklist();
    let trace = [read("src/lib.rs")];
    let nph = not_a_problem_at("RUST-ZED-1", "src/lib.rs", 3, "fine");
    let clean = clean_at("RUST-ZED-1", "src/lib.rs", Some(1));
    let na = answer("RUST-ZED-1", AnswerKind::NotApplicable);
    let other = answer("ARCH-ALPHA-1", AnswerKind::NotApplicable);
    let run = |a: &RuleAnswer, b: &RuleAnswer| {
        let file = file_of(vec![a.clone(), b.clone(), other.clone()]);
        let out = list.check(&file, &[], &trace, &piece());
        (state_of(&out, "RUST-ZED-1"), out.not_a_problem_notes.len())
    };
    assert_eq!(run(&nph, &na), (Effective::NotAProblemHere, 1));
    assert!(matches!(run(&nph, &clean), (Effective::Clean(_), 1)));
    assert!(matches!(run(&clean, &nph), (Effective::Clean(_), 1)));
}

#[test]
fn a_not_a_problem_here_place_that_was_not_read_or_is_outside_the_piece_is_not_completed() {
    let list = checklist();
    let other = answer("ARCH-ALPHA-1", AnswerKind::NotApplicable);
    for (path, line) in [
        ("src/lib.rs", 5),
        ("src/elsewhere.rs", 1),
        ("src/lib.rs", 99),
    ] {
        let file = file_of(vec![
            not_a_problem_at("RUST-ZED-1", path, line, "Fine."),
            other.clone(),
        ]);
        // Only line 1 was read.
        let trace = [ToolEvent::Read {
            path: "src/lib.rs".into(),
            first_line: 1,
            line_count: 10,
            lines_returned: 2,
        }];
        let out = list.check(&file, &[], &trace, &piece());
        assert!(
            matches!(state_of(&out, "RUST-ZED-1"), Effective::NotCompleted(_)),
            "{path}:{line}"
        );
        assert_eq!(out.not_completed, vec!["RUST-ZED-1".to_string()]);
        assert!(out.not_a_problem_notes.is_empty(), "{path}:{line}");
    }
}

#[test]
fn a_not_a_problem_here_place_that_was_read_gives_the_note() {
    let list = checklist();
    let file = file_of(vec![
        not_a_problem_at("RUST-ZED-1", "src/lib.rs", 2, "Fine."),
        answer("ARCH-ALPHA-1", AnswerKind::NotApplicable),
    ]);
    let trace = [ToolEvent::Read {
        path: "src/lib.rs".into(),
        first_line: 1,
        line_count: 10,
        lines_returned: 2,
    }];
    let out = list.check(&file, &[], &trace, &piece());
    assert_eq!(state_of(&out, "RUST-ZED-1"), Effective::NotAProblemHere);
    assert_eq!(out.not_a_problem_notes.len(), 1);
}

#[test]
fn render_words_the_places_the_duplicate_and_the_not_applicable_rules() {
    let text = checklist().render();
    assert!(
        text.contains(
            "When your answer is `problems`, put the places that break the rule in the `places` \
             of the problem they are an instance of, and under options give the counts only. \
             For any other answer, under options give the counts and up to ten places for each \
             option."
        ),
        "{text}"
    );
    assert!(
        !text.contains("name places only for the option the code should follow"),
        "{text}"
    );
    assert!(
        text.contains(
            "If an earlier problem already covers the place for the same reason, cite it \
             (step 1); never write a second problem."
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "`not_applicable` only when this part has no code of the kind the rule governs; \
             give one sentence saying what is absent. If the code is here and the safeguard is \
             missing, that is a problem, not `not_applicable`."
        ),
        "{text}"
    );
}

#[test]
fn render_words_the_dismissal_rule_between_not_applicable_and_the_problems_answer() {
    let text = checklist().render();
    let dismissal = "`not_a_problem_here` only when you are sure the place does no harm. If your \
         reason would name a real cost and call it small, that is a problem at low severity, \
         not `not_a_problem_here`. If your reason would be that another of your problems \
         reports it, answer `problems` with that problem's number, and if that problem does \
         not name this place, add the place to its `places` or write it as its own problem.";
    assert!(text.contains(dismissal), "{text}");
    let after = "that is a problem, not `not_applicable`. ";
    let before = "When your answer is `problems`,";
    let a = text.find(after).expect("not_applicable sentence") + after.len();
    assert_eq!(text.find(dismissal), Some(a), "{text}");
    assert_eq!(text[a + dismissal.len()..].find(before), Some(1), "{text}");
}

#[test]
fn a_not_applicable_reason_is_noted_for_the_reviewer_and_none_is_no_note() {
    let list = checklist();
    let mut with = answer("ARCH-ALPHA-1", AnswerKind::NotApplicable);
    with.reason = Some(" No queues here. ".into());
    let other = answer("RUST-ZED-1", AnswerKind::NotApplicable);
    let out = list.check(&file_of(vec![with, other]), &[], &[], &piece());
    assert_eq!(out.not_applicable_notes.len(), 1);
    assert_eq!(out.not_applicable_notes[0].rule, "ARCH-ALPHA-1");
    assert_eq!(out.not_applicable_notes[0].reason, "No queues here.");
    assert_eq!(state_of(&out, "ARCH-ALPHA-1"), Effective::NotApplicable);
}

#[test]
fn render_shows_the_optional_reason_on_not_applicable_in_the_example() {
    let text = checklist().render();
    assert!(
        text.contains("\"answer\": \"not_applicable\", \"reason\""),
        "{text}"
    );
}
