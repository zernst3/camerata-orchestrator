//! The run figures a saved run's folder yields (#293, #61): wall clock over every session, and
//! where each batch stage's batches stand. The folder is made by the real store and the real
//! batch journal (a scripted provider, no live call), so its files have the shape a run leaves.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use camerata_api_types::run_figures::{BatchStageStatus, RunRecordFigures};

use super::super::journal::JournalBatchApi;
use super::super::*;
use super::record_figures;
use crate::ai_audit::BatchApi;
use crate::checkpoint::{BatchRecord, StageCheckpoint};
use crate::llm::{
    BatchItem, BatchItemParams, BatchMessage, BatchRequestCounts, BatchResultRow, BatchStatus,
    CacheTtl, LlmPort, LlmRequest, LlmResponse,
};

fn state_dir() -> (tempfile::TempDir, camerata_test_env::EnvGuard) {
    let dir = tempfile::tempdir().unwrap();
    let guard = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", dir.path());
    (dir, guard)
}

fn new_run(repo: &std::path::Path) -> Arc<RunStore> {
    RunStore::create(NewRun {
        pin: engine_pin::EnginePin::for_run(None, &Default::default()),
        project_key: "acme-portal".to_string(),
        repos: vec![("acme/portal".to_string(), repo.to_path_buf())],
        models: ModelsRecord {
            audit: "claude-a".to_string(),
            calibration: "claude-a".to_string(),
            fix: "claude-a".to_string(),
            backend: "api".to_string(),
            mode: "batch".to_string(),
            resolved: true,
            group_review: String::new(),
            finder: String::new(),
            finder_model: String::new(),
            scan_exclusions: Vec::new(),
        },
        ai_scan_key: AiScanKey {
            audit_model: "claude-a".to_string(),
            calibration_model: String::new(),
            prompt_schema_version: 2,
            directives_fingerprint: "fp1".to_string(),
            terse_output: true,
            applies_to_routing: true,
        },
        args: serde_json::json!({}),
        estimate_usd: None,
        include_alternatives: false,
        origin: RunOrigin::Cli,
        project_id: None,
    })
    .unwrap()
}

/// A provider whose batches answer the status a test sets, one id per submit.
#[derive(Default)]
struct Provider {
    submits: AtomicUsize,
    status: Mutex<String>,
}

struct NoRealtime;

