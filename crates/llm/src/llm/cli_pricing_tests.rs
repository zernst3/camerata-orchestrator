//! The cli backend prices a call from the token counts the CLI reports and the model that
//! answered, through the engine's own price registry. The CLI's `total_cost_usd` prices a model
//! id it does not know at a default (Opus-class) rate, so it is kept beside the modeled figure
//! for comparison and never used as the cost when the engine can price the call itself.
//!
//! The event shapes are what `claude -p --output-format stream-json` prints.

use super::*;

fn spawn(script: &str) -> tokio::process::Child {
    tokio::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .kill_on_drop(true)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn the scripted stand-in for `claude`")
}

async fn read(requested: &str, result_event: &str) -> LlmResponse {
    let script = format!("printf '%s\\n' '{result_event}'\n");
    read_cli_stream(
        spawn(&script),
        requested,
        std::time::Duration::from_secs(30),
        std::time::Duration::from_secs(30),
        &mut |_| {},
    )
    .await
    .expect("a healthy scripted call")
}

fn list_price(model: &str, input: u64, output: u64, read: u64, write: u64) -> f64 {
    usage_cost_usd(
        model,
        &CallUsage {
            input: Some(input + read + write),
            output: Some(output),
            cache_read: read,
            cache_creation: write,
            cache_creation_1h: None,
        },
        PriceBasis {
            batch: false,
            cache_ttl: CacheTtl::FiveMinutes,
        },
    )
    .expect("priced model")
}

/// A fix call that asked for Haiku and ran on Haiku: the CLI said $0.3527 (an Opus-class figure
/// for this usage); the engine prices the same tokens at Haiku rates.
#[tokio::test]
async fn a_haiku_fix_call_is_priced_at_haiku_rates_not_the_cli_figure() {
    let resp = read(
        "claude-haiku-5-5",
        r#"{"type":"result","is_error":false,"result":"FINAL","total_cost_usd":0.3527,"usage":{"input_tokens":120,"output_tokens":8184,"cache_read_input_tokens":0,"cache_creation_input_tokens":23632},"modelUsage":{"claude-haiku-5-5":{"inputTokens":120,"outputTokens":8184,"costUSD":0.3527}}}"#,
    )
    .await;
    let modeled = list_price("claude-haiku-5-5", 120, 8184, 0, 23632);
    assert!((resp.cost_usd.unwrap() - modeled).abs() < 1e-9, "{resp:?}");
    assert!(resp.cost_usd.unwrap() < 0.05, "Haiku, not Opus-class");
    assert_eq!(resp.cli_cost_usd, Some(0.3527), "the CLI figure is kept");
}

/// When the CLI reports a different model than the one requested, the model that answered sets
/// the rate.
#[tokio::test]
async fn the_reported_model_sets_the_rate_when_it_differs_from_the_requested_one() {
    let resp = read(
        "claude-haiku-5-5",
        r#"{"type":"result","is_error":false,"result":"FINAL","total_cost_usd":0.5,"usage":{"input_tokens":1000,"output_tokens":2000},"modelUsage":{"claude-opus-5-5":{"inputTokens":1000,"outputTokens":2000,"costUSD":0.5}}}"#,
    )
    .await;
    assert_eq!(resp.reported_model.as_deref(), Some("claude-opus-5-5"));
    let modeled = list_price("claude-opus-5-5", 1000, 2000, 0, 0);
    assert!((resp.cost_usd.unwrap() - modeled).abs() < 1e-9, "{resp:?}");
}

/// With no reported model the requested one prices the call.
#[tokio::test]
async fn the_requested_model_prices_a_call_that_reports_none() {
    let resp = read(
        "claude-sonnet-5-5",
        r#"{"type":"result","is_error":false,"result":"FINAL","total_cost_usd":9.0,"usage":{"input_tokens":1000,"output_tokens":2000,"cache_read_input_tokens":4000}}"#,
    )
    .await;
    assert_eq!(resp.reported_model, None);
    let modeled = list_price("claude-sonnet-5-5", 1000, 2000, 4000, 0);
    assert!((resp.cost_usd.unwrap() - modeled).abs() < 1e-9, "{resp:?}");
    assert_eq!(resp.cli_cost_usd, Some(9.0));
}

/// A model the registry does not price, or a result with no token counts, keeps the CLI's own
/// figure: an unknown cost would let a spend limit admit nothing at all.
#[tokio::test]
async fn an_unpriced_model_or_missing_tokens_keep_the_cli_figure() {
    let unpriced = read(
        "totally-unknown-model",
        r#"{"type":"result","is_error":false,"result":"FINAL","total_cost_usd":0.25,"usage":{"input_tokens":10,"output_tokens":20}}"#,
    )
    .await;
    assert_eq!(unpriced.cost_usd, Some(0.25));
    assert_eq!(unpriced.cli_cost_usd, Some(0.25));
    let no_tokens = read(
        "claude-sonnet-5-5",
        r#"{"type":"result","is_error":false,"result":"FINAL","total_cost_usd":0.07}"#,
    )
    .await;
    assert_eq!(no_tokens.cost_usd, Some(0.07));
    let no_figure = read(
        "claude-sonnet-5-5",
        r#"{"type":"result","is_error":false,"result":"FINAL","usage":{"input_tokens":10,"output_tokens":20}}"#,
    )
    .await;
    assert!(no_figure.cost_usd.is_some(), "modeled without a CLI figure");
    assert_eq!(no_figure.cli_cost_usd, None);
}
