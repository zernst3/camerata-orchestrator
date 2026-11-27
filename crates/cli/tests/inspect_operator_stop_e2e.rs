#![allow(clippy::unwrap_used)]
//! #583, end to end over the fixture repo with a FAKE batch provider (no network, no spend):
//! the operator's stop (Ctrl+C or SIGTERM) during a live batch.
//!
//! * The stop cancels that batch at the provider, saves the run and ends with the stop code, in
//!   well under a few seconds.
//! * A resume after it gives the same output as an uninterrupted run.
//! * A batch the provider will not cancel is listed, with its id, in the failure report.
//!
//! The signal is the stop future [`run_stoppable`] takes; production hands it the OS signals
//! (`os_stop_signal`). Here it resolves the moment the provider holds a live batch.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use camerata::inspect_cmd::operator_stop::run_stoppable;
use camerata::inspect_cmd::{
    resume_inspect, run_inspect_pipeline_controlled, run_inspect_pipeline_with, InspectArgs,
    InspectError, Seams, EXIT_OPERATOR_STOPPED,
};
use camerata_server::ai_audit::{BatchApi, BatchSeam};
use camerata_server::llm::{
    BatchItem, BatchRequestCounts, BatchResultRow, BatchStatus, CacheTtl, LlmPort, LlmRequest,
    LlmResponse, ProjectBackend,
};

// ── The fake provider ────────────────────────────────────────────────────────────────────────

/// Answers every request with nothing found, and the group reviewer with its stand-in.
struct Model;

impl Model {
    fn answer(req: &LlmRequest) -> LlmResponse {
        let text = camerata_server::ai_audit::group_review::stand_in_reply(
            req.system.as_deref(),
            &req.prompt,
        )
        .unwrap_or_else(|| {
            r#"{"findings":[],"proposed_rules":[],"needs_files":[],"verdicts":[],"fixes":[],"recommendations":[]}"#
                .to_string()
        });
        LlmResponse {
            reported_model: None,
            cli_cost_usd: None,
            elapsed_ms: None,
            text,
            model: req.model.clone(),
            backend: "api".to_string(),
            cost_usd: Some(0.0),
            input_tokens: Some(10),
            output_tokens: Some(10),
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
            truncated: false,
        }
    }
}

#[async_trait]
impl LlmPort for Model {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        Ok(Self::answer(&req))
    }
    async fn complete_streaming(
        &self,
        req: LlmRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        let r = Self::answer(&req);
        on_delta(&r.text);
        Ok(r)
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

fn request_of(item: &BatchItem) -> LlmRequest {
    let text = match item.params.messages[0].content.as_str() {
        Some(t) => t.to_string(),
        None => item.params.messages[0]
            .content
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|b| b["text"].as_str())
            .collect::<String>(),
    };
    let mut req = LlmRequest::new(text);
    req.system.clone_from(&item.params.system);
    req
}

/// A batch provider. While `hold` is set its batches stay in progress; a cancelled batch ends with
/// no result for any item (it was cancelled before any finished).
struct Provider {
    model: Model,
    hold: AtomicBool,
    /// Batch ids whose cancel is refused.
    refuse_cancel: Vec<String>,
    submits: AtomicUsize,
    items: Mutex<HashMap<String, Vec<BatchItem>>>,
    cancelled: Mutex<Vec<String>>,
    /// Set once a batch was accepted: the operator presses Ctrl+C then.
    live: tokio::sync::Notify,
}

impl Provider {
    fn new(hold: bool, refuse_cancel: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            model: Model,
            hold: AtomicBool::new(hold),
            refuse_cancel: refuse_cancel.iter().map(|s| (*s).to_string()).collect(),
            submits: AtomicUsize::new(0),
            items: Mutex::new(HashMap::new()),
            cancelled: Mutex::new(Vec::new()),
            live: tokio::sync::Notify::new(),
        })
    }

    fn was_cancelled(&self, id: &str) -> bool {
        self.cancelled.lock().unwrap().iter().any(|c| c == id)
    }
}

struct Shared(Arc<Provider>);

