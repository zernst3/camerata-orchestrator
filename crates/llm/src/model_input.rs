//! The model-input chokepoint: every request that leaves for a model is redacted here first.
//!
//! The ruling (Zach, 2026-11-06): nothing sent to any model, on any backend, contains a raw
//! secret, the operator chat assistant included. The redaction itself is
//! [`camerata_secrets::model_input`] (re-exported below, one function for text and one for a
//! JSON message tree); this module adds the request-level guard, [`guarded`], which every wire
//! builder in this crate calls before it reads the request:
//!
//! - [`crate::llm`]'s `messages_request_body` (the Messages API, real time),
//! - `build_batch_item` and `Llm::submit_batch` (the Message Batches API),
//! - `count_tokens_body` (the token count),
//! - `build_cli_command` (the `claude -p` argv: the prompt and the system prompt),
//! - `build_openrouter_chat_body` (OpenRouter).
//!
//! The run store names a request by a digest, and it computes that digest over the request AFTER
//! this guard (`camerata_server::run_store::journal`), so the key a stored reply is filed under
//! is the key of what was sent. A prompt that was already redacted upstream comes out of the
//! guard byte for byte, so its digest, and every saved run filed under it, is unchanged.
//!
//! `crates/llm/tests/model_input_guard.rs` fails when any crate builds or sends a model request
//! without going through here.

use std::borrow::Cow;

pub use camerata_secrets::model_input::{
    model_prompt_redaction_enabled, pin_operator_redaction_setting, redact_for_model,
    redact_for_model_with, redact_for_model_with_offsets, redact_json_for_model,
    redact_with_offsets_using, MODEL_PROMPT_REDACTION_ENV,
};

use crate::llm::LlmRequest;

/// `req` as a model may see it: the system prompt and the prompt redacted while the switch is
/// on, the prompt-cache breakpoints (byte offsets into the prompt) moved to the same places in
/// the redacted prompt. Borrows (the request itself) when nothing was replaced.
pub fn guarded(req: &LlmRequest) -> Cow<'_, LlmRequest> {
    guarded_with(req, model_prompt_redaction_enabled())
}

/// [`guarded`] with the switch decided by the caller (the pure, testable core).
pub fn guarded_with(req: &LlmRequest, enabled: bool) -> Cow<'_, LlmRequest> {
    let (prompt, breakpoints) =
        redact_with_offsets_using(&req.prompt, &req.cache_breakpoints, enabled);
    let system = req
        .system
        .as_deref()
        .map(|system| redact_for_model_with(system, enabled));
    let prompt_changed = matches!(prompt, Cow::Owned(_));
    let system_changed = matches!(system, Some(Cow::Owned(_)));
    if !prompt_changed && !system_changed {
        return Cow::Borrowed(req);
    }
    let mut guarded = req.clone();
    if let Cow::Owned(prompt) = prompt {
        guarded.prompt = prompt;
        guarded.cache_breakpoints = breakpoints;
    }
    if let Some(Cow::Owned(system)) = system {
        guarded.system = Some(system);
    }
    Cow::Owned(guarded)
}

/// [`guarded`], consuming the request: the request itself when nothing was replaced (no copy),
/// the redacted one otherwise. For a seam that passes the request on (the run store's journal
/// hands the model the same request it named).
pub fn guarded_owned(req: LlmRequest) -> LlmRequest {
    match guarded(&req) {
        Cow::Borrowed(_) => req,
        Cow::Owned(redacted) => redacted,
    }
}
