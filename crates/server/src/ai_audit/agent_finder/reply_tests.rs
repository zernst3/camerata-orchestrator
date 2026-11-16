//! Reading the output files and turning problems into findings (design section 2).

use std::collections::{BTreeMap, HashSet};

use super::checklist::AnswerKind;
use super::reply::{
    citation_map, parse_leads_answers, parse_outputs, scan_reply, to_findings, LeadOutcome,
    Problem, FINDER_EFFORT_CAPTURE, FINDER_END_LINE_CAPTURE, FINDER_FIX_CAPTURE,
    FINDER_PIECE_CAPTURE, FINDER_PROBLEM_CAPTURE, FINDER_RULES_CAPTURE, FINDER_SURE_CAPTURE,
};

fn entry(n: u32, path: &str) -> serde_json::Value {
    serde_json::json!({
        "n": n, "path": path, "line": 12, "end_line": 14, "severity": "high",
        "title": "Handler skips the authorization check",
        "code": "db.delete(id)", "what": "The route deletes without checking the caller.",
        "why": "Any caller can delete any record.", "fix": "Check the caller owns the record.",
        "sure": "yes", "effort": "low", "label": "missing authorization"
    })
}

fn outputs(files: &[(&str, String)]) -> BTreeMap<String, String> {
    files
        .iter()
        .map(|(k, v)| (format!("_camerata/out/{k}"), v.clone()))
        .collect()
}

fn piece_files() -> Vec<String> {
    vec!["src/a.rs".to_string(), "src/b.rs".to_string()]
}

fn problem_for(n: u32, label: &str) -> Problem {
    Problem {
        n,
        path: "src/a.rs".into(),
        line: 12,
        end_line: Some(14),
        severity: "high".into(),
        title: "Handler skips the authorization check".into(),
        code: "db.delete(id)".into(),
        what: "The route deletes without checking the caller.".into(),
        why: "Any caller can delete any record.".into(),
        fix: "Check the caller owns the record.".into(),
        sure: true,
        effort: "low".into(),
        label: label.into(),
    }
}

#[test]
fn a_valid_problems_file_parses_every_field() {
    let json = serde_json::json!([entry(1, "src/a.rs"), entry(2, "src/b.rs")]).to_string();
    let out = parse_outputs(&outputs(&[("problems-1.json", json)]), &piece_files());
    assert!(out.unusable.is_empty(), "{:?}", out.unusable);
    assert_eq!(out.problems.len(), 2);
    let p = &out.problems[0];
    assert_eq!((p.n, p.line, p.end_line), (1, 12, Some(14)));
    assert_eq!(p.severity, "high");
    assert!(p.sure);
    assert_eq!(p.effort, "low");
    assert_eq!(p.label, "missing authorization");
}

#[test]
fn problem_files_are_read_in_order_and_unsure_is_kept() {
    let mut second = entry(41, "src/b.rs");
    second["sure"] = serde_json::json!("unsure");
    let out = parse_outputs(
        &outputs(&[
            ("problems-2.json", serde_json::json!([second]).to_string()),
            (
                "problems-1.json",
                serde_json::json!([entry(1, "src/a.rs")]).to_string(),
            ),
        ]),
        &piece_files(),
    );
    let ns: Vec<u32> = out.problems.iter().map(|p| p.n).collect();
    assert_eq!(ns, vec![1, 41]);
    assert!(!out.problems[1].sure);
}

#[test]
fn a_malformed_file_is_reported_as_unusable_with_the_reason() {
    let mut missing_title = entry(3, "src/a.rs");
    missing_title.as_object_mut().map(|o| o.remove("title"));
    let mut bad_severity = entry(4, "src/a.rs");
    bad_severity["severity"] = serde_json::json!("catastrophic");
    let out = parse_outputs(
        &outputs(&[
            ("problems-1.json", "{not json".to_string()),
            (
                "problems-2.json",
                serde_json::json!([missing_title]).to_string(),
            ),
            (
                "problems-3.json",
                serde_json::json!([bad_severity]).to_string(),
            ),
            ("problems-4.json", serde_json::json!({"n": 1}).to_string()),
        ]),
        &piece_files(),
    );
    assert!(out.problems.is_empty());
    assert_eq!(out.unusable.len(), 4);
    let reasons: Vec<&str> = out.unusable.iter().map(|u| u.reason.as_str()).collect();
    assert!(reasons[0].contains("not valid JSON"), "{reasons:?}");
    assert!(reasons[1].contains("title"), "{reasons:?}");
    assert!(reasons[2].contains("severity"), "{reasons:?}");
    assert!(reasons[3].contains("array"), "{reasons:?}");
    assert!(out.unusable[0].file.ends_with("problems-1.json"));
}

