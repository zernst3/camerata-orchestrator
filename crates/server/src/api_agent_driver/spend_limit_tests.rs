//! The spend limit of the in-process agent tool loop: every provider turn is priced from the
//! model registry and recorded in the run ledger, and a run stops BEFORE the turn that would
//! pass its limit, with the reason. All turns here are scripted (no model, no cost).

use std::path::PathBuf;
use std::sync::Arc;

use camerata_core::{AgentDriver, Role};
use serde_json::json;

use super::scripted::{final_turn, tool_turn, ScriptedCompleter};
use super::{AgentRunLedger, ApiAgentDriver, SpendLimit, SpendLimitReached, SpendStopCause};

fn role() -> Role {
    Role {
        name: "implementer".to_string(),
        rule_subset: vec![],
        allowed_paths: vec![],
    }
}

fn worktree() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let wt = tmp.path().join("wt");
    std::fs::create_dir_all(&wt).unwrap();
    std::fs::write(wt.join("notes.md"), "synthetic\n").unwrap();
    (tmp, wt)
}

/// A model that never stops asking to read a file: only the limit (or the turn cap) ends it.
fn endless_reads() -> Vec<serde_json::Value> {
    vec![tool_turn("Read", &[json!({ "file_path": "notes.md" })])]
}

fn limited(usd: f64) -> Arc<AgentRunLedger> {
    Arc::new(AgentRunLedger::new().with_spend_limit(SpendLimit::usd(usd).expect("a valid limit")))
}

#[tokio::test]
async fn a_scripted_run_that_would_exceed_the_limit_stops_at_the_limit_with_a_clear_reason() {
    let (_tmp, wt) = worktree();
    let completer = Arc::new(ScriptedCompleter::new(endless_reads(), Some(0.40)));
    let ledger = limited(1.00);
    let driver =
        ApiAgentDriver::new(completer.clone(), "scripted", wt).with_run_ledger(ledger.clone());

    let result = driver.run(&role(), "read forever").await;

    assert!(
        result.is_err(),
        "a run past its spend limit must stop with an error, got: {:?}",
        result.map(|o| o.result)
    );
    let err = result.unwrap_err();
    let reached = err
        .downcast_ref::<SpendLimitReached>()
        .expect("the error is the spend limit");
    assert_eq!(
        completer.calls(),
        2,
        "$0.40 turns under a $1.00 limit: the third turn would pass it and is never sent"
    );
    assert!(
        (ledger.spent_usd() - 0.80).abs() < 1e-9,
        "{}",
        ledger.spent_usd()
    );
    assert!(ledger.spent_usd() <= 1.00, "the run never passed its limit");
    assert_eq!(
        reached.cause,
        SpendStopCause::NextTurnWouldExceed { reserve_usd: 0.40 }
    );
    let msg = err.to_string();
    assert!(msg.contains("spend limit reached"), "{msg}");
    assert!(msg.contains("$1.00"), "the limit is named: {msg}");
}

#[tokio::test]
async fn cost_is_recorded_per_turn() {
    let (_tmp, wt) = worktree();
    let completer = Arc::new(ScriptedCompleter::new(
        vec![
            tool_turn("Read", &[json!({ "file_path": "notes.md" })]),
            tool_turn("LS", &[json!({})]),
            final_turn("done"),
        ],
        Some(0.25),
    ));
    let ledger = Arc::new(AgentRunLedger::new());
    let driver = ApiAgentDriver::new(completer, "scripted", wt).with_run_ledger(ledger.clone());

    let out = driver.run(&role(), "three turns").await.expect("run");

    let turns = ledger.turns();
    assert_eq!(turns.len(), 3, "one record per provider turn: {turns:?}");
    assert_eq!(turns.iter().map(|t| t.turn).collect::<Vec<_>>(), [1, 2, 3]);
    assert!(turns
        .iter()
        .all(|t| t.cost_usd == Some(0.25) && t.agent == "implementer" && t.model == "scripted"));
    assert_eq!(
        out.cost_usd,
        Some(0.75),
        "the outcome carries the run's total"
    );
    assert!((ledger.spent_usd() - 0.75).abs() < 1e-9);
}

