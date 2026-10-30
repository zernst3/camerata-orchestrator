//! Why a part of a run failed, and what each attempt at it said.
//!
//! Run to completion (Zach, 2026-10-28): a client deliverable must never contain a failed or
//! unfinished part, so every failure needs a reason the run can act on. One enum, [`FailureClass`],
//! says what kind of failure it was (and so what the run does next: retry at the end, count toward
//! an outage stop, or stop at once). [`AttemptRecord`] keeps what ONE attempt got back, so the
//! operator can diagnose it without reading logs. [`LlmCallFailure`] is the typed error the
//! transport returns: its text is exactly what the transport said before, and the trail rides on
//! it for the audit to pick up.

use serde::{Deserialize, Serialize};

/// Why a part failed. The class decides what the run does with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    /// The provider could not be reached or was overloaded: a dropped connection, a timeout, a
    /// 429, a 5xx or a 529 that outlasted the retries. Retried once at the end of the run; a
    /// streak of these stops the run.
    Transport,
    /// A credential or the account is the problem: a 401 or 403, a missing CLI binary, exhausted
    /// credit or quota. Waiting does not fix it, so one of these stops the run.
    AuthOrBilling,
    /// The provider answered but the answer was unusable after the in-call retries: cut off,
    /// malformed, or refused. NOT retried at the end of the run: sending the same request again
    /// pays twice for the same answer. A part that retries after a content failure changes the
    /// request (a higher cap, a smaller batch, a feedback line) inside its own rounds. Never
    /// counts toward an outage.
    #[default]
    Content,
    /// The spend limit stopped the part. Not a fault; raising the limit and resuming runs it.
    SpendLimit,
    /// The run stopped after a streak of transport failures (or an auth or billing failure)
    /// before this part was sent. It was never tried.
    OutageStop,
    /// A file could not be read for a reason other than being binary or not UTF-8.
    Io,
}

impl FailureClass {
    /// `SpendLimit`, for a serde default on a record that only ever held spend-limit refusals
    /// before the class existed.
    pub fn spend_limit() -> Self {
        Self::SpendLimit
    }

    /// A short stable name for logs and the failure report.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Transport => "transport",
            Self::AuthOrBilling => "auth_or_billing",
            Self::Content => "content",
            Self::SpendLimit => "spend_limit",
            Self::OutageStop => "outage_stop",
            Self::Io => "io",
        }
    }

    /// True for the classes the end-of-run second try covers: transport trouble only (a dropped
    /// connection, a timeout, a 429, a 5xx). The second try re-sends the SAME request, which is
    /// right when the provider could not be reached and wrong when it answered.
    pub fn is_retried_at_end(self) -> bool {
        self == Self::Transport
    }

    /// True for the one class that counts toward the outage stop's streak.
    pub fn counts_toward_outage(self) -> bool {
        self == Self::Transport
    }

    /// True for the class that stops the run on the first occurrence.
    pub fn stops_run(self) -> bool {
        self == Self::AuthOrBilling
    }
}

/// What one attempt at a call got back. Everything the operator needs to diagnose a failure
/// without the logs; never the prompt, never client code, secrets scrubbed, each message capped.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AttemptRecord {
    /// RFC3339 UTC time of the attempt.
    #[serde(default)]
    pub at: String,
    /// `api` or `cli`.
    #[serde(default)]
    pub backend: String,
    /// The model the attempt asked for.
    #[serde(default)]
    pub model: String,
    /// The HTTP status, for the API backend.
    #[serde(default)]
    pub http_status: Option<u16>,
    /// The provider's error type from the response body (`overloaded_error`, ...).
    #[serde(default)]
    pub error_type: Option<String>,
    /// The provider's error message, or the transport's own message, scrubbed and capped.
    #[serde(default)]
    pub message: String,
    /// The `request-id` response header.
    #[serde(default)]
    pub request_id: Option<String>,
    /// The `retry-after` the provider sent, in seconds.
    #[serde(default)]
    pub retry_after_secs: Option<u64>,
    /// How long the transport waited before the next attempt; `None` on the last attempt.
    #[serde(default)]
    pub wait_secs: Option<u64>,
    /// The CLI's exit code, for the CLI backend.
    #[serde(default)]
    pub exit_code: Option<i32>,
    /// The last lines of the CLI's stderr, scrubbed and capped.
    #[serde(default)]
    pub stderr_tail: Option<String>,
    /// True for an attempt made by the end-of-run second try.
    #[serde(default)]
    pub second_try: bool,
}

