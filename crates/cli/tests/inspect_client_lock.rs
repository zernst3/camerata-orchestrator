#![allow(clippy::unwrap_used)]
//! The client-engagement lock on `camerata inspect`, end to end with a SCRIPTED model (no
//! network, no spend):
//!
//! * a client engagement on the `cli` backend fails closed: no pre-flight, no model call, no
//!   export, no ledger, no scan cache;
//! * a client engagement with an OpenRouter model fails closed the same way;
//! * a client engagement on the API with Anthropic models runs, and its export and ledger
//!   record the backend, the models and the engagement;
//! * the maintainer's own development (`--internal`) keeps the `cli` backend.

use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use camerata::inspect_cmd::{ledger_path, run_inspect_pipeline_with, InspectArgs, Seams};
use camerata_server::llm::{LlmPort, LlmRequest, LlmResponse, ProjectBackend};

#[derive(Default)]
struct Scripted {
    calls: AtomicUsize,
}

#[async_trait]
impl LlmPort for Scripted {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(LlmResponse {
            text: r#"{"findings":[],"proposed_rules":[],"needs_files":[],"verdicts":[],"fixes":[],"recommendations":[]}"#
                .to_string(),
            model: req.model.clone(),
            backend: "stub".to_string(),
            cost_usd: Some(0.0),
            input_tokens: Some(10),
            output_tokens: Some(10),
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
            truncated: false,
        })
    }
    async fn complete_streaming(
        &self,
        req: LlmRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        let r = self.complete(req).await?;
        on_delta(&r.text);
        Ok(r)
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

fn typst_available(test: &str) -> bool {
    let on_path = std::process::Command::new("typst")
        .arg("--version")
        .output()
        .ok()
        .is_some_and(|o| o.status.success());
    if on_path {
        return true;
    }
    assert!(
        std::env::var_os("CI").is_none(),
        "{test}: `typst` is not on PATH, and this test cannot run without it."
    );
    eprintln!("\n*** SKIPPED {test}: `typst` is not on PATH. ***\n");
    false
}

fn stage_repo(dest: &Path) {
    std::fs::write(
        dest.join("app.py"),
        "def handler(request):\n    return request.args.get('q')\n",
    )
    .unwrap();
    let g = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(dest)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    };
    g(&["init", "-q", "-b", "main"]);
    g(&["config", "user.email", "lock-e2e@camerata.local"]);
    g(&["config", "user.name", "Lock E2E"]);
    g(&["add", "."]);
    g(&["commit", "-q", "-m", "fixture"]);
}

fn args(repo: &Path, export: &Path, backend: ProjectBackend) -> InspectArgs {
    InspectArgs {
        repo: repo.to_path_buf(),
        also_repos: Vec::new(),
        repo_names: Vec::new(),
        export: export.to_path_buf(),
        backend,
        batch: false,
        // The scripted model has no batches endpoint, so the run is real-time: a batch run would
        // fail its AI review and (rightly) be refused as incomplete.
        realtime: true,
        model: Some("claude-sonnet-5".to_string()),
        calibration_model: Some("claude-sonnet-5".to_string()),
        fix_model: None,
        full: true,
        verbose: false,
        max_cost_usd: None,
        efficiency: Vec::new(),
        client: "Example Client Ltd".to_string(),
        client_engagement: true,
        run_client_tooling: false,
        consensus: false,
    }
}

struct Run {
    result: Result<camerata::inspect_cmd::InspectOutcome, String>,
    model_calls: usize,
    preflight_printed: bool,
}

async fn run(args: InspectArgs) -> Run {
    let llm = Scripted::default();
    let printed = AtomicUsize::new(0);
    let result = run_inspect_pipeline_with(
        args,
        true,
        &camerata_server::credentials::MemoryCredentialStore::new(),
        Seams {
            resume: None,
            llm: Some(&llm),
            preflight_sink: &|_| {
                printed.fetch_add(1, Ordering::SeqCst);
            },
        },
    )
    .await
    .map_err(|e| e.to_string());
    Run {
        result,
        model_calls: llm.calls.load(Ordering::SeqCst),
        preflight_printed: printed.load(Ordering::SeqCst) > 0,
    }
}

