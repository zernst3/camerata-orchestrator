//! The operator's stop (#583): Ctrl+C or SIGTERM on the command line, and the app's own stop
//! (#567), end a run the same way.
//!
//! [`RunControl`] is the run's control handle. It rides on the run's model seam
//! ([`crate::network_wait::run_model_seam`]), so it sees every send and every batch the run makes:
//!
//! * it holds the run's network wait ([`camerata_llm::network_wait::NetworkWait`]);
//! * it keeps the list of batches the provider accepted and has not finished (a submit adds, a
//!   poll that reads "ended", a fetch or a cancel removes);
//! * once stopped, it refuses every new send and every new batch at once, with class
//!   `OutageStop` ("never tried"), so no part starts after the stop.
//!
//! [`RunControl::stop`] is THE stop: it refuses further dispatch, ends every network wait, and
//! asks the provider to cancel every live batch, so nothing keeps running and billing after the
//! operator said stop. A batch the provider would not cancel comes back in the [`StopReport`],
//! with its id, so it lands in the failure report. What the run already finished is in its run
//! folder (the journal writes every reply as it comes), and `--resume` runs what remains; a
//! cancelled batch's finished items are fetched by that resume, never paid for twice.
//!
//! The app's stop calls the same [`RunControl::stop`] on the control it handed to its seam.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use camerata_llm::network_wait::NetworkWait;
use serde::{Deserialize, Serialize};

use crate::ai_audit::BatchApi;
use crate::failure::{FailureClass, LlmCallFailure};

/// The longest the stop waits on the provider to cancel one batch. A cancel that does not answer
/// in time is recorded as not cancelled, so the operator knows to cancel it by hand.
pub const CANCEL_TIMEOUT: Duration = Duration::from_secs(10);

/// What a refused send says, once the run was stopped.
pub const STOPPED_TEXT: &str = "the operator stopped this inspection before this part was sent";

/// What a part refused after the run's outage stop says.
pub const OUTAGE_TEXT: &str =
    "the run stopped because the model provider could not be reached, so this part was not sent";

/// A batch the stop asked the provider to cancel and could not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UncancelledBatch {
    pub batch_id: String,
    /// Why, in the provider's words where it gave any (scrubbed). Operator only.
    pub error: String,
}

/// What [`RunControl::stop`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopReport {
    /// RFC3339 UTC time of the stop.
    pub at: String,
    /// Every live batch the provider cancelled.
    pub cancelled: Vec<String>,
    /// Every live batch it did not, with the reason. Each may still run and be billed.
    pub not_cancelled: Vec<UncancelledBatch>,
}

/// The run's control handle. See the module docs.
#[derive(Default)]
pub struct RunControl {
    wait: NetworkWait,
    stopped: AtomicBool,
    /// Set once the run's outage stop tripped ([`Self::note_outage_stop`]).
    outage: AtomicBool,
    /// Live batches, by id, with the API that accepted each (to cancel it through).
    live: Mutex<Vec<(String, Arc<dyn BatchApi>)>>,
    /// The run's operator ledger file, once the run knows it: the stop's failure report goes
    /// beside it.
    ledger_file: Mutex<Option<PathBuf>>,
    /// The run's folder, once it exists: the stop notes itself there, and names the run that
    /// `--resume` takes.
    store: Mutex<Option<Arc<crate::run_store::RunStore>>>,
}

impl std::fmt::Debug for RunControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunControl")
            .field("stopped", &self.is_stopped())
            .field("live", &self.live_batches())
            .finish_non_exhaustive()
    }
}

impl RunControl {
    pub fn new() -> Self {
        Self::default()
    }

    /// The run's network wait.
    pub fn network_wait(&self) -> &NetworkWait {
        &self.wait
    }

    /// True once [`Self::stop`] began.
    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    /// The run's outage stop tripped (the run's health record, #582): every network wait ends at
    /// its next step and no further request is sent. Unlike [`Self::stop`], nothing is cancelled:
    /// a batch the provider is running still finishes, and a resume reads it.
    pub fn note_outage_stop(&self) {
        self.outage.store(true, Ordering::SeqCst);
        self.wait.stop();
    }