#[test]
fn more_than_forty_problems_in_one_file_or_a_repeated_number_make_the_file_unusable() {
    let many: Vec<serde_json::Value> = (1..=41).map(|n| entry(n, "src/a.rs")).collect();
    let out = parse_outputs(
        &outputs(&[("problems-1.json", serde_json::json!(many).to_string())]),
        &piece_files(),
    );
    assert!(out.problems.is_empty());
    assert!(out.unusable[0].reason.contains("40"), "{:?}", out.unusable);

    let out = parse_outputs(
        &outputs(&[
            (
                "problems-1.json",
                serde_json::json!([entry(1, "src/a.rs")]).to_string(),
            ),
            (
                "problems-2.json",
                serde_json::json!([entry(1, "src/b.rs")]).to_string(),
            ),
        ]),
        &piece_files(),
    );
    assert_eq!(out.problems.len(), 1, "the repeat is refused");
    assert!(
        out.unusable[0].reason.contains("number"),
        "{:?}",
        out.unusable
    );
}

#[test]
fn a_problem_whose_path_is_a_context_file_is_rejected_into_leads() {
    let json =
        serde_json::json!([entry(1, "src/a.rs"), entry(2, "src/context_only.rs")]).to_string();
    let out = parse_outputs(&outputs(&[("problems-1.json", json)]), &piece_files());
    assert_eq!(out.problems.len(), 1);
    assert_eq!(out.problems[0].path, "src/a.rs");
    assert_eq!(out.leads.len(), 1);
    assert_eq!(out.leads[0].path, "src/context_only.rs");
    assert_eq!(out.leads[0].line, 12);
    assert!(
        out.leads[0].what.contains("authorization"),
        "{:?}",
        out.leads[0]
    );
}

#[test]
fn leads_checklist_card_and_unread_are_parsed() {
    let leads =
        serde_json::json!([{"path": "lib/x.rs", "line": 5, "what": "unchecked input"}]).to_string();
    let checklist = serde_json::json!({
        "citations": [{"n": 1, "rule": "arch-alpha-1"}],
        "answers": [
            {"rule": "ARCH-ALPHA-1", "answer": "problems", "problems": [1]},
            {"rule": "RUST-ZED-1", "answer": "clean", "places": [{"path": "src/a.rs", "line": 3}, "src/b.rs:9", "src/b.rs"]},
            {"rule": "R-3", "answer": "not_applicable"},
            {"rule": "R-4", "answer": "clean", "places": ["src/a.rs:1"],
             "options": [{"id": "repo", "count": 3, "places": ["src/a.rs:1", "src/a.rs:2"]}, {"id": "direct", "count": 1}]}
        ]
    })
    .to_string();
    let out = parse_outputs(
        &outputs(&[
            ("leads.json", leads),
            ("checklist.json", checklist),
            ("card.md", "Entry points: main.\n".to_string()),
            ("unread.json", serde_json::json!(["src/b.rs"]).to_string()),
        ]),
        &piece_files(),
    );
    assert!(out.unusable.is_empty(), "{:?}", out.unusable);
    assert_eq!(out.leads[0].path, "lib/x.rs");
    assert_eq!(out.card.as_deref(), Some("Entry points: main.\n"));
    assert_eq!(out.unread, Some(vec!["src/b.rs".to_string()]));
    let file = out.checklist.expect("checklist parsed");
    assert_eq!(file.citations[0].problem, 1);
    assert_eq!(
        file.citations[0].rule, "ARCH-ALPHA-1",
        "rule ids are upper-cased"
    );
    assert_eq!(file.answers.len(), 4);
    assert_eq!(file.answers[0].kind, AnswerKind::Problems);
    assert_eq!(file.answers[1].kind, AnswerKind::Clean);
    assert_eq!(file.answers[1].places.len(), 3);
    assert_eq!(file.answers[1].places[1].path, "src/b.rs");
    assert_eq!(file.answers[1].places[1].line, Some(9));
    assert_eq!(file.answers[1].places[2].line, None);
    assert_eq!(file.answers[2].kind, AnswerKind::NotApplicable);
    assert_eq!(file.answers[3].options[0].count, 3);
    assert_eq!(file.answers[3].options[0].places.len(), 2);
}

