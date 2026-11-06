//! The one gate every text bound for a model passes through.
//!
//! The ruling (Zach, 2026-11-06): nothing sent to any model, on any backend, contains a raw secret.
//! Redaction used to be applied site by site (the scan's file set, the grounding digest, the L3
//! diff), which is convention: one site that forgot leaks. This module is the mechanical part. The
//! transports in `camerata-llm` call [`redact_for_model`] on every text part of every request
//! (system prompt, user prompt, assistant turns, tool results) while the switch is on, and the
//! agent drivers call it on the task they hand a CLI. A guard test in `camerata-llm` fails if any
//! crate builds or sends a model request in a way that does not reach it.
//!
//! The text is redacted with [`crate::redact_str`], so text that holds no secret comes back
//! BORROWED, byte for byte: a prompt that was already redacted upstream is unchanged, and so is the
//! digest the run store files its stored replies under.
//!
//! Why this lives in the leaf crate and not in `camerata-llm` itself: the agent drivers
//! (`camerata-agent`) must reach the same function, and `camerata-llm` pulls in the OS keychain
//! and the HTTP stack, which the client CI binary (which depends on the agent crate through the
//! checks crate) must not carry. `camerata_llm::model_input` re-exports everything here and adds
//! the request-level guard.

use std::borrow::Cow;
use std::sync::OnceLock;

use serde_json::Value;

use crate::redact::redact;

/// The one switch for `model_prompt_redaction`: set to `false` to send raw text to the model.
pub const MODEL_PROMPT_REDACTION_ENV: &str = "CAMERATA_FEATURE_MODEL_PROMPT_REDACTION";

/// The operator's value of [`MODEL_PROMPT_REDACTION_ENV`] as it was before any `.env` loader ran.
static OPERATOR_VALUE: OnceLock<Option<String>> = OnceLock::new();

/// Record the operator's real environment value for the redaction switch. A binary calls this
/// FIRST in `main`, before it loads a `.env` file: a `.env` is resolved against the working
/// directory, which may be a client repository, and must not be able to switch redaction off.
/// Idempotent; without a call the live environment is read.
pub fn pin_operator_redaction_setting() {
    OPERATOR_VALUE.get_or_init(|| std::env::var(MODEL_PROMPT_REDACTION_ENV).ok());
}

fn is_false(value: &str) -> bool {
    value.trim().eq_ignore_ascii_case("false")
}

/// Whether text is redacted before any model sees it. Reads the environment ONLY (never a file in
/// the working directory or a client repository); ON unless the variable is explicitly `false`.
pub fn model_prompt_redaction_enabled() -> bool {
    let value = match OPERATOR_VALUE.get() {
        Some(pinned) => pinned.clone(),
        None => std::env::var(MODEL_PROMPT_REDACTION_ENV).ok(),
    };
    !value.is_some_and(|v| is_false(&v))
}

/// The warning logged on every call that goes out unredacted because the operator switched
/// redaction off.
fn warn_unredacted() {
    tracing::warn!(
        "model_prompt_redaction is OFF: raw text, secret values included, is being sent to the \
         model ({MODEL_PROMPT_REDACTION_ENV}=false)"
    );
}

/// `text` as a model may see it: every secret value replaced by its redacted form while the switch
/// is on. Borrows (the same bytes) when nothing matched, and when the switch is off (which logs a
/// warning, as every unredacted call does).
pub fn redact_for_model(text: &str) -> Cow<'_, str> {
    redact_for_model_with(text, model_prompt_redaction_enabled())
}

/// [`redact_for_model`] with the switch decided by the caller (the pure, testable core).
pub fn redact_for_model_with(text: &str, enabled: bool) -> Cow<'_, str> {
    if !enabled {
        warn_unredacted();
        return Cow::Borrowed(text);
    }
    crate::redact_str(text)
}

/// [`redact_for_model`] for a text that other positions point into (the prompt-cache breakpoints
/// of a request are byte offsets into its prompt). Returns the redacted text and each of
/// `offsets` moved to the same place in it: an offset after a replaced value shifts by the change
/// in length; one that fell inside a value moves to the start of its replacement, so a value is
/// never split across two segments. Offsets are returned in the order given. When nothing is
/// replaced the text is borrowed and the offsets are returned as they were.
pub fn redact_for_model_with_offsets<'a>(
    text: &'a str,
    offsets: &[usize],
) -> (Cow<'a, str>, Vec<usize>) {
    redact_with_offsets_using(text, offsets, model_prompt_redaction_enabled())
}

/// [`redact_for_model_with_offsets`] with the switch decided by the caller.
pub fn redact_with_offsets_using<'a>(
    text: &'a str,
    offsets: &[usize],
    enabled: bool,
) -> (Cow<'a, str>, Vec<usize>) {
    if !enabled {
        warn_unredacted();
        return (Cow::Borrowed(text), offsets.to_vec());
    }
    let redacted = redact(text);
    if redacted.redactions.is_empty() {
        return (Cow::Borrowed(text), offsets.to_vec());
    }
    let moved = offsets
        .iter()
        .map(|&offset| {
            let mut shift: isize = 0;
            let mut inside_at: Option<usize> = None;
            for r in &redacted.redactions {
                if r.span.end <= offset {
                    shift += r.form.len() as isize - (r.span.end - r.span.start) as isize;
                } else if r.span.start < offset {
                    // The offset fell inside this value: land on the start of its replacement.
                    inside_at = Some(r.span.start);
                    break;
                } else {
                    break;
                }
            }
            let base = inside_at.unwrap_or(offset);
            usize::try_from(base as isize + shift).unwrap_or(0)
        })
        .collect();
    (Cow::Owned(redacted.text), moved)
}

/// Redact every string in a JSON tree that is bound for a model (a messages array with assistant
/// turns, tool calls and tool results), in place, while the switch is on. Returns the number of
/// values replaced. Keys are not touched; numbers and booleans carry no secret.
pub fn redact_json_for_model(value: &mut Value) -> usize {
    if !model_prompt_redaction_enabled() {
        warn_unredacted();
        return 0;
    }
    crate::redact_json_values(value)
}
