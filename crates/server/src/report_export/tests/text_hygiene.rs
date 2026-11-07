//! Text hygiene for what a client reads: no em dash, en dash or multiplication sign in the prose
//! the producers hand to the exports. The release gate holds the finished files to the same rule
//! (`release_gate::text_style_tests`); these tests hold the producers.

use super::*;
use crate::client_text::banned_in;

const EM: &str = "\u{2014}";

fn row(rule_id: &str, detail: &str) -> Finding {
    Finding {
        detail: detail.to_string(),
        snippet: format!("src/a.ts:15 {EM} an unbounded list query"),
        fix_specific: Some(format!("Use a limit{EM}and a cursor")),
        calibration_rationale: Some(format!(
            "Severity floor: a cross-tenant read {EM} escalates"
        )),
        ..finding(rule_id, "src/a.ts", 15, "high")
    }
}

#[test]
fn sanitizing_a_finding_leaves_no_banned_character_in_any_prose_field() {
    let mut f = row(
        "AI-X",
        &format!("Any user can read any invoice{EM}the handler never checks the owner"),
    );
    f.also_locations.push(
        crate::onboard::MergedLocation::site(
            "owner/repo",
            "src/b.ts",
            3,
            "AI-X",
            format!("also {EM} here"),
            false,
        )
        .with_rating("high", format!("Also {EM} there")),
    );
    let clean = sanitize_finding_text(&f);
    let mut fields = vec![
        clean.detail.clone(),
        clean.snippet.clone(),
        clean.fix_specific.clone().unwrap_or_default(),
        clean.calibration_rationale.clone().unwrap_or_default(),
    ];
    for site in &clean.also_locations {
        fields.push(site.snippet.clone());
        fields.push(site.headline.clone());
    }
    for field in fields {
        assert!(banned_in(&field).is_empty(), "{field}");
    }
}

#[test]
fn the_title_and_explanation_the_parser_joined_read_as_title_colon_explanation() {
    use crate::ai_audit::HEADLINE_SEP;
    let f = row(
        "AI-X",
        &format!("Service role key under NEXT_PUBLIC_ prefix{HEADLINE_SEP}Line 6 defines the key."),
    );
    assert_eq!(
        sanitize_finding_text(&f).detail,
        "Service role key under NEXT_PUBLIC_ prefix: Line 6 defines the key."
    );
    // A lower-case explanation joins the same way: the join is the parser's, not the model's.
    let f = row(
        "AI-X",
        &format!("Any user can read any invoice{HEADLINE_SEP}the handler never checks the owner"),
    );
    assert_eq!(
        sanitize_finding_text(&f).detail,
        "Any user can read any invoice: the handler never checks the owner"
    );
    // A label opening the explanation keeps its own colon.
    let f = row(
        "AI-X",
        &format!("Messages readable by everyone{HEADLINE_SEP}Borderline: the policy is open."),
    );
    assert_eq!(
        sanitize_finding_text(&f).detail,
        "Messages readable by everyone. Borderline: the policy is open."
    );
}

#[test]
fn a_headline_and_detail_reach_the_client_without_a_dash_or_a_hedge_label() {
    let f = row(
        "AI-X",
        &format!("Borderline: the policy {EM} on messages {EM} is open."),
    );
    let (headline, detail) = client_headline_and_detail(&f, None, "Fallback", None);
    assert!(banned_in(&headline).is_empty(), "{headline}");
    assert!(banned_in(&detail).is_empty(), "{detail}");
    assert!(
        crate::client_text::banned_headline_prefix(&headline).is_none(),
        "{headline}"
    );
    assert!(headline.starts_with("The policy"), "{headline}");
}

#[test]
fn a_citation_written_with_any_of_the_house_separators_still_names_its_place() {
    for evidence in [
        format!("supabase/config.toml:26 {EM} the only endpoints"),
        "supabase/config.toml:26 - the only endpoints".to_string(),
        "supabase/config.toml:26: the only endpoints".to_string(),
        "supabase/config.toml:26, the only endpoints".to_string(),
        "supabase/config.toml:26".to_string(),
    ] {
        assert_eq!(
            parse_evidence_citation(&evidence),
            ("supabase/config.toml".to_string(), 26),
            "{evidence}"
        );
    }
    assert_eq!(
        parse_evidence_citation("a sentence with no place"),
        (String::new(), 0)
    );
}

#[test]
fn a_title_with_nothing_after_it_keeps_the_colon_that_says_so() {
    use crate::ai_audit::HEADLINE_SEP;
    let f = row("AI-X", &format!("Raw SQL{HEADLINE_SEP}"));
    assert_eq!(sanitize_finding_text(&f).detail, "Raw SQL:");
    // A detail with no separator at all is left alone.
    let f = row("AI-X", "Raw SQL without a join");
    assert_eq!(sanitize_finding_text(&f).detail, "Raw SQL without a join");
}
