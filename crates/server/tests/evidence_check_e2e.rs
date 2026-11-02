#![allow(clippy::unwrap_used)]
//! END-TO-END: the M5 evidence check runs inside the real scan (`onboard::audit_repos`) and its
//! counts reach the ledger. The scan is deterministic only (`run_ai_review: false`, zero API
//! spend), so every row is one a detector read off a real line: the check must verify all of them
//! and flag none. A flagged row here would be a false alarm held for a reviewer.
//!
//! FIXTURE: `tests/fixtures/pipeline_integrity_ledger_repo/` (a clean file and a planted raw-SQL
//! concatenation).

use std::path::Path;

use camerata_server::ai_audit::ScanMode;
use camerata_server::onboard::{self, SelectedRule};

fn copy_dir_recursive(src: &Path, dest: &Path) {
    std::fs::create_dir_all(dest).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir_recursive(&from, &to);
        } else {
            std::fs::copy(&from, &to).unwrap();
        }
    }
}

#[tokio::test]
async fn the_scan_checks_every_row_and_the_ledger_counts_reconcile() {
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(errors.is_empty(), "{errors:?}");

    let dir = tempfile::tempdir().unwrap();
    copy_dir_recursive(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/pipeline_integrity_ledger_repo"),
        dir.path(),
    );
    let sources = vec![("e2e/evidence-check".to_string(), dir.path().to_path_buf())];
    let selected = vec![SelectedRule {
        id: "SEC-NO-RAW-SQL-CONCAT-1".to_string(),
        directive: "no raw SQL built by string concatenation".to_string(),
        repos: Vec::new(),
    }];
    let (report, _manifest) = onboard::audit_repos(
        &camerata_server::credentials::MemoryCredentialStore::new(),
        &sources,
        &selected,
        Vec::new(),
        None,
        None,
        ScanMode::Sequential,
        false,
        None,
        None,
        None,
        false,
        false,
        false,
        true,
        None,
        camerata_server::llm::BackendResolution::Api,
        Some(&corpus),
        &std::collections::HashMap::new(),
    )
    .await;

    assert!(
        report
            .findings
            .iter()
            .any(|f| f.rule_id == "SEC-NO-RAW-SQL-CONCAT-1"),
        "the planted violation must fire: {:?}",
        report.findings
    );
    let counts = report.ledger.evidence_check();
    assert!(counts.checked >= 1, "{counts:?}");
    assert!(counts.reconciles(), "{counts:?}");
    assert_eq!(counts.rows_seen(), report.findings.len(), "{counts:?}");
    assert_eq!(
        counts.unverified, 0,
        "a detector's own rows are real: {report:#?}"
    );
    assert!(report
        .findings
        .iter()
        .all(|f| !f.captures.contains_key("evidence_unverified")));
}