#[async_trait::async_trait]
impl LlmPort for NoRealtime {
    async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
        anyhow::bail!("no real-time call in these tests")
    }
    async fn complete_streaming(
        &self,
        _req: LlmRequest,
        _on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        anyhow::bail!("no real-time call in these tests")
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[async_trait::async_trait]
impl BatchApi for Provider {
    fn has_api_key(&self) -> bool {
        true
    }
    fn realtime(&self) -> &dyn LlmPort {
        &NoRealtime
    }
    async fn count_input_tokens(&self, _req: &LlmRequest, _model: &str) -> Option<u64> {
        None
    }
    async fn submit(&self, _items: Vec<BatchItem>) -> anyhow::Result<String> {
        let n = self.submits.fetch_add(1, Ordering::SeqCst);
        Ok(format!("msgbatch_acme_{n}"))
    }
    async fn poll(&self, _id: &str) -> anyhow::Result<BatchStatus> {
        Ok(BatchStatus {
            processing_status: self.status.lock().unwrap().clone(),
            request_counts: BatchRequestCounts::default(),
        })
    }
    async fn fetch(&self, _id: &str, _ttl: CacheTtl) -> anyhow::Result<Vec<BatchResultRow>> {
        Ok(Vec::new())
    }
}

fn item(custom_id: &str) -> BatchItem {
    BatchItem {
        custom_id: custom_id.to_string(),
        params: BatchItemParams {
            model: "claude-a".to_string(),
            max_tokens: 100,
            system: None,
            messages: vec![BatchMessage {
                role: "user".to_string(),
                content: serde_json::json!(custom_id),
            }],
            output_config: None,
            temperature: None,
        },
    }
}

async fn seeded<F: std::future::Future>(fut: F) -> F::Output {
    crate::prompt_frame::with_seed(Some([9u8; crate::prompt_frame::NONCE_BYTES]), fut).await
}

/// The batch a stage's checkpoint lists.
fn listed(batch_id: &str, settled: bool) -> BatchRecord {
    BatchRecord {
        batch_id: batch_id.to_string(),
        ttl: "1h".to_string(),
        items: Default::default(),
        members: Default::default(),
        settled,
        nonce: None,
    }
}

fn stage(name: &str, submitted: u32, processing: u32, ended: u32) -> BatchStageStatus {
    BatchStageStatus {
        stage: name.to_string(),
        submitted,
        processing,
        ended,
    }
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_run_counts_its_batches_per_stage_in_the_three_states_and_names_none() {
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = tempfile::tempdir().unwrap();
        let store = new_run(repo.path());
        let provider = Arc::new(Provider::default());
        let api = JournalBatchApi::new(provider.clone(), store.clone());

        // Scan passes: one batch ended and read, one the provider is still working on.
        let scan_done = api.submit(vec![item("c0-b0")]).await.unwrap();
        *provider.status.lock().unwrap() = "ended".to_string();
        api.poll(&scan_done).await.unwrap();
        api.fetch(&scan_done, CacheTtl::OneHour).await.unwrap();
        let scan_busy = api.submit(vec![item("c1-b0")]).await.unwrap();
        *provider.status.lock().unwrap() = "in_progress".to_string();
        api.poll(&scan_busy).await.unwrap();
        // Calibration: one settled, one accepted and never polled.
        let cal_done = api.submit(vec![item("s01")]).await.unwrap();
        let cal_new = api.submit(vec![item("s02")]).await.unwrap();
        // Fix: one the last poll called ended, results not read yet.
        let fix_ended = api.submit(vec![item("s03")]).await.unwrap();
        *provider.status.lock().unwrap() = "ended".to_string();
        api.poll(&fix_ended).await.unwrap();

        let checkpoints = crate::checkpoint::checkpoint_dir_of(store.dir());
        std::fs::create_dir_all(&checkpoints).unwrap();
        StageCheckpoint::new(&checkpoints, "calibration_acme_portal")
            .save_batches(&[listed(&cal_done, true), listed(&cal_new, false)])
            .unwrap();
        StageCheckpoint::new(&checkpoints, "fix_acme_portal")
            .save_batches(&[listed(&fix_ended, false)])
            .unwrap();
        store.finish(&FinishOutcome::default());

        let figures = record_figures(store.dir()).unwrap();
        assert_eq!(
            figures.batches,
            vec![
                stage("scan_pass", 0, 1, 1),
                stage("calibration", 1, 0, 1),
                stage("fix_generation", 0, 0, 1),
            ]
        );
        let body = serde_json::to_string(&figures).unwrap();
        for leak in ["msgbatch", "acme", "portal", "/"] {
            assert!(!body.contains(leak), "`{leak}` reached the figures: {body}");
        }
    })
    .await;
}

#[test]
#[serial_test::serial(process_env)]
fn wall_clock_adds_up_every_session_of_the_run() {
    let (_s, _g) = state_dir();
    let repo = tempfile::tempdir().unwrap();
    let store = new_run(repo.path());
    let id = store.run_id();
    store.finish(&FinishOutcome::default());
    drop(store);
    let resumed = RunStore::open(&id).unwrap();
    resumed.finish(&FinishOutcome::default());
    let dir = resumed.dir().to_path_buf();
    drop(resumed);

    // The saved segments, set to known lengths the way the record holds them.
    let path = dir.join("run.json");
    let mut record: RunRecord = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(record.segments.len(), 2);
    record.segments[0].elapsed_ms = 61_000;
    record.segments[1].elapsed_ms = 4_500;
    std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();

    let figures = record_figures(&dir).unwrap();
    assert_eq!(figures.wall_clock_ms, Some(65_500));
    assert_eq!(figures.sessions, 2);
    assert!(figures.batches.is_empty(), "no batch was submitted");
}

