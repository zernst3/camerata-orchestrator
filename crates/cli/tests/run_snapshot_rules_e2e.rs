#![allow(clippy::unwrap_used)]
//! `camerata run snapshot-rules` (#560, part b): what the command says, and that it writes only
//! from the rule library the run ran with. The server's tests cover every refusal of the shared
//! backfill; this covers the command's own words around it. Synthetic data only.

use std::path::{Path, PathBuf};

use camerata::run_snapshot_cmd::snapshot_rules;
use camerata_server::run_store::{self as rs, rule_snapshot};

fn copy(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        let target = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy(&path, &target);
        } else {
            std::fs::copy(&path, &target).unwrap();
        }
    }
}

/// A run pinned to the bundled library, made as runs were before snapshots were kept.
async fn old_run() -> (String, PathBuf) {
    let (live, errors) =
        camerata_rules::load_corpus_lenient(&camerata_rules::corpus_path()).await;
    assert!(errors.is_empty());
    let repo = tempfile::tempdir().unwrap();
    let store = rs::RunStore::create(rs::NewRun {
        pin: rs::engine_pin::EnginePin::for_run(Some(&live), &Default::default()),
        rules: Some(&live),
        project_key: "acme-cli-backfill".to_string(),
        repos: vec![("acme/one".to_string(), repo.path().to_path_buf())],
        models: rs::ModelsRecord {
            audit: "m".to_string(),
            calibration: "m".to_string(),
            fix: String::new(),
            backend: "cli".to_string(),
            mode: "parallel".to_string(),
            resolved: true,
            group_review: String::new(),
            finder: String::new(),
            finder_model: String::new(),
            scan_exclusions: Vec::new(),
        },
        ai_scan_key: camerata_server::scan_cache::AiScanKey {
            audit_model: "m".to_string(),
            calibration_model: String::new(),
            prompt_schema_version: 2,
            directives_fingerprint: "fp".to_string(),
            terse_output: true,
            applies_to_routing: true,
        },
        args: serde_json::json!({}),
        estimate_usd: None,
        include_alternatives: false,
        origin: rs::RunOrigin::Cli,
        project_id: None,
    })
    .unwrap();
    let dir = store.dir().to_path_buf();
    std::fs::remove_dir_all(dir.join(rule_snapshot::SNAPSHOT_DIR)).unwrap();
    (store.run_id(), dir)
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn the_command_refuses_another_library_then_saves_the_one_the_run_ran_with() {
    let state = tempfile::tempdir().unwrap();
    let _guard = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state.path());
    let (run_id, run_dir) = old_run().await;
    let library = tempfile::tempdir().unwrap();
    copy(&camerata_rules::corpus_path(), library.path());

    // Another library: one rule file changed.
    let other = tempfile::tempdir().unwrap();
    copy(&camerata_rules::corpus_path(), other.path());
    let rule = other.path().join("universal/sec-no-raw-sql-concat-1.toml");
    let text = std::fs::read_to_string(&rule).unwrap();
    std::fs::write(&rule, text.replacen("parameterized", "parametrized", 1)).unwrap();
    let refusal = snapshot_rules(&run_id, other.path())
        .await
        .expect_err("another library is refused");
    assert!(
        refusal.starts_with("camerata run snapshot-rules: ")
            && refusal.contains("Nothing was written"),
        "{refusal}"
    );
    assert!(!run_dir.join(rule_snapshot::SNAPSHOT_DIR).exists());

    let line = snapshot_rules(&run_id, library.path())
        .await
        .expect("the run's own library is saved");
    assert!(
        line.contains(&run_id) && line.contains("can be exported"),
        "{line}"
    );
    rule_snapshot::read_run(&run_id).expect("the snapshot reads back");

    let again = snapshot_rules(&run_id, library.path()).await.unwrap();
    assert!(again.contains("already keeps"), "{again}");
}
