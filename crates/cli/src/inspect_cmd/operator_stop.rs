//! Ctrl+C and SIGTERM on `camerata inspect` (#583).
//!
//! The first SIGINT or SIGTERM stops the run the way the app's stop does, with the one server
//! function both call ([`camerata_server::operator_stop::RunControl::stop`]): no new part is sent,
//! every live batch is canceled at the provider, and what finished is already saved in the run
//! folder. The failure report beside the ledger records the stop and every batch that could not be
//! canceled, by id. The command exits with [`EXIT_OPERATOR_STOPPED`], and `--resume` runs what
//! remains.
//!
//! A second SIGINT or SIGTERM while the stop is still at work (a provider slow to answer a
//! cancel) exits at once with the same code.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use camerata_server::operator_stop::{run_until_stopped, Ended, RunControl, StopReport};

use super::{InspectError, InspectOutcome};

/// The exit code when the operator stopped the run (Ctrl+C or SIGTERM). Distinct from every other
/// code of `camerata inspect`; the run is saved and `--resume` accepts it.
pub const EXIT_OPERATOR_STOPPED: i32 = 7;

/// What a stopped run tells the operator.
#[derive(Debug, Clone)]
pub struct OperatorStopped {
    pub report: StopReport,
    /// The saved run `--resume` takes, when the run had a folder by the time of the stop.
    pub run_id: Option<String>,
    /// The failure report the stop was written into, when it could be.
    pub failure_report: Option<PathBuf>,
}

impl std::fmt::Display for OperatorStopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "STOPPED BY THE OPERATOR. No further part was sent, and {} live batch(es) were \
             canceled at the provider.",
            self.report.canceled.len()
        )?;
        for b in &self.report.not_canceled {
            write!(
                f,
                "\nbatch {} could NOT be canceled ({}); it may still run and be billed. Cancel it \
                 in the Anthropic console.",
                b.batch_id, b.error
            )?;
        }
        if let Some(path) = &self.failure_report {
            write!(f, "\nfailure report: {}", path.display())?;
        }
        match &self.run_id {
            Some(id) => write!(
                f,
                "\nWhat finished is saved. camerata inspect --resume {id} runs what remains."
            ),
            None => write!(
                f,
                "\nThe run stopped before its run folder was made; start it again."
            ),
        }
    }
}

/// Run `pipeline` on a fresh run control until it finishes or `stop` resolves. A stop cancels the
/// live batches, records itself in the failure report and ends as
/// [`InspectError::StoppedByOperator`].
pub async fn run_stoppable<F, Fut>(
    stop: impl Future<Output = ()>,
    pipeline: F,
) -> Result<InspectOutcome, InspectError>
where
    F: FnOnce(Arc<RunControl>) -> Fut,
    Fut: Future<Output = Result<InspectOutcome, InspectError>>,
{
    let control = Arc::new(RunControl::new());
    match run_until_stopped(&control, pipeline(control.clone()), stop).await {
        Ended::Finished(result) => result,
        Ended::Stopped(report) => {
            let failure_report = control.ledger_file().and_then(|ledger| {
                match camerata_server::failure_report::write_operator_stop(&ledger, &report) {
                    Ok(paths) => paths.map(|(json, _md)| json),
                    Err(e) => {
                        eprintln!("camerata inspect: could not record the stop: {e}");
                        None
                    }
                }
            });
            let run_id = control.run_id();
            // The run's folder is let go, so a resume (even in this same process) can open it.
            control.release_store();
            Err(InspectError::StoppedByOperator(Box::new(OperatorStopped {
                report,
                run_id,
                failure_report,
            })))
        }
    }
}

/// Start listening for SIGINT and SIGTERM now, on a thread of its own with its own runtime, and
/// return a future that resolves on the first one. The listener is installed before this returns,
/// so a Ctrl+C at any moment from here on is the operator's stop, never the default kill. From
/// the first signal on, a second one exits the process at once with [`EXIT_OPERATOR_STOPPED`]:
/// the listener's thread does that itself, so it works even while the pipeline's own thread is
/// blocked or the stop is slow to finish (a provider slow to answer a cancel).
///
/// If the listener cannot be set up (no thread, no signal handler), the returned future never
/// resolves: a run must not stop because a handler could not be installed.
pub fn spawn_stop_listener() -> std::pin::Pin<Box<dyn Future<Output = ()> + Send>> {
    let (first_tx, first_rx) = tokio::sync::oneshot::channel::<()>();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<bool>();
    let spawned = std::thread::Builder::new()
        .name("camerata-stop-signal".to_string())
        .spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                let _ = ready_tx.send(false);
                return;
            };
            rt.block_on(async move {
                let Some(mut signals) = Signals::install() else {
                    let _ = ready_tx.send(false);
                    return;
                };
                let _ = ready_tx.send(true);
                signals.next().await;
                eprintln!(
                    "camerata inspect: stopping. No further part is sent and the live batches are \
                     being canceled. Press Ctrl+C again to exit at once."
                );
                let _ = first_tx.send(());
                signals.next().await;
                eprintln!(
                    "camerata inspect: exiting at once; check the provider console for batches"
                );
                std::process::exit(EXIT_OPERATOR_STOPPED);
            });
        });
    // Wait until the handlers are in place (milliseconds), so nothing after this line can be
    // killed by a Ctrl+C the listener does not see yet.
    let ready = spawned.is_ok()
        && ready_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap_or(false);
    if !ready {
        eprintln!(
            "camerata inspect: Ctrl+C could not be set up to stop this run cleanly; it runs on"
        );
    }
    Box::pin(async move {
        if first_rx.await.is_err() {
            std::future::pending::<()>().await;
        }
    })
}

/// The signals the stop listens for: SIGINT and SIGTERM, every delivery.
struct Signals {
    #[cfg(unix)]
    interrupt: tokio::signal::unix::Signal,
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
}

impl Signals {
    /// Install the handlers; `None` when one cannot be.
    fn install() -> Option<Self> {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            Some(Self {
                interrupt: signal(SignalKind::interrupt()).ok()?,
                terminate: signal(SignalKind::terminate()).ok()?,
            })
        }
        #[cfg(not(unix))]
        {
            Some(Self {})
        }
    }

    /// The next SIGINT or SIGTERM.
    async fn next(&mut self) {
        #[cfg(unix)]
        {
            tokio::select! {
                Some(()) = self.interrupt.recv() => {}
                Some(()) = self.terminate.recv() => {}
                else => std::future::pending::<()>().await,
            }
        }
        #[cfg(not(unix))]
        {
            if tokio::signal::ctrl_c().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }
}
