//! The backfill of a run's rule snapshot (#560, part b): it writes only when the folder's rules
//! have the run's pinned digest, and never guesses.

use super::*;

fn state_dir() -> (tempfile::TempDir, camerata_test_env::EnvGuard) {
    let dir = tempfile::tempdir().expect("tempdir");
    let guard = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", dir.path());
    (dir, guard)
}

async fn bundled() -> RuleSet {
    let (set, errors) = camerata_rules::load_corpus_lenient(&camerata_rules::corpus_path()).await;
    assert!(errors.is_empty(), "the bundled corpus loads: {errors:?}");
    set
}

/// A copy of the bundled rule library in a folder of its own.
fn library_copy() -> tempfile::TempDir {
    fn copy(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).expect("mkdir");
        for entry in std::fs::read_dir(from).expect("read_dir") {
            let path = entry.expect("entry").path();
            let target = to.join(path.file_name().expect("name"));
            if path.is_dir() {
                copy(&path, &target);
            } else {
                std::fs::copy(&path, &target).expect("copy");
            }
        }
    }
    let dir = tempfile::tempdir().expect("tempdir");
    copy(&camerata_rules::corpus_path(), dir.path());
    dir
}

/// A saved run pinned to `pinned` (`None`: no rule library), made the way a run was made before
/// snapshots were kept: its `rules/` folder is not there.
fn run_without_snapshot(pinned: Option<&RuleSet>) -> crate::run_store::RunRecord {
    use crate::run_store::{ModelsRecord, NewRun, RunOrigin, RunStore};
    let repo = tempfile::tempdir().expect("repo");
    let store = RunStore::create(NewRun {
        pin: crate::run_store::engine_pin::EnginePin::for_run(pinned, &Default::default()),
        rules: pinned,
        project_key: "acme-backfill".to_string(),
        repos: vec![("acme/api".to_string(), repo.path().to_path_buf())],
        models: ModelsRecord {
            audit: "claude-a".to_string(),
            calibration: "claude-a".to_string(),
            fix: "claude-a".to_string(),
            backend: "api".to_string(),
            mode: "parallel".to_string(),
            resolved: true,
            group_review: String::new(),
            finder: String::new(),
            finder_model: String::new(),
            scan_exclusions: Vec::new(),
        },
        ai_scan_key: crate::scan_cache::AiScanKey {
            audit_model: "claude-a".to_string(),
            calibration_model: String::new(),
            prompt_schema_version: 2,
            directives_fingerprint: "fp".to_string(),
            terse_output: true,
            applies_to_routing: true,
        },
        args: serde_json::json!({}),
        estimate_usd: None,
        include_alternatives: false,
        origin: RunOrigin::Cli,
        project_id: None,
    })
    .expect("the run is created");
    let _ = std::fs::remove_dir_all(store.dir().join(SNAPSHOT_DIR));
    store.record()
}

