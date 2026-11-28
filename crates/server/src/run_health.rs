//! The outage stop: when the model provider cannot be reached, the run stops cleanly instead of
//! failing every remaining part one by one.
//!
//! Zach, 2026-10-28: "We can't ship a product to anyone that didn't fully run end-to-end
//! properly." Retries catch nearly everything; a true outage must stop the run, record every
//! part it did not send, and leave the run re-triggerable later.
//!
//! [`RunHealth`] is the run-wide counter. Every model call reports to it as it completes:
//!
//! * a success resets the streak;
//! * a transport failure (after the transport's own retries) adds to the streak, and the run
//!   stops at [`OUTAGE_STREAK`] in a row that span at least [`OUTAGE_SPAN`] (owner call,
//!   2026-11-16: an unattended run rides out a short provider blip, so one wave of concurrent
//!   failures within seconds does not stop it; a long outage still does, and a resume retries
//!   the failed parts);
//! * an auth or billing failure stops the run at once (waiting does not fix a key or a balance);
//! * a rate limit (a 429 that outlasted the transport's retries) neither counts nor resets: it
//!   is a pause, and the batch runner waits it out under its own deadline;
//! * a content failure (a cut-off or malformed reply) neither counts nor resets: the provider
//!   answered, so it is not an outage, and it is not proof the provider is healthy either.
//!
//! "In a row" is completion order, and the counter is safe under concurrent passes. Once stopped,
//! admission refuses every new part ([`crate::ai_audit::UsageMeter::admit`]); calls already in
//! flight finish. Each refused part is recorded as not run with class `OutageStop`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use camerata_llm::failure::FailureClass;
use serde::{Deserialize, Serialize};

use crate::cost_cap::{NotRunPass, NotRunStage};

/// How many transport failures in a row stop the run.
pub const OUTAGE_STREAK: u32 = 3;

/// How long the failures of a streak must span before the run stops. A wave of concurrent calls
/// failing within seconds of one another is a blip the run rides out.
pub const OUTAGE_SPAN: Duration = Duration::from_secs(120);

/// TEST SUPPORT: a process-wide replacement for [`OUTAGE_SPAN`], in milliseconds (`u64::MAX` =
/// none). An end-to-end test that drives a whole run through instantly-failing stub calls cannot
/// reach the real span; it holds an [`OutageSpanGuard`] instead. Production never sets it.
#[cfg(any(test, feature = "test-support"))]
static SPAN_OVERRIDE_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(u64::MAX);

/// While alive, the outage stop needs `span` of failures instead of [`OUTAGE_SPAN`]. A test that
/// holds one must run serially (it is process-wide).
#[cfg(any(test, feature = "test-support"))]
#[must_use = "the override ends when the guard is dropped"]
pub struct OutageSpanGuard(());

