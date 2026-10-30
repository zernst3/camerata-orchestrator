//! Retry/backoff policy for the Anthropic API transport (`complete_api`, `submit_batch`,
//! `poll_batch_status`, `fetch_batch_results` in [`crate::llm`]) and the `claude` CLI backend.
//!
//! A production scan can run for hours across hundreds of HTTP calls; a single 429 (rate
//! limit), 529 (Anthropic overloaded), transient 5xx, or dropped connection used to fail
//! the ENTIRE pass. This module is the decision layer — "should this failure be retried,
//! and after how long" — kept PURE and separate from the HTTP call itself so it is
//! unit-testable without a live API key or a mock server (see the `tests` module below).
//! [`drive`] is the one place that actually loops + sleeps; everything it calls into is a
//! plain function of (status code / error kind, attempt number).
//!
//! Policy: retry 429, 529, and any 5xx, plus network/connect errors — up to
//! [`MAX_RETRIES`] additional attempts with exponential backoff (2s, 8s, 30s), UNLESS the
//! response carries a `retry-after` header, in which case that value wins for that attempt
//! (the server knows better than our fixed schedule), capped at [`MAX_RETRY_AFTER`]. An
//! OVERLOADED answer (529, 503) clears more slowly than a rate limit, so it waits longer and
//! tries once more: 5s, 20s, 60s, 120s. 400/401/403 are never retried — a bad request or bad
//! credential doesn't get better by waiting.
//!
//! ONE loop: [`drive`] is the only retry loop in the crate. The API transport and the CLI
//! backend both run through it, each supplying one attempt as a closure and the sleeper to
//! wait with (a test injects a recording sleeper and so never waits).
//!
//! Same-model guarantee: this module has no notion of "model" or "fallback chain" at all —
//! [`drive`] re-invokes the SAME attempt closure every time, so retrying can never silently
//! switch models the way [`crate::llm::call_with_fallback`] does. Fallback-chain routing is a
//! distinct, higher-level concern (see [`crate::llm::is_retryable_for_chain`]) and is untouched
//! by this module.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use crate::failure::{AttemptRecord, FailureClass, LlmCallFailure};

/// The most retries any failure gets on the standard schedule.
#[cfg(test)]
pub(crate) const MAX_RETRIES: u32 = 3;

/// Fixed exponential backoff schedule in seconds, indexed by (zero-based) attempt number.
const BACKOFF_SCHEDULE_SECS: [u64; 3] = [2, 8, 30];

/// The schedule for an overloaded provider (529, 503): it clears more slowly than a rate limit,
/// so the waits are longer and there is one more of them.
const OVERLOADED_SCHEDULE_SECS: [u64; 4] = [5, 20, 60, 120];

/// The longest a `retry-after` is honoured. A provider asking for more than this is not going to
/// recover inside a run; the failure is recorded and the end-of-run second try picks it up.
pub(crate) const MAX_RETRY_AFTER: Duration = Duration::from_secs(300);

/// Waits between attempts. Production sleeps for real; a test injects a recorder.
pub trait Sleeper: Send + Sync {
    fn sleep<'a>(&'a self, d: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
}

/// The real sleeper.
#[derive(Debug, Clone, Copy, Default)]
pub struct TokioSleeper;

impl Sleeper for TokioSleeper {
    fn sleep<'a>(&'a self, d: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(tokio::time::sleep(d))
    }
}

/// The kind of failure being classified: an HTTP status, a transport-level error with no status
/// (network/connect/timeout), or an overloaded answer reported as text (a CLI's stderr).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RetryReason {
    HttpStatus(u16),
    Network,
    Overloaded,
    /// A streamed reply went silent for the idle window with nothing usable delivered. Retried
    /// ONCE: a stall that repeats is not a blip the schedule will clear, and each stalled
    /// attempt may already have been billed by the provider for the tokens it generated.
    StreamStall,
}

impl RetryReason {
    /// True for the answers that get the longer schedule.
    fn is_overloaded(self) -> bool {
        matches!(
            self,
            Self::HttpStatus(529) | Self::HttpStatus(503) | Self::Overloaded
        )
    }
}

