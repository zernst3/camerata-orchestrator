//! The run's model seam with the long wait for the network (#582).
//!
//! [`camerata_llm::network_wait`] holds the schedule and the rules (1, 2, 4 and 8 minutes, a
//! transport failure only, an auth failure never). This module puts it around the two seams a run
//! sends through: the real-time [`LlmPort`] and the Message Batches [`BatchApi`]. A batch seam
//! stays a batch seam: the batch dispatch finds a [`BatchSeam`] by downcast, so a seam that carries
//! one is rebuilt as a [`BatchSeam`] over a [`WaitingBatchApi`], exactly as the run journal
//! rebuilds it over its own ([`crate::run_store::journal::wrap`]).
//!
//! [`run_model_seam`] is the ONE place a run's seam is built, for the command line and the app
//! alike: the journal (when the run has a folder), then the wait around it.

use std::sync::Arc;

pub use camerata_llm::network_wait::{
    is_network_wait_exhausted, NetworkWait, NetworkWaitExhausted, NETWORK_WAIT_STEPS,
    NETWORK_WAIT_TOTAL,
};

/// TEST SUPPORT: the process-wide sleeper every wait sleeps through, for a virtual clock in an
/// end-to-end test that cannot reach the seam it exercises. Never compiled into production.
#[cfg(any(test, feature = "test-support"))]
pub use camerata_llm::set_process_sleeper;
pub use camerata_llm::Sleeper;

use crate::ai_audit::{BatchApi, BatchSeam};
use crate::llm::{
    BatchItem, BatchResultRow, BatchStatus, CacheTtl, LlmPort, LlmRequest, LlmResponse,
};
use crate::operator_stop::RunControl;
use crate::run_store::RunStore;

/// Build a run's model seam over `base`: journaled into `store` when the run has a folder, then
/// with the long network wait of `wait` around every send. The command line and the app both call
/// this, so a short blip costs either of them minutes, never the run.
///
/// `control` is the run's control handle ([`crate::operator_stop::RunControl`]): it holds the
/// network wait, refuses every send once the run is stopped and keeps the list of live batches
/// the stop cancels.
pub fn run_model_seam<'a>(
    base: &'a dyn LlmPort,
    store: Option<Arc<RunStore>>,
    control: Arc<RunControl>,
) -> Box<dyn LlmPort + 'a> {
    if let Some(store) = &store {
        control.attach_store(store.clone());
    }
    match store {
        Some(store) => wrap_owned(crate::run_store::journal::wrap(base, store), control),
        None => wrap(base, control),
    }
}

/// The model seam of a deterministic rerun (#588): the store-only seam, journaled into the run's
/// store and NOT wrapped in the network wait. A rerun asks no model, so a miss is a refusal to
/// report at once; waiting minutes for a network nobody uses would only hide it. `None` when
/// the run has no store (the caller then uses `base` as it is).
pub fn rerun_model_seam<'a>(
    base: &'a dyn LlmPort,
    store: Option<Arc<RunStore>>,
) -> Option<Box<dyn LlmPort + 'a>> {
    store.map(|store| crate::run_store::journal::wrap(base, store))
}

/// Put the wait around a borrowed seam. A seam that carries a [`BatchApi`] (a [`BatchSeam`], or
/// the real client) becomes a [`BatchSeam`] over a [`WaitingBatchApi`]; any other seam becomes a
/// [`WaitingLlm`].
pub fn wrap<'a>(llm: &'a dyn LlmPort, control: Arc<RunControl>) -> Box<dyn LlmPort + 'a> {
    if let Some(api) = batch_api_of(llm) {
        return Box::new(BatchSeam::new(Arc::new(WaitingBatchApi::new(api, control))));
    }
    Box::new(WaitingLlm {
        inner: Inner::Borrowed(llm),
        control,
    })
}

/// [`wrap`] for a seam the caller hands over.
fn wrap_owned<'a>(llm: Box<dyn LlmPort + 'a>, control: Arc<RunControl>) -> Box<dyn LlmPort + 'a> {
    if let Some(api) = batch_api_of(llm.as_ref()) {
        return Box::new(BatchSeam::new(Arc::new(WaitingBatchApi::new(api, control))));
    }
    Box::new(WaitingLlm {
        inner: Inner::Owned(llm),
        control,
    })
}

/// The batch API a seam carries, shared: a [`BatchSeam`]'s own, or the real client's.
fn batch_api_of(llm: &dyn LlmPort) -> Option<Arc<dyn BatchApi>> {
    if let Some(seam) = llm.as_any().downcast_ref::<BatchSeam>() {
        return Some(seam.api_arc());
    }
    llm.as_any()
        .downcast_ref::<crate::llm::Llm>()
        .map(|real| Arc::new(real.clone()) as Arc<dyn BatchApi>)
}

