//! H4 item 2: the engine version, the git commit it was built from and the rule-library lock
//! hash reach the PDF, `json/inspection.json`, the README and `manifest.json`, reusing the existing
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
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&sample(), &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );
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
    let bare = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&sample(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    assert_eq!(bare.cover.corpus_version, None);
}

#[tokio::test]
async fn findings_json_provenance_carries_the_engine_identity() {
    let corpus = corpus().await;
    let report = sample();
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report, &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );
    let export = crate::xlsx_export::build_findings_export(
        &crate::report_export::ReviewedReport::unreviewed(&report, &HashMap::new()),
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
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report, &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );
    let provenance =
        crate::client_provenance::export_provenance(&report, &json.cover, Some(&corpus));
    let readme = crate::product_export_readme("stem", &provenance, true);
    // The README states the package build (and the scanning build, not recorded for this sample)
    // in the build lines (issue #341); the manifest keeps `engine_version` for old readers.
    assert!(
        readme.contains(&format!(
            "Scanned with: not recorded\nPackage built with: {}\n",
            crate::onboard::ScanBuild::package_build().readme_label()
        )),
        "{readme}"
    );
    assert!(!readme.contains("Engine version:"), "{readme}");
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

/// The engine build and the rule library hash are in manifest.json and the README, not in the PDF,
/// with one exception (owner ruling Q2, issue #341): the last page carries exactly one plain
/// sentence naming the build that SCANNED the inspection by its 12-character reference. Nothing
/// else identifies a build: not the full commit, not the exporting build, not the rule library
/// hash.
#[tokio::test]
async fn the_pdf_names_the_scanning_build_in_one_sentence_and_no_other_build_identity() {
    if which_typst().is_none() {
        eprintln!("skipping the_pdf_names_the_scanning_build: typst not on PATH");
        return;
    }
    const SCAN_COMMIT: &str = "1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b";
    let corpus = corpus().await;
    let mut report = report_with(
        vec![finding(
            "SEC-NO-HARDCODED-SECRETS-1",
            "src/a.rs",
            10,
            "high",
        )],
        vec!["SEC-NO-HARDCODED-SECRETS-1"],
    );
    report.provenance.scanned_with = Some(crate::onboard::ScanBuild {
        version: "0.3.9".to_string(),
        commit: Some(SCAN_COMMIT.to_string()),
        dirty: false,
        corpus_version: Some("sha256:the-scan-library".to_string()),
    });
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.seal(), &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );
    // The cover data (which the manifest is built from) still has the rule library hash.
    let lock_hash = camerata_rules::corpus_version(&corpus);
    assert_eq!(
        json.cover.corpus_version.as_deref(),
        Some(lock_hash.as_str())
    );
    let pdf = compile_pdf(&json).await.expect("the PDF compiles");
    let text: String = pdf_extract::extract_text_from_mem(&pdf)
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let sentence = "Inspected with Camerata 0.3.9 (reference 1a2b3c4d5e6f). The full technical \
                    record is in the manifest file included with this report.";
    assert_eq!(text.matches(sentence).count(), 1, "{text}");
    assert_eq!(
        text.matches("1a2b3c4d5e6f").count(),
        1,
        "the reference appears in that sentence only: {text}"
    );
    let signed = text.find("Prepared by").expect("the signature block");
    assert!(
        text.find(sentence).is_some_and(|at| at > signed),
        "after the signature block, on the last page: {text}"
    );
    assert!(!text.contains(SCAN_COMMIT), "{text}");
    assert!(!text.contains("Rule library lock hash"), "{text}");
    assert!(!text.contains(&lock_hash[..23]), "{text}");
    assert!(!text.contains("the-scan-library"), "{text}");
    if let Some(commit) = crate::ENGINE_COMMIT {
        assert!(!text.contains(&commit[..12]), "the exporting build: {text}");
    }

    // An inspection saved before its build was recorded carries no such sentence, and no build.
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&sample(), &HashMap::new()),
        Some(&corpus),
        &empty_opts(),
    );
    let pdf = compile_pdf(&json).await.expect("the PDF compiles");
    let text = pdf_extract::extract_text_from_mem(&pdf).unwrap_or_default();
    assert!(!text.contains("Inspected with Camerata"), "{text}");
    if let Some(commit) = crate::ENGINE_COMMIT {
        assert!(!text.contains(&commit[..12]), "{text}");
    }
}
