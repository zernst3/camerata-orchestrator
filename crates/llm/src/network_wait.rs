//! The long wait for the network (#582): a short blip costs minutes, not the run.
//!
//! The transport's own retries ([`crate::retry`], 2, 8 and 30 seconds within a one-minute budget)
//! clear a dropped packet or a brief provider hiccup. A local network that is gone for a few
//! minutes outlasts them, and before this wait every call in flight then failed, the outage stop
//! fired, and the run ended with the remaining repos unread.
//!
//! [`NetworkWait::run`] sits above those retries. When a call still fails with a transport-class
//! error (a dropped connection, a timeout, a 5xx), it waits [`NETWORK_WAIT_STEPS`] (1, 2, 4 and 8
//! minutes, [`NETWORK_WAIT_TOTAL`] in all) and sends the SAME request again each time. Owner
//! approved, 2026-11-27.
//!
//! * Only [`FailureClass::Transport`] waits. An auth or billing failure never waits (waiting does
//!   not fix a key or a balance), and it stops every other call's wait at its next step: one auth
//!   failure ends the run at once, so a waiting call must not send again after it.
//! * A rate limit, a content failure and anything else come back unchanged: their own runners
//!   pace or record them.
//! * When the wait runs out and no call through this wait got an answer from the provider since
//!   this call started, the error is tagged [`NetworkWaitExhausted`], and the run's outage stop
//!   trips on it at once (the server's `RunHealth`). When another call did get an answer, the
//!   network is up and this request alone is failing: the error comes back untagged and counts
//!   toward the usual outage streak.
//! * [`NetworkWait::stop`] ends every wait at its next step (an auth failure seen here, or an
//!   operator stop), so a stopped run never sends again from a waiting call.
//!
//! Every wait sleeps through the process sleeper ([`crate::retry`]), so a test installs a
//! virtual clock with `set_process_sleeper` and an hour of outage passes in milliseconds.

use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use crate::failure::{classify_error, FailureClass};
use crate::retry::{ProcessSleeper, Sleeper};

/// The waits between sends of a request whose network is down, in order. Their sum is
/// [`NETWORK_WAIT_TOTAL`].
pub const NETWORK_WAIT_STEPS: [Duration; 4] = [
    Duration::from_secs(60),
    Duration::from_secs(2 * 60),
    Duration::from_secs(4 * 60),
    Duration::from_secs(8 * 60),
];

/// The longest one request waits for the network, in all, before the run's outage stop.
pub const NETWORK_WAIT_TOTAL: Duration = Duration::from_secs(15 * 60);

// The steps and the total are one schedule: a change to one without the other fails the build.
const _: () = {
    let mut sum = 0;
    let mut i = 0;
    while i < NETWORK_WAIT_STEPS.len() {
        sum += NETWORK_WAIT_STEPS[i].as_secs();
        i += 1;
    }
    assert!(sum == NETWORK_WAIT_TOTAL.as_secs());
};

/// The tag on an error whose request waited [`NETWORK_WAIT_TOTAL`] for the network while no other
/// call got an answer. The outage stop trips on it at once. It is anyhow context over the last
/// transport error, so the typed failure inside (its class, its attempts) still reads through it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkWaitExhausted {
    /// How long the request waited, in all.
    pub waited: Duration,
}

impl std::fmt::Display for NetworkWaitExhausted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the model provider could not be reached for {} minutes",
            self.waited.as_secs() / 60
        )
    }
}

/// True when `e` carries the [`NetworkWaitExhausted`] tag anywhere in its chain.
pub fn is_network_wait_exhausted(e: &anyhow::Error) -> bool {
    e.downcast_ref::<NetworkWaitExhausted>().is_some()
}

/// The run's shared wait: one per run, handed to every seam that sends a request.
#[derive(Debug, Default)]
pub struct NetworkWait {
    /// How many sends got an answer from the provider (a reply, or any failure that is not a
    /// transport failure: the provider was reached).
    answers: AtomicU64,
    stopped: AtomicBool,
}

impl NetworkWait {
    pub fn new() -> Self {
        Self::default()
    }