enum Inner<'a> {
    Borrowed(&'a dyn LlmPort),
    Owned(Box<dyn LlmPort + 'a>),
}

impl Inner<'_> {
    fn get(&self) -> &dyn LlmPort {
        match self {
            Inner::Borrowed(l) => *l,
            Inner::Owned(l) => l.as_ref(),
        }
    }
}

/// A real-time seam whose every send waits for the network ([`NetworkWait::run`]).
pub struct WaitingLlm<'a> {
    inner: Inner<'a>,
    control: Arc<RunControl>,
}

/// The two real-time calls, with the wait, over any seam.
async fn complete_waiting(
    inner: &dyn LlmPort,
    control: &RunControl,
    req: LlmRequest,
) -> anyhow::Result<LlmResponse> {
    if control.refuses_sends() {
        return Err(control.refusal());
    }
    control
        .network_wait()
        .run(|| inner.complete(req.clone()), &|| true)
        .await
}

async fn complete_streaming_waiting(
    inner: &dyn LlmPort,
    control: &RunControl,
    req: LlmRequest,
    on_delta: &mut (dyn for<'b> FnMut(&'b str) + Send),
) -> anyhow::Result<LlmResponse> {
    if control.refuses_sends() {
        return Err(control.refusal());
    }
    let wait = control.network_wait();
    // A streamed reply that already delivered text is never sent again: the reader would get the
    // text twice. Only a send that delivered nothing waits and goes again.
    let delivered = std::sync::atomic::AtomicBool::new(false);
    let sink = std::sync::Mutex::new(on_delta);
    let may_retry = || !delivered.load(std::sync::atomic::Ordering::SeqCst);
    wait.run(
        || {
            let req = req.clone();
            let delivered = &delivered;
            let sink = &sink;
            async move {
                let mut forward = |d: &str| {
                    delivered.store(true, std::sync::atomic::Ordering::SeqCst);
                    let mut guard = sink
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    (*guard)(d);
                };
                inner.complete_streaming(req, &mut forward).await
            }
        },
        &may_retry,
    )
    .await
}

#[async_trait::async_trait]
impl LlmPort for WaitingLlm<'_> {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        complete_waiting(self.inner.get(), &self.control, req).await
    }

    async fn complete_streaming(
        &self,
        req: LlmRequest,
        on_delta: &mut (dyn for<'b> FnMut(&'b str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        complete_streaming_waiting(self.inner.get(), &self.control, req, on_delta).await
    }

    fn note_rejected_reply(&self, request_key: &str, reason: &str, whole: bool) {
        self.inner
            .get()
            .note_rejected_reply(request_key, reason, whole);
    }

    fn replays(&self, req: &LlmRequest) -> bool {
        self.inner.get().replays(req)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        // A borrowed seam cannot be downcast (it is not 'static), and a seam with a batch path is
        // a `BatchSeam` over a `WaitingBatchApi` instead (see `wrap`).
        &()
    }
}

/// A [`BatchApi`] whose every call (the real-time half, submit, poll, fetch, the token count's
/// network is the provider's own and is left as it is) waits for the network.
pub struct WaitingBatchApi {
    inner: Arc<dyn BatchApi>,
    control: Arc<RunControl>,
}

impl WaitingBatchApi {
    pub fn new(inner: Arc<dyn BatchApi>, control: Arc<RunControl>) -> Self {
        Self { inner, control }
    }
}