/// The longest message an attempt record keeps.
pub const MAX_RECORD_MESSAGE_CHARS: usize = 400;

impl AttemptRecord {
    /// A record for an attempt on `backend` with `model`, stamped now.
    pub fn new(backend: &str, model: &str) -> Self {
        Self {
            at: crate::session_sink::now_rfc3339(),
            backend: backend.to_string(),
            model: model.to_string(),
            ..Self::default()
        }
    }
}

/// Redact credentials from a diagnostic string, collapse it onto one line and cap it at
/// `max_chars`. Errors from a provider or a CLI can echo a key or a token; the failure report
/// must never carry one.
pub fn scrub_message(text: &str, max_chars: usize) -> String {
    let mut out = String::with_capacity(text.len().min(max_chars * 2));
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    for word in flat.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&scrub_word(word));
    }
    if out.chars().count() > max_chars {
        let cut: String = out.chars().take(max_chars).collect();
        return format!("{cut}...");
    }
    out
}

/// Redact one whitespace-free word when it looks like a credential.
fn scrub_word(word: &str) -> String {
    let trimmed = word.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_');
    // `key=value` style: keep the name, drop the value.
    if let Some((name, value)) = word.split_once('=') {
        let lower = name.to_ascii_lowercase();
        if !value.is_empty()
            && ["key", "token", "secret", "password", "auth", "bearer"]
                .iter()
                .any(|k| lower.contains(k))
        {
            return format!("{name}=[REDACTED]");
        }
    }
    let lower = trimmed.to_ascii_lowercase();
    let prefixed = [
        "sk-",
        "ghp_",
        "gho_",
        "github_pat_",
        "xox",
        "akia",
        "asia",
        "ya29.",
    ]
    .iter()
    .any(|p| lower.starts_with(p));
    let long_opaque = trimmed.len() >= 32
        && trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '+' || c == '/')
        && trimmed.chars().any(|c| c.is_ascii_digit())
        && trimmed.chars().any(|c| c.is_ascii_alphabetic());
    if (prefixed && trimmed.len() >= 12) || (long_opaque && !is_rule_id(trimmed)) {
        return word.replace(trimmed, "[REDACTED]");
    }
    word.to_string()
}

/// Rule ids the loaded corpus knows, so a long one is never mistaken for a credential. Filled by
/// the server when it loads a corpus; the shape test in [`has_rule_id_shape`] covers ids that
/// were never registered.
fn known_rule_ids() -> &'static std::sync::RwLock<std::collections::HashSet<String>> {
    static IDS: std::sync::OnceLock<std::sync::RwLock<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    IDS.get_or_init(Default::default)
}

/// Tell the scrubber which rule ids exist, so an operator message that names one keeps it.
pub fn register_rule_ids<I, S>(ids: I)
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    if let Ok(mut set) = known_rule_ids().write() {
        set.extend(ids.into_iter().map(Into::into));
    }
}

/// The shape every Camerata rule id has: upper-case words joined by hyphens and ending in a
/// number (`SUPABASE-FN-SEARCH-PATH-PINNED-1`). A credential has lower-case letters or no such
/// structure, so this never exempts one.
fn has_rule_id_shape(token: &str) -> bool {
    let mut parts = token.split('-');
    let Some(last) = parts.next_back() else {
        return false;
    };
    let words: Vec<&str> = parts.collect();
    words.len() >= 2
        && !last.is_empty()
        && last.len() <= 4
        && last.chars().all(|c| c.is_ascii_digit())
        && words.iter().all(|w| {
            w.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                && w.chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
                && w.len() <= 16
        })
}