    /// True once the run sends nothing more: the operator stopped it, or its outage stop tripped.
    pub fn refuses_sends(&self) -> bool {
        self.is_stopped() || self.outage.load(Ordering::SeqCst)
    }

    /// The error a send gets once the run sends nothing more: class `OutageStop`, never tried.
    pub fn refusal(&self) -> anyhow::Error {
        let text = if self.is_stopped() {
            STOPPED_TEXT
        } else {
            OUTAGE_TEXT
        };
        anyhow::Error::new(LlmCallFailure::new(FailureClass::OutageStop, text))
    }

    /// The ids of the batches the provider accepted and has not finished, sorted.
    pub fn live_batches(&self) -> Vec<String> {
        let ids: BTreeSet<String> = self.live_lock().iter().map(|(id, _)| id.clone()).collect();
        ids.into_iter().collect()
    }

    fn live_lock(&self) -> std::sync::MutexGuard<'_, Vec<(String, Arc<dyn BatchApi>)>> {
        self.live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The provider accepted `batch_id` through `api`.
    pub fn note_batch_submitted(&self, batch_id: &str, api: Arc<dyn BatchApi>) {
        let mut live = self.live_lock();
        if !live.iter().any(|(id, _)| id == batch_id) {
            live.push((batch_id.to_string(), api));
        }
    }

    /// `batch_id` is no longer running at the provider (it ended, was fetched or cancelled).
    pub fn note_batch_finished(&self, batch_id: &str) {
        self.live_lock().retain(|(id, _)| id != batch_id);
    }

    /// The run polled `batch_id` and the provider has never told this process about it: a resume
    /// polling a batch an earlier process submitted. It is live until it ends.
    pub fn note_batch_polled(&self, batch_id: &str, api: Arc<dyn BatchApi>, ended: bool) {
        if ended {
            self.note_batch_finished(batch_id);
        } else {
            self.note_batch_submitted(batch_id, api);
        }
    }

    /// Where the run's operator ledger is written; the stop's failure report goes beside it.
    pub fn set_ledger_file(&self, path: PathBuf) {
        *self
            .ledger_file
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(path);
    }