/// Pure classification: should a failure of this kind ever be retried (ignoring attempt
/// count)? 429 (rate limit), 529 (Anthropic-specific overloaded), any 5xx, and network
/// errors are retryable. 400/401/403 (and any other 4xx) are fatal — no amount of waiting
/// fixes a malformed request or a bad key.
pub(crate) fn should_retry(reason: RetryReason) -> bool {
    match reason {
        RetryReason::HttpStatus(status) => {
            status == 429 || status == 529 || (500..600).contains(&status)
        }
        RetryReason::Network | RetryReason::Overloaded | RetryReason::StreamStall => true,
    }
}

/// The one wait before the one retry of a stalled stream, in seconds.
const STREAM_STALL_SCHEDULE_SECS: [u64; 1] = [2];

/// The waits for this kind of failure, in seconds, one per retry.
fn schedule_for(reason: RetryReason) -> &'static [u64] {
    if reason == RetryReason::StreamStall {
        &STREAM_STALL_SCHEDULE_SECS
    } else if reason.is_overloaded() {
        &OVERLOADED_SCHEDULE_SECS
    } else {
        &BACKOFF_SCHEDULE_SECS
    }
}

/// Full decision: given how many retries have ALREADY happened (`attempt`, zero-based) and
/// the failure kind, should one more attempt be made?
pub(crate) fn should_retry_now(attempt: u32, reason: RetryReason) -> bool {
    (attempt as usize) < schedule_for(reason).len() && should_retry(reason)
}

/// The standard backoff delay for a given (zero-based) attempt number, absent a `retry-after`
/// header. 2s / 8s / 30s for attempts 0/1/2; the schedule's last entry repeats beyond that.
#[cfg(test)]
pub(crate) fn backoff_for_attempt(attempt: u32) -> Duration {
    let idx = (attempt as usize).min(BACKOFF_SCHEDULE_SECS.len() - 1);
    Duration::from_secs(BACKOFF_SCHEDULE_SECS[idx])
}

/// The schedule's delay for `reason` at `attempt`; the last entry repeats beyond the schedule.
pub(crate) fn backoff_for(reason: RetryReason, attempt: u32) -> Duration {
    let schedule = schedule_for(reason);
    Duration::from_secs(schedule[(attempt as usize).min(schedule.len() - 1)])
}

/// The delay to actually wait before the next attempt: the server's `retry-after` value
/// when present (it knows the real reset time), capped at [`MAX_RETRY_AFTER`], otherwise the
/// schedule for the kind of failure.
pub(crate) fn effective_delay(
    reason: RetryReason,
    attempt: u32,
    retry_after: Option<Duration>,
) -> Duration {
    match retry_after {
        Some(d) => d.min(MAX_RETRY_AFTER),
        None => backoff_for(reason, attempt),
    }
}

/// Parse a `retry-after` HEADER VALUE as delta-seconds (the form Anthropic sends — an
/// integer count of seconds to wait). Anything else (missing, non-numeric, an HTTP-date)
/// returns `None` so the caller falls back to the fixed backoff schedule; delta-seconds is
/// the only form worth special-casing here.
pub(crate) fn parse_retry_after_secs(value: &str) -> Option<Duration> {
    value.trim().parse::<u64>().ok().map(Duration::from_secs)
}

/// Read the `retry-after` header off a response, if present and parseable.
pub(crate) fn retry_after_from_headers(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_retry_after_secs)
}

/// What one attempt came to.
pub(crate) enum Step<T, E> {
    /// The call succeeded.
    Done(T),
    /// The call failed in a way that can clear: retry while the schedule allows. `last` is what
    /// the caller gets back if the retries run out.
    Retry {
        reason: RetryReason,
        retry_after: Option<Duration>,
        record: AttemptRecord,
        last: E,
    },
    /// The call failed in a way waiting does not fix: stop at once.
    Stop { record: AttemptRecord, last: E },
}