#[test]
fn a_bad_checklist_answer_makes_checklist_json_unusable() {
    let bad = serde_json::json!({"answers": [{"rule": "R-1", "answer": "maybe"}]}).to_string();
    let out = parse_outputs(&outputs(&[("checklist.json", bad)]), &piece_files());
    assert!(out.checklist.is_none());
    assert!(
        out.unusable[0].reason.contains("answer"),
        "{:?}",
        out.unusable
    );
    let out = parse_outputs(
        &outputs(&[("unread.json", "{\"x\": 1}".to_string())]),
        &piece_files(),
    );
    assert!(out.unread.is_none());
    assert_eq!(out.unusable.len(), 1);
}

#[test]
fn a_cited_problem_carries_the_corpus_rule_id() {
    let adopted: HashSet<String> = ["ARCH-STRICT-LAYERING-1".to_string()].into();
    let citations: BTreeMap<u32, Vec<String>> =
        [(1, vec!["ARCH-STRICT-LAYERING-1".to_string()])].into();
    let (findings, _) = to_findings(
        "acme/app",
        "acme/app#1:abcd1234",
        &[problem_for(1, "layering")],
        &citations,
        &adopted,
    );
    assert_eq!(findings.len(), 1);
    let f = &findings[0];
    assert_eq!(f.rule_id, "ARCH-STRICT-LAYERING-1");
    assert_eq!(f.repo, "acme/app");
    assert_eq!(f.path, "src/a.rs");
    assert_eq!(f.line, 12);
    assert_eq!(f.severity, "high");
    assert_eq!(f.snippet, "db.delete(id)");
    assert!(f.detail.contains("Handler skips the authorization check"));
    assert!(f
        .detail
        .contains("The route deletes without checking the caller."));
    assert!(f.detail.contains("Any caller can delete any record."));
}

#[test]
fn an_uncited_problem_gets_an_ai_id_from_its_label() {
    let adopted: HashSet<String> = ["ARCH-STRICT-LAYERING-1".to_string()].into();
    let (findings, _) = to_findings(
        "acme/app",
        "k",
        &[problem_for(1, "missing authorization")],
        &BTreeMap::new(),
        &adopted,
    );
    assert_eq!(findings[0].rule_id, "AI-MISSING-AUTHORIZATION");
}

#[test]
fn fix_end_line_piece_key_sure_and_effort_ride_as_captures() {
    let (findings, _) = to_findings(
        "acme/app",
        "acme/app#2:ffff0000",
        &[problem_for(7, "x y")],
        &BTreeMap::new(),
        &HashSet::new(),
    );
    let c = &findings[0].captures;
    assert_eq!(
        c.get("finder-fix").map(String::as_str),
        Some("Check the caller owns the record.")
    );
    assert_eq!(c.get("finder-end-line").map(String::as_str), Some("14"));
    assert_eq!(
        c.get("finder-piece").map(String::as_str),
        Some("acme/app#2:ffff0000")
    );
    assert_eq!(c.get("finder-sure").map(String::as_str), Some("yes"));
    assert_eq!(c.get("finder-effort").map(String::as_str), Some("low"));
    assert_eq!(c.get("finder-problem").map(String::as_str), Some("7"));
}

