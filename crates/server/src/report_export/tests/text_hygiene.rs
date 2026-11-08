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
    // The snippets are the client's own code and are not prose: see
    // `a_snippet_is_the_clients_own_code_and_ships_byte_identical`.
    let mut fields = vec![
        clean.detail.clone(),
        clean.fix_specific.clone().unwrap_or_default(),
        clean.calibration_rationale.clone().unwrap_or_default(),
    ];
    for site in &clean.also_locations {
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

#[test]
fn a_snippet_is_the_clients_own_code_and_ships_byte_identical() {
    // CSS with `margin:0;` and a comment with an em dash: neither is ours to rewrite.
    let code = format!("a {{ margin:0; }} // {EM} keep the reset");
    let mut f = row("AI-X", "Title");
    f.snippet = code.clone();
    f.also_locations.push(crate::onboard::MergedLocation::site(
        "owner/repo",
        "src/b.css",
        3,
        "AI-X",
        code.clone(),
        false,
    ));
    let clean = sanitize_finding_text(&f);
    assert_eq!(clean.snippet, code);
    assert_eq!(clean.also_locations[0].snippet, code);
    // Zero-width characters are still stripped from it, as before.
    f.snippet = format!("{code}\u{200B}");
    assert_eq!(sanitize_finding_text(&f).snippet, code);
}

#[test]
fn the_site_key_of_a_finding_is_not_moved_by_the_normaliser() {
    // The snippet is the fingerprint input of the site key, so a baseline or a decision keyed on
    // it must survive the export's sanitising.
    let mut f = row("AI-X", "Title");
    f.snippet = format!("a {{ margin:0; }} // {EM} keep the reset");
    f.site_key = String::new();
    let sanitized = sanitize_finding_text(&f);
    assert_eq!(finding_key(&sanitized), finding_key(&f));
    assert_eq!(
        finding_key(&f),
        camerata_api_types::site_key::site_key(&f.repo, &f.path, &f.rule_id, &f.snippet, 0)
    );
}