#[cfg(any(test, feature = "test-support"))]
impl OutageSpanGuard {
    pub fn set(span: Duration) -> Self {
        let ms = u64::try_from(span.as_millis()).unwrap_or(u64::MAX - 1);
        SPAN_OVERRIDE_MS.store(ms.min(u64::MAX - 1), Ordering::SeqCst);
        Self(())
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for OutageSpanGuard {
    fn drop(&mut self) {
        SPAN_OVERRIDE_MS.store(u64::MAX, Ordering::SeqCst);
    }
}

#[cfg(any(test, feature = "test-support"))]
fn outage_span() -> Duration {
    match SPAN_OVERRIDE_MS.load(Ordering::SeqCst) {
        u64::MAX => OUTAGE_SPAN,
        ms => Duration::from_millis(ms),
    }
}

#[cfg(not(any(test, feature = "test-support")))]
fn outage_span() -> Duration {
    OUTAGE_SPAN
}

/// Time as the health counter sees it: the time since some fixed origin. Production reads the
/// monotonic clock; a test injects its own.
type Clock = Arc<dyn Fn() -> Duration + Send + Sync>;

/// The failures of the current streak: how many, and when the first came.
#[derive(Debug, Default, Clone, Copy)]
struct Streak {
    count: u32,
    first_at: Duration,
}

/// What stopped the run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutageTrigger {
    /// `Transport` (a streak), `AuthOrBilling` (one failure) or `RateLimited` (a wait that did
    /// not clear in time).
    pub class: FailureClass,
    /// A plain sentence naming what happened. Operator only.
    pub detail: String,
    /// RFC3339 UTC time the run stopped.
    pub at: String,
}

/// What the provider could not be reached for, as a clause after "the run stopped because".
pub const STOP_PHRASE_UNREACHABLE: &str = "the model provider could not be reached";
/// The same clause for a stop caused by a rate limit or an overload that did not clear.
pub const STOP_PHRASE_RATE_LIMIT: &str =
    "the model provider kept answering that it was over its rate limit or busy for 30 minutes";
/// The same clause for a stop caused by an exhausted account.
pub const STOP_PHRASE_BILLING: &str =
    "the model provider refused the request for billing (credit balance)";

impl OutageTrigger {
    /// The clause that says why the run stopped: a credit stop reads as billing, a rate limit
    /// that did not clear as such, any other stop as the provider not being reached. Operator
    /// only: it ends up in the failed-part reasons of the operator ledger, and a client export is
    /// refused while any exist.
    pub fn phrase(&self) -> &'static str {
        if self.class == FailureClass::RateLimited {
            STOP_PHRASE_RATE_LIMIT
        } else if self.class == FailureClass::AuthOrBilling
            && camerata_llm::failure::is_exhausted_account_text(&self.detail)
        {
            STOP_PHRASE_BILLING
        } else {
            STOP_PHRASE_UNREACHABLE
        }
    }
}

/// The run-wide health counter. See the module docs for the rules.
pub struct RunHealth {
    streak: Mutex<Streak>,
    stopped: AtomicBool,
    trigger: Mutex<Option<OutageTrigger>>,
    /// How many model calls have succeeded in all, never reset ([`Self::successes`]).
    successes: std::sync::atomic::AtomicU64,
    /// When a real model call last succeeded (wall clock, compared with the start of a call whose
    /// network wait ran out: [`Self::note_failed_error`]).
    last_success: Mutex<Option<std::time::Instant>>,
    clock: Clock,
}

impl std::fmt::Debug for RunHealth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunHealth")
            .field("streak", &self.streak)
            .field("stopped", &self.stopped)
            .field("trigger", &self.trigger)
            .finish_non_exhaustive()
    }
}

impl Default for RunHealth {
    fn default() -> Self {
        // tokio's clock, so a test that pauses time sees the run's waits count.
        let origin = tokio::time::Instant::now();
        Self::with_clock(Arc::new(move || origin.elapsed()))
    }
}

impl RunHealth {
    /// A counter that reads time from `clock` (the time since any fixed origin). Tests inject
    /// one so a streak's span is set, not waited for.
    pub fn with_clock(clock: Clock) -> Self {
        Self {
            streak: Mutex::new(Streak::default()),
            stopped: AtomicBool::new(false),
            trigger: Mutex::new(None),
            successes: std::sync::atomic::AtomicU64::new(0),
            last_success: Mutex::new(None),
            clock,
        }
    }

    /// TEST SUPPORT: a counter whose clock moves `step` seconds on every reading, so each failure
    /// lands `step` seconds after the one before it.
    #[cfg(test)]
    pub(crate) fn stepping(step: u64) -> Self {
        let ticks = std::sync::atomic::AtomicU64::new(0);
        Self::with_clock(Arc::new(move || {
            Duration::from_secs(ticks.fetch_add(step, Ordering::SeqCst))
        }))
    }

    fn streak_lock(&self) -> std::sync::MutexGuard<'_, Streak> {
        self.streak
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// A model call succeeded: the provider is reachable, so the streak starts over.
    pub fn note_success(&self) {
        *self.streak_lock() = Streak::default();
        *self
            .last_success
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(std::time::Instant::now());
        self.successes.fetch_add(1, Ordering::SeqCst);
    }

    /// Whether a real model call succeeded at or after `since` (a reply replayed from the run's
    /// journal is never one: it is not counted here).
    fn succeeded_since(&self, since: std::time::Instant) -> bool {
        self.last_success
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some_and(|at| at >= since)
    }

