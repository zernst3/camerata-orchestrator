//! Model-written prose is rewritten as it is parsed: a reply with an em dash, an en dash or a
//! multiplication sign ships without one (`client_text::plain`). The model's verbatim quote of
//! the code (`code`) is the one text left as it was written, because the evidence check compares
//! it with the file.

use super::alternatives::trimmed_evidence;
use super::detail_regen::parse_detail_reply;
use super::fix_stage::parse_fix_specifics;
use super::group_review::parse_rule_summary;
use super::*;
use crate::client_text::banned_in;
use serde_json::json;

const EM: &str = "\u{2014}";

fn clean(text: &str) {
    assert!(banned_in(text).is_empty(), "{text}");
}

#[test]
fn a_scan_reply_is_rewritten_but_the_quoted_code_is_left_as_written() {
    let reply = json!({ "findings": [{
        "rule": "SEC-X-1",
        "title": format!("Any user can read any invoice {EM} the handler never checks"),
        "detail": format!("The query takes the id from the URL{EM}and trusts it."),
        "code": format!("// {EM} trusted input"),
        "path": "a.ts",
        "line": 3,
        "severity": "high"
    }]})
    .to_string();
    let adopted = std::collections::HashSet::new();
    let (findings, _) = parse_ai_findings("acme/app", &reply, &adopted);
    assert_eq!(findings.len(), 1);
    let f = &findings[0];
    // The engine's own title/explanation separator is the one dash that stays: the detail stage
    // splits on it, and the export joins it to a colon.
    let (title, explanation) = f.detail.split_once(HEADLINE_SEP).expect("the separator");
    clean(title);
    clean(explanation);
    // The title's own em dash is the engine's separator character, which the parser has always
    // written as a hyphen so a title cannot move the headline boundary.
    assert_eq!(
        title,
        "Any user can read any invoice - the handler never checks"
    );
    assert_eq!(
        explanation,
        "The query takes the id from the URL, and trusts it."
    );
    assert_eq!(
        f.snippet,
        format!("// {EM} trusted input"),
        "the quote is verbatim"
    );
}

#[test]
fn a_fix_reply_is_rewritten() {
    let reply = json!({ "fixes": [
        { "id": "a", "fix": format!("Use a limit{EM}and a cursor {EM} see lib/page.ts") }
    ]})
    .to_string();
    let fixes = parse_fix_specifics(&reply);
    clean(&fixes["a"]);
    assert_eq!(fixes["a"], "Use a limit, and a cursor, see lib/page.ts");
}

#[test]
fn a_detail_reply_is_rewritten() {
    let reply = json!({ "details": [{
        "id": "a",
        "why": format!("Anyone can read it {EM} the policy is open."),
        "fix_direction": format!("Add a USING clause {EM} owner only.")
    }]})
    .to_string();
    let parsed = parse_detail_reply(&reply, false);
    assert_eq!(parsed.entries.len(), 1);
    clean(&parsed.entries[0].detail);
    clean(parsed.entries[0].why.as_deref().unwrap_or(""));
}

#[test]
fn a_group_review_summary_is_rewritten_and_names_a_whole_file_as_one() {
    let reply = json!({ "summary": format!(
        "The sites share one cause {EM} a key in git. `.env.production:0` is the outlier."
    )})
    .to_string();
    let summary = parse_rule_summary(&reply).expect("a summary");
    clean(&summary);
    assert!(
        summary.contains(".env.production (whole file)"),
        "{summary}"
    );
    assert!(!summary.contains(":0"), "{summary}");
}

#[test]
fn a_recommendation_s_evidence_is_rewritten_and_still_names_its_place() {
    let evidence = trimmed_evidence(&json!({
        "evidence": format!("supabase/config.toml:26 {EM} the only HTTP endpoints are edge functions")
    }))
    .expect("evidence");
    clean(&evidence);
    assert_eq!(
        crate::report_export::parse_evidence_citation(&evidence),
        ("supabase/config.toml".to_string(), 26)
    );
}

#[test]
fn the_convention_observed_row_reads_without_a_dash() {
    let option = RuleOptionView {
        id: "good".to_string(),
        label: "Keys live in a vault".to_string(),
        directive: "d".to_string(),
        why: "w".to_string(),
        violates: false,
    };
    let alternatives = vec![RuleAlternatives {
        rule_id: "ARCH-X-1".to_string(),
        options: vec![option],
        selected_option_id: Some("good".to_string()),
        kind: None,
    }];
    let rec = RuleRecommendation {
        repo: String::new(),
        rule_id: "ARCH-X-1".to_string(),
        recommended_option_id: "good".to_string(),
        recommendation_reasoning: "The vault client is used everywhere.".to_string(),
        evidence: Some("lib/vault.ts:3".to_string()),
        currently_followed: Some(true),
        ..RuleRecommendation::default()
    };
    let rows = conventions_observed_findings("acme/app", &[], &[rec], &alternatives);
    assert_eq!(rows.len(), 1);
    clean(&rows[0].detail);
    assert!(
        rows[0]
            .detail
            .contains("Zero violations were observed this scan."),
        "{}",
        rows[0].detail
    );
}
