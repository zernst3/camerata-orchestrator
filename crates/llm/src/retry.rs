//! Retry/backoff policy for the Anthropic API transport (`complete_api`, `submit_batch`,
//! `poll_batch_status`, `fetch_batch_results` in [`crate::llm`]).
//!
//! A production scan can run for hours across hundreds of HTTP calls; a single 429 (rate
//! limit), 529 (Anthropic overloaded), transient 5xx, or dropped connection used to fail
//! the ENTIRE pass. This module is the decision layer — "should this failure be retried,
//! and after how long" — kept PURE and separate from the HTTP call itself so it is
//! unit-testable without a live API key or a mock server (see the `tests` module below).
//! [`execute_with_retry`] is the one place that actually loops + sleeps; everything it
//! calls into is a plain function of (status code / error kind, attempt number).
//!
//! Policy: retry 429, 529, and any 5xx, plus network/connect errors — up to
//! [`MAX_RETRIES`] additional attempts with exponential backoff (2s, 8s, 30s), UNLESS the
//! response carries a `retry-after` header, in which case that value wins for that attempt
//! (the server knows better than our fixed schedule). 400/401/403 are never retried — a bad
//! request or bad credential doesn't get better by waiting.
//!
//! Same-model guarantee: this module has no notion of "model" or "fallback chain" at all —
//! [`execute_with_retry`] re-invokes the SAME request-builder closure on every attempt, so
//! retrying can never silently switch models the way [`crate::llm::call_with_fallback`]
//! does. Fallback-chain routing is a distinct, higher-level concern (see
//! [`crate::llm::is_retryable_for_chain`]) and is untouched by this module.

use std::time::Duration;

/// One additional attempt beyond the first, capped by [`MAX_RETRIES`]. Chosen to bound an
/// already-long batch/scan run to a handful of extra minutes at most, not retry forever.
pub(crate) const MAX_RETRIES: u32 = 3;

/// Fixed exponential backoff schedule in seconds, indexed by (zero-based) attempt number.
/// The last entry repeats for any attempt beyond the array's length (defensive; `MAX_RETRIES`
/// never actually reaches that branch today).
const BACKOFF_SCHEDULE_SECS: [u64; 3] = [2, 8, 30];

/// The kind of failure being classified — either an HTTP status code or a transport-level
/// (network/connect/timeout) error with no status code at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RetryReason {
    HttpStatus(u16),
    Network,
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
        RetryReason::Network => true,
    }
}

/// Full decision: given how many retries have ALREADY happened (`attempt`, zero-based) and
/// the failure kind, should one more attempt be made? Folds in the [`MAX_RETRIES`] cap so
/// callers have a single yes/no gate instead of checking `should_retry` and the cap
/// separately.
pub(crate) fn should_retry_now(attempt: u32, reason: RetryReason) -> bool {
    attempt < MAX_RETRIES && should_retry(reason)
}

/// The fixed backoff delay for a given (zero-based) attempt number, absent a `retry-after`
/// header. 2s / 8s / 30s for attempts 0/1/2; the schedule's last entry repeats beyond that.
pub(crate) fn backoff_for_attempt(attempt: u32) -> Duration {
    let idx = (attempt as usize).min(BACKOFF_SCHEDULE_SECS.len() - 1);
    Duration::from_secs(BACKOFF_SCHEDULE_SECS[idx])
}

