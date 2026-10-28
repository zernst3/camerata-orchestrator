#![allow(clippy::unwrap_used)]
//! Rank 2: which scan mode `camerata inspect` runs in, end to end over the fixture repo with a
//! SCRIPTED model (no network, no spend). The scripted model only answers real-time calls (it has
//! no batches endpoint), so whether it was called tells which path the scan took:
//!
//! * CLI backend: real time, and the run says so exactly once, before the first model call;
//! * API backend, no mode forced: the batch path (the scripted model is never called);
//! * API backend with `batch_default=off`: real time again, exactly as before rank 2.
//!
//! The export step needs `typst`; these tests stop at the scan, so they assert on what happened
//! before it and accept either outcome of the export.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use async_trait::async_trait;
use camerata::inspect_cmd::{run_inspect_pipeline_with, InspectArgs, Seams};
use camerata_server::ai_audit::CLI_BACKEND_REALTIME_NOTICE;
use camerata_server::llm::{LlmPort, LlmRequest, LlmResponse, ProjectBackend};

/// Counts the real-time SCAN passes it answers (other stages, such as the rule-alternative
/// recommendation that runs before the scan, are answered but not counted).
struct Scripted {
    calls: AtomicUsize,
}

#[async_trait]
impl LlmPort for Scripted {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        if req.prompt.contains("Check the code above against") {
            self.calls.fetch_add(1, Ordering::SeqCst);
        }
        Ok(LlmResponse {
            text: r#"{"findings":[],"proposed_rules":[],"needs_files":[],"verdicts":[],"fixes":[],"recommendations":[]}"#
                .to_string(),
            model: req.model.clone(),
            backend: "stub".to_string(),
            cost_usd: Some(0.0),
            input_tokens: Some(1_000),
            output_tokens: Some(100),
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
    g(&["config", "user.email", "mode-e2e@camerata.local"]);
    g(&["config", "user.name", "Mode E2E"]);
    g(&["add", "."]);
    g(&["commit", "-q", "-m", "fixture"]);
}

/// Run the pipeline; returns (real-time scan passes, every text the run printed before the scan).
async fn run(backend: ProjectBackend, efficiency: Vec<String>) -> (usize, Vec<String>) {
    let repo = tempfile::tempdir().unwrap();
    stage_fixture(repo.path());
    let out = tempfile::tempdir().unwrap();
    let llm = Scripted {
        calls: AtomicUsize::new(0),
    };
    let printed = Mutex::new(Vec::new());
    let args = InspectArgs {
        repo: repo.path().to_path_buf(),
        export: out.path().join("inspect.zip"),
        backend,
        batch: false,
        realtime: false,
        model: None,
        calibration_model: None,
        full: true,
        verbose: false,
        max_cost_usd: None,
        efficiency,
    };
    // The export needs `typst`; either outcome is fine here (see the module doc).
    let _ = run_inspect_pipeline_with(
        args,
        true,
        &camerata_server::credentials::MemoryCredentialStore::new(),
        Seams {
            llm: Some(&llm),
            preflight_sink: &|text| {
                assert_eq!(
                    llm.calls.load(Ordering::SeqCst),
                    0,
                    "printed before the first scan pass: {text}"
                );
                printed.lock().unwrap().push(text.to_string());
            },
        },
    )
    .await;
    let printed = printed.into_inner().unwrap();
    (llm.calls.load(Ordering::SeqCst), printed)
}

// Serial: the pipeline reads process-env switches (dep audit, semgrep opt-out).
#[tokio::test]
#[serial_test::serial(process_env)]
async fn the_cli_backend_runs_in_real_time_and_says_so_once() {
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1");
    let (calls, printed) = run(ProjectBackend::Cli, Vec::new()).await;
    assert!(calls > 0, "the scan ran in real time on the scripted model");
    let notices = printed
        .iter()
        .filter(|t| t.contains(CLI_BACKEND_REALTIME_NOTICE))
        .count();
    assert_eq!(notices, 1, "{printed:?}");
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn the_api_backend_defaults_to_batch_and_switch_off_restores_real_time() {
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1");
    let (calls, printed) = run(ProjectBackend::Api, Vec::new()).await;
    assert_eq!(
        calls, 0,
        "no mode forced on the API backend: the scan took the batch path"
    );
    assert!(
        printed
            .iter()
            .all(|t| !t.contains(CLI_BACKEND_REALTIME_NOTICE)),
        "{printed:?}"
    );

    let (calls, _) = run(ProjectBackend::Api, vec!["batch_default=off".to_string()]).await;
    assert!(
        calls > 0,
        "batch_default=off: real time is the default again"
    );
}