#[tokio::test]
async fn a_turn_that_cannot_be_priced_stops_a_limited_run() {
    let (_tmp, wt) = worktree();
    // No dollar figure and no token counts: the registry has nothing to price.
    let completer = Arc::new(ScriptedCompleter::new(endless_reads(), None));
    let driver =
        ApiAgentDriver::new(completer.clone(), "unknown-model", wt).with_run_ledger(limited(5.00));

    let result = driver.run(&role(), "read forever").await;

    assert!(
        result.is_err(),
        "an unpriceable turn under a limit stops the run"
    );
    assert_eq!(
        completer.calls(),
        1,
        "nothing is sent after the unpriced turn"
    );
    let err = result.unwrap_err();
    let reached = err
        .downcast_ref::<SpendLimitReached>()
        .expect("spend limit");
    assert_eq!(
        reached.cause,
        SpendStopCause::Unpriced {
            model: "unknown-model".to_string()
        }
    );
}

#[tokio::test]
async fn a_run_with_no_limit_is_not_stopped_by_spend() {
    let (_tmp, wt) = worktree();
    let completer = Arc::new(ScriptedCompleter::new(
        vec![
            tool_turn("Read", &[json!({ "file_path": "notes.md" })]),
            final_turn("done"),
        ],
        Some(50.0),
    ));
    let driver = ApiAgentDriver::new(completer, "scripted", wt);
    let out = driver
        .run(&role(), "two turns")
        .await
        .expect("no limit, no stop");
    assert_eq!(out.result, "done");
}

#[test]
fn a_spend_limit_is_positive_and_finite() {
    assert!(SpendLimit::usd(0.0).is_none());
    assert!(SpendLimit::usd(-1.0).is_none());
    assert!(SpendLimit::usd(f64::NAN).is_none());
    assert!(SpendLimit::usd(f64::INFINITY).is_none());
    assert!(SpendLimit::from_cents(0).is_none());
    assert_eq!(
        SpendLimit::from_cents(2_500).map(SpendLimit::as_usd),
        Some(25.0)
    );
}

/// The Anthropic Messages API reports `input_tokens` WITHOUT the cache reads and writes; the
/// turn must be priced on all of them, each at its own rate.
#[test]
fn an_anthropic_turn_is_priced_from_the_registry_with_cache_reads_and_writes() {
    let model = "claude-sonnet-5";
    let rates = crate::model_registry::rates_for(model, 3_300).expect("a priced catalog model");
    let v = json!({
        "model": model,
        "content": [{ "type": "text", "text": "done" }],
        "stop_reason": "end_turn",
        "usage": {
            "input_tokens": 1_000,
            "output_tokens": 500,
            "cache_read_input_tokens": 2_000,
            "cache_creation_input_tokens": 300
        }
    });
    let resp = super::anthropic_turn_response(&v, model);
    assert!(resp.cost_usd.is_some(), "the turn is priced: {resp:?}");
    let expected = 1_000.0 * rates.input
        + 2_000.0 * rates.cache_read
        + 300.0 * rates.cache_write_5m
        + 500.0 * rates.output;
    let cost = resp.cost_usd.expect("the turn is priced");
    assert!((cost - expected).abs() < 1e-12, "cost {cost} != {expected}");
    assert_eq!(
        resp.input_tokens,
        Some(3_300),
        "input folds in the cache reads and writes, as every other transport reports it"
    );
}