/// THE retry loop. Runs `attempt_fn(n)` for n = 0, 1, ... until it succeeds, reports a failure
/// that waiting cannot fix, or the schedule for that failure runs out. Returns the success with
/// the trail of the failed attempts before it, or the last failure with the whole trail. Each
/// retried attempt's record gets the wait before the next one and the `retry-after` the provider
/// sent.
pub(crate) async fn drive<T, E, F, Fut>(
    sleeper: &dyn Sleeper,
    mut attempt_fn: F,
) -> Result<(T, Vec<AttemptRecord>), (E, Vec<AttemptRecord>)>
where
    F: FnMut(u32) -> Fut,
    Fut: Future<Output = Step<T, E>>,
{
    let mut trail: Vec<AttemptRecord> = Vec::new();
    let mut attempt: u32 = 0;
    loop {
        match attempt_fn(attempt).await {
            Step::Done(value) => return Ok((value, trail)),
            Step::Stop { record, last } => {
                trail.push(record);
                return Err((last, trail));
            }
            Step::Retry {
                reason,
                retry_after,
                mut record,
                last,
            } => {
                record.retry_after_secs = retry_after.map(|d| d.as_secs());
                if !should_retry_now(attempt, reason) {
                    trail.push(record);
                    return Err((last, trail));
                }
                let delay = effective_delay(reason, attempt, retry_after);
                record.wait_secs = Some(delay.as_secs());
                eprintln!(
                    "[camerata-llm] {} attempt {} failed ({}), retrying in {delay:?}",
                    record.backend,
                    attempt + 1,
                    record.message,
                );
                trail.push(record);
                sleeper.sleep(delay).await;
                attempt += 1;
            }
        }
    }
}

/// One HTTP answer, reduced to what the policy and the failure record need.
pub(crate) struct RawResponse {
    pub status: u16,
    pub body: String,
    pub retry_after: Option<Duration>,
    pub request_id: Option<String>,
}

/// What a retried call remembers of its last failed attempt: the HTTP answer, if the failure was
/// one, and the message (`String::new()` for an HTTP answer).
type LastAttempt = (Option<RawResponse>, String);

/// The step a non-2xx HTTP answer comes to: retried while the schedule allows when the status
/// can clear, stopped when waiting cannot fix it. Shared by the one-shot and the streamed call.
fn non_success_step<T>(resp: RawResponse, mut record: AttemptRecord) -> Step<T, LastAttempt> {
    let (etype, emsg) = crate::failure::provider_error_of(&resp.body);
    record.http_status = Some(resp.status);
    record.error_type = etype;
    record.message = crate::failure::scrub_message(
        emsg.as_deref().unwrap_or(&resp.body),
        crate::failure::MAX_RECORD_MESSAGE_CHARS,
    );
    record.request_id.clone_from(&resp.request_id);
    let reason = RetryReason::HttpStatus(resp.status);
    // A 429 that says the account is out of credit or usage will not clear.
    let (_, helps) =
        crate::failure::classify_http(resp.status, record.error_type.as_deref(), &record.message);
    let retry_after = resp.retry_after;
    if should_retry(reason) && helps {
        Step::Retry {
            reason,
            retry_after,
            record,
            last: (Some(resp), String::new()),
        }
    } else {
        Step::Stop {
            record,
            last: (Some(resp), String::new()),
        }
    }
}

/// The final HTTP answer of a call (success or a terminal failure) and the trail of the
/// attempts that failed on the way.
pub(crate) struct ApiOutcome {
    pub status: u16,
    pub body: String,
    pub attempts: Vec<AttemptRecord>,
}

/// Drive an HTTP call with retry/backoff. `send` is invoked fresh on EVERY attempt so the SAME
/// request — same model, same body — is what gets retried. `Ok` carries the final status and
/// body (success, a fatal status, or the last retryable status once retries ran out; the caller
/// formats its own error from a non-success status, exactly as before). A network-level error
/// that outlasts the retries is an `Err` holding a typed [`LlmCallFailure`].
pub(crate) async fn execute_http<F, Fut>(
    send: F,
    sleeper: &dyn Sleeper,
    model: &str,
) -> anyhow::Result<ApiOutcome>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<RawResponse, String>>,
{
    let result = drive::<RawResponse, LastAttempt, _, _>(sleeper, |_attempt| {
        let sent = send();
        async move {
            let mut record = AttemptRecord::new("api", model);
            match sent.await {
                Ok(resp) if (200..300).contains(&resp.status) => Step::Done(resp),
                Ok(resp) => non_success_step(resp, record),
                Err(e) => {
                    record.message =
                        crate::failure::scrub_message(&e, crate::failure::MAX_RECORD_MESSAGE_CHARS);
                    Step::Retry {
                        reason: RetryReason::Network,
                        retry_after: None,
                        record,
                        last: (None, e),
                    }
                }
            }
        }
    })
    .await;
    match result {
        Ok((resp, attempts)) => Ok(ApiOutcome {
            status: resp.status,
            body: resp.body,
            attempts,
        }),
        Err(((Some(resp), _), attempts)) => Ok(ApiOutcome {
            status: resp.status,
            body: resp.body,
            attempts,
        }),
        Err(((None, message), attempts)) => {
            let retries = attempts.len().saturating_sub(1);
            let mut failure = LlmCallFailure::new(
                FailureClass::Transport,
                format!("Anthropic API request failed after {retries} retries: {message}"),
            );
            failure.attempts = attempts;
            failure.retryable = true;
            Err(anyhow::Error::new(failure))
        }
    }
}

