//! A problem in a whole file reads "whole file" wherever a place is printed, never "line 0" and
//! never `path:0`. (The release gate refuses the same shapes in a finished file:
//! `release_gate::text_style_tests`.)

use std::collections::HashMap;

use super::*;

#[test]
fn a_site_in_a_whole_file_is_named_without_a_line() {
    let whole = crate::onboard::MergedLocation::site(
        "owner/repo",
        ".env.production",
        0,
        "SEC-NO-SECRET-FILE-1",
        "",
        false,
    );
    let sentence = flagged_sentence(&whole, false, Some("A secret file"));
    assert!(
        sentence.contains("at .env.production (whole file)"),
        "{sentence}"
    );
    assert!(!sentence.contains(":0"), "{sentence}");
    let placed =
        crate::onboard::MergedLocation::site("owner/repo", "src/a.ts", 12, "SEC-X", "", false);
    assert!(flagged_sentence(&placed, false, Some("X")).contains("at src/a.ts:12"));
}

#[tokio::test]
async fn the_pdf_says_whole_file_and_never_line_zero() {
    if which_typst().is_none() {
        eprintln!("skipping the_pdf_says_whole_file_and_never_line_zero: typst not on PATH");
        return;
    }
    let mut whole = finding("SEC-NO-SECRET-FILE-1", ".env.production", 0, "critical");
    whole.detail = "The file holds a live key.".to_string();
    let report = report_with(vec![whole], vec!["SEC-NO-SECRET-FILE-1"]).seal();
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report, &HashMap::new()),
        None,
        &empty_opts(),
    );
    let pdf = compile_pdf(&json).await.expect("the PDF compiles");
    let text = pdf_extract::extract_text_from_mem(&pdf).unwrap_or_default();
    let joined: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(joined.contains(".env.production,wholefile"), "{text}");
    assert!(!joined.contains("line0"), "{text}");
    assert!(!joined.contains(".env.production:0"), "{text}");
}

#[test]
fn the_template_prints_no_line_number_of_its_own() {
    // Every place the PDF prints arrives as text from the vocabulary (`place_text`), which says
    // "whole file" for line 0. The template never formats a line number itself, so it can never
    // print "line 0" or `path:0`.
    let template = include_str!("../../../templates/audit_report.typ");
    for line in template
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
    {
        assert!(
            !(line.contains("str(") && (line.contains(".line") || line.contains("(line)"))),
            "the template prints a line number itself: {line}"
        );
    }
}
