//! A scripted `LlmPort` for driving the agent tool loop in tests without a model: it answers
//! call `n` with turn `n` of its script (the last turn repeats once the script runs out), each
//! priced at a fixed figure. No network, no cost.

use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{json, Value};

use crate::llm::{LlmPort, LlmRequest, LlmResponse};

pub(super) struct ScriptedCompleter {
    turns: Vec<Value>,
    cost_per_turn: Option<f64>,
    calls: AtomicUsize,
}

impl ScriptedCompleter {
    pub(super) fn new(turns: Vec<Value>, cost_per_turn: Option<f64>) -> Self {
        assert!(!turns.is_empty(), "a script needs at least one turn");
        Self {
            turns,
            cost_per_turn,
            calls: AtomicUsize::new(0),
        }
    }

    /// How many provider calls the loop made.
    pub(super) fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

/// One assistant turn (OpenAI shape) calling `tool` once per input, ids `call-<turn>-<i>`.
pub(super) fn tool_turn(tool: &str, inputs: &[Value]) -> Value {
    let calls: Vec<Value> = inputs
        .iter()
        .enumerate()
        .map(|(i, input)| {
            json!({
                "id": format!("call-{tool}-{i}"),
                "type": "function",
                "function": { "name": tool, "arguments": input.to_string() }
            })
        })
        .collect();
    json!({ "message": { "role": "assistant", "content": null, "tool_calls": calls } })
}

/// A final assistant turn (no tool calls).
pub(super) fn final_turn(text: &str) -> Value {
    json!({ "message": { "role": "assistant", "content": text } })
}

#[async_trait::async_trait]
impl LlmPort for ScriptedCompleter {
    async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let turn = &self.turns[n.min(self.turns.len() - 1)];
        Ok(LlmResponse {
            reported_model: None,
            cli_cost_usd: None,
            text: turn.to_string(),
            model: "scripted".to_string(),
            backend: "scripted".to_string(),
            cost_usd: self.cost_per_turn,
            input_tokens: None,
            output_tokens: None,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
            truncated: false,
        })
    }
    async fn complete_streaming(
        &self,
        req: LlmRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        let r = self.complete(req).await?;
        on_delta(&r.text);
        Ok(r)
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