/// A one-hour cache write is billed at the one-hour rate when the API splits it out.
#[test]
fn an_anthropic_turns_one_hour_cache_writes_take_the_one_hour_rate() {
    let model = "claude-sonnet-5";
    let rates = crate::model_registry::rates_for(model, 1_400).expect("priced");
    let v = json!({
        "content": [],
        "usage": {
            "input_tokens": 100, "output_tokens": 10,
            "cache_read_input_tokens": 0, "cache_creation_input_tokens": 1_300,
            "cache_creation": { "ephemeral_1h_input_tokens": 1_000, "ephemeral_5m_input_tokens": 300 }
        }
    });
    let resp = super::anthropic_turn_response(&v, model);
    assert!(resp.cost_usd.is_some(), "the turn is priced: {resp:?}");
    let cost = resp.cost_usd.expect("priced");
    let expected = 100.0 * rates.input
        + 1_000.0 * rates.cache_write_1h
        + 300.0 * rates.cache_write_5m
        + 10.0 * rates.output;
    assert!((cost - expected).abs() < 1e-12, "cost {cost} != {expected}");
}

/// A completer that reports tokens but no dollar figure is priced from the registry.
#[test]
fn a_turn_reporting_only_tokens_is_priced_from_the_registry() {
    let model = "claude-sonnet-5";
    let rates = crate::model_registry::rates_for(model, 1_000).expect("priced");
    let resp = crate::llm::LlmResponse {
        reported_model: None,
        cli_cost_usd: None,
        text: String::new(),
        model: model.to_string(),
        backend: "stub".to_string(),
        cost_usd: None,
        input_tokens: Some(1_000),
        output_tokens: Some(100),
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
        or_cache_discount: None,
        truncated: false,
    };
    let priced = super::price_turn(model, &resp);
    assert!(
        priced.is_some(),
        "a turn with token counts is priced from the registry"
    );
    let cost = priced.expect("priced from tokens");
    assert!((cost - (1_000.0 * rates.input + 100.0 * rates.output)).abs() < 1e-12);
}

fn openrouter_entry(
    id: &str,
    price_in: f64,
    price_out: f64,
    known: bool,
) -> crate::model_registry::RegistryEntry {
    crate::model_registry::RegistryEntry {
        id: id.to_string(),
        provider: "openrouter".to_string(),
        display: "Synthetic OpenRouter model".to_string(),
        free: false,
        tool_use: true,
        context: 64_000,
        coding: 0.5,
        price_in,
        price_out,
        weight: 0,
        caching: false,
        vision: false,
        price_known: known,
    }
}

fn openrouter_reply() -> serde_json::Value {
    json!({
        "model": "vendor/synthetic-coder",
        "choices": [{ "message": { "role": "assistant", "content": "done" } }],
        "usage": { "prompt_tokens": 10_000, "completion_tokens": 2_000 }
    })
}

/// An OpenRouter turn is priced at its registry entry's list prices (USD per million tokens);
/// every prompt token is charged at the input price, since a cache discount is not reported.
#[test]
fn an_openrouter_turn_is_priced_from_the_registry_entry() {
    let registry = crate::model_registry::ModelRegistry::new();
    registry.seed_openrouter_entries(vec![openrouter_entry(
        "vendor/synthetic-coder",
        0.5,
        2.0,
        true,
    )]);
    let resp = super::openrouter_turn_response(
        &openrouter_reply(),
        "vendor/synthetic-coder",
        "s",
        &registry,
    );
    assert_eq!(
        resp.cost_usd,
        Some(10_000.0 * 0.5 / 1e6 + 2_000.0 * 2.0 / 1e6),
        "{resp:?}"
    );
}

/// An entry whose price is not known prices nothing (a limited run then stops: unknown cost).
#[test]
fn an_openrouter_turn_with_an_unknown_price_is_unpriced() {
    let registry = crate::model_registry::ModelRegistry::new();
    registry.seed_openrouter_entries(vec![openrouter_entry(
        "vendor/synthetic-coder",
        0.0,
        0.0,
        false,
    )]);
    let resp = super::openrouter_turn_response(
        &openrouter_reply(),
        "vendor/synthetic-coder",
        "s",
        &registry,
    );
    assert_eq!(resp.cost_usd, None);
    let empty = crate::model_registry::ModelRegistry::new();
    let resp =
        super::openrouter_turn_response(&openrouter_reply(), "vendor/synthetic-coder", "s", &empty);
    assert_eq!(
        resp.cost_usd, None,
        "a model the registry does not list is unpriced"
    );
}