    /// How many model calls have succeeded so far. A call that waits out a rate limit reads it
    /// before it starts and hands it back to [`Self::note_wait_exhausted`].
    pub fn successes(&self) -> u64 {
        self.successes.load(Ordering::SeqCst)
    }

    /// A model call failed after its retries, with `class`. `detail` is a short note for the
    /// operator (the error text).
    pub fn note_failure(&self, class: FailureClass, detail: &str) {
        match class {
            FailureClass::Transport => {
                let now = (self.clock)();
                let (streak, span) = {
                    let mut s = self.streak_lock();
                    if s.count == 0 {
                        s.first_at = now;
                    }
                    s.count += 1;
                    (s.count, now.saturating_sub(s.first_at))
                };
                if streak >= OUTAGE_STREAK && span >= outage_span() {
                    self.trip(
                        FailureClass::Transport,
                        format!(
                            "{streak} model calls in a row, over {} s, could not reach the \
                             provider (last: {detail})",
                            span.as_secs()
                        ),
                    );
                }
            }
            FailureClass::AuthOrBilling => self.trip(
                FailureClass::AuthOrBilling,
                format!("the provider refused the credentials or the account (last: {detail})"),
            ),
            // The provider answered: not an outage, and not proof of health either. A rate limit
            // is the provider asking for a slower pace, so a streak of them is a pause that the
            // batch runner waits out under its own wall-clock deadline, never an outage stop.
            FailureClass::Content
            | FailureClass::RateLimited
            | FailureClass::SpendLimit
            | FailureClass::OutageStop
            | FailureClass::Io => {}
        }
    }

    /// A model call failed with `error`, after its retries and the long network wait. The one
    /// entry point for a failed call's error: an error tagged
    /// [`camerata_llm::network_wait::NetworkWaitExhausted`] (the call waited the full network
    /// wait while no other call got an answer) stops the run at once, since the streak's own
    /// span has been waited out already, many times over; any other error goes to
    /// [`Self::note_failure`] with its class.
    pub fn note_failed_error(&self, error: &anyhow::Error) {
        if let Some(tag) = camerata_llm::network_wait::network_wait_exhausted_of(error) {
            // This record is the one source of truth for "did anything get through": a real call
            // that succeeded since this one started means the network is up and only this
            // request is failing, so it is one step of the usual streak.
            if self.succeeded_since(tag.started) {
                self.note_failure(FailureClass::Transport, &error.to_string());
                return;
            }
            self.trip(
                FailureClass::Transport,
                format!(
                    "a model call waited {} minutes for the network and no other call got through \
                     (last: {error})",
                    camerata_llm::network_wait::NETWORK_WAIT_TOTAL.as_secs() / 60
                ),
            );
            return;
        }
        self.note_failure(
            crate::ai_audit::classify_pass_error(error),
            &error.to_string(),
        );
    }

    /// A rate limit or an overload did not clear within the longest a call waits for one.
    /// `successes_before` is [`Self::successes`] as the call started. If no call succeeded since,
    /// the provider is answering but will not serve the run at all, and every piece would wait
    /// the same time and fail the same way: stop the run, cleanly and resumable, as an outage
    /// does, so the pieces in flight stop at their next check and no new piece starts. If some
    /// call did succeed, the refusal is this piece's own (a request too large for the limit, say)
    /// and the run goes on: only this piece fails, and a resume runs it again. Returns whether the
    /// run was stopped.
    pub fn note_wait_exhausted(&self, detail: &str, successes_before: u64) -> bool {
        if self.successes() != successes_before {
            return false;
        }
        self.trip(
            FailureClass::RateLimited,
            format!("a rate limit or overload did not clear in time (last: {detail})"),
        );
        true
    }

    /// A call waited the whole long network wait ([`camerata_llm::network_wait`]) and the
    /// provider still could not be reached. `successes_before` is [`Self::successes`] as the call
    /// started. If no call succeeded since, the network is down for the whole run: stop it, as a
    /// transport outage, at once. If one did, this request alone is failing and the run goes on.
    /// Returns whether the run was stopped.
    pub fn note_network_wait_exhausted(&self, detail: &str, successes_before: u64) -> bool {
        if self.successes() != successes_before {
            return false;
        }
        self.trip(
            FailureClass::Transport,
            format!(
                "a model call waited {} minutes for the network and no other call got through \
                 (last: {detail})",
                camerata_llm::network_wait::NETWORK_WAIT_TOTAL.as_secs() / 60
            ),
        );
        true
    }