#[async_trait]
impl BatchApi for Shared {
    fn has_api_key(&self) -> bool {
        true
    }
    fn realtime(&self) -> &dyn LlmPort {
        &self.0.model
    }
    async fn count_input_tokens(&self, _req: &LlmRequest, _model: &str) -> Option<u64> {
        Some(1_000)
    }
    async fn submit(&self, items: Vec<BatchItem>) -> anyhow::Result<String> {
        let n = self.0.submits.fetch_add(1, Ordering::SeqCst);
        let id = format!("msgbatch_acme_{n}");
        self.0.items.lock().unwrap().insert(id.clone(), items);
        self.0.live.notify_one();
        Ok(id)
    }
    async fn poll(&self, id: &str) -> anyhow::Result<BatchStatus> {
        let running = self.0.hold.load(Ordering::SeqCst) && !self.0.was_cancelled(id);
        Ok(BatchStatus {
            processing_status: if running { "in_progress" } else { "ended" }.to_string(),
            request_counts: BatchRequestCounts::default(),
        })
    }
    async fn fetch(&self, id: &str, _ttl: CacheTtl) -> anyhow::Result<Vec<BatchResultRow>> {
        if self.0.was_cancelled(id) {
            // As the provider does: every item the cancel caught comes back as a `canceled` row.
            let items = self.0.items.lock().unwrap().get(id).cloned().unwrap_or_default();
            return Ok(items
                .iter()
                .map(|item| BatchResultRow {
                    custom_id: item.custom_id.clone(),
                    response: None,
                    error: Some("canceled".to_string()),
                    billed: None,
                })
                .collect());
        }
        let items = self
            .0
            .items
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .unwrap_or_default();
        Ok(items
            .iter()
            .map(|item| BatchResultRow {
                custom_id: item.custom_id.clone(),
                response: Some(Model::answer(&request_of(item))),
                error: None,
                billed: None,
            })
            .collect())
    }
    async fn cancel_batch(&self, id: &str) -> anyhow::Result<()> {
        if self.0.refuse_cancel.iter().any(|r| r == id) {
            anyhow::bail!("Message Batches API HTTP 500: internal error");
        }
        self.0.cancelled.lock().unwrap().push(id.to_string());
        Ok(())
    }
}

// ── The runs ─────────────────────────────────────────────────────────────────────────────────

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

fn stage_fixture(dest: &Path) {
    let root =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../server/tests/fixtures/e2e_report_repo");
    copy(&root, dest);
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["config", "user.email", "stop-e2e@camerata.local"],
        &["config", "user.name", "Stop E2E"],
        &["add", "."],
        &["commit", "-q", "-m", "fixture"],
    ] {
        let out = std::process::Command::new("git")
            .current_dir(dest)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    }
}

fn args(repo: &Path, export: &Path) -> InspectArgs {
    InspectArgs {
        repo: repo.to_path_buf(),
        also_repos: Vec::new(),
        repo_names: vec!["acme/widgets".to_string()],
        export: export.to_path_buf(),
        backend: ProjectBackend::Api,
        batch: true,
        realtime: false,
        model: None,
        calibration_model: None,
        fix_model: None,
        clone_judge_model: None,
        group_review_model: None,
        dup_check_model: None,
        escalation_rate: None,
        escalation_floor: None,
        full: true,
        verbose: false,
        max_cost_usd: None,
        efficiency: vec!["warm_schedule=off".to_string()],
        client: "Acme Corp".to_string(),
        client_engagement: false,
        run_client_tooling: false,
        consensus: false,
        finder: Default::default(),
        exclude: Vec::new(),
    }
}

fn creds() -> camerata_server::credentials::MemoryCredentialStore {
    camerata_server::credentials::MemoryCredentialStore::new()
}

fn noop(_: &str) {}

fn seams(seam: &BatchSeam) -> Seams<'_> {
    Seams {
        resume: None,
        llm: Some(seam),
        preflight_sink: &noop,
    }
}

struct Workspace {
    _state: tempfile::TempDir,
    _state_env: camerata_test_env::EnvGuard,
    _dep_env: camerata_test_env::EnvGuard,
    _poll_env: camerata_test_env::EnvGuard,
    repo: tempfile::TempDir,
    out: tempfile::TempDir,
}

impl Workspace {
    fn new() -> Self {
        let state = tempfile::tempdir().unwrap();
        let state_env = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state.path());
        let dep_env = camerata_test_env::EnvGuard::set("CAMERATA_DISABLE_DEP_AUDIT", "1");
        // Poll the fake batches every second, not every ten.
        let poll_env = camerata_test_env::EnvGuard::set("CAMERATA_BATCH_POLL_SECS", "1");
        let repo = tempfile::tempdir().unwrap();
        stage_fixture(repo.path());
        Self {
            _state: state,
            _state_env: state_env,
            _dep_env: dep_env,
            _poll_env: poll_env,
            repo,
            out: tempfile::tempdir().unwrap(),
        }
    }

    fn export(&self, name: &str) -> PathBuf {
        self.out.path().join(name)
    }
}