fn snapshot_folder(run_id: &str) -> PathBuf {
    crate::run_store::runs_dir().join(run_id).join(SNAPSHOT_DIR)
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_backfill_from_the_library_the_run_ran_with_writes_a_snapshot_that_verifies() {
    let (_state, _guard) = state_dir();
    let live = bundled().await;
    let run = run_without_snapshot(Some(&live));
    assert_eq!(
        read_run(&run.run_id).expect_err("no snapshot yet").kind,
        RefusalKind::Missing
    );
    let folder = library_copy();
    let done = backfill(&run.run_id, folder.path())
        .await
        .expect("the backfill writes");
    let digest = camerata_rules::corpus_version(&live);
    assert_eq!(
        done,
        Backfilled::Written {
            digest: digest.clone(),
            rules: live.len()
        }
    );
    let kept = read_run(&run.run_id).expect("it reads back");
    assert_eq!(kept.digest, digest);
    // A second backfill leaves it as it is.
    assert_eq!(
        backfill(&run.run_id, folder.path()).await,
        Ok(Backfilled::AlreadyKept { digest })
    );
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_backfill_from_a_folder_whose_digest_differs_refuses_and_writes_nothing() {
    let (_state, _guard) = state_dir();
    let live = bundled().await;
    let run = run_without_snapshot(Some(&live));
    let folder = library_copy();
    // One rule's text changed: another library.
    let rule_file = folder
        .path()
        .join("universal")
        .join("sec-no-raw-sql-concat-1.toml");
    let text = std::fs::read_to_string(&rule_file).expect("the rule file");
    std::fs::write(
        &rule_file,
        text.replacen("parameterized", "parametrized", 1),
    )
    .expect("edit");
    let err = backfill(&run.run_id, folder.path())
        .await
        .expect_err("refused");
    let BackfillRefusal::DigestMismatch { pinned, found, .. } = &err else {
        panic!("refused as a digest mismatch: {err:?}");
    };
    assert_eq!(*pinned, camerata_rules::corpus_version(&live));
    assert_ne!(found, pinned);
    assert!(err.to_string().contains("Nothing was written"), "{err}");
    assert!(
        !snapshot_folder(&run.run_id).exists(),
        "nothing is written on a mismatch"
    );
    assert_eq!(
        read_run(&run.run_id).expect_err("still none").kind,
        RefusalKind::Missing
    );
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_folder_with_a_rule_file_that_does_not_load_is_refused() {
    let (_state, _guard) = state_dir();
    let live = bundled().await;
    let run = run_without_snapshot(Some(&live));
    let folder = library_copy();
    std::fs::write(folder.path().join("acme-broken.toml"), "id = [not toml").expect("write");
    let err = backfill(&run.run_id, folder.path())
        .await
        .expect_err("refused");
    assert!(
        matches!(&err, BackfillRefusal::Unloadable { files, .. } if files.len() == 1),
        "{err:?}"
    );
    assert!(!snapshot_folder(&run.run_id).exists());
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_missing_or_empty_folder_is_refused() {
    let (_state, _guard) = state_dir();
    let live = bundled().await;
    let run = run_without_snapshot(Some(&live));
    let empty = tempfile::tempdir().expect("tempdir");
    for folder in [empty.path().to_path_buf(), empty.path().join("absent")] {
        let err = backfill(&run.run_id, &folder).await.expect_err("refused");
        assert!(
            matches!(err, BackfillRefusal::NoLibrary { .. }),
            "{folder:?}: {err:?}"
        );
    }
    assert!(!snapshot_folder(&run.run_id).exists());
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_run_that_cannot_take_a_snapshot_is_refused_by_name() {
    let (_state, _guard) = state_dir();
    let folder = library_copy();
    // A run that loaded no rule library.
    let run = run_without_snapshot(None);
    let err = backfill(&run.run_id, folder.path())
        .await
        .expect_err("refused");
    assert!(
        matches!(&err, BackfillRefusal::Run(r) if r.kind == RefusalKind::NoRules),
        "{err:?}"
    );
    // A run saved before pins were recorded.
    let live = bundled().await;
    let run = run_without_snapshot(Some(&live));
    let path = crate::run_store::runs_dir()
        .join(&run.run_id)
        .join("run.json");
    let mut record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("read")).expect("json");
    record["pin"] = serde_json::Value::Null;
    std::fs::write(&path, serde_json::to_vec(&record).expect("json")).expect("write");
    let err = backfill(&run.run_id, folder.path())
        .await
        .expect_err("refused");
    assert!(
        matches!(&err, BackfillRefusal::Run(r) if r.kind == RefusalKind::NoPin),
        "{err:?}"
    );
    assert!(!snapshot_folder(&run.run_id).exists());
    // No such run, and a run id that is a path.
    for id in [
        "20261126T000000Z-000000".to_string(),
        format!("../runs/{}", run.run_id),
    ] {
        let err = backfill(&id, folder.path()).await.expect_err("refused");
        assert!(
            matches!(&err, BackfillRefusal::Run(r) if matches!(r.kind, RefusalKind::RunUnreadable(_))),
            "{id}: {err:?}"
        );
    }
}