/// True for a word that is a rule id: one the corpus registered, or one with a rule id's shape.
fn is_rule_id(word: &str) -> bool {
    has_rule_id_shape(word) || known_rule_ids().read().is_ok_and(|set| set.contains(word))
}

/// The typed error the transport returns. `Display` is exactly the message the transport used to
/// return as a plain string, so every caller that read the text still does; the class, the
/// attempt trail and the retry decision ride along for the audit.
#[derive(Debug, Clone)]
pub struct LlmCallFailure {
    pub class: FailureClass,
    /// The message, as before.
    pub message: String,
    /// Every attempt that failed, oldest first, the last one included.
    pub attempts: Vec<AttemptRecord>,
    /// Whether the transport judged another attempt could help. Used by the CLI retry loop.
    pub retryable: bool,
    /// The final HTTP status and body, when the failure was an HTTP answer. Lets a caller that
    /// formats its own message keep doing so.
    pub http: Option<(u16, String)>,
}

impl LlmCallFailure {
    pub fn new(class: FailureClass, message: impl Into<String>) -> Self {
        Self {
            class,
            message: message.into(),
            attempts: Vec::new(),
            retryable: false,
            http: None,
        }
    }
}

impl std::fmt::Display for LlmCallFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for LlmCallFailure {}

/// The typed failure inside `e`, if the transport produced one.
pub fn call_failure_of(e: &anyhow::Error) -> Option<&LlmCallFailure> {
    e.downcast_ref::<LlmCallFailure>()
}

/// The text the provider put in an error response body: `(error type, message)`.
pub fn provider_error_of(body: &str) -> (Option<String>, Option<String>) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return (None, None);
    };
    let err = v.get("error").unwrap_or(&v);
    (
        err.get("type").and_then(|t| t.as_str()).map(str::to_string),
        err.get("message")
            .and_then(|m| m.as_str())
            .map(str::to_string),
    )
}

/// True when `text` says the account is out of credit, quota or usage rather than briefly busy.
fn mentions_exhausted_account(lower: &str) -> bool {
    [
        "credit balance",
        "usage limit",
        "quota",
        "billing",
        "insufficient funds",
        "payment required",
    ]
    .iter()
    .any(|p| lower.contains(p))
}

/// True when `text` says the credential is missing or wrong.
fn mentions_bad_credential(lower: &str) -> bool {
    [
        "invalid x-api-key",
        "invalid api key",
        "authentication",
        "unauthorized",
        "not logged in",
        "/login",
        "permission_error",
        "forbidden",
        "oauth token",
    ]
    .iter()
    .any(|p| lower.contains(p))
}

/// Classify an HTTP answer: `(class, whether another attempt could help)`.
pub fn classify_http(status: u16, error_type: Option<&str>, message: &str) -> (FailureClass, bool) {
    let lower = message.to_ascii_lowercase();
    let etype = error_type.unwrap_or("");
    if matches!(
        etype,
        "authentication_error" | "permission_error" | "billing_error"
    ) || matches!(status, 401..=403)
    {
        return (FailureClass::AuthOrBilling, false);
    }
    if mentions_exhausted_account(&lower) {
        return (FailureClass::AuthOrBilling, false);
    }
    if status == 429 || status == 408 || (500..600).contains(&status) {
        return (FailureClass::Transport, true);
    }
    if matches!(etype, "rate_limit_error" | "overloaded_error" | "api_error") {
        return (FailureClass::Transport, true);
    }
    (FailureClass::Content, false)
}