    fn trip(&self, class: FailureClass, detail: String) {
        let mut trigger = self
            .trigger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if trigger.is_none() {
            *trigger = Some(OutageTrigger {
                class,
                detail: camerata_llm::failure::scrub_message(&detail, 400),
                at: crate::client_source::records::now_rfc3339(),
            });
        }
        self.stopped.store(true, Ordering::SeqCst);
    }

    /// The clause that says why the run stopped ([`OutageTrigger::phrase`]); the plain
    /// "could not be reached" clause while the run has not stopped.
    pub fn stop_phrase(&self) -> &'static str {
        self.trigger()
            .map_or(STOP_PHRASE_UNREACHABLE, |t| t.phrase())
    }

    /// True once the run has stopped. New work is refused from here on.
    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    /// What stopped the run, once it has stopped.
    pub fn trigger(&self) -> Option<OutageTrigger> {
        self.trigger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// The current streak, for tests and the operator log.
    pub fn streak(&self) -> u32 {
        self.streak_lock().count
    }
}

/// What the outage stop left undone, as the operator ledger records it. Like the spend-limit
/// record it names every part that was never sent, so a later run can pick them up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutageRecord {
    pub trigger: OutageTrigger,
    pub not_run_passes: Vec<NotRunPass>,
    pub not_run_stages: Vec<NotRunStage>,
    /// Repos the run reached after it had stopped; their model work was never started.
    #[serde(default)]
    pub repos_not_finished: Vec<String>,
}

impl OutageRecord {
    /// The failed-pass entries for everything the stop left undone. Operator only: a client
    /// export is refused while any of these exist.
    pub fn failed_passes(&self) -> Vec<crate::ai_audit::FailedPass> {
        crate::cost_cap::not_run_failed_passes(
            &self.not_run_passes,
            &self.not_run_stages,
            &format!("the run stopped because {}", self.trigger.phrase()),
        )
    }

