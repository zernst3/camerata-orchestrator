#![allow(clippy::unwrap_used)]
//! Run to completion, end to end over the fixture repo with a SCRIPTED model (no network, no
//! spend). Zach, 2026-10-28: "We can't ship a product to anyone that didn't fully run end-to-end
//! properly."
//!
//! * A client run whose model cannot be reached stops (exit 5), writes NO zip, and says why.
//! * A client run with a part that failed after its retries and its second try is refused
//!   (exit 6), writes NO zip.
//! * The same runs with `--internal` write the export marked INTERNAL INCOMPLETE, and the
//!   release gate refuses that zip.

use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use camerata::inspect_cmd::{
    run_inspect_pipeline_with, run_inspect_with_key_presence, InspectArgs, InspectError, Seams,
    EXIT_INCOMPLETE_RUN, EXIT_OUTAGE_STOPPED,
};
use camerata_server::cost_cap::FailureClass;
use camerata_server::llm::{LlmPort, LlmRequest, LlmResponse, ProjectBackend};

/// What the scripted model does on every call.
enum Mode {
    /// The provider cannot be reached.
    Unreachable,
    /// The reply is not JSON.
    Malformed,
}

struct Scripted {
    mode: Mode,
    calls: AtomicUsize,
}

impl Scripted {
    fn respond(&self) -> anyhow::Result<LlmResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match self.mode {
            Mode::Unreachable => {
                let mut f = camerata_server::failure::LlmCallFailure::new(
                    FailureClass::Transport,
                    "Anthropic API HTTP 529: overloaded",
                );
                f.retryable = true;
                Err(anyhow::Error::new(f))
            }
            Mode::Malformed => Ok(LlmResponse {
                text: "I could not produce JSON for this.".to_string(),
                model: "stub".to_string(),
                backend: "stub".to_string(),
                cost_usd: Some(0.01),
                input_tokens: Some(100),
                output_tokens: Some(10),
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
                or_cache_discount: None,
                truncated: false,
            }),
        }
    }
}

#[async_trait]
impl LlmPort for Scripted {
    async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.respond()
    }
    async fn complete_streaming(
        &self,
        _req: LlmRequest,
        _on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        self.respond()
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

fn typst_available(test: &str) -> bool {
    camerata_test_env::typst_on_path_for(test)
}

fn stage_fixture(dest: &Path) {
    let root =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../server/tests/fixtures/e2e_report_repo");
    fn copy(src: &Path, dest: &Path) {
        std::fs::create_dir_all(dest).unwrap();
        for entry in std::fs::read_dir(src).unwrap() {
            let entry = entry.unwrap();
            let to = dest.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy(&entry.path(), &to);
            } else {
                std::fs::copy(entry.path(), to).unwrap();
            }
        }
    }
    copy(&root, dest);
    let g = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(dest)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    };
    g(&["init", "-q", "-b", "main"]);
    g(&["config", "user.email", "incomplete-e2e@camerata.local"]);
    g(&["config", "user.name", "Incomplete E2E"]);
    g(&["add", "."]);
    g(&["commit", "-q", "-m", "fixture"]);
}

fn args(repo: &Path, export: &Path, client_engagement: bool) -> InspectArgs {
    InspectArgs {
        repo: repo.to_path_buf(),
        also_repos: Vec::new(),
        repo_names: Vec::new(),
        export: export.to_path_buf(),
        backend: ProjectBackend::Api,
        batch: false,
        realtime: true,
        model: None,
        calibration_model: None,
        fix_model: None,
        full: true,
        verbose: false,
        max_cost_usd: None,
        efficiency: vec!["warm_schedule=off".to_string()],
        client: "Example Client Ltd".to_string(),
        client_engagement,
        run_client_tooling: false,
        consensus: false,
    }
}

async fn run(
    mode: Mode,
    client_engagement: bool,
) -> (
    Result<camerata::inspect_cmd::InspectOutcome, InspectError>,
    std::path::PathBuf,
    tempfile::TempDir,
    tempfile::TempDir,
) {
    let repo = tempfile::tempdir().unwrap();
    stage_fixture(repo.path());
    let out = tempfile::tempdir().unwrap();
    let export = out.path().join("inspect.zip");
    let llm = Scripted {
        mode,
        calls: AtomicUsize::new(0),
    };
    let result = run_inspect_pipeline_with(
        args(repo.path(), &export, client_engagement),
        true,
        &camerata_server::credentials::MemoryCredentialStore::new(),
        Seams {
            llm: Some(&llm),
            preflight_sink: &|_| {},
        },
    )
    .await;
    (result, export, out, repo)
}