#[async_trait::async_trait]
impl LlmPort for WaitingBatchApi {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        complete_waiting(self.inner.realtime(), &self.control, req).await
    }

    async fn complete_streaming(
        &self,
        req: LlmRequest,
        on_delta: &mut (dyn for<'b> FnMut(&'b str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        complete_streaming_waiting(self.inner.realtime(), &self.control, req, on_delta).await
    }

    fn note_rejected_reply(&self, request_key: &str, reason: &str, whole: bool) {
        self.inner
            .realtime()
            .note_rejected_reply(request_key, reason, whole);
    }

    fn replays(&self, req: &LlmRequest) -> bool {
        self.inner.realtime().replays(req)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[async_trait::async_trait]
impl BatchApi for WaitingBatchApi {
    fn has_api_key(&self) -> bool {
        self.inner.has_api_key()
    }

    fn realtime(&self) -> &dyn LlmPort {
        self
    }

    async fn count_input_tokens(&self, req: &LlmRequest, model: &str) -> Option<u64> {
        // Best effort by contract (`None` when unavailable): a count that fails is not a failed
        // part, so it does not wait.
        self.inner.count_input_tokens(req, model).await
    }

    async fn submit(&self, items: Vec<BatchItem>) -> anyhow::Result<String> {
        // No batch is submitted once the run is stopped: it would run and bill with nobody to
        // read it.
        if self.control.refuses_sends() {
            return Err(self.control.refusal());
        }
        // A submit is waited out and sent again only while every failure provably never reached
        // the provider (refused, DNS, connect timeout: by kind, `never_reached_provider`). A
        // failure after the request went out (a timeout after sending, a dropped reply) may be
        // a batch the provider accepted: sending again could bill a second batch nobody tracks,
        // so it comes back at once.
        let maybe_sent = std::sync::atomic::AtomicBool::new(false);
        let id = self
            .control
            .network_wait()
            .run(
                || async {
                    let out = self.inner.submit(items.clone()).await;
                    if let Err(e) = &out {
                        if !camerata_llm::failure::never_reached_provider(e) {
                            maybe_sent.store(true, std::sync::atomic::Ordering::SeqCst);
                        }
                    }
                    out
                },
                &|| !maybe_sent.load(std::sync::atomic::Ordering::SeqCst),
            )
            .await?;
        self.control.note_batch_submitted(&id, self.inner.clone());
        // A stop that began while the submit was on its way cancels this batch too.
        if self.control.is_stopped() {
            if let Err(e) = self.inner.cancel_batch(&id).await {
                eprintln!(
                    "[camerata-server] batch {id} was accepted as the run stopped and could not \
                     be canceled ({e:#}); cancel it in the Anthropic console"
                );
            } else {
                self.control.note_batch_finished(&id);
            }
        }
        Ok(id)
    }

    // A poll or a fetch after the run stopped is let through on purpose: neither is billed, and
    // reading a batch the provider ran (or is running) is how the run keeps what it paid for.
    // Only `submit` is refused once the run refuses sends, and the network wait itself ends
    // at its next step after a stop (`NetworkWait::stop`), so a stopped run never waits on one.
    async fn poll(&self, batch_id: &str) -> anyhow::Result<BatchStatus> {
        let status = self
            .control
            .network_wait()
            .run(|| self.inner.poll(batch_id), &|| true)
            .await?;
        self.control.note_batch_polled(
            batch_id,
            self.inner.clone(),
            status.processing_status == "ended",
        );
        Ok(status)
    }

    async fn fetch(&self, batch_id: &str, ttl: CacheTtl) -> anyhow::Result<Vec<BatchResultRow>> {
        let rows = self
            .control
            .network_wait()
            .run(|| self.inner.fetch(batch_id, ttl), &|| true)
            .await?;
        self.control.note_batch_finished(batch_id);
        Ok(rows)
    }

    async fn cancel_batch(&self, batch_id: &str) -> anyhow::Result<()> {
        // A cancel is sent once, at once: it never waits for the network.
        let out = self.inner.cancel_batch(batch_id).await;
        if out.is_ok() {
            self.control.note_batch_finished(batch_id);
        }
        out
    }
}

/// TEST SUPPORT: while alive, every network wait (and every transport retry wait of the real
/// client) returns at once and is recorded, so a test that drives a run through a provider that
/// cannot be reached ends in milliseconds. Process-wide: a test that holds one runs serially.
#[cfg(any(test, feature = "test-support"))]
#[must_use = "the waits sleep for real again once the guard is dropped"]
pub struct InstantWaits(Arc<WaitRecorder>);

#[cfg(any(test, feature = "test-support"))]
#[derive(Default)]
struct WaitRecorder(std::sync::Mutex<Vec<std::time::Duration>>);

#[cfg(any(test, feature = "test-support"))]
impl Sleeper for WaitRecorder {
    fn sleep<'a>(
        &'a self,
        d: std::time::Duration,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(d);
        Box::pin(tokio::task::yield_now())
    }
}

#[cfg(any(test, feature = "test-support"))]
impl InstantWaits {
    pub fn install() -> Self {
        let recorder = Arc::new(WaitRecorder::default());
        set_process_sleeper(Some(recorder.clone()));
        Self(recorder)
    }

    /// Every wait asked for so far, in order.
    pub fn waits(&self) -> Vec<std::time::Duration> {
        self.0
             .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for InstantWaits {
    fn drop(&mut self) {
        set_process_sleeper(None);
    }
}

#[cfg(test)]
#[path = "network_wait_tests.rs"]
mod tests;
