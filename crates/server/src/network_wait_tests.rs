//! The run's model seam waits for the network (#582): the decorator keeps a batch seam a batch
//! seam, sends the same request again on a transport failure, never resends a stream that already
//! delivered text, and an exhausted wait trips the outage stop at once.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use super::*;
use crate::ai_audit::UsageMeter;
use crate::failure::{FailureClass, LlmCallFailure};
use crate::llm::BatchRequestCounts;
use crate::stage_telemetry::Stage;

fn transport() -> anyhow::Error {
    anyhow::Error::new(LlmCallFailure::new(
        FailureClass::Transport,
        "Anthropic API request failed: connection reset by peer",
    ))
}

fn reply(text: &str) -> LlmResponse {
    LlmResponse {
        reported_model: None,
        cli_cost_usd: None,
        elapsed_ms: None,
        text: text.to_string(),
        model: "claude-sonnet-5-5".to_string(),
        backend: "api".to_string(),
        cost_usd: Some(0.01),
        input_tokens: Some(10),
        output_tokens: Some(1),
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
        or_cache_discount: None,
        truncated: false,
    }
}

/// Down for the first `down` sends of each kind, then up. Streams deliver a delta first when
/// `stream_delivers_then_fails` is set, then fail.
#[derive(Default)]
struct Flaky {
    down: usize,
    sends: AtomicUsize,
    polls: AtomicUsize,
    prompts: Mutex<Vec<String>>,
    stream_delivers_then_fails: bool,
}