fn zip_entry(path: &Path, name: &str) -> String {
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let mut text = String::new();
    archive
        .by_name(name)
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    text
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_client_run_whose_provider_is_unreachable_stops_with_exit_5_and_writes_no_zip() {
    if !typst_available("a_client_run_whose_provider_is_unreachable") {
        return;
    }
    let state = tempfile::tempdir().unwrap();
    let _state = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state.path());
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1");
    let (result, export, _out, _repo) = run(Mode::Unreachable, true).await;
    let err = result.expect_err("a client export of an unreachable-provider run is refused");
    assert_eq!(err.exit_code(), EXIT_OUTAGE_STOPPED);
    assert!(!export.exists(), "no zip is written for a client");
    let text = err.to_string();
    assert!(text.contains("did not run to completion"), "{text}");
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_client_run_with_a_failed_part_is_refused_with_exit_6_and_writes_no_zip() {
    if !typst_available("a_client_run_with_a_failed_part") {
        return;
    }
    let state = tempfile::tempdir().unwrap();
    let _state = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state.path());
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1");
    let (result, export, _out, _repo) = run(Mode::Malformed, true).await;
    let err = result.expect_err("refused");
    assert_eq!(err.exit_code(), EXIT_INCOMPLETE_RUN);
    assert!(!export.exists(), "no zip is written for a client");
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn the_internal_export_of_an_unreachable_run_is_written_marked_and_gate_refused() {
    if !typst_available("the_internal_export_of_an_unreachable_run") {
        return;
    }
    let state = tempfile::tempdir().unwrap();
    let _state = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state.path());
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1");
    let (result, export, _out, _repo) = run(Mode::Unreachable, false).await;
    let outcome = result.expect("an internal export is written");
    assert!(outcome.outage_stopped());
    assert_eq!(outcome.exit_code(), EXIT_OUTAGE_STOPPED);
    assert!(export.exists());
    // The operator's failure report: structured stderr lines, a file next to the ledger, and
    // nothing of it inside the zip.
    assert!(!outcome.failure_lines.is_empty());
    assert!(outcome.ledger_summary.contains("failure report:"));
    let report_json = outcome
        .ledger_path
        .to_string_lossy()
        .replace(".ledger.json", ".failures.json");
    assert!(Path::new(&report_json).exists(), "{report_json}");
    let names =
        camerata_server::release_gate::read_zip_entries(&std::fs::read(&export).unwrap()).unwrap();
    assert!(names.keys().all(|k| !k.contains("failures")), "{names:?}");
    let manifest: serde_json::Value =
        serde_json::from_str(&zip_entry(&export, "manifest.json")).unwrap();
    assert_eq!(manifest["inspection"]["run_incomplete"], true);
    assert!(zip_entry(&export, "README.txt").contains("INTERNAL INCOMPLETE"));
    assert!(zip_entry(&export, "findings.json").contains("INTERNAL INCOMPLETE"));
    // `camerata release-gate <zip>` refuses it.
    let bytes = std::fs::read(&export).unwrap();
    assert!(!camerata_server::release_gate::audit_zip(&bytes, None).is_clean());
}

/// The AI review was requested for a CLIENT run and could not run at all (the compliance backend
/// gate blocked it: the API backend with no key). The run is incomplete, so the export is refused
/// with exit 6 and no zip is written.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_client_run_whose_ai_review_was_compliance_blocked_exits_6_and_writes_no_zip() {
    if !typst_available("a_client_run_whose_ai_review_was_compliance_blocked") {
        return;
    }
    let state = tempfile::tempdir().unwrap();
    let _state = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state.path());
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1");
    let repo = tempfile::tempdir().unwrap();
    stage_fixture(repo.path());
    let out = tempfile::tempdir().unwrap();
    let export = out.path().join("inspect.zip");
    let result = run_inspect_with_key_presence(args(repo.path(), &export, true), false).await;
    let err =
        result.expect_err("a client export of a run whose AI review could not run is refused");
    assert_eq!(err.exit_code(), EXIT_INCOMPLETE_RUN, "{err}");
    assert!(!export.exists(), "no zip is written for a client");
}
