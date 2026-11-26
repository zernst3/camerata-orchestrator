#![allow(clippy::unwrap_used)]
//! `camerata run import --run <id>` (#562): the command line brings a saved command line run into
//! the app's store through the server's import, says what it did, refuses with the named reason
//! and a non zero exit, and changes nothing on a second import. Every folder is a temporary one.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use camerata_server::onboard::{AuditedRef, Finding, ScanReport};

const RUN: &str = "run-acme-20261126-0002";
const REPO: &str = "acme/app";

struct Folders {
    _root: tempfile::TempDir,
    state: PathBuf,
    data: PathBuf,
}

fn folders() -> Folders {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let data = root.path().join("data");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    Folders {
        _root: root,
        state,
        data,
    }
}

fn camerata(f: &Folders, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_camerata"))
        .current_dir(f.state.parent().unwrap())
        .env("CAMERATA_STATE_DIR", &f.state)
        .env("CAMERATA_DATA_DIR", &f.data)
        .env("CAMERATA_NO_KEYCHAIN", "1")
        .env_remove("CAMERATA_CORPUS_PATH")
        .args(args)
        .output()
        .expect("the binary runs")
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The digest of the bundled rule library, the one the binary loads.
async fn bundled_digest() -> String {
    let path = camerata_rules::corpus_path();
    camerata_rules::corpus_version(&camerata_rules::load_corpus_lenient(&path).await.0)
}

/// A complete command line run of one repo, saved as `camerata inspect` saves it.
fn save_run(state: &Path, digest: &str) -> PathBuf {
    // Where the binary looks, by the same resolution it uses.
    let dir = camerata_api_types::state_dir::dir_from(Some(state.as_os_str()), None, None)
        .join("runs")
        .join(RUN);
    std::fs::create_dir_all(&dir).unwrap();
    let record = serde_json::json!({
        "schema": camerata_server::run_store::STORE_SCHEMA_VERSION,
        "run_id": RUN,
        "created_at": "2026-11-26T20:00:00Z",
        "updated_at": "2026-11-26T20:40:00Z",
        "status": "complete",
        "origin": "cli",
        "attempt": 1,
        "engine_version": camerata_server::run_store::engine_version(),
        "pin": {
            "build": {
                "version": env!("CARGO_PKG_VERSION"),
                "commit": "c".repeat(40),
                "dirty": false,
                "prompt_schema": 1,
                "clone_prompt_schema": 1
            },
            "corpus_version": digest,
            "clone_judge_model": "claude-haiku-5-5"
        },
        "project_key": "acme-app-1234abcd",
        "repos": [{
            "name": REPO,
            "dir": "/Users/auditor/clients/acme-app",
            "commit": "0123456789abcdef0123456789abcdef01234567",
            "clean": true,
            "tree_at_start": "tree-digest"
        }],
        "models": {
            "audit": "claude-sonnet-5-5",
            "calibration": "claude-sonnet-5-5",
            "fix": "claude-haiku-5-5",
            "backend": "api",
            "mode": "parallel"
        },
        "rules_fingerprint": "rules-fp-acme-2",
        "ai_scan_key": {
            "audit_model": "claude-sonnet-5-5",
            "calibration_model": "",
            "prompt_schema_version": 1,
            "directives_fingerprint": "rules-fp-acme-2"
        },
        "nonce_seed": "00",
        "args": {},
        "plan": []
    });
    std::fs::write(dir.join("run.json"), serde_json::to_vec(&record).unwrap()).unwrap();
    let mut report = ScanReport::gated(&[REPO.to_string()]);
    report.gated = false;
    report.message = None;
    report.run_id = Some(RUN.to_string());
    report.provenance.camerata_version = env!("CARGO_PKG_VERSION").to_string();
    report.provenance.rules_fingerprint = "rules-fp-acme-2".to_string();
    report.provenance.started_at = "2026-11-26T20:00:00Z".to_string();
    report.provenance.finished_at = "2026-11-26T20:40:00Z".to_string();
    report.provenance.audited_refs = vec![AuditedRef {
        repo: REPO.to_string(),
        sha: Some("0123456789abcdef0123456789abcdef01234567".to_string()),
        ..AuditedRef::default()
    }];
    for i in 0..3 {
        report.findings.push(Finding {
            repo: REPO.to_string(),
            path: format!("src/q{i}.rs"),
            line: 10 + i,
            rule_id: "SEC-NO-RAW-SQL-CONCAT-1".to_string(),
            severity: "high".to_string(),
            snippet: format!("query(\"select\" + {i})"),
            detail: "Title. The query is built from a request field.".to_string(),
            kind: Some(camerata_api_types::audit::FindingKind::Defect),
            ..Finding::default()
        });
    }
    camerata_server::run_store::write_report(
        &dir.join(camerata_server::run_store::REPORT_FILE),
        &report.seal(),
    )
    .unwrap();
    dir
}

fn files_of(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out: Vec<(PathBuf, Vec<u8>)> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .map(|p| {
            let bytes = std::fs::read(&p).unwrap();
            (p, bytes)
        })
        .collect();
    out.sort();
    out
}

#[tokio::test]
async fn run_import_brings_a_saved_run_into_the_app_once_and_leaves_the_run_folder_as_it_was() {
    let f = folders();
    let run_dir = save_run(&f.state, &bundled_digest().await);
    let project = camerata_server::AppState::from_data_dir(&f.data.join("camerata"))
        .projects()
        .create("Acme", vec![REPO.to_string()])
        .unwrap();
    let before = files_of(&run_dir);

    let first = camerata(&f, &["run", "import", "--run", RUN]);
    assert!(first.status.success(), "{}", text(&first));
    let said = text(&first);
    assert!(
        said.contains(&format!("Imported run {RUN} into project Acme")),
        "{said}"
    );
    assert!(said.contains("with 3 findings"), "{said}");

    let second = camerata(&f, &["run", "import", "--run", RUN]);
    assert!(second.status.success(), "{}", text(&second));
    assert!(
        text(&second).contains("is already in the app"),
        "{}",
        text(&second)
    );

    let state = camerata_server::AppState::from_data_dir(&f.data.join("camerata"));
    let entries = state.inspections().list(&project.id);
    assert_eq!(entries.len(), 1, "one entry after two imports");
    assert_eq!(entries[0].report.run_id.as_deref(), Some(RUN));
    assert_eq!(
        files_of(&run_dir),
        before,
        "the run folder is byte identical"
    );
}

#[tokio::test]
async fn run_import_refuses_with_the_named_reason_and_a_non_zero_exit() {
    let f = folders();
    // Pinned to another rule library than the one this build loads.
    save_run(&f.state, "corpus-digest-acme-other");
    camerata_server::AppState::from_data_dir(&f.data.join("camerata"))
        .projects()
        .create("Acme", vec![REPO.to_string()])
        .unwrap();
    let out = camerata(&f, &["run", "import", "--run", RUN]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    let said = text(&out);
    assert!(said.contains("camerata run import:"), "{said}");
    assert!(said.contains("corpus-digest-acme-other"), "{said}");
    assert!(said.contains("rule library"), "{said}");
    assert!(
        camerata_server::AppState::from_data_dir(&f.data.join("camerata"))
            .inspections()
            .list("proj-1")
            .is_empty(),
        "nothing was written"
    );

    let missing = camerata(&f, &["run", "import", "--run", "run-acme-missing"]);
    assert_eq!(missing.status.code(), Some(1), "{}", text(&missing));
    assert!(
        text(&missing).contains("There is no saved run run-acme-missing"),
        "{}",
        text(&missing)
    );

    let escape = camerata(&f, &["run", "import", "--run", "../x"]);
    assert_eq!(escape.status.code(), Some(1), "{}", text(&escape));
    assert!(
        text(&escape).contains("is not a run id"),
        "{}",
        text(&escape)
    );
}