#[async_trait::async_trait]
impl LlmPort for Flaky {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.prompts.lock().unwrap().push(req.prompt.clone());
        if self.sends.fetch_add(1, Ordering::SeqCst) < self.down {
            return Err(transport());
        }
        Ok(reply("{}"))
    }
    async fn complete_streaming(
        &self,
        req: LlmRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        if self.stream_delivers_then_fails {
            self.sends.fetch_add(1, Ordering::SeqCst);
            on_delta("partial");
            return Err(transport());
        }
        let r = self.complete(req).await?;
        on_delta(&r.text);
        Ok(r)
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[async_trait::async_trait]
impl BatchApi for Flaky {
    fn has_api_key(&self) -> bool {
        true
    }
    fn realtime(&self) -> &dyn LlmPort {
        self
    }
    async fn count_input_tokens(&self, _req: &LlmRequest, _model: &str) -> Option<u64> {
        None
    }
    async fn submit(&self, _items: Vec<BatchItem>) -> anyhow::Result<String> {
        Ok("msgbatch_acme_0".to_string())
    }
    async fn poll(&self, _batch_id: &str) -> anyhow::Result<BatchStatus> {
        if self.polls.fetch_add(1, Ordering::SeqCst) < self.down {
            return Err(transport());
        }
        Ok(BatchStatus {
            processing_status: "ended".to_string(),
            request_counts: BatchRequestCounts::default(),
        })
    }
    async fn fetch(&self, _batch_id: &str, _ttl: CacheTtl) -> anyhow::Result<Vec<BatchResultRow>> {
        Ok(Vec::new())
    }
}

fn control() -> Arc<RunControl> {
    Arc::new(RunControl::new())
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_real_time_send_waits_out_a_short_drop_and_sends_the_same_request_again() {
    let waits = InstantWaits::install();
    let flaky = Flaky {
        down: 3,
        ..Flaky::default()
    };
    let seam = wrap(&flaky, control());
    let r = seam.complete(LlmRequest::new("the same question")).await;
    assert_eq!(r.unwrap().text, "{}");
    assert_eq!(waits.waits(), NETWORK_WAIT_STEPS[..3].to_vec());
    let prompts = flaky.prompts.lock().unwrap().clone();
    assert_eq!(prompts.len(), 4);
    assert!(prompts.iter().all(|p| p == "the same question"));
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn an_exhausted_wait_trips_the_outage_stop_at_once() {
    let _waits = InstantWaits::install();
    let flaky = Flaky {
        down: usize::MAX,
        ..Flaky::default()
    };
    let seam = wrap(&flaky, control());
    let e = seam
        .complete(LlmRequest::new("q"))
        .await
        .expect_err("the network never came back");
    assert!(is_network_wait_exhausted(&e), "{e:#}");
    // One failed call is enough: the streak's span was waited out many times over.
    let meter = UsageMeter::default();
    meter.record_failed(Stage::Calibration, &e);
    assert!(meter.outage_stopped(), "the tagged failure stops the run");
    assert_eq!(
        meter.health().trigger().map(|t| t.class),
        Some(FailureClass::Transport)
    );
    // An untagged transport failure is one step of the streak, as before.
    let meter = UsageMeter::default();
    meter.record_failed(Stage::Calibration, &transport());
    assert!(!meter.outage_stopped());
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_stream_that_delivered_text_is_never_sent_again() {
    let waits = InstantWaits::install();
    let flaky = Flaky {
        stream_delivers_then_fails: true,
        ..Flaky::default()
    };
    let seam = wrap(&flaky, control());
    let mut seen = String::new();
    let r = seam
        .complete_streaming(LlmRequest::new("q"), &mut |d| seen.push_str(d))
        .await;
    assert!(r.is_err());
    assert_eq!(seen, "partial", "the reader got the text once");
    assert_eq!(flaky.sends.load(Ordering::SeqCst), 1);
    assert!(waits.waits().is_empty());
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_batch_seam_stays_a_batch_seam_and_its_polls_wait_too() {
    let waits = InstantWaits::install();
    let flaky = Arc::new(Flaky {
        down: 2,
        ..Flaky::default()
    });
    let base = BatchSeam::new(flaky.clone());
    let seam = wrap(&base, control());
    let api = seam
        .as_any()
        .downcast_ref::<BatchSeam>()
        .expect("the batch dispatch still finds its batch seam")
        .api();
    let status = api.poll("msgbatch_acme_0").await.unwrap();
    assert_eq!(status.processing_status, "ended");
    assert_eq!(flaky.polls.load(Ordering::SeqCst), 3);
    assert_eq!(waits.waits(), NETWORK_WAIT_STEPS[..2].to_vec());
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn the_journaled_run_seam_keeps_the_batch_path() {
    let _waits = InstantWaits::install();
    let state = tempfile::tempdir().unwrap();
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state.path());
    let store = new_store();
    let flaky = Arc::new(Flaky::default());
    let base = BatchSeam::new(flaky);
    let control = control();
    let seam = run_model_seam(&base, Some(store.clone()), control.clone());
    assert!(
        seam.as_any().downcast_ref::<BatchSeam>().is_some(),
        "journal, then wait: still a batch seam"
    );
    assert_eq!(control.run_id(), Some(store.run_id()));
}

/// A run folder in the state directory the caller set.
fn new_store() -> Arc<crate::run_store::RunStore> {
    crate::run_store::RunStore::create(crate::run_store::NewRun {
        project_key: "acme-project".to_string(),
        repos: Vec::new(),
        models: crate::run_store::ModelsRecord {
            audit: "claude-sonnet-5-5".to_string(),
            calibration: "claude-sonnet-5-5".to_string(),
            fix: "claude-sonnet-5-5".to_string(),
            backend: "api".to_string(),
            mode: "batch".to_string(),
            resolved: true,
            group_review: String::new(),
            finder: String::new(),
            finder_model: String::new(),
            scan_exclusions: Vec::new(),
        },
        ai_scan_key: crate::scan_cache::AiScanKey::default(),
        args: serde_json::Value::Null,
        estimate_usd: None,
        include_alternatives: false,
        origin: crate::run_store::RunOrigin::Cli,
        project_id: None,
        pin: crate::run_store::engine_pin::EnginePin::for_run(None, &Default::default()),
    })
    .expect("a run folder")
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_rerun_seam_never_waits_for_the_network() {
    // A rerun (#588) answers only from the run's store; a miss is reported at once, and a
    // failure the seam passes through is never waited out, with or without a store.
    let waits = InstantWaits::install();
    let state = tempfile::tempdir().unwrap();
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_STATE_DIR", state.path());
    for store in [None, Some(new_store())] {
        let flaky = Flaky {
            down: usize::MAX,
            ..Flaky::default()
        };
        let journaled = rerun_model_seam(&flaky, store.clone());
        let seam: &dyn LlmPort = journaled.as_deref().unwrap_or(&flaky);
        let e = crate::prompt_frame::with_seed(
            Some([7u8; crate::prompt_frame::NONCE_BYTES]),
            seam.complete(LlmRequest::new("a question the run never stored")),
        )
        .await
        .expect_err("the network is down");
        assert!(
            waits.waits().is_empty(),
            "a rerun never sleeps: {:?}",
            waits.waits()
        );
        assert_eq!(
            flaky.sends.load(Ordering::SeqCst),
            1,
            "sent once, never again"
        );
        assert_eq!(crate::failure::classify_error(&e), FailureClass::Transport);
        assert_eq!(
            journaled.is_some(),
            store.is_some(),
            "journaled when there is a store"
        );
    }
}

/// A batch API whose submit fails `down` times and then accepts. `not_sent` says whether each
/// failure happened before a connection was made (the provider never saw it) or after the
/// request went out (a timeout after sending: the provider may have accepted the batch).
#[derive(Default)]
struct FailingSubmit {
    down: usize,
    not_sent: bool,
    submits: AtomicUsize,
}

#[async_trait::async_trait]
impl LlmPort for FailingSubmit {
    async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
        Ok(reply("{}"))
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

#[async_trait::async_trait]
impl BatchApi for FailingSubmit {
    fn has_api_key(&self) -> bool {
        true
    }
    fn realtime(&self) -> &dyn LlmPort {
        self
    }
    async fn count_input_tokens(&self, _req: &LlmRequest, _model: &str) -> Option<u64> {
        None
    }
    async fn submit(&self, _items: Vec<BatchItem>) -> anyhow::Result<String> {
        let n = self.submits.fetch_add(1, Ordering::SeqCst);
        if n < self.down {
            let mut f = LlmCallFailure::new(
                FailureClass::Transport,
                "Anthropic API request failed after 3 retries: error sending request",
            );
            f.attempts = vec![
                camerata_llm::failure::AttemptRecord {
                    not_sent: self.not_sent,
                    ..Default::default()
                };
                4
            ];
            return Err(anyhow::Error::new(f));
        }
        Ok(format!("msgbatch_acme_{n}"))
    }
    async fn poll(&self, _batch_id: &str) -> anyhow::Result<BatchStatus> {
        Ok(BatchStatus {
            processing_status: "ended".to_string(),
            request_counts: BatchRequestCounts::default(),
        })
    }
    async fn fetch(&self, _batch_id: &str, _ttl: CacheTtl) -> anyhow::Result<Vec<BatchResultRow>> {
        Ok(Vec::new())
    }
}

fn batch_api_of_seam(seam: &dyn LlmPort) -> &dyn BatchApi {
    seam.as_any()
        .downcast_ref::<BatchSeam>()
        .expect("a batch seam")
        .api()
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_submit_that_may_have_reached_the_provider_is_never_sent_again() {
    // A timeout after the request went out: the provider may have accepted the batch and the
    // reply was lost. Sending it again could bill a second batch nobody tracks.
    let waits = InstantWaits::install();
    let provider = Arc::new(FailingSubmit {
        down: 1,
        not_sent: false,
        ..FailingSubmit::default()
    });
    let base = BatchSeam::new(provider.clone());
    let control = control();
    let seam = wrap(&base, control.clone());
    let e = batch_api_of_seam(seam.as_ref())
        .submit(Vec::new())
        .await
        .expect_err("the submit is not sent again");
    assert_eq!(
        provider.submits.load(Ordering::SeqCst),
        1,
        "a submit that may have been accepted was sent again"
    );
    assert!(waits.waits().is_empty(), "{:?}", waits.waits());
    assert_eq!(crate::failure::classify_error(&e), FailureClass::Transport);
    assert!(control.live_batches().is_empty());
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_submit_refused_before_any_connection_waits_and_is_sent_again() {
    let waits = InstantWaits::install();
    let provider = Arc::new(FailingSubmit {
        down: 2,
        not_sent: true,
        ..FailingSubmit::default()
    });
    let base = BatchSeam::new(provider.clone());
    let control = control();
    let seam = wrap(&base, control.clone());
    let id = batch_api_of_seam(seam.as_ref())
        .submit(Vec::new())
        .await
        .expect("the network came back");
    assert_eq!(id, "msgbatch_acme_2");
    assert_eq!(provider.submits.load(Ordering::SeqCst), 3);
    assert_eq!(waits.waits(), NETWORK_WAIT_STEPS[..2].to_vec());
    assert_eq!(control.live_batches(), vec![id]);
}
