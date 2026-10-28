#![allow(clippy::unwrap_used)]
//! Rank 2: which scan mode `camerata inspect` runs in, end to end over the fixture repo with a
//! SCRIPTED model (no network, no spend) with a fake Message Batches API behind the model seam
//! ([`BatchSeam`]). Which path the scan took is asserted from BOTH sides: the scripted model's
//! real-time scan passes, and the batches the fake API accepted.
//!
//! * CLI backend: real time, and the run says so exactly once, before the first model call;
//! * API backend, no mode forced: a Message Batch is submitted and no scan pass runs in real
//!   time (zero real-time calls alone would also be true of a scan that never ran);
//! * API backend with `batch_default=off`: real time again, exactly as before rank 2.
//!
//! The export step needs `typst`; these tests stop at the scan, so they assert on what happened
//! before it and accept either outcome of the export.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use camerata::inspect_cmd::{run_inspect_pipeline_with, InspectArgs, Seams};
use camerata_server::ai_audit::{BatchApi, BatchSeam, CLI_BACKEND_REALTIME_NOTICE};
use camerata_server::llm::{
    BatchItem, BatchRequestCounts, BatchResultRow, BatchStatus, CacheTtl, LlmPort, LlmRequest,
    LlmResponse, ProjectBackend,
};

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

/// A Message Batches API that runs no network and answers every item with an empty findings
/// reply, and counts what it was asked: whether the scan SUBMITTED a batch is the positive
/// evidence that the batch path ran. Its real-time side is the [`Scripted`] model.
struct FakeBatches {
    scripted: Scripted,
    /// The custom ids of every submitted batch, in order.
    submitted: Mutex<Vec<Vec<String>>>,
}

impl FakeBatches {
    fn new() -> Self {
        Self {
            scripted: Scripted {
                calls: AtomicUsize::new(0),
            },
            submitted: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl BatchApi for FakeBatches {
    fn has_api_key(&self) -> bool {
        true
    }
    fn realtime(&self) -> &dyn LlmPort {
        &self.scripted
    }
    async fn count_input_tokens(&self, _req: &LlmRequest, _model: &str) -> Option<u64> {
        None
    }
    async fn submit(&self, items: Vec<BatchItem>) -> anyhow::Result<String> {
        let mut submitted = self.submitted.lock().unwrap();
        let id = format!("msgbatch_e2e_{}", submitted.len());
        submitted.push(items.iter().map(|i| i.custom_id.clone()).collect());
        Ok(id)
    }
    async fn poll(&self, _batch_id: &str) -> anyhow::Result<BatchStatus> {
        Ok(BatchStatus {
            processing_status: "ended".to_string(),
            request_counts: BatchRequestCounts::default(),
        })
    }
    async fn fetch(&self, batch_id: &str, _ttl: CacheTtl) -> anyhow::Result<Vec<BatchResultRow>> {
        let n: usize = batch_id
            .trim_start_matches("msgbatch_e2e_")
            .parse()
            .unwrap();
        let ids = self.submitted.lock().unwrap()[n].clone();
        Ok(ids
            .into_iter()
            .map(|custom_id| BatchResultRow {
                custom_id,
                response: Some(LlmResponse {
                    text: r#"{"findings":[],"needs_files":[]}"#.to_string(),
                    model: "stub".to_string(),
                    backend: "api/batch".to_string(),
                    cost_usd: Some(0.0),
                    input_tokens: Some(1_000),
                    output_tokens: Some(100),
                    cache_read_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                    or_cache_discount: None,
                    truncated: false,
                }),
                error: None,
                billed: None,
            })
            .collect())
    }
}

/// What one run did: the real-time scan passes the scripted model answered, the batches the fake
/// API accepted, and every text the run printed before the scan.
struct RunResult {
    realtime_scan_passes: usize,
    batches_submitted: usize,
    printed: Vec<String>,
}

/// Run the pipeline over the staged fixture with the fake batch API behind the model seam.
async fn run(backend: ProjectBackend, efficiency: Vec<String>) -> RunResult {
    let repo = tempfile::tempdir().unwrap();
    stage_fixture(repo.path());
    let out = tempfile::tempdir().unwrap();
    let fake = Arc::new(FakeBatches::new());
    let seam = BatchSeam::new(fake.clone());
    let printed = Mutex::new(Vec::new());
    let args = InspectArgs {
        repo: repo.path().to_path_buf(),
        export: out.path().join("inspect.zip"),
        backend,
        batch: false,
        realtime: false,
        model: None,
        calibration_model: None,
        fix_model: None,
        full: true,
        verbose: false,
        max_cost_usd: None,
        efficiency,
        client: "Example Client Ltd".to_string(),
        consensus: false,
    };
    // The export needs `typst`; either outcome is fine here (see the module doc).
    let _ = run_inspect_pipeline_with(
        args,
        true,
        &camerata_server::credentials::MemoryCredentialStore::new(),
        Seams {
            llm: Some(&seam),
            preflight_sink: &|text| {
                assert_eq!(
                    fake.scripted.calls.load(Ordering::SeqCst),
                    0,
                    "printed before the first scan pass: {text}"
                );
                assert!(
                    fake.submitted.lock().unwrap().is_empty(),
                    "printed before the first batch: {text}"
                );
                printed.lock().unwrap().push(text.to_string());
            },
        },
    )
    .await;
    let printed = printed.into_inner().unwrap();
    let batches_submitted = fake.submitted.lock().unwrap().len();
    RunResult {
        realtime_scan_passes: fake.scripted.calls.load(Ordering::SeqCst),
        batches_submitted,
        printed,
    }
}

// Serial: the pipeline reads process-env switches (dep audit, semgrep opt-out).
#[tokio::test]
#[serial_test::serial(process_env)]
async fn the_cli_backend_runs_in_real_time_and_says_so_once() {
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1");
    let r = run(ProjectBackend::Cli, Vec::new()).await;
    assert!(
        r.realtime_scan_passes > 0,
        "the scan ran in real time on the scripted model"
    );
    assert_eq!(r.batches_submitted, 0, "the CLI backend cannot batch");
    let notices = r
        .printed
        .iter()
        .filter(|t| t.contains(CLI_BACKEND_REALTIME_NOTICE))
        .count();
    assert_eq!(notices, 1, "{:?}", r.printed);
}

/// The API backend with no mode forced SUBMITS a Message Batch and makes no real-time scan call.
/// Asserted positively through the fake `BatchApi`: a scan that never started, or one that failed
/// before dispatch, would make zero real-time calls too, but it would not submit a batch.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn the_api_backend_defaults_to_batch() {
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1");
    let r = run(ProjectBackend::Api, Vec::new()).await;
    assert!(
        r.batches_submitted >= 1,
        "no mode forced on the API backend: the scan submitted a Message Batch"
    );
    assert_eq!(
        r.realtime_scan_passes, 0,
        "and none of its scan passes ran in real time"
    );
    assert!(
        r.printed
            .iter()
            .all(|t| !t.contains(CLI_BACKEND_REALTIME_NOTICE)),
        "{:?}",
        r.printed
    );
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn batch_default_off_restores_real_time_on_the_api_backend() {
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1");
    let r = run(ProjectBackend::Api, vec!["batch_default=off".to_string()]).await;
    assert!(
        r.realtime_scan_passes > 0,
        "batch_default=off: real time is the default again"
    );
    assert_eq!(r.batches_submitted, 0, "and no batch is submitted");
}