#[test]
fn an_em_dash_in_model_prose_does_not_survive() {
    let mut p = problem_for(1, "label");
    p.title = "Missing check \u{2014} on delete".into();
    p.what = "The route skips it \u{2014} always.".into();
    p.fix = "Add it \u{2014} then test.".into();
    let (findings, _) = to_findings("acme/app", "k", &[p], &BTreeMap::new(), &HashSet::new());
    let f = &findings[0];
    // The one em dash the engine writes itself is the separator between the headline and the
    // detail; the dashes the model wrote must be gone from both sides of it and from the fix.
    let (headline, rest) = f
        .detail
        .split_once(" \u{2014} ")
        .expect("headline separator");
    let all = format!("{headline} {rest} {} {:?}", f.snippet, f.captures);
    assert!(
        !all.contains('\u{2014}') && !all.contains('\u{2013}'),
        "{all}"
    );
    assert!(headline.contains("Missing check"), "{headline}");
}

#[test]
fn the_scan_reply_names_the_cited_rule_or_the_label() {
    let citations: BTreeMap<u32, Vec<String>> = [(1, vec!["RUST-ZED-1".to_string()])].into();
    let reply = scan_reply(
        &[problem_for(1, "ignored label"), problem_for(2, "own label")],
        &citations,
    );
    let v: serde_json::Value = serde_json::from_str(&reply).expect("valid json");
    let list = v["findings"].as_array().expect("findings array");
    assert_eq!(list.len(), 2);
    assert_eq!(list[0]["rule"], "RUST-ZED-1");
    assert_eq!(list[1]["rule"], "own label");
    assert_eq!(list[0]["path"], "src/a.rs");
    assert_eq!(list[0]["line"], 12);
    assert_eq!(list[0]["code"], "db.delete(id)");
}

#[test]
fn a_secret_in_the_models_quote_is_redacted_by_the_parser() {
    let secret = "sk_live_4eC39HqLyjWDarjtT1zdp7dcAbCdEfGhIjKl";
    let mut p = problem_for(1, "hardcoded key");
    p.code = format!("const KEY = \"{secret}\";");
    let (findings, _) = to_findings("acme/app", "k", &[p], &BTreeMap::new(), &HashSet::new());
    assert!(!serde_json::to_string(&findings[0])
        .expect("serializes")
        .contains(secret));
}

const ALL_FINDER_KEYS: [&str; 7] = [
    FINDER_PIECE_CAPTURE,
    FINDER_SURE_CAPTURE,
    FINDER_EFFORT_CAPTURE,
    FINDER_FIX_CAPTURE,
    FINDER_PROBLEM_CAPTURE,
    FINDER_END_LINE_CAPTURE,
    FINDER_RULES_CAPTURE,
];

#[test]
fn every_finder_capture_key_is_named_once_and_listed_as_bookkeeping() {
    let names = [
        "finder-piece",
        "finder-sure",
        "finder-effort",
        "finder-fix",
        "finder-problem",
        "finder-end-line",
        "finder-rules",
    ];
    assert_eq!(ALL_FINDER_KEYS, names);
    for key in ALL_FINDER_KEYS {
        assert!(
            crate::ai_audit::BOOKKEEPING_CAPTURE_KEYS.contains(&key),
            "{key} is not a bookkeeping key"
        );
    }
}

#[test]
fn two_unrelated_problems_from_one_piece_are_not_one_cause() {
    let mut a = problem_for(1, "missing authorization");
    a.path = "src/a.rs".into();
    let mut b = problem_for(2, "weak hashing");
    b.path = "src/b.rs".into();
    b.title = "Passwords are hashed with MD5".into();
    b.what = "The hash is fast and unsalted.".into();
    b.why = "Stolen hashes fall quickly.".into();
    b.code = "md5(password)".into();
    b.fix = "Use a slow salted hash.".into();
    let (findings, _) = to_findings(
        "acme/app",
        "acme/app#1:abcd1234",
        &[a, b],
        &BTreeMap::new(),
        &HashSet::new(),
    );
    assert_eq!(findings.len(), 2);
    // Both carry the same piece, sureness and effort: that is bookkeeping, not an object.
    assert_eq!(
        findings[0].captures.get(FINDER_PIECE_CAPTURE),
        findings[1].captures.get(FINDER_PIECE_CAPTURE)
    );
    assert!(
        !crate::ai_audit::finding_merge::shared_captured_object(&findings[0], &findings[1]),
        "the finder captures linked two problems"
    );
    assert!(
        crate::ai_audit::finding_merge::same_cause_signal(&findings[0], &findings[1], None)
            .is_none(),
        "two unrelated agent problems were read as one cause"
    );
}

