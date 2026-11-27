//! Ctrl+C and SIGTERM on `camerata inspect` (#583).
//!
//! The first SIGINT or SIGTERM stops the run the way the app's stop does, with the one server
//! function both call ([`camerata_server::operator_stop::RunControl::stop`]): no new part is sent,
//! every live batch is cancelled at the provider, and what finished is already saved in the run
//! folder. The failure report beside the ledger records the stop and every batch that could not be
//! cancelled, by id. The command exits with [`EXIT_OPERATOR_STOPPED`], and `--resume` runs what
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
             cancelled at the provider.",
            self.report.cancelled.len()
        )?;
        for b in &self.report.not_cancelled {
            write!(
                f,
                "\nbatch {} could NOT be cancelled ({}); it may still run and be billed. Cancel it \
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

/// Resolves on the first SIGINT (Ctrl+C) or SIGTERM. From then on a second one exits the process
/// at once with [`EXIT_OPERATOR_STOPPED`]: the stop is at work, and the operator wants out now.
pub async fn os_stop_signal() {
    first_signal().await;
    eprintln!(
        "camerata inspect: stopping. No further part is sent and the live batches are being \
         cancelled. Press Ctrl+C again to exit at once."
    );
    tokio::spawn(async {
        first_signal().await;
        eprintln!("camerata inspect: exiting at once; check the provider console for batches");
        std::process::exit(EXIT_OPERATOR_STOPPED);
    });
}

/// The next SIGINT or SIGTERM. A signal that cannot be listened for never resolves: a run must not
/// stop because a handler could not be installed.
async fn first_signal() {
    let interrupt = async {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                if term.recv().await.is_none() {
                    std::future::pending::<()>().await;
                }
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = interrupt => {}
        () = terminate => {}
    }
}