#[test]
#[serial_test::serial(process_env)]
fn a_run_saved_before_segments_existed_has_its_wall_clock_not_recorded() {
    let (_s, _g) = state_dir();
    let repo = tempfile::tempdir().unwrap();
    let store = new_run(repo.path());
    store.finish(&FinishOutcome::default());
    let dir = store.dir().to_path_buf();
    drop(store);
    let path = dir.join("run.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    value.as_object_mut().unwrap().remove("segments");
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();

    let figures = record_figures(&dir).unwrap();
    assert_eq!(
        figures,
        RunRecordFigures {
            wall_clock_ms: None,
            sessions: 0,
            batches: Vec::new(),
        },
        "not recorded, never zero"
    );
}

#[test]
fn a_folder_with_no_run_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    assert!(matches!(
        record_figures(dir.path()),
        Err(RunStoreError::NotFound(_))
    ));
}

#[test]
fn a_checkpoint_name_keeps_only_its_stage() {
    use super::stage_of_checkpoint;
    assert_eq!(
        stage_of_checkpoint("calibration_acme_portal"),
        "calibration"
    );
    assert_eq!(
        stage_of_checkpoint("detail_acme_portal"),
        "detail_regeneration"
    );
    assert_eq!(
        stage_of_checkpoint("alternatives_acme_portal"),
        "alternatives"
    );
    assert_eq!(stage_of_checkpoint("fix_acme_portal"), "fix_generation");
    assert_eq!(stage_of_checkpoint("group-review"), "group_review");
    assert_eq!(stage_of_checkpoint("group-review-summary"), "group_review");
    assert_eq!(stage_of_checkpoint("finder_acme_portal"), "finder");
    assert_eq!(stage_of_checkpoint("anything_else"), "other");
}

// ── The route ───────────────────────────────────────────────────────────────────────────────