    pub fn ledger_file(&self) -> Option<PathBuf> {
        self.ledger_file
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// The run's folder, once it exists ([`crate::network_wait::run_model_seam`] attaches it).
    pub fn attach_store(&self, store: Arc<crate::run_store::RunStore>) {
        *self.store_lock() = Some(store);
    }

    fn store_lock(&self) -> std::sync::MutexGuard<'_, Option<Arc<crate::run_store::RunStore>>> {
        self.store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The run's saved id, once its folder exists.
    pub fn run_id(&self) -> Option<String> {
        self.store_lock().as_ref().map(|s| s.run_id())
    }

    /// Let go of the run's folder, so a resume in this same process can open it.
    pub fn release_store(&self) {
        *self.store_lock() = None;
    }

    /// THE stop. From the first instruction no new send or batch leaves the run, and every network
    /// wait ends at its next step. Then every live batch is cancelled at the provider, each within
    /// [`CANCEL_TIMEOUT`], and the result says which were and which were not. Calling it again
    /// cancels nothing twice: a batch leaves the list once its cancel was asked.
    pub async fn stop(&self) -> StopReport {
        self.stopped.store(true, Ordering::SeqCst);
        self.wait.stop();
        let live: Vec<(String, Arc<dyn BatchApi>)> = std::mem::take(&mut *self.live_lock());
        let mut report = StopReport {
            at: crate::client_source::records::now_rfc3339(),
            ..StopReport::default()
        };
        let cancels = live.into_iter().map(|(id, api)| async move {
            let outcome = tokio::time::timeout(CANCEL_TIMEOUT, api.cancel_batch(&id)).await;
            (id, outcome)
        });
        for (id, outcome) in futures::future::join_all(cancels).await {
            match outcome {
                Ok(Ok(())) => report.cancelled.push(id),
                Ok(Err(e)) => report.not_cancelled.push(UncancelledBatch {
                    batch_id: id,
                    error: crate::failure::scrub_message(
                        &crate::ai_audit::disclosable_error(&format!("{e:#}")),
                        crate::failure::MAX_RECORD_MESSAGE_CHARS,
                    ),
                }),
                Err(_elapsed) => report.not_cancelled.push(UncancelledBatch {
                    batch_id: id,
                    error: format!(
                        "the provider did not answer the cancel within {} seconds",
                        CANCEL_TIMEOUT.as_secs()
                    ),
                }),
            }
        }
        report.cancelled.sort();
        report
            .not_cancelled
            .sort_by(|a, b| a.batch_id.cmp(&b.batch_id));
        // The run folder says it was stopped, and by whom, beside what the journal saved.
        if let Some(store) = self.store_lock().as_ref() {
            let mut note = format!("stopped by the operator at {}", report.at);
            if !report.not_cancelled.is_empty() {
                let ids: Vec<&str> = report
                    .not_cancelled
                    .iter()
                    .map(|b| b.batch_id.as_str())
                    .collect();
                note.push_str(&format!(
                    "; these batches could not be cancelled and may still be billed: {}",
                    ids.join(", ")
                ));
            }
            // `add_note` saves the run record.
            store.add_note(&note);
        }
        report
    }
}

/// How a run under [`run_until_stopped`] ended.
#[derive(Debug)]
pub enum Ended<T> {
    /// The run finished on its own, with this result.
    Finished(T),
    /// The operator stopped it first; the run's future was dropped after [`RunControl::stop`].
    Stopped(StopReport),
}

/// Drive `run` until it finishes or `stop` resolves (the operator's signal), whichever comes
/// first. On a stop, [`RunControl::stop`] runs BEFORE `run` is dropped: dispatch is refused and
/// every live batch is cancelled while the run is held still, then the run is dropped where it
/// stood. Everything it finished is already in the run folder.
pub async fn run_until_stopped<T>(
    control: &RunControl,
    run: impl std::future::Future<Output = T>,
    stop: impl std::future::Future<Output = ()>,
) -> Ended<T> {
    tokio::pin!(run);
    tokio::pin!(stop);
    tokio::select! {
        biased;
        out = &mut run => Ended::Finished(out),
        () = &mut stop => Ended::Stopped(control.stop().await),
    }
}

tokio::task_local! {
    /// The run control of the run this task belongs to ([`with_run_control`]).
    static RUN_CONTROL: Arc<RunControl>;
}

/// Run `fut` as part of the run `control` controls: a meter made inside it links the run's
/// outage stop to `control` ([`crate::ai_audit::UsageMeter::with_controls`]), so a call waiting
/// out the network never sends after the run stopped. Scoped to this task, like the run's
/// checkpoint folder and prompt seed.
pub async fn with_run_control<F: std::future::Future>(
    control: Arc<RunControl>,
    fut: F,
) -> F::Output {
    RUN_CONTROL.scope(control, fut).await
}

/// The control of the run this task belongs to, inside [`with_run_control`].
pub fn current_run_control() -> Option<Arc<RunControl>> {
    RUN_CONTROL.try_with(Arc::clone).ok()
}

/// How often [`stop_on_cancel`] looks at the app's cancel flag.
pub const CANCEL_POLL: Duration = Duration::from_millis(200);

/// A task that stops a run's control when the app's cancel flag is set; it ends with the guard.
/// Hold it for as long as the run lasts.
pub struct CancelWatch(tokio::task::JoinHandle<()>);

impl Drop for CancelWatch {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Watch `cancelled` (the app's cancel: a flag on the job, set by the cockpit's Stop) and call
/// [`RunControl::stop`] once it reads true, so the cancel refuses every later send, ends every
/// network wait without another send and cancels the live batches, exactly as the command
/// line's Ctrl+C does. The run itself is left to end on its own (the job's own cancel handling
/// saves what it finished). Must be called inside a Tokio runtime.
pub fn stop_on_cancel(
    control: Arc<RunControl>,
    cancelled: impl Fn() -> bool + Send + 'static,
) -> CancelWatch {
    CancelWatch(tokio::spawn(async move {
        loop {
            if control.is_stopped() {
                return;
            }
            if cancelled() {
                control.stop().await;
                return;
            }
            tokio::time::sleep(CANCEL_POLL).await;
        }
    }))
}

#[cfg(test)]
#[path = "operator_stop_tests.rs"]
mod tests;