/// Classify the text a CLI or a stub reported: `(class, whether another attempt could help)`.
/// Used for a CLI's stderr and in-band error text, and as the fallback for an error that carries
/// no typed failure (a stub in a test, a provider without a typed transport).
pub fn classify_text(text: &str) -> (FailureClass, bool) {
    let lower = text.to_ascii_lowercase();
    if mentions_exhausted_account(&lower) || mentions_bad_credential(&lower) {
        return (FailureClass::AuthOrBilling, false);
    }
    let transport = [
        "overloaded",
        "rate limit",
        "rate_limit",
        "too many requests",
        "429",
        "529",
        "503",
        "502",
        "500",
        "timed out",
        "timeout",
        "connection",
        "network",
        "econnreset",
        "produced no model output",
        "backstop",
        "temporarily unavailable",
        "service unavailable",
        "bad gateway",
    ];
    if transport.iter().any(|p| lower.contains(p)) {
        return (FailureClass::Transport, true);
    }
    (FailureClass::Content, false)
}

/// The class of any error: the typed failure's own class, else the text classifier. A model
/// refusal, a cut-off reply and a malformed reply are content by construction (the audit marks
/// them); an error nobody can place is content, so it never counts toward an outage stop.
pub fn classify_error(e: &anyhow::Error) -> FailureClass {
    match call_failure_of(e) {
        Some(f) => f.class,
        None => classify_text(&e.to_string()).0,
    }
}