async fn route(id: &str) -> (axum::http::StatusCode, serde_json::Value) {
    use axum::response::IntoResponse;
    use http_body_util::BodyExt;
    let resp = crate::inspection_figures(axum::extract::Path(id.to_string()))
        .await
        .into_response();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn the_route_answers_the_figures_and_refuses_what_is_not_a_saved_run() {
    let (_s, _g) = state_dir();
    let repo = tempfile::tempdir().unwrap();
    let store = new_run(repo.path());
    let id = store.run_id();
    store.finish(&FinishOutcome::default());
    drop(store);

    let (status, body) = route(&id).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    let back: RunRecordFigures = serde_json::from_value(body).unwrap();
    assert_eq!(back.sessions, 1);
    assert!(back.wall_clock_ms.is_some());

    for bad in ["20991231T000000Z-ffffff", "..", "../runs", "a/b", ""] {
        let (status, body) = route(bad).await;
        assert_eq!(status, axum::http::StatusCode::NOT_FOUND, "{bad}");
        assert_eq!(body["error"], "no saved run with this id", "{bad}");
    }
}

// ── The client package pin ──────────────────────────────────────────────────────────────────

/// The saved report the app's panel test renders, parsed as the engine's own report type: the
/// fixture has the shape the engine writes.
const SAVED_REPORT: &str =
    include_str!("../../../ui-core/src/test_support/run_figures/acme-portal-report.json");

fn saved_report() -> crate::onboard::ScanReport {
    serde_json::from_str(SAVED_REPORT).expect("the panel's fixture is a saved report")
}

/// Every member of the client package built from `report`, around a stand-in PDF (no Typst
/// here), by the same assembly the export uses.
fn package_members(report: &crate::onboard::ScanReport) -> Vec<(String, Vec<u8>)> {
    use crate::report_export::{
        build_report_json, render_data_json, ReportOptions, ReviewedReport,
    };
    let sealed = report.clone().seal();
    let reviewed = ReviewedReport::unreviewed(&sealed, &std::collections::HashMap::new());
    let options = ReportOptions {
        client_name: "Acme Corporation".to_string(),
        ..ReportOptions::default()
    };
    let json = build_report_json(&reviewed, None, &options);
    let data = render_data_json(&json).unwrap();
    let package = crate::release_gate::assemble_package(
        &reviewed,
        None,
        &options,
        "acme-portal-inspection",
        (json, data, b"%PDF-1.7 stand-in".to_vec()),
    )
    .unwrap();
    let mut members = vec![
        ("data.json".to_string(), package.data_json.into_bytes()),
        ("json/inspection.json".to_string(), package.findings_json),
        ("README.txt".to_string(), package.readme.into_bytes()),
        ("workbook.xlsx".to_string(), package.xlsx),
        (
            "manifest provenance".to_string(),
            format!("{:?}", package.provenance).into_bytes(),
        ),
    ];
    members.extend(package.datasets);
    members
}

#[test]
fn the_panels_fixture_is_a_report_the_engine_reads_with_its_figures() {
    let report = saved_report();
    assert_eq!(report.code_lines, 48_213);
    assert_eq!(report.files_scanned, 311);
    assert_eq!(report.code_lines_by_language.len(), 3);
    let telemetry = report.ledger.stage_telemetry().unwrap();
    assert_eq!(telemetry.stages.len(), 3);
    assert_eq!(telemetry.total().calls, 166);
    assert_eq!(
        telemetry.stages[0].usage.last_call_unix_ms,
        Some(1_795_000_750_000)
    );
    // The panel reads the same stage names the engine writes.
    let names: Vec<String> = telemetry
        .stages
        .iter()
        .map(|s| {
            serde_json::to_value(s.stage)
                .unwrap()
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(names, ["scan_pass", "calibration", "fix_generation"]);
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn the_client_package_is_byte_identical_with_and_without_the_run_figures_panel() {
    let report = saved_report();
    let before = package_members(&report);

    // Everything the panel's sources do for the same run: the batch journal records where each
    // batch stands, and the folder's figures are read and served.
    seeded(async {
        let (_s, _g) = state_dir();
        let repo = tempfile::tempdir().unwrap();
        let store = new_run(repo.path());
        let provider = Arc::new(Provider::default());
        let api = JournalBatchApi::new(provider.clone(), store.clone());
        let busy = api.submit(vec![item("c0-b0")]).await.unwrap();
        *provider.status.lock().unwrap() = "in_progress".to_string();
        api.poll(&busy).await.unwrap();
        store.finish(&FinishOutcome::default());
        let id = store.run_id();
        let figures = record_figures(store.dir()).unwrap();
        assert_eq!(figures.batches, vec![stage("scan_pass", 0, 1, 0)]);
        drop(api);
        drop(store);
        let (status, _) = route(&id).await;
        assert_eq!(status, axum::http::StatusCode::OK);
    })
    .await;
    let after = package_members(&report);

    // And the same report without its ledger's figures: what the panel shows is not what the
    // package is built from.
    let mut without = report.clone();
    let mut ledger = serde_json::to_value(&without.ledger).unwrap();
    ledger.as_object_mut().unwrap().remove("stage_telemetry");
    without.ledger = serde_json::from_value(ledger).unwrap();
    assert!(without.ledger.stage_telemetry().is_none());
    let without_figures = package_members(&without);

    assert_eq!(before.len(), after.len());
    for ((name, a), (_, b)) in before.iter().zip(&after) {
        assert!(a == b, "{name} changed after the run figures were read");
    }
    for ((name, a), (_, b)) in before.iter().zip(&without_figures) {
        if name == "workbook.xlsx" {
            continue;
        }
        assert!(
            a == b,
            "{name} differs with the ledger's run figures removed"
        );
    }
    // The figures the panel shows appear nowhere in the package's text members.
    for (name, bytes) in &before {
        if name == "workbook.xlsx" {
            continue;
        }
        let text = String::from_utf8_lossy(bytes);
        for figure in [
            "1234567",
            "1,234,567",
            "654321",
            "2401000",
            "1795000750000",
            "640000",
            "Run figures",
            "wall_clock",
            "batch_wait",
            "busy_ms",
            "processing",
        ] {
            assert!(!text.contains(figure), "`{figure}` is in {name}");
        }
    }
}
