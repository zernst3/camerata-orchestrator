//! Reading the output files and turning problems into findings (design section 2).

use std::collections::{BTreeMap, HashSet};

use super::checklist::AnswerKind;
use super::reply::{parse_outputs, scan_reply, to_findings, Problem};

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
    let citations: BTreeMap<u32, String> = [(1, "ARCH-STRICT-LAYERING-1".to_string())].into();
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
    let citations: BTreeMap<u32, String> = [(1, "RUST-ZED-1".to_string())].into();
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