/// Run until the provider holds a live batch, then stop as Ctrl+C does.
async fn stopped_run(
    ws: &Workspace,
    provider: &Arc<Provider>,
) -> (camerata::inspect_cmd::operator_stop::OperatorStopped, Duration) {
    let seam = BatchSeam::new(Arc::new(Shared(provider.clone())));
    let export = ws.export("stopped.zip");
    let pressed: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
    let at = pressed.clone();
    let p = provider.clone();
    let stop = async move {
        p.live.notified().await;
        *at.lock().unwrap() = Some(Instant::now());
    };
    let result = run_stoppable(stop, |control| {
        run_inspect_pipeline_controlled(
            args(ws.repo.path(), &export),
            true,
            creds_static(),
            seams(&seam),
            control,
        )
    })
    .await;
    let took = pressed.lock().unwrap().expect("the stop fired").elapsed();
    let err = result.expect_err("a stopped run is not a finished one");
    assert_eq!(err.exit_code(), EXIT_OPERATOR_STOPPED, "{err}");
    assert!(!export.exists(), "a stopped run writes no export");
    let InspectError::StoppedByOperator(stopped) = err else {
        panic!("not an operator stop: {err}")
    };
    (*stopped, took)
}

fn creds_static() -> &'static camerata_server::credentials::MemoryCredentialStore {
    Box::leak(Box::new(creds()))
}

fn findings_of(export: &Path) -> Vec<String> {
    let mut archive = zip::ZipArchive::new(std::fs::File::open(export).unwrap()).unwrap();
    let mut text = String::new();
    archive
        .by_name("json/inspection.json")
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
    let mut out: Vec<String> = doc["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            format!(
                "{} | {} | {} | {} | {}",
                f["rule_id"], f["file"], f["line"], f["severity"], f["title"]
            )
        })
        .collect();
    out.sort();
    out
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn ctrl_c_during_a_live_batch_cancels_it_saves_the_run_and_a_resume_finishes_it() {
    if !camerata_test_env::typst_on_path_for("ctrl_c_during_a_live_batch") {
        return;
    }
    // The uninterrupted run, for the output to compare with.
    let want = {
        let ws = Workspace::new();
        let provider = Provider::new(false, &[]);
        let seam = BatchSeam::new(Arc::new(Shared(provider)));
        let export = ws.export("reference.zip");
        let outcome = run_inspect_pipeline_with(args(ws.repo.path(), &export), true, &creds(), seams(&seam))
            .await
            .expect("the reference run completes");
        assert!(outcome.incomplete_reasons.is_empty(), "{:?}", outcome.incomplete_reasons);
        findings_of(&export)
    };

    let ws = Workspace::new();
    let provider = Provider::new(true, &[]);
    let (stopped, took) = stopped_run(&ws, &provider).await;
    assert!(took < Duration::from_secs(5), "the stop took {took:?}");
    // The live batch was cancelled at the provider.
    assert_eq!(stopped.report.cancelled, vec!["msgbatch_acme_0".to_string()]);
    assert!(provider.was_cancelled("msgbatch_acme_0"));
    assert!(stopped.report.not_cancelled.is_empty());
    // The run was saved, and the failure report records the stop.
    let run_id = stopped.run_id.clone().expect("the run was saved");
    let failures = stopped.failure_report.clone().expect("the stop is recorded");
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&failures).unwrap()).unwrap();
    assert!(json["operator_stop"].is_object(), "{json}");
    let record = camerata_server::run_store::read_record(&run_id).unwrap();
    assert!(
        record.notes.iter().any(|n| n.contains("stopped by the operator")),
        "{:?}",
        record.notes
    );

    // The resume accepts the stopped run and gives the same output as an uninterrupted run.
    provider.hold.store(false, Ordering::SeqCst);
    let seam = BatchSeam::new(Arc::new(Shared(provider.clone())));
    let export = ws.export("resumed.zip");
    let resumed = resume_inspect(
        &run_id,
        &[],
        Some(export.clone()),
        None,
        true,
        &creds(),
        seams(&seam),
    )
    .await
    .expect("the resume completes");
    assert!(
        resumed.incomplete_reasons.is_empty(),
        "{:?}\n{:#?}",
        resumed.incomplete_reasons,
        resumed.failure_lines
    );
    assert_eq!(findings_of(&export), want, "the same output as an uninterrupted run");
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_batch_that_cannot_be_cancelled_is_listed_with_its_id_in_the_failure_report() {
    if !camerata_test_env::typst_on_path_for("a_batch_that_cannot_be_cancelled") {
        return;
    }
    let ws = Workspace::new();
    let provider = Provider::new(true, &["msgbatch_acme_0"]);
    let (stopped, _) = stopped_run(&ws, &provider).await;
    assert_eq!(stopped.report.not_cancelled.len(), 1);
    assert_eq!(stopped.report.not_cancelled[0].batch_id, "msgbatch_acme_0");
    assert!(stopped.to_string().contains("msgbatch_acme_0"), "{stopped}");
    let failures = stopped.failure_report.clone().expect("the stop is recorded");
    let json = std::fs::read_to_string(&failures).unwrap();
    assert!(json.contains("msgbatch_acme_0"), "{json}");
    let md = std::fs::read_to_string(failures.with_extension("md")).unwrap();
    assert!(md.contains("batch msgbatch_acme_0"), "{md}");
    assert!(md.contains("could NOT be cancelled"), "{md}");
}
