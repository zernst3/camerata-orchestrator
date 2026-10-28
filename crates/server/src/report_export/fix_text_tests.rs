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
    let json = build_report_json(&report, &HashMap::new(), Some(&corpus), &empty_opts());
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
    let json = build_report_json(&report, &HashMap::new(), Some(&corpus), &empty_opts());
    let site = &json.curated_findings[0].sites[0];
    assert_eq!(site.fix, None, "{:?}", site.fix);
}

/// Compile the shipped template with `data_json` and return Typst's raw/text query output.
fn typst_query(data_json: &str, selector: &str) -> serde_json::Value {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("data.json"), data_json).unwrap();
    std::fs::write(
        tmp.path().join("report.typ"),
        include_str!("../../templates/audit_report.typ"),
    )
    .unwrap();
    std::fs::create_dir_all(tmp.path().join("fonts")).unwrap();
    for (name, bytes) in TEMPLATE_ASSETS {
        std::fs::write(tmp.path().join(name), bytes).unwrap();
    }
    let out = std::process::Command::new("typst")
        .current_dir(tmp.path())
        .args(["query", "report.typ", selector, "--root"])
        .arg(tmp.path())
        .arg("--font-path")
        .arg(tmp.path().join("fonts"))
        .args(["--format", "json"])
        .output()
        .expect("run typst query");
    assert!(
        out.status.success(),
        "typst query failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("query output is JSON")
}

#[tokio::test]
async fn backtick_spans_in_fix_text_reach_typst_as_raw_and_paragraphs_split() {
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
    let report = report_with(vec![f], vec![SERVICE_ROLE]).seal();
    let json = build_report_json(&report, &HashMap::new(), Some(&corpus), &empty_opts());
    let data = render_data_json(&json).unwrap();

    let raws = typst_query(&data, "raw");
    let raw_texts: Vec<String> = raws
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["text"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        raw_texts.iter().any(|t| t == "VITE_KEY"),
        "the span must be inline raw, got raws: {raw_texts:?}"
    );
    assert!(
        raw_texts.iter().any(|t| t == "apps/web/.env.production"),
        "{raw_texts:?}"
    );
    // The generic guidance's spans (NEXT_PUBLIC_ etc.) are raw too.
    assert!(
        raw_texts.iter().any(|t| t == "NEXT_PUBLIC_"),
        "{raw_texts:?}"
    );
    assert!(
        raw_texts.iter().all(|t| !t.contains('`')),
        "no raw run may carry a literal backtick: {raw_texts:?}"
    );

    // No literal backtick survives in any paragraph, and "Redeploy afterwards." is a paragraph
    // of its own, apart from the one holding the first span.
    let pars: Vec<String> = typst_query(&data, "par")
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.to_string())
        .collect();
    assert!(
        pars.iter().all(|p| !p.contains('`')),
        "a literal backtick reached the document"
    );
    let with_redeploy: Vec<&String> = pars
        .iter()
        .filter(|p| p.contains("Redeploy afterwards."))
        .collect();
    assert_eq!(with_redeploy.len(), 1, "{with_redeploy:?}");
    assert!(
        !with_redeploy[0].contains("VITE_KEY"),
        "the blank line must split the paragraphs"
    );
}