/// The delay to actually wait before the next attempt: the server's `retry-after` value
/// when present (it knows the real reset time), otherwise the fixed backoff schedule.
pub(crate) fn effective_delay(attempt: u32, retry_after: Option<Duration>) -> Duration {
    retry_after.unwrap_or_else(|| backoff_for_attempt(attempt))
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

/// Drive an HTTP call with retry/backoff. `make_request` is invoked fresh on EVERY attempt
/// (including the first) so the SAME request — same model, same body — is what gets
/// retried; there is no fallback-chain behavior here. Returns the final `(status, body)`
/// pair once either the call succeeds, a fatal status is hit, or retries are exhausted —
/// the caller is responsible for checking `status.is_success()` and formatting its own
/// error, exactly as the pre-retry code did.
///
/// Network-level errors (`reqwest::Error` with no response, e.g. connect/timeout/DNS
/// failures) are retried the same way as a retryable status code; once retries are
/// exhausted the network error is surfaced as an `anyhow::Error`.
pub(crate) async fn execute_with_retry<F>(
    make_request: F,
) -> anyhow::Result<(reqwest::StatusCode, String)>
where
    F: Fn() -> reqwest::RequestBuilder,
{
    let mut attempt: u32 = 0;
    loop {
        match make_request().send().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    let text = resp.text().await.unwrap_or_default();
                    return Ok((status, text));
                }
                let retry_after = retry_after_from_headers(resp.headers());
                let text = resp.text().await.unwrap_or_default();
                let reason = RetryReason::HttpStatus(status.as_u16());
                if should_retry_now(attempt, reason) {
                    let delay = effective_delay(attempt, retry_after);
                    eprintln!(
                        "[camerata-llm] Anthropic API HTTP {status} (attempt {}/{}), retrying in {delay:?}: {}",
                        attempt + 1,
                        MAX_RETRIES,
                        text.chars().take(300).collect::<String>(),
                    );
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                    continue;
                }
                // Fatal (400/401/403/other) or retries exhausted: return the terminal
                // status/body so the caller formats the same error message it always has.
                return Ok((status, text));
            }
            Err(e) => {
                let reason = RetryReason::Network;
                if should_retry_now(attempt, reason) {
                    let delay = backoff_for_attempt(attempt);
                    eprintln!(
                        "[camerata-llm] Anthropic API network error (attempt {}/{}), retrying in {delay:?}: {e}",
                        attempt + 1,
                        MAX_RETRIES,
                    );
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                    continue;
                }
                anyhow::bail!("Anthropic API request failed after {attempt} retries: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryable_statuses() {
        assert!(should_retry(RetryReason::HttpStatus(429)), "429 rate limit must retry");
        assert!(should_retry(RetryReason::HttpStatus(529)), "529 overloaded must retry");
        assert!(should_retry(RetryReason::HttpStatus(500)), "500 must retry");
        assert!(should_retry(RetryReason::HttpStatus(502)), "502 must retry");
        assert!(should_retry(RetryReason::HttpStatus(503)), "503 must retry");
        assert!(should_retry(RetryReason::HttpStatus(599)), "boundary 599 must retry");
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
        assert!(!should_retry(RetryReason::HttpStatus(404)), "404 is fatal (not in scope's list)");
        assert!(!should_retry(RetryReason::HttpStatus(422)), "other 4xx is fatal");
    }

    #[test]
    fn max_retries_caps_the_loop_regardless_of_reason() {
        // At attempt == MAX_RETRIES, even an inherently-retryable reason stops.
        assert!(should_retry_now(0, RetryReason::HttpStatus(429)));
        assert!(should_retry_now(MAX_RETRIES - 1, RetryReason::HttpStatus(429)));
        assert!(!should_retry_now(MAX_RETRIES, RetryReason::HttpStatus(429)));
        assert!(!should_retry_now(MAX_RETRIES + 1, RetryReason::HttpStatus(429)));
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
        assert_eq!(
            effective_delay(0, Some(Duration::from_secs(5))),
            Duration::from_secs(5),
            "retry-after must override the attempt-0 fixed backoff (2s)"
        );
        assert_eq!(
            effective_delay(1, None),
            Duration::from_secs(8),
            "no retry-after -> falls back to the fixed schedule"
        );
    }

    #[test]
    fn parse_retry_after_secs_handles_delta_seconds_and_rejects_garbage() {
        assert_eq!(parse_retry_after_secs("5"), Some(Duration::from_secs(5)));
        assert_eq!(parse_retry_after_secs("  30  "), Some(Duration::from_secs(30)));
        assert_eq!(parse_retry_after_secs("0"), Some(Duration::from_secs(0)));
        // HTTP-date form and garbage are not supported (fixed backoff is used instead) —
        // this must return None, not panic or misparse.
        assert_eq!(parse_retry_after_secs("Wed, 21 Oct 2026 07:28:00 GMT"), None);
        assert_eq!(parse_retry_after_secs(""), None);
        assert_eq!(parse_retry_after_secs("not-a-number"), None);
    }
}