#[test]
fn a_problems_json_file_is_unusable_never_ignored_and_counted_files_are_exact() {
    let one = serde_json::json!([entry(1, "src/a.rs")]).to_string();
    // A stray name that does NOT parse as a problems list is never ignored.
    let out = parse_outputs(
        &outputs(&[("problems.json", "{\"note\": 1}".to_string())]),
        &piece_files(),
    );
    assert!(out.problems.is_empty());
    assert_eq!(out.unusable.len(), 1, "{:?}", out.unusable);
    assert!(out.unusable[0].file.ends_with("problems.json"));
    assert_eq!(
        out.problems_files, 0,
        "problems.json is not a problems file"
    );
    // An intended change (round 2): a stray name that parses as a problems list is read as one.
    let out = parse_outputs(&outputs(&[("problems.json", one.clone())]), &piece_files());
    assert_eq!(out.problems.len(), 1);
    assert!(out.unusable.is_empty(), "{:?}", out.unusable);
    assert_eq!(out.problems_files, 1);

    // A file nobody reads, in a folder or at the top, is visible too.
    let out = parse_outputs(
        &outputs(&[
            ("notes.txt", "x".to_string()),
            ("sub/problems-1.json", one.clone()),
            ("card.md", "c".to_string()),
        ]),
        &piece_files(),
    );
    assert_eq!(out.unusable.len(), 2, "{:?}", out.unusable);
    assert!(out.card.is_some());
    assert_eq!(out.problems_files, 0);

    // No output at all: zero problems files, so the runner can fail the piece.
    let none = parse_outputs(&BTreeMap::new(), &piece_files());
    assert_eq!(none.problems_files, 0);
    assert!(none.unusable.is_empty());

    // Two problems files, one of them unusable: both were seen.
    let two = parse_outputs(
        &outputs(&[
            ("problems-1.json", one),
            ("problems-2.json", "{not json".to_string()),
        ]),
        &piece_files(),
    );
    assert_eq!(two.problems_files, 2);
    assert_eq!(two.unusable.len(), 1);
}

