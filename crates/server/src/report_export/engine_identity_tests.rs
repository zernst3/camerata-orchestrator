//! H4 item 2: the engine version, the git commit it was built from and the rule-library lock
//! hash reach the PDF, `findings.json`, the README and `manifest.json`, reusing the existing
//! fields (`engine_version`, `corpus_version`) rather than adding parallel ones.

use std::collections::HashMap;

use super::tests::{empty_opts, finding, report_with, which_typst};
use super::*;

async fn corpus() -> camerata_rules::RuleSet {
    let (corpus, errors) =
        camerata_rules::load_corpus_lenient(&camerata_rules::corpus_path()).await;
    assert!(errors.is_empty(), "{errors:?}");
    corpus
}

fn sample() -> crate::onboard::SealedReport {
    report_with(
        vec![finding(
            "SEC-NO-HARDCODED-SECRETS-1",
            "src/a.rs",
            10,
            "high",
        )],
        vec!["SEC-NO-HARDCODED-SECRETS-1"],
    )
    .seal()
}

/// In a git checkout the build script records the commit; outside one it records none, and
/// every surface then simply omits it.
#[test]
fn the_build_commit_is_recorded_in_a_git_checkout() {
    let workspace_git = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.git");
    if workspace_git.exists() {
        let commit = crate::ENGINE_COMMIT.expect("a build in a git checkout records its commit");
        assert!(commit.len() == 40 || commit.len() == 64, "{commit}");
        assert!(commit.bytes().all(|b| b.is_ascii_hexdigit()), "{commit}");
    }
}

#[test]
fn the_engine_version_names_the_build_commit() {
    let version = crate::ManifestProvenance::default().engine_version();
    match crate::ENGINE_COMMIT {
        Some(commit) => assert_eq!(
            version,
            format!("camerata {} (commit {commit})", env!("CARGO_PKG_VERSION"))
        ),
        None => assert_eq!(version, format!("camerata {}", env!("CARGO_PKG_VERSION"))),
    }
}

#[tokio::test]
async fn the_cover_carries_the_engine_commit_and_the_rule_library_lock_hash() {
    let corpus = corpus().await;
    let json = build_report_json(&sample(), &HashMap::new(), Some(&corpus), &empty_opts());
    assert_eq!(
        json.cover.engine_commit.as_deref(),
        crate::ENGINE_COMMIT,
        "the commit the exporting binary was built from"
    );
    let lock_hash = camerata_rules::corpus_version(&corpus);
    assert!(lock_hash.starts_with("sha256:"));
    assert_eq!(
        json.cover.corpus_version.as_deref(),
        Some(lock_hash.as_str())
    );

    // No corpus: no hash, never a fabricated one.
    let bare = build_report_json(&sample(), &HashMap::new(), None, &empty_opts());
    assert_eq!(bare.cover.corpus_version, None);
}

#[tokio::test]
async fn findings_json_provenance_carries_the_engine_identity() {
    let corpus = corpus().await;
    let report = sample();
    let json = build_report_json(&report, &HashMap::new(), Some(&corpus), &empty_opts());
    let export = crate::xlsx_export::build_findings_export(
        &report,
        &HashMap::new(),
        Some(&corpus),
        &json,
        &HashMap::new(),
    );
    let value = serde_json::to_value(&export).unwrap();
    let provenance = &value["provenance"];
    assert_eq!(
        provenance["corpus_version"],
        camerata_rules::corpus_version(&corpus).as_str()
    );
    assert_eq!(
        provenance["engine_commit"].as_str(),
        crate::ENGINE_COMMIT,
        "{provenance}"
    );
}

fn provenance_engine_version() -> String {
    crate::ManifestProvenance::default().engine_version()
}

#[tokio::test]
async fn the_readme_and_manifest_state_the_engine_version_with_its_commit() {
    let corpus = corpus().await;
    let report = sample();
    let json = build_report_json(&report, &HashMap::new(), Some(&corpus), &empty_opts());
    let provenance =
        crate::client_provenance::export_provenance(&report, &json.cover, Some(&corpus));
    let readme = crate::product_export_readme("stem", &provenance);
    assert!(
        readme.contains(&format!(
            "Engine version: {}\n",
            provenance_engine_version()
        )),
        "{readme}"
    );
    assert!(readme.contains(&format!(
        "Corpus version: {}\n",
        camerata_rules::corpus_version(&corpus)
    )));
    let manifest = provenance.inspection_json();
    assert_eq!(manifest["engine_version"], provenance_engine_version());
    assert_eq!(
        manifest["corpus_version"],
        camerata_rules::corpus_version(&corpus).as_str()
    );
}

#[tokio::test]
async fn the_pdf_states_the_engine_commit_and_the_rule_library_lock_hash() {
    if which_typst().is_none() {
        eprintln!("skipping the_pdf_states_the_engine_commit_and_the_rule_library_lock_hash: typst not on PATH");
        return;
    }
    let corpus = corpus().await;
    let json = build_report_json(&sample(), &HashMap::new(), Some(&corpus), &empty_opts());
    let pdf = compile_pdf(&json).await.expect("the PDF compiles");
    let text: String = pdf_extract::extract_text_from_mem(&pdf)
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(text.contains("Rule library lock hash"), "{text}");
    // Long hex strings may wrap across lines in the PDF; the first 16 characters do not.
    let lock_hash = camerata_rules::corpus_version(&corpus);
    assert!(text.contains(&lock_hash[..23]), "{text}");
    if let Some(commit) = crate::ENGINE_COMMIT {
        assert!(text.contains(&commit[..12]), "{text}");
    }
}