    /// End every wait at its next step. A waiting request then returns its last error and sends
    /// nothing more.
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
    }

    /// True once [`Self::stop`] was called or an auth or billing failure came through.
    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    /// How many sends through this wait got an answer from the provider.
    pub fn answers(&self) -> u64 {
        self.answers.load(Ordering::SeqCst)
    }

    /// Run `attempt` (one send of the same request), waiting for the network as the module docs
    /// say. `may_retry` is asked after a transport failure, before any wait: `false` returns the
    /// error at once (a streamed reply that already delivered text, which a second send would
    /// deliver again).
    pub async fn run<T, F, Fut>(
        &self,
        mut attempt: F,
        may_retry: &(dyn Fn() -> bool + Send + Sync),
    ) -> anyhow::Result<T>
    where
        F: FnMut() -> Fut + Send,
        Fut: Future<Output = anyhow::Result<T>> + Send,
    {
        let answers_at_start = self.answers();
        let mut waited = Duration::ZERO;
        let mut step = 0usize;
        loop {
            let e = match attempt().await {
                Ok(v) => {
                    self.answers.fetch_add(1, Ordering::SeqCst);
                    return Ok(v);
                }
                Err(e) => e,
            };
            match classify_error(&e) {
                FailureClass::Transport => {}
                FailureClass::AuthOrBilling => {
                    // The provider answered, and nothing a wait does fixes the key or the account.
                    self.answers.fetch_add(1, Ordering::SeqCst);
                    self.stop();
                    return Err(e);
                }
                _ => {
                    self.answers.fetch_add(1, Ordering::SeqCst);
                    return Err(e);
                }
            }
            if self.is_stopped() || !may_retry() {
                return Err(e);
            }
            let Some(&delay) = NETWORK_WAIT_STEPS.get(step) else {
                if self.answers() == answers_at_start {
                    return Err(e.context(NetworkWaitExhausted { waited }));
                }
                return Err(e);
            };
            step += 1;
            eprintln!(
                "[camerata] the model provider could not be reached; waiting {} minute{} for the \
                 network before sending the request again ({} of {} minutes waited so far)",
                delay.as_secs() / 60,
                if delay.as_secs() == 60 { "" } else { "s" },
                waited.as_secs() / 60,
                NETWORK_WAIT_TOTAL.as_secs() / 60
            );
            ProcessSleeper.sleep(delay).await;
            waited += delay;
            if self.is_stopped() {
                return Err(e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::failure::LlmCallFailure;
    use std::sync::{Arc, Mutex};

    /// A sleeper that records each wait and returns at once.
    #[derive(Default)]
    struct Recorder(Mutex<Vec<Duration>>);

    impl Sleeper for Recorder {
        fn sleep<'a>(
            &'a self,
            d: Duration,
        ) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
            self.0.lock().unwrap().push(d);
            Box::pin(async {})
        }
    }

    /// Serializes the tests here: the process sleeper is process-wide.
    static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn transport() -> anyhow::Error {
        anyhow::Error::new(LlmCallFailure::new(
            FailureClass::Transport,
            "Anthropic API request failed: connection reset",
        ))
    }

    fn auth() -> anyhow::Error {
        anyhow::Error::new(LlmCallFailure::new(
            FailureClass::AuthOrBilling,
            "Anthropic API HTTP 401: invalid x-api-key",
        ))
    }

    async fn with_recorder<T>(f: impl Future<Output = T>) -> (T, Vec<Duration>) {
        let rec = Arc::new(Recorder::default());
        crate::retry::set_process_sleeper(Some(rec.clone()));
        let out = f.await;
        crate::retry::set_process_sleeper(None);
        let waits = rec.0.lock().unwrap().clone();
        (out, waits)
    }

    #[tokio::test]
    async fn a_short_drop_waits_and_sends_the_same_request_again() {
        let _g = SERIAL.lock().await;
        let wait = NetworkWait::new();
        let sends = AtomicU64::new(0);
        let (out, waits) = with_recorder(wait.run(
            || {
                let n = sends.fetch_add(1, Ordering::SeqCst);
                async move {
                    if n < 2 {
                        Err(transport())
                    } else {
                        Ok("reply")
                    }
                }
            },
            &|| true,
        ))
        .await;
        assert_eq!(out.unwrap(), "reply");
        assert_eq!(sends.load(Ordering::SeqCst), 3);
        assert_eq!(waits, NETWORK_WAIT_STEPS[..2].to_vec());
    }

    #[tokio::test]
    async fn a_long_drop_waits_fifteen_minutes_then_is_tagged_for_the_outage_stop() {
        let _g = SERIAL.lock().await;
        let wait = NetworkWait::new();
        let sends = AtomicU64::new(0);
        let (out, waits) = with_recorder(wait.run(
            || {
                sends.fetch_add(1, Ordering::SeqCst);
                async { Err::<(), _>(transport()) }
            },
            &|| true,
        ))
        .await;
        let e = out.unwrap_err();
        assert!(is_network_wait_exhausted(&e), "{e:#}");
        // The typed failure still reads through the tag: the class is unchanged.
        assert_eq!(classify_error(&e), FailureClass::Transport);
        assert_eq!(waits, NETWORK_WAIT_STEPS.to_vec());
        assert_eq!(waits.iter().sum::<Duration>(), NETWORK_WAIT_TOTAL);
        assert_eq!(sends.load(Ordering::SeqCst), 5);
    }

    #[tokio::test]
    async fn an_answer_to_another_call_meanwhile_leaves_the_error_untagged() {
        let _g = SERIAL.lock().await;
        let wait = Arc::new(NetworkWait::new());
        let w = wait.clone();
        let (out, _) = with_recorder(wait.run(
            move || {
                // Another call through the same wait gets an answer while this one waits.
                w.answers.fetch_add(1, Ordering::SeqCst);
                async { Err::<(), _>(transport()) }
            },
            &|| true,
        ))
        .await;
        let e = out.unwrap_err();
        assert!(!is_network_wait_exhausted(&e), "{e:#}");
    }

    #[tokio::test]
    async fn an_auth_failure_never_waits_and_stops_every_other_wait() {
        let _g = SERIAL.lock().await;
        let wait = NetworkWait::new();
        let (out, waits) =
            with_recorder(wait.run(|| async { Err::<(), _>(auth()) }, &|| true)).await;
        let e = out.unwrap_err();
        assert!(!is_network_wait_exhausted(&e));
        assert!(waits.is_empty(), "an auth failure waited: {waits:?}");
        assert!(wait.is_stopped());
        // A transport failure after the stop does not wait either.
        let (out, waits) =
            with_recorder(wait.run(|| async { Err::<(), _>(transport()) }, &|| true)).await;
        assert!(out.is_err());
        assert!(waits.is_empty(), "a stopped run waited: {waits:?}");
    }

    /// A sleeper that stops `wait` while the request sleeps.
    struct StopsWhileAsleep(Arc<NetworkWait>);

    impl Sleeper for StopsWhileAsleep {
        fn sleep<'a>(
            &'a self,
            _d: Duration,
        ) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
            self.0.stop();
            Box::pin(async {})
        }
    }

    #[tokio::test]
    async fn a_stop_that_arrives_while_asleep_ends_the_wait_without_another_send() {
        let _g = SERIAL.lock().await;
        let wait = Arc::new(NetworkWait::new());
        crate::retry::set_process_sleeper(Some(Arc::new(StopsWhileAsleep(wait.clone()))));
        let sends = AtomicU64::new(0);
        let out = wait
            .run(
                || {
                    sends.fetch_add(1, Ordering::SeqCst);
                    async { Err::<(), _>(transport()) }
                },
                &|| true,
            )
            .await;
        crate::retry::set_process_sleeper(None);
        assert!(out.is_err());
        assert_eq!(
            sends.load(Ordering::SeqCst),
            1,
            "a stopped run sent the request again after its wait"
        );
    }

    #[tokio::test]
    async fn a_stop_during_a_wait_ends_it_without_another_send() {
        let _g = SERIAL.lock().await;
        let wait = Arc::new(NetworkWait::new());
        let sends = AtomicU64::new(0);
        let w = wait.clone();
        let (out, waits) = with_recorder(wait.run(
            || {
                sends.fetch_add(1, Ordering::SeqCst);
                // The operator stops the run while this request is down.
                w.stop();
                async { Err::<(), _>(transport()) }
            },
            &|| true,
        ))
        .await;
        assert!(out.is_err());
        assert_eq!(sends.load(Ordering::SeqCst), 1);
        assert!(waits.is_empty());
    }

    #[tokio::test]
    async fn a_content_failure_and_a_rate_limit_come_back_unchanged() {
        let _g = SERIAL.lock().await;
        for class in [FailureClass::Content, FailureClass::RateLimited] {
            let wait = NetworkWait::new();
            let (out, waits) = with_recorder(wait.run(
                || async move { Err::<(), _>(anyhow::Error::new(LlmCallFailure::new(class, "x"))) },
                &|| true,
            ))
            .await;
            assert!(out.is_err());
            assert!(waits.is_empty(), "{class:?} waited");
            assert!(!wait.is_stopped(), "{class:?} stopped the run");
        }
    }

    #[tokio::test]
    async fn a_request_that_may_not_retry_returns_at_once() {
        let _g = SERIAL.lock().await;
        let wait = NetworkWait::new();
        let (out, waits) =
            with_recorder(wait.run(|| async { Err::<(), _>(transport()) }, &|| false)).await;
        let e = out.unwrap_err();
        assert!(!is_network_wait_exhausted(&e));
        assert!(waits.is_empty());
    }
}