/// The refusal happened before anything ran: no estimate, no model call, nothing written.
fn assert_failed_closed(run: &Run, repo: &Path, export: &Path) -> String {
    let err = run.result.as_ref().expect_err("the run must be refused");
    assert_eq!(run.model_calls, 0, "no model may be called");
    assert!(
        !run.preflight_printed,
        "nothing may be estimated (no file read)"
    );
    assert!(!export.exists(), "no export may be written");
    assert!(
        !ledger_path(repo).exists(),
        "no ledger may be written: the scan never started"
    );
    err.clone()
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
async fn a_client_engagement_on_the_cli_backend_fails_closed() {
    let state = tempfile::tempdir().unwrap();
    let _state = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state.path());
    let repo = tempfile::tempdir().unwrap();
    stage_repo(repo.path());
    let out = tempfile::tempdir().unwrap();
    let export = out.path().join("inspect.zip");

    let run = run(args(repo.path(), &export, ProjectBackend::Cli)).await;
    let err = assert_failed_closed(&run, repo.path(), &export);
    assert!(err.contains("`cli` backend"), "{err}");
    assert!(
        err.contains("--internal"),
        "the message names the escape hatch: {err}"
    );
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_client_engagement_with_an_openrouter_model_fails_closed() {
    let state = tempfile::tempdir().unwrap();
    let _state = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state.path());
    let repo = tempfile::tempdir().unwrap();
    stage_repo(repo.path());
    let out = tempfile::tempdir().unwrap();
    let export = out.path().join("inspect.zip");

    let mut scan = args(repo.path(), &export, ProjectBackend::Api);
    scan.model = Some("deepseek/deepseek-chat".to_string());
    let run_scan = run(scan).await;
    let err = assert_failed_closed(&run_scan, repo.path(), &export);
    assert!(err.contains("`deepseek/deepseek-chat`"), "{err}");

    // The calibration model is checked too, including an OpenRouter route to a Claude model.
    let mut calibration = args(repo.path(), &export, ProjectBackend::Api);
    calibration.calibration_model = Some("anthropic/claude-sonnet-4".to_string());
    let run_cal = run(calibration).await;
    let err = assert_failed_closed(&run_cal, repo.path(), &export);
    assert!(err.contains("`anthropic/claude-sonnet-4`"), "{err}");
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_client_engagement_on_the_api_runs_and_records_backend_models_and_engagement() {
    if !typst_available(
        "a_client_engagement_on_the_api_runs_and_records_backend_models_and_engagement",
    ) {
        return;
    }
    let state = tempfile::tempdir().unwrap();
    let _state = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state.path());
    let _dep = camerata_test_env::EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1");
    let repo = tempfile::tempdir().unwrap();
    stage_repo(repo.path());
    let out = tempfile::tempdir().unwrap();
    let export = out.path().join("inspect.zip");

    let run = run(args(repo.path(), &export, ProjectBackend::Api)).await;
    let outcome = run.result.expect("an API client engagement runs");
    assert!(run.preflight_printed);
    assert!(run.model_calls > 0, "the scan called the (scripted) model");

    let manifest: serde_json::Value =
        serde_json::from_str(&zip_entry(&export, "manifest.json")).unwrap();
    let inspection = &manifest["inspection"];
    assert_eq!(inspection["model_backend"], "api", "{inspection}");
    assert_eq!(inspection["model_provider"], "anthropic");
    assert_eq!(inspection["client_engagement"], true);
    assert_eq!(inspection["models"][0]["model"], "claude-sonnet-5");
    let readme = zip_entry(&export, "README.txt");
    assert!(readme.contains("Model backend: api\n"), "{readme}");
    assert!(
        readme.contains("Model (the scan): claude-sonnet-5\n"),
        "{readme}"
    );
    assert!(readme.contains("Client engagement: yes\n"), "{readme}");

    let ledger: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&outcome.ledger_path).unwrap()).unwrap();
    let engagement = &ledger["engagement"];
    assert_eq!(engagement["client_engagement"], true, "{engagement}");
    assert_eq!(engagement["backend"], "api");
    assert_eq!(engagement["client_tooling_opt_in"], false);
    assert_eq!(engagement["client_tooling_allowed"], false);
    assert!(
        engagement["models"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["model"] == "claude-sonnet-5"),
        "{engagement}"
    );
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn internal_development_keeps_the_cli_backend() {
    if !typst_available("internal_development_keeps_the_cli_backend") {
        return;
    }
    let state = tempfile::tempdir().unwrap();
    let _state = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state.path());
    let _dep = camerata_test_env::EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1");
    let repo = tempfile::tempdir().unwrap();
    stage_repo(repo.path());
    let out = tempfile::tempdir().unwrap();
    let export = out.path().join("inspect.zip");

    let mut internal = args(repo.path(), &export, ProjectBackend::Cli);
    internal.client_engagement = false;
    let run = run(internal).await;
    run.result.expect("--internal keeps the cli backend");
    let readme = zip_entry(&export, "README.txt");
    assert!(readme.contains("Model backend: cli\n"), "{readme}");
    assert!(readme.contains("Client engagement: no\n"), "{readme}");
}
