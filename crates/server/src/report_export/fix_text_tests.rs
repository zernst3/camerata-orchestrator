//! Fix text in the report data and the PDF: the rule TOML's hard line wraps never reach a card,
//! blank lines still split paragraphs, and backtick spans reach Typst as inline raw (mono).

use std::collections::HashMap;

use super::tests::{empty_opts, finding, report_with, which_typst};
use super::*;

const SERVICE_ROLE: &str = "SUPABASE-KEY-SERVICE-ROLE-CLIENT-1";

async fn corpus() -> camerata_rules::RuleSet {
    let (corpus, errors) =
        camerata_rules::load_corpus_lenient(&camerata_rules::corpus_path()).await;
    assert!(errors.is_empty(), "{errors:?}");
    corpus
}

/// The shipped rule's remediation is hard-wrapped in its TOML, with backtick spans.
#[tokio::test]
async fn resolve_fix_returns_the_authored_remediation_as_one_paragraph() {
    let corpus = corpus().await;
    let f = finding(SERVICE_ROLE, "apps/web/.env.production", 3, "critical");
    let fix = resolve_fix(SERVICE_ROLE, Some(&corpus), &f, None).expect("authored remediation");
    assert!(!fix.contains('\n'), "hard wraps must be joined: {fix:?}");
    // Backtick spans survive for the PDF to set as mono; the placeholder is filled first.
    assert!(fix.contains("`apps/web/.env.production`"), "{fix:?}");
    assert!(fix.contains("`VITE_`, `REACT_APP_`"), "{fix:?}");
}

#[tokio::test]
async fn the_pdf_data_joins_a_hard_wrapped_specific_fix_and_keeps_paragraph_breaks() {
    let corpus = corpus().await;
    let mut f = finding(SERVICE_ROLE, "apps/web/.env.production", 3, "critical");
    f.fix_specific = Some(
        "Move the key out of `apps/web/.env.production`\nand treat it as\ncompromised.\n\nRedeploy."
            .to_string(),
    );
    let report = report_with(vec![f], vec![SERVICE_ROLE]).seal();
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report, &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );
    let site = &json.curated_findings[0].sites[0];
    assert_eq!(
        site.fix_for_this_finding.as_deref(),
        Some("Move the key out of `apps/web/.env.production` and treat it as compromised.\n\nRedeploy.")
    );
    let generic = site.fix.as_deref().expect("generic guidance");
    assert!(!generic.contains('\n'), "{generic:?}");
}

/// A specific fix that is just the rule's own text (the give-up path) is still suppressed from
/// "General guidance" when the two are the same sentence, even if the model text was wrapped.
#[tokio::test]
async fn an_identical_wrapped_specific_fix_is_not_repeated_as_general_guidance() {
    let corpus = corpus().await;
    let f0 = finding(SERVICE_ROLE, "apps/web/.env.production", 3, "critical");
    let generic = resolve_fix(SERVICE_ROLE, Some(&corpus), &f0, None).unwrap();
    let mut f = f0.clone();
    f.fix_specific = Some(generic.replace(". ", ".\n"));
    let report = report_with(vec![f], vec![SERVICE_ROLE]).seal();
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report, &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );
    let site = &json.curated_findings[0].sites[0];
    assert_eq!(site.fix, None, "{:?}", site.fix);
}

/// The PDF prints a fix only for an escalated place, in two sentences, as plain text (the rows the
/// PDF is built from carry `fix_text::plain`, the form the workbook prints): the backticks of a
/// code span never reach the document.
#[tokio::test]
async fn a_code_span_in_fix_text_prints_without_its_backticks() {
    if which_typst().is_none() {
        eprintln!("skipping: typst is not on PATH");
        return;
    }
    let corpus = corpus().await;
    let mut f = finding(SERVICE_ROLE, "apps/web/.env.production", 3, "critical");
    f.fix_specific = Some(
        "Move the key out of `apps/web/.env.production` and rename `VITE_KEY`.\n\nRedeploy afterwards."
            .to_string(),
    );
    f.review.escalation_state = camerata_api_types::review::EscalationState::Escalated;
    let report = report_with(vec![f], vec![SERVICE_ROLE]).seal();
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report, &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );
    assert_eq!(
        json.escalations.items[0].what_to_do,
        "Move the key out of apps/web/.env.production and rename VITE_KEY. Redeploy afterwards."
    );
    let data = render_data_json(&json).unwrap();
    let pdf = crate::report_export::compile_pdf_from_data(&data)
        .await
        .expect("the PDF compiles");
    let text = pdf_extract::extract_text_from_mem(&pdf).expect("the PDF's text extracts");
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        !flat.contains('`'),
        "a literal backtick reached the document"
    );
    assert!(
        flat.contains("rename VITE_KEY. Redeploy afterwards."),
        "the fix is printed in two sentences: {flat}"
    );
}