/// The boxed `send` future [`execute_http`] drives for a `reqwest` call.
type SendFuture = Pin<Box<dyn Future<Output = Result<RawResponse, String>> + Send>>;

/// Build the `send` closure [`execute_http`] wants from a `reqwest` request builder.
fn reqwest_send<F>(make_request: F) -> impl Fn() -> SendFuture
where
    F: Fn() -> reqwest::RequestBuilder,
{
    move || {
        let request = make_request();
        Box::pin(async move {
            match request.send().await {
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    let retry_after = retry_after_from_headers(resp.headers());
                    let request_id = resp
                        .headers()
                        .get("request-id")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string);
                    let body = resp.text().await.unwrap_or_default();
                    Ok(RawResponse {
                        status,
                        body,
                        retry_after,
                        request_id,
                    })
                }
                Err(e) => Err(e.to_string()),
            }
        })
    }
}

/// [`execute_http`] over a `reqwest` request, keeping the trail of failed attempts.
pub(crate) async fn execute_traced<F>(
    make_request: F,
    sleeper: &dyn Sleeper,
    model: &str,
) -> anyhow::Result<ApiOutcome>
where
    F: Fn() -> reqwest::RequestBuilder,
{
    execute_http(reqwest_send(make_request), sleeper, model).await
}

/// What one attempt at a STREAMED call came to.
pub(crate) enum StreamAttempt<T> {
    /// The reply arrived (whole, or salvaged from a stream that died after delivering text).
    Done(T),
    /// The provider answered with a non-2xx status before any stream began.
    Status(RawResponse),
    /// No bytes for the idle window and no usable text delivered.
    Stalled(String),
    /// The stream broke or closed early with no usable text delivered.
    Interrupted(String),
    /// The provider sent an `error` event with no usable text delivered: its type and message.
    Provider { error_type: String, message: String },
    /// The request could not be sent at all (connect, DNS, TLS).
    Connect(String),
}

/// What a streamed call came to once its retries ran out or it succeeded.
pub(crate) enum StreamResult<T> {
    /// The reply, with the trail of failed attempts before it.
    Done(T, Vec<AttemptRecord>),
    /// A non-2xx answer that outlasted the retries or that waiting does not fix: the caller
    /// formats its own error from the status, as for the one-shot call.
    Http(ApiOutcome),
}