    /// The repos the stop left unfinished: those named plus any with a part that did not run.
    pub fn unfinished_repos(&self) -> Vec<String> {
        let mut repos: std::collections::BTreeSet<String> =
            self.repos_not_finished.iter().cloned().collect();
        repos.extend(self.not_run_passes.iter().map(|p| p.repo.clone()));
        repos.extend(self.not_run_stages.iter().map(|s| s.repo.clone()));
        repos.into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    /// A health counter on a clock the test moves, and that clock (seconds).
    fn health() -> (RunHealth, Arc<AtomicU64>) {
        let now = Arc::new(AtomicU64::new(0));
        let clock = now.clone();
        let h = RunHealth::with_clock(Arc::new(move || {
            Duration::from_secs(clock.load(Ordering::SeqCst))
        }));
        (h, now)
    }

    /// `n` transport failures, `gap` seconds apart, the first at the clock's current time.
    fn transport_failures(h: &RunHealth, now: &AtomicU64, n: u32, gap: u64) {
        for i in 0..n {
            if i > 0 {
                now.fetch_add(gap, Ordering::SeqCst);
            }
            h.note_failure(FailureClass::Transport, "x");
        }
    }

    #[test]
    fn a_wait_that_ran_out_stops_the_run_only_if_no_call_succeeded_meanwhile() {
        let (h, _) = health();
        let before = h.successes();
        h.note_success(); // another piece's call got through while this one waited
        assert_eq!(h.successes(), before + 1);
        assert!(!h.note_wait_exhausted("HTTP 429", before));
        assert!(!h.is_stopped(), "the refusal is this piece's own");
        assert!(h.trigger().is_none());
        // With nothing succeeding since the call started, the same refusal stops the run.
        assert!(h.note_wait_exhausted("HTTP 429", h.successes()));
        assert!(h.is_stopped());
    }

    #[test]
    fn a_network_wait_that_ran_out_stops_the_run_only_if_no_call_succeeded_meanwhile() {
        let (h, _) = health();
        let before = h.successes();
        h.note_success(); // another piece's call got through while this one waited
        assert!(!h.note_network_wait_exhausted("connection reset", before));
        assert!(
            !h.is_stopped(),
            "the network is up: this request alone fails"
        );
        assert!(h.note_network_wait_exhausted("connection reset", h.successes()));
        assert!(h.is_stopped());
        assert_eq!(h.trigger().map(|t| t.class), Some(FailureClass::Transport));
        assert_eq!(h.stop_phrase(), STOP_PHRASE_UNREACHABLE);
    }

    #[test]
    fn a_rate_limit_that_did_not_clear_stops_the_run_and_says_so() {
        let (h, _) = health();
        assert_eq!(h.stop_phrase(), STOP_PHRASE_UNREACHABLE, "not stopped yet");
        assert!(h.note_wait_exhausted("HTTP 429: slow down", h.successes()));
        assert!(h.is_stopped());
        let trigger = h.trigger().expect("trigger");
        assert_eq!(trigger.class, FailureClass::RateLimited);
        assert!(trigger.detail.contains("slow down"));
        assert_eq!(h.stop_phrase(), STOP_PHRASE_RATE_LIMIT);
        assert_eq!(
            STOP_PHRASE_RATE_LIMIT,
            "the model provider kept answering that it was over its rate limit or busy for 30 minutes"
        );
        // A real outage and a credit stop keep their own clauses.
        let (h, now) = health();
        transport_failures(&h, &now, 3, 90);
        assert_eq!(h.stop_phrase(), STOP_PHRASE_UNREACHABLE);
        let (h, _) = health();
        h.note_failure(FailureClass::AuthOrBilling, "credit balance is too low");
        assert_eq!(h.stop_phrase(), STOP_PHRASE_BILLING);
    }

    #[test]
    fn three_transport_failures_in_a_row_over_three_minutes_stop_the_run() {
        let (h, now) = health();
        h.note_failure(FailureClass::Transport, "a");
        now.fetch_add(90, Ordering::SeqCst);
        h.note_failure(FailureClass::Transport, "b");
        assert!(!h.is_stopped(), "two is not an outage");
        now.fetch_add(90, Ordering::SeqCst);
        h.note_failure(FailureClass::Transport, "c");
        assert!(h.is_stopped(), "three over 180 s is an outage");
        assert_eq!(h.trigger().unwrap().class, FailureClass::Transport);
    }

    /// Owner call 2026-11-16: an unattended run rides out a short provider blip.
    #[test]
    fn three_transport_failures_within_ten_seconds_do_not_stop_the_run() {
        let (h, now) = health();
        transport_failures(&h, &now, 3, 5);
        assert!(!h.is_stopped(), "one wave of failures is a blip");
        assert_eq!(h.streak(), 3);
        // The blip goes on: the streak now spans two minutes, and the next failure stops the run.
        now.fetch_add(OUTAGE_SPAN.as_secs(), Ordering::SeqCst);
        h.note_failure(FailureClass::Transport, "late");
        assert!(
            h.is_stopped(),
            "a streak that outlasts the span is an outage"
        );
    }

    #[test]
    fn a_success_inside_the_blip_starts_the_span_over() {
        let (h, now) = health();
        transport_failures(&h, &now, 3, 5);
        h.note_success();
        now.fetch_add(600, Ordering::SeqCst);
        transport_failures(&h, &now, 3, 5);
        assert!(
            !h.is_stopped(),
            "the span is counted from the new streak's first failure"
        );
    }

    /// Zach 2026-11-02 (design 6.1): a 429 streak is a pause, not an outage. Rate-limited failures
    /// neither count toward the streak nor reset it, however many arrive.
    #[test]
    fn rate_limited_streak_does_not_trip_the_outage_stop() {
        let (h, now) = health();
        for _ in 0..50 {
            h.note_failure(FailureClass::RateLimited, "HTTP 429");
        }
        assert!(!h.is_stopped(), "fifty 429s in a row are a pause");
        assert_eq!(h.streak(), 0, "and they add nothing to the streak");
        // They do not reset a transport streak either: two transport failures, then 429s, then a
        // third transport failure is still an outage.
        h.note_failure(FailureClass::Transport, "a");
        now.fetch_add(100, Ordering::SeqCst);
        h.note_failure(FailureClass::Transport, "b");
        h.note_failure(FailureClass::RateLimited, "HTTP 429");
        assert_eq!(h.streak(), 2);
        now.fetch_add(100, Ordering::SeqCst);
        h.note_failure(FailureClass::Transport, "c");
        assert!(h.is_stopped());
    }

    /// Fable wave 3: an overload (529, `overloaded_error`) is the provider being busy, a pause like
    /// a 429. Real errors, classified by the transport's own rules, never reach the streak; a
    /// connection failure, a timeout and a 500 to 504 still do.
    #[test]
    fn three_overloads_in_a_row_do_not_trip_the_outage_stop_but_connection_failures_still_do() {
        use camerata_llm::failure::{classify_error, classify_http, LlmCallFailure};
        let typed = |status: u16, etype: Option<&str>| {
            let (class, _) = classify_http(status, etype, "x");
            anyhow::Error::new(LlmCallFailure::new(class, format!("HTTP {status}")))
        };
        let overloads = [
            typed(529, Some("overloaded_error")),
            typed(529, None),
            anyhow::anyhow!("API Error: 529 overloaded_error"),
            typed(200, Some("overloaded_error")),
        ];
        let h = RunHealth::default();
        for e in &overloads {
            h.note_failure(classify_error(e), &e.to_string());
        }
        assert!(!h.is_stopped(), "four overloads in a row are a pause");
        assert_eq!(h.streak(), 0);

        for outage in [
            typed(500, None),
            typed(502, None),
            typed(503, None),
            typed(504, None),
            typed(408, None),
            anyhow::anyhow!("connection reset by peer"),
            anyhow::anyhow!("request timed out"),
        ] {
            let (h, now) = health();
            for _ in 0..OUTAGE_STREAK {
                h.note_failure(classify_error(&outage), &outage.to_string());
                now.fetch_add(OUTAGE_SPAN.as_secs(), Ordering::SeqCst);
            }
            assert!(h.is_stopped(), "{outage} three times is an outage");
        }
    }

    #[test]
    fn any_success_resets_the_streak() {
        let (h, _now) = health();
        h.note_failure(FailureClass::Transport, "a");
        h.note_failure(FailureClass::Transport, "b");
        h.note_success();
        h.note_failure(FailureClass::Transport, "c");
        h.note_failure(FailureClass::Transport, "d");
        assert!(!h.is_stopped());
        assert_eq!(h.streak(), 2);
    }

    #[test]
    fn content_failures_never_count_and_never_reset() {
        let (h, now) = health();
        for _ in 0..10 {
            h.note_failure(FailureClass::Content, "malformed");
        }
        assert!(!h.is_stopped(), "content failures are not an outage");
        h.note_failure(FailureClass::Transport, "a");
        h.note_failure(FailureClass::Content, "malformed");
        now.fetch_add(100, Ordering::SeqCst);
        h.note_failure(FailureClass::Transport, "b");
        now.fetch_add(100, Ordering::SeqCst);
        h.note_failure(FailureClass::Transport, "c");
        assert!(
            h.is_stopped(),
            "a content failure between them did not reset"
        );
    }

    #[test]
    fn an_auth_or_billing_failure_stops_the_run_at_once() {
        let h = RunHealth::default();
        h.note_failure(FailureClass::AuthOrBilling, "HTTP 401");
        assert!(h.is_stopped());
        assert_eq!(h.trigger().unwrap().class, FailureClass::AuthOrBilling);
    }

    #[test]
    fn the_first_trigger_is_kept() {
        let (h, now) = health();
        h.note_failure(FailureClass::AuthOrBilling, "first");
        transport_failures(&h, &now, 3, OUTAGE_SPAN.as_secs());
        assert_eq!(h.trigger().unwrap().class, FailureClass::AuthOrBilling);
    }

    #[test]
    fn the_counter_is_safe_under_concurrent_failures() {
        let h = std::sync::Arc::new(RunHealth::default());
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let h = h.clone();
                std::thread::spawn(move || {
                    for _ in 0..50 {
                        h.note_failure(FailureClass::Transport, "x");
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        assert!(
            !h.is_stopped(),
            "400 failures within milliseconds are one wave"
        );
        assert_eq!(h.streak(), 400, "no increment was lost");
    }
}