#[test]
fn leads_are_numbered_keep_their_source_problem_and_are_cleaned() {
    let secret = "sk_live_4eC39HqLyjWDarjtT1zdp7dcAbCdEfGhIjKl";
    let mut moved = entry(2, "src/context_only.rs");
    moved["what"] = serde_json::json!("It skips the check \u{2014} always.");
    let problems = serde_json::json!([entry(1, "src/a.rs"), moved]).to_string();
    let leads = serde_json::json!([
        {"path": "lib/x.rs", "line": 5, "what": format!("Key {secret} \u{2013} in the open")},
        {"path": "lib/y.rs", "what": "no line given"}
    ])
    .to_string();
    let out = parse_outputs(
        &outputs(&[("problems-1.json", problems), ("leads.json", leads)]),
        &piece_files(),
    );
    assert_eq!(out.leads.len(), 3);
    assert_eq!(
        out.leads.iter().map(|l| l.n).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(out.leads[0].source_problem, Some(2));
    assert_eq!(out.leads[1].source_problem, None);
    for lead in &out.leads {
        assert!(
            !lead.what.contains('\u{2014}') && !lead.what.contains('\u{2013}'),
            "{lead:?}"
        );
        assert!(!lead.what.contains(secret), "{lead:?}");
    }
    assert!(out.leads[1].what.contains("Key"), "{:?}", out.leads[1]);
}

fn free_text_adopted() -> HashSet<String> {
    [
        "JAVASCRIPT-TYPESCRIPT-NO-NON-NULL-ASSERTION-1".to_string(),
        "SEC-NO-RAW-SQL-CONCAT-1".to_string(),
        "ARCH-STRICT-LAYERING-1".to_string(),
    ]
    .into()
}

#[test]
fn a_free_text_label_is_never_matched_to_a_rule_however_close() {
    let adopted = free_text_adopted();
    for label in [
        // A typo of an adopted id.
        "JAVASCRIPT-TYPESRIPT-NO-NON-NULL-ASSERTION-1",
        // An adopted id without its version suffix.
        "SEC-NO-RAW-SQL-CONCAT",
        // Words that spell an adopted id.
        "sec no raw sql concat 1",
    ] {
        let (findings, _) = to_findings(
            "acme/app",
            "k",
            &[problem_for(1, label)],
            &BTreeMap::new(),
            &adopted,
        );
        assert!(
            findings[0].rule_id.starts_with("AI-"),
            "label {label:?} cited {}",
            findings[0].rule_id
        );
        assert!(!adopted.contains(&findings[0].rule_id), "{label:?}");
    }
}

#[test]
fn only_an_exact_rule_id_as_the_label_cites_a_rule() {
    let adopted = free_text_adopted();
    for label in ["ARCH-STRICT-LAYERING-1", "arch-strict-layering-1"] {
        let (findings, _) = to_findings(
            "acme/app",
            "k",
            &[problem_for(1, label)],
            &BTreeMap::new(),
            &adopted,
        );
        assert_eq!(findings[0].rule_id, "ARCH-STRICT-LAYERING-1", "{label:?}");
    }
}

#[test]
fn every_rule_a_problem_cites_is_recorded_not_only_the_first() {
    let adopted: HashSet<String> = [
        "ARCH-STRICT-LAYERING-1".to_string(),
        "SEC-NO-RAW-SQL-CONCAT-1".to_string(),
    ]
    .into();
    let cites = vec![
        super::checklist::Citation {
            problem: 1,
            rule: "ARCH-STRICT-LAYERING-1".into(),
        },
        super::checklist::Citation {
            problem: 1,
            rule: "SEC-NO-RAW-SQL-CONCAT-1".into(),
        },
        super::checklist::Citation {
            problem: 1,
            rule: "arch-strict-layering-1".into(),
        },
    ];
    let map = citation_map(&super::checklist::Checklist::default(), &cites);
    assert_eq!(
        map[&1],
        vec![
            "ARCH-STRICT-LAYERING-1".to_string(),
            "SEC-NO-RAW-SQL-CONCAT-1".to_string()
        ],
        "in the order cited, none twice"
    );
    let (findings, _) = to_findings("acme/app", "k", &[problem_for(1, "x y")], &map, &adopted);
    let f = &findings[0];
    assert_eq!(f.rule_id, "ARCH-STRICT-LAYERING-1");
    assert_eq!(f.also_matches, vec!["SEC-NO-RAW-SQL-CONCAT-1".to_string()]);
    assert_eq!(
        f.captures.get(FINDER_RULES_CAPTURE).map(String::as_str),
        Some("ARCH-STRICT-LAYERING-1,SEC-NO-RAW-SQL-CONCAT-1")
    );
}

fn lead(n: u32, path: &str) -> super::reply::Lead {
    super::reply::Lead {
        n,
        path: path.into(),
        line: 5,
        what: "w".into(),
        source_problem: None,
        severity: None,
    }
}

#[test]
fn every_lead_must_be_answered_and_an_unanswered_one_is_reported() {
    let leads = vec![
        lead(1, "lib/x.rs"),
        lead(2, "lib/y.rs"),
        lead(3, "lib/z.rs"),
    ];
    let problems = vec![(10, "lib/x.rs".to_string()), (11, "lib/y.rs".to_string())];
    let text = serde_json::json!([
        {"lead": 1, "covered_by": 10},
        {"lead": 2, "new_problem": 11}
    ])
    .to_string();
    let a = parse_leads_answers(Some(&text), &leads, &problems);
    assert_eq!(a.answered.len(), 2);
    assert_eq!(a.answered[0].outcome, LeadOutcome::CoveredBy(10));
    assert_eq!(a.answered[1].outcome, LeadOutcome::NewProblem(11));
    assert_eq!(a.unanswered, vec![3]);
    assert_eq!(a.unresolved(), vec![3]);
    assert!(a.unusable.is_none());

    // No file at all: every lead is unanswered.
    let none = parse_leads_answers(None, &leads, &problems);
    assert_eq!(none.unanswered, vec![1, 2, 3]);
    // Not JSON: every lead is unanswered and the reason is kept.
    let bad = parse_leads_answers(Some("{nope"), &leads, &problems);
    assert_eq!(bad.unanswered, vec![1, 2, 3]);
    assert!(bad.unusable.is_some());
}

#[test]
fn a_covered_by_answer_must_name_a_problem_at_the_leads_file() {
    let leads = vec![
        lead(1, "lib/x.rs"),
        lead(2, "lib/y.rs"),
        lead(3, "lib/z.rs"),
    ];
    let problems = vec![(10, "lib/other.rs".to_string())];
    let text = serde_json::json!({"answers": [
        {"lead": 1, "covered_by": 10},
        {"lead": 2, "covered_by": 99},
        {"lead": 3},
        {"lead": 7, "covered_by": 10}
    ]})
    .to_string();
    let a = parse_leads_answers(Some(&text), &leads, &problems);
    assert!(a.answered.is_empty());
    assert_eq!(a.invalid.len(), 3, "{:?}", a.invalid);
    assert_eq!(a.unknown_leads, vec![7]);
    assert_eq!(a.unresolved(), vec![1, 2, 3]);
}

fn bounded_rule(id: &str, min: Option<&str>, max: Option<&str>) -> super::checklist::ChecklistRule {
    super::checklist::ChecklistRule {
        id: id.to_string(),
        title: id.to_string(),
        directive: "d".to_string(),
        min_severity: min.map(str::to_string),
        max_severity: max.map(str::to_string),
        options: Vec::new(),
    }
}

fn cite(problem: u32, rule: &str) -> super::checklist::Citation {
    super::checklist::Citation {
        problem,
        rule: rule.to_string(),
    }
}

#[test]
fn the_strictest_cited_rule_is_first_whatever_order_the_model_cited_in() {
    let checklist = super::checklist::Checklist {
        rules: vec![
            bounded_rule("A-CAPPED-1", None, Some("medium")),
            bounded_rule("B-FLOOR-1", Some("high"), None),
            bounded_rule("C-HIGHER-FLOOR-1", Some("critical"), None),
            bounded_rule("D-PLAIN-1", None, None),
        ],
    };
    let forward = citation_map(&checklist, &[cite(1, "A-CAPPED-1"), cite(1, "B-FLOOR-1")]);
    let backward = citation_map(&checklist, &[cite(1, "B-FLOOR-1"), cite(1, "A-CAPPED-1")]);
    assert_eq!(forward[&1][0], "B-FLOOR-1");
    assert_eq!(backward[&1][0], "B-FLOOR-1");
    assert_eq!(forward, backward);
    // No maximum before a maximum, then the higher minimum, then the id.
    let all = citation_map(
        &checklist,
        &[
            cite(2, "A-CAPPED-1"),
            cite(2, "D-PLAIN-1"),
            cite(2, "B-FLOOR-1"),
            cite(2, "C-HIGHER-FLOOR-1"),
        ],
    );
    assert_eq!(
        all[&2],
        vec!["C-HIGHER-FLOOR-1", "B-FLOOR-1", "D-PLAIN-1", "A-CAPPED-1"]
    );
}

#[test]
fn the_finding_takes_its_rule_from_the_strictest_citation() {
    let checklist = super::checklist::Checklist {
        rules: vec![
            bounded_rule("A-CAPPED-1", None, Some("medium")),
            bounded_rule("B-FLOOR-1", Some("high"), None),
        ],
    };
    let adopted: HashSet<String> = ["A-CAPPED-1".to_string(), "B-FLOOR-1".to_string()].into();
    let map = citation_map(&checklist, &[cite(1, "A-CAPPED-1"), cite(1, "B-FLOOR-1")]);
    let (findings, _) = to_findings("acme/app", "k", &[problem_for(1, "x y")], &map, &adopted);
    assert_eq!(findings[0].rule_id, "B-FLOOR-1");
    assert_eq!(findings[0].also_matches, vec!["A-CAPPED-1".to_string()]);
}