/// [`execute_http`] for a streamed call. `send` runs one attempt end to end (send the request,
/// read the stream) and reports what came of it; the same request is sent every time. Policy:
/// a connection failure, an interrupted stream and an `api_error` event follow the standard
/// schedule; an overloaded answer or event follows the longer one; a stream that stalls with
/// nothing usable is retried ONCE ([`RetryReason::StreamStall`]); an error event waiting cannot
/// fix stops at once. Whatever outlasts the retries is a typed Transport failure.
pub(crate) async fn execute_stream<F, Fut, T>(
    send: F,
    sleeper: &dyn Sleeper,
    model: &str,
) -> anyhow::Result<StreamResult<T>>
where
    F: Fn() -> Fut,
    Fut: Future<Output = StreamAttempt<T>>,
{
    let result = drive::<T, LastAttempt, _, _>(sleeper, |_attempt| {
        let sent = send();
        async move {
            let mut record = AttemptRecord::new("api", model);
            let transport = |reason: RetryReason, message: String, mut record: AttemptRecord| {
                record.message = crate::failure::scrub_message(
                    &message,
                    crate::failure::MAX_RECORD_MESSAGE_CHARS,
                );
                Step::Retry {
                    reason,
                    retry_after: None,
                    record,
                    last: (None, message),
                }
            };
            match sent.await {
                StreamAttempt::Done(value) => Step::Done(value),
                StreamAttempt::Status(resp) => non_success_step(resp, record),
                StreamAttempt::Stalled(m) => transport(RetryReason::StreamStall, m, record),
                StreamAttempt::Interrupted(m) | StreamAttempt::Connect(m) => {
                    transport(RetryReason::Network, m, record)
                }
                StreamAttempt::Provider {
                    error_type,
                    message,
                } => {
                    let reason = match error_type.as_str() {
                        "overloaded_error" => Some(RetryReason::Overloaded),
                        "rate_limit_error" => Some(RetryReason::HttpStatus(429)),
                        "api_error" => Some(RetryReason::HttpStatus(500)),
                        _ => None,
                    };
                    record.error_type = Some(error_type.clone());
                    let text = format!("{error_type}: {message}");
                    match reason {
                        Some(reason) => transport(reason, text, record),
                        None => {
                            record.message = crate::failure::scrub_message(
                                &text,
                                crate::failure::MAX_RECORD_MESSAGE_CHARS,
                            );
                            Step::Stop {
                                record,
                                last: (None, text),
                            }
                        }
                    }
                }
            }
        }
    })
    .await;
    match result {
        Ok((value, attempts)) => Ok(StreamResult::Done(value, attempts)),
        Err(((Some(resp), _), attempts)) => Ok(StreamResult::Http(ApiOutcome {
            status: resp.status,
            body: resp.body,
            attempts,
        })),
        Err(((None, message), attempts)) => {
            let retries = attempts.len().saturating_sub(1);
            let mut failure = LlmCallFailure::new(
                FailureClass::Transport,
                format!("Anthropic API request failed after {retries} retries: {message}"),
            );
            failure.attempts = attempts;
            failure.retryable = true;
            Err(anyhow::Error::new(failure))
        }
    }
}