/// The attempt trail inside `e`, empty when the error is not a typed transport failure.
pub fn attempts_of(e: &anyhow::Error) -> Vec<AttemptRecord> {
    call_failure_of(e)
        .map(|f| f.attempts.clone())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes_serialize_as_snake_case_and_default_to_content() {
        assert_eq!(
            serde_json::to_string(&FailureClass::AuthOrBilling).unwrap(),
            "\"auth_or_billing\""
        );
        assert_eq!(
            serde_json::from_str::<FailureClass>("\"outage_stop\"").unwrap(),
            FailureClass::OutageStop
        );
        assert_eq!(FailureClass::default(), FailureClass::Content);
    }

    #[test]
    fn only_transport_counts_toward_an_outage_and_only_auth_stops_at_once() {
        for c in [
            FailureClass::Content,
            FailureClass::SpendLimit,
            FailureClass::OutageStop,
            FailureClass::Io,
            FailureClass::AuthOrBilling,
        ] {
            assert!(!c.counts_toward_outage(), "{c:?}");
        }
        assert!(FailureClass::Transport.counts_toward_outage());
        assert!(FailureClass::AuthOrBilling.stops_run());
        assert!(!FailureClass::Transport.stops_run());
        assert!(FailureClass::Transport.is_retried_at_end());
        // A content failure is not sent again unchanged: the provider answered.
        assert!(!FailureClass::Content.is_retried_at_end());
        assert!(!FailureClass::AuthOrBilling.is_retried_at_end());
        assert!(!FailureClass::SpendLimit.is_retried_at_end());
        assert!(!FailureClass::OutageStop.is_retried_at_end());
        assert!(!FailureClass::Io.is_retried_at_end());
    }

    #[test]
    fn http_statuses_classify_by_what_waiting_can_fix() {
        for s in [429, 500, 502, 503, 529, 408] {
            assert_eq!(classify_http(s, None, "").0, FailureClass::Transport, "{s}");
        }
        for s in [401, 402, 403] {
            assert_eq!(
                classify_http(s, None, "").0,
                FailureClass::AuthOrBilling,
                "{s}"
            );
        }
        assert_eq!(
            classify_http(400, None, "bad shape").0,
            FailureClass::Content
        );
        // A 400 that says the credit is gone is billing, not a bad request.
        assert_eq!(
            classify_http(
                400,
                Some("invalid_request_error"),
                "Your credit balance is too low"
            )
            .0,
            FailureClass::AuthOrBilling
        );
        // A 429 that says the usage limit is reached will not clear by waiting.
        let (c, retry) = classify_http(
            429,
            Some("rate_limit_error"),
            "You have reached your specified API usage limits.",
        );
        assert_eq!(c, FailureClass::AuthOrBilling);
        assert!(!retry);
        assert_eq!(
            classify_http(200, Some("billing_error"), "").0,
            FailureClass::AuthOrBilling
        );
    }

    #[test]
    fn cli_text_classifies_overload_as_transport_and_login_as_auth() {
        assert_eq!(
            classify_text("API Error: 529 overloaded_error").0,
            FailureClass::Transport
        );
        assert_eq!(
            classify_text("Claude AI usage limit reached").0,
            FailureClass::AuthOrBilling
        );
        assert_eq!(
            classify_text("Not logged in. Please run /login").0,
            FailureClass::AuthOrBilling
        );
        assert_eq!(classify_text("boom").0, FailureClass::Content);
    }

    #[test]
    fn an_untyped_error_is_classified_from_its_text() {
        let e = anyhow::anyhow!("connection reset by peer");
        assert_eq!(classify_error(&e), FailureClass::Transport);
        let typed = anyhow::Error::new(LlmCallFailure::new(FailureClass::Io, "unreadable"));
        assert_eq!(classify_error(&typed), FailureClass::Io);
        // Context wrapped around a typed failure still finds it.
        let wrapped = typed.context("while scanning");
        assert_eq!(classify_error(&wrapped), FailureClass::Io);
    }

    #[test]
    fn scrub_redacts_keys_collapses_lines_and_caps_length() {
        let s = scrub_message(
            "bad key sk-ant-api03-AAAAAAAAAAAAAAAA1234 sent\nin x-api-key=hunter2value here",
            400,
        );
        assert!(!s.contains("AAAAAAAA"), "{s}");
        assert!(!s.contains("hunter2value"), "{s}");
        assert!(!s.contains('\n'));
        assert!(s.contains("[REDACTED]"));
        let long = scrub_message(&"word ".repeat(500), 100);
        assert!(long.chars().count() <= 103, "{}", long.chars().count());
    }

    #[test]
    fn a_long_rule_id_survives_the_scrub_and_a_credential_does_not() {
        // The shape of a rule id keeps it, even at 32 characters or more with a digit.
        let id = "SUPABASE-FN-SEARCH-PATH-PINNED-1";
        assert!(id.len() >= 32);
        let s = scrub_message(&format!("rule {id} did not run"), 400);
        assert!(s.contains(id), "{s}");
        // A registered id that does not have the shape is kept too.
        register_rule_ids(["Odd_registered_rule_identifier_number_7x"]);
        let s = scrub_message("see Odd_registered_rule_identifier_number_7x now", 400);
        assert!(
            s.contains("Odd_registered_rule_identifier_number_7x"),
            "{s}"
        );
        // Credential-like words are still redacted: opaque, mixed case, or shaped like an id
        // only in part.
        for secret in [
            "k9Zx2LmQ8vB7nR4tY6uW1eP3aS5dF0gH",
            "ABCDEFGH12345678ABCDEFGH12345678",
            "ABCDEFGH-ABCDEFGH-12345678-ABCDEFGH-ABCDEFGH123",
            "abcdef0123456789abcdef0123456789-1",
        ] {
            let s = scrub_message(&format!("value {secret} rejected"), 400);
            assert!(!s.contains(secret), "{secret} survived: {s}");
            assert!(s.contains("[REDACTED]"), "{s}");
        }
    }

    #[test]
    fn provider_error_body_gives_type_and_message() {
        let (t, m) = provider_error_of(
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
        );
        assert_eq!(t.as_deref(), Some("overloaded_error"));
        assert_eq!(m.as_deref(), Some("Overloaded"));
        assert_eq!(provider_error_of("<html>502</html>"), (None, None));
    }

    #[test]
    fn display_is_the_message_unchanged() {
        let f = LlmCallFailure::new(FailureClass::Transport, "Anthropic API HTTP 529: x");
        assert_eq!(f.to_string(), "Anthropic API HTTP 529: x");
    }
}
