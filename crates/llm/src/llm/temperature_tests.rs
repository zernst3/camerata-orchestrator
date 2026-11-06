//! H5 item 3: a request's sampling temperature reaches the API only where the model accepts
//! it. The catalog's request shape decides (`model_registry::accepts_sampling_params`), and
//! `conform_messages_body` still runs last, so a strict model never sees the field and the
//! "a call site sent something this model rejects" warning never fires for it.

use super::*;

const LEGACY: &str = "claude-haiku-4-5-20251001";
const STRICT: &str = "claude-opus-5";

#[test]
fn the_catalog_says_which_models_accept_a_temperature() {
    assert!(crate::model_registry::accepts_sampling_params(LEGACY));
    assert!(!crate::model_registry::accepts_sampling_params(STRICT));
    assert!(
        !crate::model_registry::accepts_sampling_params("claude-from-the-future"),
        "an unknown model is presumed strict"
    );
    // The built-in defaults (Sonnet 5.5 and Haiku 5.5) reject sampling parameters: a request
    // that asks for temperature 0 reaches them with the field stripped, never rejected. Only a
    // project that picks Haiku 4.5 gets the temperature on the wire.
    for strict_default in ["claude-sonnet-5-5", "claude-haiku-5-5", "claude-opus-5-5"] {
        assert!(
            !crate::model_registry::accepts_sampling_params(strict_default),
            "{strict_default} is strict"
        );
        let body = messages_request_body(
            &LlmRequest::new("audit this").with_temperature(0.0),
            strict_default,
            CacheTtl::FiveMinutes,
        );
        assert!(body.get("temperature").is_none(), "{body}");
    }
}

#[test]
fn a_request_carries_no_temperature_unless_asked() {
    assert_eq!(LlmRequest::new("p").temperature, None);
    assert_eq!(
        LlmRequest::new("p").with_temperature(0.0).temperature,
        Some(0.0)
    );
}

#[test]
fn the_messages_body_sends_the_temperature_only_to_a_model_that_accepts_it() {
    let req = LlmRequest::new("audit this").with_temperature(0.0);
    let legacy = messages_request_body(&req, LEGACY, CacheTtl::FiveMinutes);
    assert_eq!(legacy["temperature"], serde_json::json!(0.0));
    let strict = messages_request_body(&req, STRICT, CacheTtl::FiveMinutes);
    assert!(strict.get("temperature").is_none(), "{strict}");

    let unset = messages_request_body(&LlmRequest::new("p"), LEGACY, CacheTtl::FiveMinutes);
    assert!(
        unset.get("temperature").is_none(),
        "no temperature unless asked"
    );
}

#[test]
fn the_batch_item_sends_the_temperature_only_to_a_model_that_accepts_it() {
    let req = LlmRequest::new("audit this").with_temperature(0.0);
    let legacy =
        serde_json::to_value(build_batch_item("c", &req, LEGACY, BATCH_CACHE_TTL)).unwrap();
    assert_eq!(legacy["params"]["temperature"], serde_json::json!(0.0));
    let strict =
        serde_json::to_value(build_batch_item("c", &req, STRICT, BATCH_CACHE_TTL)).unwrap();
    assert!(strict["params"].get("temperature").is_none(), "{strict}");
}