/// Drive an HTTP call with retry/backoff on the real clock and return the final `(status, body)`.
/// The batch endpoints and `count_tokens` use this; the completion path uses
/// [`execute_traced`] so its failures carry the attempt trail.
pub(crate) async fn execute_with_retry<F>(
    make_request: F,
) -> anyhow::Result<(reqwest::StatusCode, String)>
where
    F: Fn() -> reqwest::RequestBuilder,
{
    let out = execute_traced(make_request, &TokioSleeper, "").await?;
    let status = reqwest::StatusCode::from_u16(out.status)
        .unwrap_or(reqwest::StatusCode::INTERNAL_SERVER_ERROR);
    Ok((status, out.body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// A sleeper that records what it was asked to wait and returns at once.
    #[derive(Default, Clone)]
    struct RecordingSleeper {
        waits: Arc<Mutex<Vec<Duration>>>,
    }

    impl RecordingSleeper {
        fn waits(&self) -> Vec<u64> {
            self.waits
                .lock()
                .unwrap()
                .iter()
                .map(|d| d.as_secs())
                .collect()
        }
    }

    impl Sleeper for RecordingSleeper {
        fn sleep<'a>(&'a self, d: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
            self.waits.lock().unwrap().push(d);
            Box::pin(async {})
        }
    }

    fn answer(status: u16, body: &str) -> Result<RawResponse, String> {
        Ok(RawResponse {
            status,
            body: body.to_string(),
            retry_after: None,
            request_id: Some("req_1".to_string()),
        })
    }

    #[test]
    fn retryable_statuses() {
        assert!(
            should_retry(RetryReason::HttpStatus(429)),
            "429 rate limit must retry"
        );
        assert!(
            should_retry(RetryReason::HttpStatus(529)),
            "529 overloaded must retry"
        );
        assert!(should_retry(RetryReason::HttpStatus(500)), "500 must retry");
        assert!(should_retry(RetryReason::HttpStatus(502)), "502 must retry");
        assert!(should_retry(RetryReason::HttpStatus(503)), "503 must retry");
        assert!(
            should_retry(RetryReason::HttpStatus(599)),
            "boundary 599 must retry"
        );
    }

    #[test]
    fn network_errors_are_retryable() {
        assert!(should_retry(RetryReason::Network));
    }

    #[test]
    fn fatal_statuses_never_retry() {
        assert!(!should_retry(RetryReason::HttpStatus(400)), "400 is fatal");
        assert!(!should_retry(RetryReason::HttpStatus(401)), "401 is fatal");
        assert!(!should_retry(RetryReason::HttpStatus(403)), "403 is fatal");
        assert!(
            !should_retry(RetryReason::HttpStatus(404)),
            "404 is fatal (not in scope's list)"
        );
        assert!(
            !should_retry(RetryReason::HttpStatus(422)),
            "other 4xx is fatal"
        );
    }

    #[test]
    fn max_retries_caps_the_loop_regardless_of_reason() {
        // At attempt == MAX_RETRIES, even an inherently-retryable reason stops.
        assert!(should_retry_now(0, RetryReason::HttpStatus(429)));
        assert!(should_retry_now(
            MAX_RETRIES - 1,
            RetryReason::HttpStatus(429)
        ));
        assert!(!should_retry_now(MAX_RETRIES, RetryReason::HttpStatus(429)));
        assert!(!should_retry_now(
            MAX_RETRIES + 1,
            RetryReason::HttpStatus(429)
        ));
        // A fatal reason never retries even at attempt 0.
        assert!(!should_retry_now(0, RetryReason::HttpStatus(401)));
    }

    #[test]
    fn backoff_schedule_is_2s_8s_30s() {
        assert_eq!(backoff_for_attempt(0), Duration::from_secs(2));
        assert_eq!(backoff_for_attempt(1), Duration::from_secs(8));
        assert_eq!(backoff_for_attempt(2), Duration::from_secs(30));
        // Beyond the schedule's length, repeat the last entry rather than panic/overflow.
        assert_eq!(backoff_for_attempt(10), Duration::from_secs(30));
    }

    #[test]
    fn retry_after_header_wins_over_fixed_backoff() {
        let r = RetryReason::HttpStatus(429);
        assert_eq!(
            effective_delay(r, 0, Some(Duration::from_secs(5))),
            Duration::from_secs(5),
            "retry-after must override the attempt-0 fixed backoff (2s)"
        );
        assert_eq!(
            effective_delay(r, 1, None),
            Duration::from_secs(8),
            "no retry-after -> falls back to the fixed schedule"
        );
    }

    #[test]
    fn parse_retry_after_secs_handles_delta_seconds_and_rejects_garbage() {
        assert_eq!(parse_retry_after_secs("5"), Some(Duration::from_secs(5)));
        assert_eq!(
            parse_retry_after_secs("  30  "),
            Some(Duration::from_secs(30))
        );
        assert_eq!(parse_retry_after_secs("0"), Some(Duration::from_secs(0)));
        // HTTP-date form and garbage are not supported (fixed backoff is used instead) —
        // this must return None, not panic or misparse.
        assert_eq!(
            parse_retry_after_secs("Wed, 21 Oct 2026 07:28:00 GMT"),
            None
        );
        assert_eq!(parse_retry_after_secs(""), None);
        assert_eq!(parse_retry_after_secs("not-a-number"), None);
    }

    // ── Item 3: the longer schedule for overloaded, the retry-after cap ──────────────────

    #[test]
    fn overloaded_answers_use_the_longer_5_20_60_120_schedule() {
        for status in [529, 503] {
            let r = RetryReason::HttpStatus(status);
            let waits: Vec<u64> = (0..4).map(|a| backoff_for(r, a).as_secs()).collect();
            assert_eq!(waits, vec![5, 20, 60, 120], "status {status}");
            assert!(should_retry_now(3, r), "a fourth retry for {status}");
            assert!(!should_retry_now(4, r));
        }
        // The text form a CLI reports uses the same schedule.
        assert_eq!(backoff_for(RetryReason::Overloaded, 3).as_secs(), 120);
    }

    #[test]
    fn other_failures_keep_the_standard_schedule_and_three_retries() {
        for r in [
            RetryReason::HttpStatus(429),
            RetryReason::HttpStatus(500),
            RetryReason::HttpStatus(502),
            RetryReason::Network,
        ] {
            let waits: Vec<u64> = (0..3).map(|a| backoff_for(r, a).as_secs()).collect();
            assert_eq!(waits, vec![2, 8, 30], "{r:?}");
            assert!(!should_retry_now(3, r), "{r:?} gets three retries only");
        }
    }

    #[test]
    fn retry_after_is_capped_at_300_seconds() {
        let r = RetryReason::HttpStatus(429);
        assert_eq!(
            effective_delay(r, 0, Some(Duration::from_secs(900))),
            Duration::from_secs(300)
        );
        assert_eq!(
            effective_delay(r, 0, Some(Duration::from_secs(299))),
            Duration::from_secs(299)
        );
    }

    #[tokio::test]
    async fn a_transient_529_then_success_leaves_no_failure_and_uses_the_overload_waits() {
        let sleeper = RecordingSleeper::default();
        let calls = AtomicUsize::new(0);
        let out = execute_http(
            || {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                async move {
                    if n < 2 {
                        answer(
                            529,
                            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
                        )
                    } else {
                        answer(200, r#"{"ok":true}"#)
                    }
                }
            },
            &sleeper,
            "claude-test",
        )
        .await
        .expect("the third attempt succeeds");
        assert_eq!(out.status, 200);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert_eq!(
            sleeper.waits(),
            vec![5, 20],
            "the overloaded schedule, no real wait"
        );
    }

    #[tokio::test]
    async fn exhausted_retries_return_the_last_answer_with_a_full_trail() {
        let sleeper = RecordingSleeper::default();
        let out = execute_http(
            || async {
                Ok(RawResponse {
                    status: 529,
                    body: r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded sk-ant-api03-AAAAAAAAAAAAAAAA1234"}}"#.to_string(),
                    retry_after: Some(Duration::from_secs(900)),
                    request_id: Some("req_zz".to_string()),
                })
            },
            &sleeper,
            "claude-test",
        )
        .await
        .expect("a terminal HTTP answer is returned, not raised");
        assert_eq!(out.status, 529);
        assert_eq!(out.attempts.len(), 5, "first try plus four retries");
        assert_eq!(
            sleeper.waits(),
            vec![300, 300, 300, 300],
            "retry-after capped"
        );
        let first = &out.attempts[0];
        assert_eq!(first.http_status, Some(529));
        assert_eq!(first.error_type.as_deref(), Some("overloaded_error"));
        assert_eq!(first.request_id.as_deref(), Some("req_zz"));
        assert_eq!(first.retry_after_secs, Some(900));
        assert_eq!(first.wait_secs, Some(300));
        assert!(!first.message.contains("AAAAAAAA"), "secrets are scrubbed");
        assert_eq!(
            out.attempts[4].wait_secs, None,
            "no wait after the last try"
        );
    }

    #[tokio::test]
    async fn a_401_is_not_retried_and_records_the_attempt() {
        let sleeper = RecordingSleeper::default();
        let calls = AtomicUsize::new(0);
        let out = execute_http(
            || {
                calls.fetch_add(1, Ordering::SeqCst);
                async {
                    answer(
                        401,
                        r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#,
                    )
                }
            },
            &sleeper,
            "m",
        )
        .await
        .unwrap();
        assert_eq!(out.status, 401);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(sleeper.waits().is_empty());
        assert_eq!(out.attempts.len(), 1);
    }

    #[tokio::test]
    async fn a_429_that_says_the_usage_limit_is_reached_is_not_retried() {
        let sleeper = RecordingSleeper::default();
        let out = execute_http(
            || async {
                answer(
                    429,
                    r#"{"type":"error","error":{"type":"rate_limit_error","message":"You have reached your specified API usage limits."}}"#,
                )
            },
            &sleeper,
            "m",
        )
        .await
        .unwrap();
        assert_eq!(out.status, 429);
        assert!(sleeper.waits().is_empty(), "waiting does not refill credit");
    }

    #[tokio::test]
    async fn a_network_error_that_outlasts_the_retries_is_a_typed_transport_failure() {
        let sleeper = RecordingSleeper::default();
        let err = execute_http(
            || async { Err("connection reset by peer".to_string()) },
            &sleeper,
            "m",
        )
        .await
        .err()
        .expect("network errors surface after the retries");
        let failure = crate::failure::call_failure_of(&err).expect("typed");
        assert_eq!(failure.class, FailureClass::Transport);
        assert_eq!(failure.attempts.len(), 4, "first try plus three retries");
        assert!(err.to_string().contains("failed after 3 retries"), "{err}");
        assert_eq!(sleeper.waits(), vec![2, 8, 30]);
    }

    // ── streamed calls ──────────────────────────────────────────────────────────────────

    /// A stalled stream (silent for the idle window, nothing usable delivered) is a Transport
    /// failure retried ONCE: two sends, one wait, then the typed failure with both attempts.
    #[tokio::test]
    async fn a_stalled_stream_is_a_transport_failure_retried_once() {
        let sleeper = RecordingSleeper::default();
        let sends = AtomicUsize::new(0);
        let err = execute_stream(
            || {
                sends.fetch_add(1, Ordering::SeqCst);
                async { StreamAttempt::<u32>::Stalled("silent for 120s".to_string()) }
            },
            &sleeper,
            "m",
        )
        .await
        .err()
        .expect("a stall that repeats surfaces");
        assert_eq!(
            sends.load(Ordering::SeqCst),
            2,
            "the first send and one retry"
        );
        assert_eq!(sleeper.waits(), vec![2]);
        let failure = crate::failure::call_failure_of(&err).expect("typed");
        assert_eq!(failure.class, FailureClass::Transport);
        assert_eq!(failure.attempts.len(), 2);
        assert!(failure.retryable);
        assert!(err.to_string().contains("silent for 120s"), "{err}");
    }

    /// The retry of a stall can answer, and then it is the reply.
    #[tokio::test]
    async fn a_stall_that_clears_on_the_retry_returns_the_reply_with_the_trail() {
        let sleeper = RecordingSleeper::default();
        let sends = AtomicUsize::new(0);
        let out = execute_stream(
            || {
                let n = sends.fetch_add(1, Ordering::SeqCst);
                async move {
                    if n == 0 {
                        StreamAttempt::Stalled("silent".to_string())
                    } else {
                        StreamAttempt::Done(7u32)
                    }
                }
            },
            &sleeper,
            "m",
        )
        .await
        .unwrap();
        match out {
            StreamResult::Done(v, trail) => {
                assert_eq!(v, 7);
                assert_eq!(trail.len(), 1);
                assert_eq!(trail[0].wait_secs, Some(2));
            }
            StreamResult::Http(_) => panic!("not an HTTP failure"),
        }
        assert_eq!(sends.load(Ordering::SeqCst), 2);
    }

    /// An interrupted stream follows the standard schedule, an overloaded event the longer one,
    /// and a provider error that waiting cannot fix stops at once.
    #[tokio::test]
    async fn interrupted_overloaded_and_fatal_stream_failures_follow_their_schedules() {
        let sleeper = RecordingSleeper::default();
        let _ = execute_stream(
            || async { StreamAttempt::<u32>::Interrupted("reset".to_string()) },
            &sleeper,
            "m",
        )
        .await;
        assert_eq!(sleeper.waits(), vec![2, 8, 30]);

        let sleeper = RecordingSleeper::default();
        let _ = execute_stream(
            || async {
                StreamAttempt::<u32>::Provider {
                    error_type: "overloaded_error".to_string(),
                    message: "Overloaded".to_string(),
                }
            },
            &sleeper,
            "m",
        )
        .await;
        assert_eq!(sleeper.waits(), vec![5, 20, 60, 120]);

        let sleeper = RecordingSleeper::default();
        let sends = AtomicUsize::new(0);
        let _ = execute_stream(
            || {
                sends.fetch_add(1, Ordering::SeqCst);
                async {
                    StreamAttempt::<u32>::Provider {
                        error_type: "invalid_request_error".to_string(),
                        message: "bad".to_string(),
                    }
                }
            },
            &sleeper,
            "m",
        )
        .await;
        assert_eq!(sends.load(Ordering::SeqCst), 1, "not retried");
        assert!(sleeper.waits().is_empty());
    }
}
