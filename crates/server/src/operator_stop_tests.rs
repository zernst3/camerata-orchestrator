//! The operator's stop (#583): THE stop cancels every live batch at the provider, lists each one it
//! could not cancel with its id, and refuses every send after it.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use super::*;
use crate::ai_audit::{BatchApi, BatchSeam};
use crate::llm::{
    BatchItem, BatchRequestCounts, BatchResultRow, BatchStatus, CacheTtl, LlmPort, LlmRequest,
    LlmResponse,
};

fn reply() -> LlmResponse {
    LlmResponse {
        reported_model: None,
        cli_cost_usd: None,
        elapsed_ms: None,
        text: "{}".to_string(),
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

/// A provider whose batches stay in progress until cancelled.
#[derive(Default)]
struct Provider {
    submits: AtomicUsize,
    sends: AtomicUsize,
    cancels: Mutex<Vec<String>>,
    /// Batch ids whose cancel the provider refuses.
    refuse_cancel: Vec<String>,
    /// Batch ids whose cancel never answers.
    hang_cancel: Vec<String>,
    /// Batch ids that read "ended" when polled.
    ended: Vec<String>,
}

#[async_trait::async_trait]
impl LlmPort for Provider {
    async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.sends.fetch_add(1, Ordering::SeqCst);
        Ok(reply())
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
impl BatchApi for Provider {
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
        Ok(format!("msgbatch_acme_{n}"))
    }
    async fn poll(&self, batch_id: &str) -> anyhow::Result<BatchStatus> {
        Ok(BatchStatus {
            processing_status: if self.ended.iter().any(|e| e == batch_id) {
                "ended"
            } else {
                "in_progress"
            }
            .to_string(),
            request_counts: BatchRequestCounts::default(),
        })
    }
    async fn fetch(&self, _batch_id: &str, _ttl: CacheTtl) -> anyhow::Result<Vec<BatchResultRow>> {
        Ok(Vec::new())
    }
    async fn cancel_batch(&self, batch_id: &str) -> anyhow::Result<()> {
        self.cancels.lock().unwrap().push(batch_id.to_string());
        if self.hang_cancel.iter().any(|h| h == batch_id) {
            std::future::pending::<()>().await;
        }
        if self.refuse_cancel.iter().any(|r| r == batch_id) {
            anyhow::bail!("Message Batches API HTTP 500: internal error");
        }
        Ok(())
    }
}

/// The run's seam over `provider`, and the batch API inside it.
fn seam(provider: Arc<Provider>, control: &Arc<RunControl>) -> Box<dyn LlmPort + 'static> {
    let base = Box::leak(Box::new(BatchSeam::new(provider)));
    crate::network_wait::wrap(base, control.clone())
}

fn api_of(seam: &dyn LlmPort) -> &dyn BatchApi {
    seam.as_any()
        .downcast_ref::<BatchSeam>()
        .expect("a batch seam stays a batch seam")
        .api()
}

#[tokio::test]
async fn the_stop_cancels_every_live_batch_at_the_provider() {
    let provider = Arc::new(Provider::default());
    let control = Arc::new(RunControl::new());
    let seam = seam(provider.clone(), &control);
    let api = api_of(seam.as_ref());
    let a = api.submit(Vec::new()).await.unwrap();
    let b = api.submit(Vec::new()).await.unwrap();
    assert_eq!(control.live_batches(), vec![a.clone(), b.clone()]);

    let report = control.stop().await;
    assert_eq!(report.cancelled, vec![a.clone(), b.clone()]);
    assert!(report.not_cancelled.is_empty());
    let mut asked = provider.cancels.lock().unwrap().clone();
    asked.sort();
    assert_eq!(asked, vec![a, b]);
    assert!(control.live_batches().is_empty());
    // A second stop cancels nothing twice.
    let again = control.stop().await;
    assert!(again.cancelled.is_empty());
    assert_eq!(provider.cancels.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn a_batch_the_provider_will_not_cancel_is_listed_with_its_id() {
    let provider = Arc::new(Provider {
        refuse_cancel: vec!["msgbatch_acme_1".to_string()],
        ..Provider::default()
    });
    let control = Arc::new(RunControl::new());
    let seam = seam(provider.clone(), &control);
    let api = api_of(seam.as_ref());
    api.submit(Vec::new()).await.unwrap();
    api.submit(Vec::new()).await.unwrap();
    let report = control.stop().await;
    assert_eq!(report.cancelled, vec!["msgbatch_acme_0".to_string()]);
    assert_eq!(report.not_cancelled.len(), 1);
    assert_eq!(report.not_cancelled[0].batch_id, "msgbatch_acme_1");
    assert!(
        report.not_cancelled[0].error.contains("HTTP 500"),
        "{:?}",
        report.not_cancelled[0]
    );
}

#[tokio::test(start_paused = true)]
async fn a_cancel_that_never_answers_is_listed_after_the_timeout() {
    let provider = Arc::new(Provider {
        hang_cancel: vec!["msgbatch_acme_0".to_string()],
        ..Provider::default()
    });
    let control = Arc::new(RunControl::new());
    let seam = seam(provider.clone(), &control);
    api_of(seam.as_ref()).submit(Vec::new()).await.unwrap();
    let started = tokio::time::Instant::now();
    let report = control.stop().await;
    assert_eq!(started.elapsed(), CANCEL_TIMEOUT);
    assert_eq!(report.not_cancelled.len(), 1);
    assert_eq!(report.not_cancelled[0].batch_id, "msgbatch_acme_0");
}

#[tokio::test]
async fn a_batch_that_ended_or_was_fetched_is_not_cancelled() {
    let provider = Arc::new(Provider {
        ended: vec!["msgbatch_acme_0".to_string()],
        ..Provider::default()
    });
    let control = Arc::new(RunControl::new());
    let seam = seam(provider.clone(), &control);
    let api = api_of(seam.as_ref());
    let ended = api.submit(Vec::new()).await.unwrap();
    let fetched = api.submit(Vec::new()).await.unwrap();
    let live = api.submit(Vec::new()).await.unwrap();
    api.poll(&ended).await.unwrap();
    api.fetch(&fetched, CacheTtl::OneHour).await.unwrap();
    // A resume polling a batch an earlier process submitted learns it is live.
    api.poll("msgbatch_earlier").await.unwrap();
    assert_eq!(
        control.live_batches(),
        vec!["msgbatch_acme_2".to_string(), "msgbatch_earlier".to_string()]
    );
    let report = control.stop().await;
    assert_eq!(report.cancelled, vec![live, "msgbatch_earlier".to_string()]);
}

#[tokio::test]
async fn after_the_stop_no_send_and_no_batch_leaves_the_run() {
    let provider = Arc::new(Provider::default());
    let control = Arc::new(RunControl::new());
    let seam = seam(provider.clone(), &control);
    seam.complete(LlmRequest::new("before")).await.unwrap();
    control.stop().await;
    let e = seam
        .complete(LlmRequest::new("after"))
        .await
        .expect_err("a send after the stop is refused");
    assert_eq!(
        crate::failure::classify_error(&e),
        FailureClass::OutageStop,
        "{e:#}"
    );
    let e = api_of(seam.as_ref())
        .submit(Vec::new())
        .await
        .expect_err("a batch after the stop is refused");
    assert!(e.to_string().contains("operator stopped"), "{e:#}");
    assert_eq!(provider.sends.load(Ordering::SeqCst), 1);
    assert_eq!(provider.submits.load(Ordering::SeqCst), 0);
    assert!(control.network_wait().is_stopped());
}

#[tokio::test]
async fn a_stop_cancels_the_live_batches_before_the_run_is_dropped() {
    let provider = Arc::new(Provider::default());
    let control = Arc::new(RunControl::new());
    let seam = seam(provider.clone(), &control);
    let api = api_of(seam.as_ref());
    let (go, stop) = tokio::sync::oneshot::channel::<()>();
    let run = async {
        api.submit(Vec::new()).await.unwrap();
        let _ = go.send(());
        // The run waits on its batch for ever.
        std::future::pending::<()>().await;
    };
    let ended = run_until_stopped(&control, run, async {
        let _ = stop.await;
    })
    .await;
    match ended {
        Ended::Stopped(report) => assert_eq!(report.cancelled, vec!["msgbatch_acme_0".to_string()]),
        Ended::Finished(()) => panic!("the run cannot finish"),
    }
}
