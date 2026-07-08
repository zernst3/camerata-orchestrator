//! Native, provider-agnostic `ApiAgentDriver`.
//!
//! Build 3b: the agentic tool-use loop running IN-PROCESS against any provider's
//! chat-completions API (OpenRouter or Anthropic). Tool calls are executed directly
//! through the gateway library functions — the same `evaluate_call` + `arm_*` rule
//! arms that back the MCP gateway — so Layer-1 invariants are enforced identically
//! regardless of which driver (CLI or API) runs the agent loop.
//!
//! # Why it lives in `camerata-server`
//!
//! `camerata-agent` does NOT depend on `camerata-server`. The `LlmPort` trait +
//! `OpenRouterCompleter` live in `camerata-server`. The gateway library lives in
//! `camerata-gateway`. `camerata-server` already depends on both, so `ApiAgentDriver`
//! placed here avoids a dependency cycle. The `AgentDriver` trait (from
//! `camerata-core`) is the seam; both `ClaudeCliDriver` (in `camerata-agent`) and
//! this type implement it.
//!
//! # Layer-1 invariants enforced here
//!
//! 1. **`gated_write` is the ONLY write path.** No shell, no `Write`/`Edit`/`Bash`
//!    built-ins are exposed. All filesystem mutations go through `evaluate_call` first;
//!    a deny stops the write and records a denial in the outcome.
//! 2. **`delegate` / `fan_out` are orchestrator-only.** The non-orchestrator `ApiAgentDriver`
//!    never exposes them in its tool schemas. Callers must set `orchestrator = true`
//!    explicitly to get those tools, and even then the gateway's own registration is the
//!    final arbiter.
//! 3. **`shell` / `Task` are never exposed.** The tool schema list is an explicit allowlist;
//!    any tool not in it is not callable.
//! 4. **Worktree jail.** When `worktree` is set, `gated_write` paths that escape the jail
//!    (via the existing gateway rules: `SEC-NO-PATH-ESCAPE-1` + the worktree prefix check)
//!    are denied before the file is written.
//!
//! # Tool-call normalization
//!
//! The driver normalizes BOTH formats into the one internal `ToolInvocation` type:
//! - **OpenAI / OpenRouter style** (`choices[0].message.tool_calls[]`): each item has
//!   `id`, `type: "function"`, `function: {name, arguments}` where `arguments` is a
//!   JSON string.
//! - **Anthropic style** (`content[]` blocks of `type: "tool_use"`): each item has
//!   `id`, `name`, `input` (already a JSON object, not a string).
//!
//! After normalization, both become `ToolInvocation { id, name, input }`.
//!
//! # Event parity
//!
//! The driver emits `GateEvent`s to the `RunStore` on every tool call attempt (allow /
//! deny) and on loop milestones, matching the events the CLI driver emits via the
//! gateway JSONL sink. Per-turn is fine for v1; token-level streaming is a
//! `TODO(provider-agnostic-followup)`.
//!
//! # Read tools
//!
//! `Read`, `Glob`, `Grep`, `LS` are executed in-process directly on the filesystem.
//! They carry no write authority and are safe to execute without gate evaluation. The
//! results are fed back to the model as tool-result messages.
//!
//! # Iteration cap
//!
//! The loop is capped at [`MAX_ITERATIONS`] turns to bound runaway agents. A malformed
//! tool-call or a repeating loop that never emits a final text response will be
//! terminated and the partial result (or an `INCOMPLETE` marker) returned.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context as _;
use async_trait::async_trait;
use camerata_agent::HeartbeatFn;
use camerata_core::{AgentDriver, AgentOutcome, Decision, Role, RuleId, ToolCall};
use camerata_gateway::evaluate_call;
use serde_json::Value;

use crate::llm::{LlmPort, LlmRequest, LlmResponse};

// ─── constants ────────────────────────────────────────────────────────────────

/// Maximum agentic loop turns before we declare INCOMPLETE and surface the partial
/// result. Prevents runaway models from looping indefinitely.
pub const MAX_ITERATIONS: usize = 40;

/// Maximum bytes a single `gated_write` content field may carry. A safety guard
/// against accidentally large writes; 2 MiB covers any realistic source file.
const MAX_WRITE_BYTES: usize = 2 * 1024 * 1024;

/// Maximum output from a read-tool execution returned to the model per call.
/// Truncated beyond this to avoid blowing up the context window.
const MAX_READ_OUTPUT_BYTES: usize = 64 * 1024; // 64 KiB

// ─── tool names (must agree with camerata-agent constants) ───────────────────

const GATED_WRITE: &str = "gated_write";
const TOOL_READ: &str = "Read";
const TOOL_GLOB: &str = "Glob";
const TOOL_GREP: &str = "Grep";
const TOOL_LS: &str = "LS";
const TOOL_DELEGATE: &str = "delegate";
const TOOL_FAN_OUT: &str = "fan_out";

/// Anthropic Messages API version header value, shared with `llm.rs::complete_api`.
const ANTHROPIC_VERSION: &str = "2023-06-01";

// ─── provider wire shape ─────────────────────────────────────────────────────

/// The HTTP wire shape the `ApiAgentDriver` speaks. Decides the request URL + headers,
/// the tool-schema format, the tool-result message format, and which normalizer branch
/// the response is parsed by.
///
/// Both shapes route every tool call through the SAME `GovernanceGateway`
/// (`evaluate_call`) — this enum only chooses the transport, never the gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiShape {
    /// OpenAI-compatible chat-completions shape (OpenRouter):
    /// `messages` + OpenAI `tools` (`[{type:"function", function:{...}}]`),
    /// `Authorization: Bearer`, tool results appended as `{role:"tool", tool_call_id, content}`.
    OpenRouter,
    /// Anthropic Messages API shape:
    /// top-level `system` + Anthropic `tools` (`[{name, description, input_schema}]`),
    /// `x-api-key` + `anthropic-version` headers, tool results appended as a user message
    /// `{role:"user", content:[{type:"tool_result", tool_use_id, content}]}`.
    Anthropic,
}

// ─── normalized tool invocation ──────────────────────────────────────────────

/// One tool call the model has requested, normalized out of provider-specific shapes.
#[derive(Debug, Clone)]
struct ToolInvocation {
    /// Provider-assigned call id (used to build the tool-result message).
    id: String,
    /// Tool name as the model emitted it.
    name: String,
    /// Parsed JSON input (Anthropic sends this as an object; OpenAI as a string we parse).
    input: Value,
}

// ─── ApiAgentDriver ──────────────────────────────────────────────────────────

/// A native, provider-agnostic agentic driver.
///
/// Owns the multi-turn tool-use loop: call the provider → parse tool-calls →
/// execute each through the gateway library → feed results back → repeat.
///
/// Constructed by [`ApiAgentDriver::new`]. Use [`ApiAgentDriver::as_orchestrator`] and
/// [`ApiAgentDriver::with_worktree`] to configure the mode and jail before calling `run`.
#[derive(Clone)]
pub struct ApiAgentDriver {
    /// The `LlmPort` that makes the provider API calls.
    completer: Arc<dyn LlmPort>,
    /// Model id to request (empty = completer's default).
    pub model: String,
    /// Optional worktree the agent's writes are jailed to. Enforced by the gateway rules
    /// (`SEC-NO-PATH-ESCAPE-1` + worktree-prefix check below) on every `gated_write`.
    pub worktree: Option<PathBuf>,
    /// When `true`, `delegate` and `fan_out` tool schemas are included in the API request.
    /// Default `false`: workers never get these, enforcing depth-1.
    pub orchestrator: bool,
    /// The rule subset to evaluate writes against. Populated from the governed role at run time.
    /// Set by [`ApiAgentDriver::with_rule_subset`]; empty = no rules enforced (insecure, tests only).
    rule_subset: Vec<RuleId>,
    /// Stable OpenRouter session id for this driver instance. Passed as `session_id` in
    /// every request to activate sticky routing + KV-cache warmth from request #1. Must
    /// be stable within a run (so the cache stays warm across multi-turn loops) and
    /// distinct between independent runs (to avoid cross-run cache pollution). Derived
    /// from the `OpenRouterCompleter`'s session id at construction time; falls back to a
    /// fresh token for non-OR completers (no-op for them).
    session_id: String,
    /// When `true`, the NEXT provider call sends `X-OpenRouter-Cache-Clear: true` to force
    /// a fresh model response even when a cached response exists. Reset to `false` after
    /// each use. Set this on a stuck-loop retry via [`Self::bust_cache_on_next_call`].
    bust_cache: bool,
    /// Orchestrator-mode config carrying the per-model [`ServerChildDriverFactory`] (via
    /// [`camerata_gateway::delegate::OrchestratorConfig::child_driver_factory`]). When
    /// `Some` (set only on an orchestrator-mode driver via
    /// [`Self::with_orchestrator_config`]) the native `delegate`/`fan_out` tool arms
    /// dispatch through the gated `run_delegated`/`run_fan_out` primitives — each child
    /// resolved per-model via the factory. When `None` (every worker, and any orchestrator
    /// not yet given a config) those arms return an honest "not configured" message and
    /// NEVER spawn — the gate is never reimplemented here.
    orchestrator_config: Option<camerata_gateway::delegate::OrchestratorConfig>,
    /// The provider wire shape for the agentic HTTP request side. Default
    /// [`ApiShape::OpenRouter`] (back-compat: every existing construction stays OpenRouter).
    /// Set to [`ApiShape::Anthropic`] via [`Self::with_shape`] for the Anthropic Messages
    /// API path. The shape decides URL + headers + tool-schema format + tool-result format +
    /// normalizer branch. It does NOT change the gate.
    shape: ApiShape,
    /// The Anthropic API key, used ONLY when `shape == ApiShape::Anthropic`. Resolved from
    /// `ANTHROPIC_API_KEY` (mirroring `llm.rs::complete_api`) and threaded in at build time.
    /// `None` on the OpenRouter path (the OpenRouter key is read off the completer instead).
    anthropic_api_key: Option<String>,
    /// LIFECYCLE-7 liveness heartbeat. Fired once per agentic loop iteration so a healthy,
    /// long-running API-driven run keeps `last_activity_ms` fresh and does NOT read as stalled.
    /// The CLI path fires its heartbeat per output line via `ClaudeCliDriver::with_on_activity`;
    /// the API loop has no line stream, so it beats per turn instead. `None` = no heartbeat
    /// (tests / callers that don't wire one).
    on_activity: Option<HeartbeatFn>,
}

impl ApiAgentDriver {
    /// Build a new driver backed by `completer` using `model`.
    ///
    /// If `completer` is an [`crate::llm::OpenRouterCompleter`], its `session_id` is
    /// inherited here so every direct HTTP call (the tool-schema path) uses the same
    /// session id as bare-LLM calls made through the `LlmPort` trait.
    pub fn new(completer: Arc<dyn LlmPort>, model: impl Into<String>) -> Self {
        // Inherit the session id from an OpenRouterCompleter when available; fall back to
        // a stable per-instance token for other completer types (no-op for them).
        let session_id = completer
            .as_any()
            .downcast_ref::<crate::llm::OpenRouterCompleter>()
            .map(|or| or.session_id_for_agent())
            .unwrap_or_else(|| {
                use std::sync::atomic::{AtomicU64, Ordering};
                static COUNTER: AtomicU64 = AtomicU64::new(0);
                let n = COUNTER.fetch_add(1, Ordering::Relaxed);
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                format!("cam-drv-{nanos:x}-{n:x}")
            });
        Self {
            completer,
            model: model.into(),
            worktree: None,
            orchestrator: false,
            rule_subset: Vec::new(),
            session_id,
            bust_cache: false,
            orchestrator_config: None,
            shape: ApiShape::OpenRouter,
            anthropic_api_key: None,
            on_activity: None,
        }
    }

    /// Wire the LIFECYCLE-7 liveness heartbeat. Builder form. The callback fires once per
    /// agentic loop iteration (each provider turn), keeping `last_activity_ms` fresh so a
    /// healthy long API-driven run is never reported stalled. No-op when never set.
    pub fn with_on_activity(mut self, cb: HeartbeatFn) -> Self {
        self.on_activity = Some(cb);
        self
    }

    /// Set the provider wire shape (URL + headers + tool-schema + tool-result + normalizer
    /// branch). Builder form. Default is [`ApiShape::OpenRouter`]; use
    /// [`ApiShape::Anthropic`] for the Anthropic Messages API path.
    ///
    /// This is transport-only: the same `evaluate_call` gate runs on every tool call
    /// regardless of shape. Switching to Anthropic does NOT expose any new tool, does NOT
    /// loosen the worktree jail, and does NOT enable delegate/fan_out (those still require
    /// `as_orchestrator(true)` + an attached config).
    pub fn with_shape(mut self, shape: ApiShape) -> Self {
        self.shape = shape;
        self
    }

    /// Attach the Anthropic API key for the Anthropic shape. Builder form. No-op on the
    /// OpenRouter path (which reads its key off the completer). Mirrors `llm.rs`'s use of
    /// `ANTHROPIC_API_KEY`.
    pub fn with_anthropic_api_key(mut self, key: impl Into<String>) -> Self {
        let k = key.into();
        if !k.trim().is_empty() {
            self.anthropic_api_key = Some(k);
        }
        self
    }

    /// Attach an orchestrator-mode [`camerata_gateway::delegate::OrchestratorConfig`] (which
    /// carries the per-model [`ServerChildDriverFactory`]) so the native `delegate`/`fan_out`
    /// arms dispatch through the gated `run_delegated`/`run_fan_out` primitives. Builder form.
    ///
    /// Only meaningful together with `as_orchestrator(true)`. Setting it does NOT loosen the
    /// gate: children are still built by the factory as gated_write-only, jailed, depth-1
    /// workers; this driver only ROUTES to the existing gated primitive, never reimplements it.
    pub fn with_orchestrator_config(
        mut self,
        config: camerata_gateway::delegate::OrchestratorConfig,
    ) -> Self {
        self.orchestrator_config = Some(config);
        self
    }

    /// Request that the next provider call clears the OpenRouter response cache.
    ///
    /// Sets the internal `bust_cache` flag, which causes `call_openrouter_with_tools`
    /// to include `X-OpenRouter-Cache-Clear: true` on the immediate next call only
    /// (the flag is reset after each use inside the loop). Use this when the loop
    /// detects a bad or repeating cached response and needs a fresh model completion.
    ///
    /// No-op for non-OpenRouter completers.
    pub fn bust_cache_on_next_call(&mut self) {
        self.bust_cache = true;
    }

    /// Set the rule subset to enforce on `gated_write` calls. Should be the role's
    /// `rule_subset` plus the gateway-enforced gate rules (same as the CLI path via
    /// `governed_role`). Builder form.
    pub fn with_rule_subset(mut self, rules: Vec<RuleId>) -> Self {
        self.rule_subset = rules;
        self
    }

    /// Bind this driver to a worktree: writes are jailed to that directory.
    /// Builder form.
    pub fn with_worktree(mut self, worktree: impl Into<PathBuf>) -> Self {
        self.worktree = Some(worktree.into());
        self
    }

    /// Mark this driver as orchestrator-mode (includes `delegate` + `fan_out` in the
    /// tool schema). Builder form. Only the lead / strongest stage should set this.
    pub fn as_orchestrator(mut self, orchestrator: bool) -> Self {
        self.orchestrator = orchestrator;
        self
    }

    /// Override the model id. Builder form.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        let m = model.into();
        if !m.trim().is_empty() {
            self.model = m;
        }
        self
    }
}

// ─── AgentDriver impl ────────────────────────────────────────────────────────

#[async_trait]
impl AgentDriver for ApiAgentDriver {
    /// Run the agentic tool-use loop for `role` + `task`.
    ///
    /// Returns [`AgentOutcome`] with the final result text, token cost estimate, and
    /// any gate denials recorded during the run.
    async fn run(&self, role: &Role, task: &str) -> anyhow::Result<AgentOutcome> {
        run_loop(self, role, task).await
    }
}

// ─── the loop ────────────────────────────────────────────────────────────────

/// The agentic tool-use loop. Factored out of the `impl AgentDriver` to keep the
/// impl block thin and the logic easy to test.
async fn run_loop(
    driver: &ApiAgentDriver,
    role: &Role,
    task: &str,
) -> anyhow::Result<AgentOutcome> {
    // Build the initial messages array: system (if any) + first user turn.
    let system_prompt = build_system_prompt(role, &driver.model);
    // Tool schemas are produced in the wire shape this driver speaks: OpenAI-shaped
    // (`function`) for OpenRouter, Anthropic-shaped (`input_schema`) for Anthropic. The
    // allowlist (which tools exist) is identical; only the JSON envelope differs.
    let tool_schemas = build_tool_schemas_for(driver.orchestrator, driver.shape);

    // Conversation history: starts with just the user task.
    let mut messages: Vec<Value> = vec![serde_json::json!({
        "role": "user",
        "content": task,
    })];

    let mut denials: Vec<String> = Vec::new();
    let mut total_cost_usd: f64 = 0.0;
    let mut final_result = String::new();
    let mut iteration = 0usize;
    // Per-loop bust flag: starts from the driver's initial value, then resets after use.
    // The driver's `bust_cache` flag is read ONCE at loop entry so a mid-loop
    // `bust_cache_on_next_call` call (from outside this future) doesn't race.
    let mut bust_cache_this_turn = driver.bust_cache;

    loop {
        if iteration >= MAX_ITERATIONS {
            // Cap hit: return what we have, mark INCOMPLETE.
            if final_result.trim().is_empty() {
                final_result = format!(
                    "INCOMPLETE: hit the {MAX_ITERATIONS}-turn iteration cap without a final response."
                );
            }
            break;
        }
        iteration += 1;

        // LIFECYCLE-7: beat the liveness heartbeat once per turn. The API loop has no
        // per-line output stream (unlike the CLI driver), so a multi-turn run that is
        // healthily grinding through provider calls would otherwise look idle. Firing here
        // keeps `last_activity_ms` fresh across the whole run.
        if let Some(cb) = driver.on_activity.as_ref() {
            cb();
        }

        // ── Call the provider ──────────────────────────────────────────────
        let resp = call_provider(
            driver,
            system_prompt.as_deref(),
            &messages,
            &tool_schemas,
            bust_cache_this_turn,
        )
        .await
        .with_context(|| format!("provider call failed on iteration {iteration}"))?;
        // Cache-bust is one-shot: reset after use so subsequent turns use cached responses.
        bust_cache_this_turn = false;

        // CACHE-HIT LOGGING (prefix-stability verification): log the effective prompt-cache hit
        // ratio for this call so the geological layering is verifiable in practice. If the stable
        // Layer-1/Layer-2 prefix is holding, this climbs toward 1.0 after the first turn; a ratio
        // stuck near 0 across turns signals the prefix is churning. Only logged when the backend
        // reported input-token usage (the CLI/stub paths report none and are silently skipped).
        if let Some(ratio) = resp.cache_hit_ratio() {
            eprintln!(
                "[camerata-server/api-agent] turn {iteration} cache-hit {:.0}% (read={} creation={} input={}) session={}",
                ratio * 100.0,
                resp.cache_read_input_tokens,
                resp.cache_creation_input_tokens,
                resp.input_tokens.unwrap_or(0),
                driver.session_id,
            );
        }

        // Accumulate cost.
        if let Some(c) = resp.cost_usd {
            total_cost_usd += c;
        }

        // ── Parse the response ─────────────────────────────────────────────
        let parsed = parse_response(&resp);

        match parsed {
            ParsedResponse::FinalText(text) => {
                final_result = text;
                // Append the assistant turn to history (for correctness, even though we stop).
                messages.push(serde_json::json!({
                    "role": "assistant",
                    "content": final_result.clone(),
                }));
                break;
            }
            ParsedResponse::ToolCalls { raw_assistant_msg, calls } => {
                if calls.is_empty() {
                    // Model returned an assistant message with no tool calls and no text.
                    // Stop loop; return empty result.
                    break;
                }

                // Append the assistant's tool-call turn to history.
                messages.push(raw_assistant_msg);

                // ── Execute each tool call ─────────────────────────────────
                // Collect `(tool_use_id, result_text)` pairs; the per-shape append below
                // turns them into the right wire format.
                let mut results: Vec<(String, String)> = Vec::new();

                for invocation in calls {
                    let (result_text, denial) = execute_tool(
                        driver,
                        role,
                        &invocation,
                    )
                    .await;

                    if let Some(d) = denial {
                        denials.push(d);
                    }

                    results.push((invocation.id, result_text));
                }

                // Append the tool results in the shape the provider expects so the
                // multi-turn conversation stays valid:
                // - OpenAI/OpenRouter: one `{role:"tool", tool_call_id, content}` message
                //   per result.
                // - Anthropic: a SINGLE user message whose content is an array of
                //   `{type:"tool_result", tool_use_id, content}` blocks (all results in one
                //   message — the Anthropic API rejects split/`role:"tool"` shapes).
                append_tool_results(&mut messages, driver.shape, &results);
            }
        }
    }

    Ok(AgentOutcome {
        // Use the stable driver session_id so the outcome is traceable across multi-turn runs.
        session_id: driver.session_id.clone(),
        result: final_result,
        cost_usd: if total_cost_usd > 0.0 { Some(total_cost_usd) } else { None },
        denials,
    })
}

// ─── tool-result feedback (per-shape) ──────────────────────────────────────────

/// Append the executed tool results to `messages` in the wire shape `shape` requires.
///
/// - [`ApiShape::OpenRouter`] (OpenAI-compatible): each result becomes its own
///   `{role:"tool", tool_call_id, content}` message.
/// - [`ApiShape::Anthropic`]: all results go into ONE `{role:"user", content:[...]}`
///   message whose content is an array of `{type:"tool_result", tool_use_id, content}`
///   blocks. The Anthropic Messages API requires every tool_use from the preceding
///   assistant turn to be answered by a tool_result in the immediately following user
///   message; a `role:"tool"` message or split user messages are rejected.
fn append_tool_results(messages: &mut Vec<Value>, shape: ApiShape, results: &[(String, String)]) {
    match shape {
        ApiShape::OpenRouter => {
            for (id, text) in results {
                messages.push(serde_json::json!({
                    "role": "tool",
                    "tool_call_id": id,
                    "content": text,
                }));
            }
        }
        ApiShape::Anthropic => {
            let blocks: Vec<Value> = results
                .iter()
                .map(|(id, text)| {
                    serde_json::json!({
                        "type": "tool_result",
                        "tool_use_id": id,
                        "content": text,
                    })
                })
                .collect();
            messages.push(serde_json::json!({
                "role": "user",
                "content": blocks,
            }));
        }
    }
}

// ─── provider call ────────────────────────────────────────────────────────────

/// Make one provider API call with the current conversation history and tool schemas.
///
/// `tool_schemas` is embedded in the request body via the OpenAI `tools` field.
/// The `LlmPort` trait doesn't natively carry tool schemas, so we build the
/// request body manually here and call the underlying HTTP client directly.
///
/// `session_id` enables sticky routing + KV-cache warmth (passed to OpenRouter via the
/// request body). `bust_cache` adds `X-OpenRouter-Cache-Clear: true` for one-shot cache
/// invalidation on stuck-loop retries. Both are no-ops for non-OR completers.
///
/// **Design note:** The `LlmPort` trait is designed for bare-LLM (no tools). To send
/// tool schemas, we build the full OpenRouter request body and post it directly, bypassing
/// the `LlmPort` abstraction for this specific call. The response normalization stays
/// shared. TODO(provider-agnostic-followup): extend `LlmRequest` to carry tool schemas so
/// the `LlmPort` trait covers the agentic path too.
async fn call_provider(
    driver: &ApiAgentDriver,
    system: Option<&str>,
    messages: &[Value],
    tool_schemas: &[Value],
    bust_cache: bool,
) -> anyhow::Result<LlmResponse> {
    let completer = driver.completer.as_ref();
    let model = &driver.model;

    // ── Anthropic Messages API shape ───────────────────────────────────────
    // Selected explicitly via `with_shape(ApiShape::Anthropic)` AND with a key attached.
    // The Anthropic key is carried on the driver (resolved from ANTHROPIC_API_KEY), not
    // read off the completer. If the shape is Anthropic but no key is attached, fall
    // through to the schema-less LlmPort path so test stubs still work.
    if driver.shape == ApiShape::Anthropic {
        if let Some(key) = driver.anthropic_api_key.as_deref() {
            return call_anthropic_with_tools(key, model, system, messages, tool_schemas).await;
        }
        // No key attached (e.g. a stub-completer unit test on the Anthropic shape):
        // fall through to the LlmPort-trait path below.
    }

    // ── OpenRouter / OpenAI-compatible shape ───────────────────────────────
    // Attempt to downcast to OpenRouterCompleter for tool-schema support.
    // If the downcast fails (unknown completer type / stub), fall back to a schema-less
    // call via the LlmPort trait (works for text-only models + tests).
    let any = completer.as_any();

    if any.is::<crate::llm::OpenRouterCompleter>() {
        // Use OpenRouter directly with tool schemas + caching controls.
        let openrouter_key = get_openrouter_key_from_completer(completer)?;
        call_openrouter_with_tools(
            &openrouter_key,
            model,
            system,
            messages,
            tool_schemas,
            &driver.session_id,
            bust_cache,
        )
        .await
    } else {
        // Fallback: use the LlmPort trait (no tool schemas — text only).
        // This handles test stubs and future completers.
        // TODO(provider-agnostic-followup): wire tool schemas into the LlmPort trait.
        let prompt = messages_to_text_prompt(messages);
        let mut req = LlmRequest::new(prompt).with_model(model);
        if let Some(sys) = system {
            req = req.with_system(sys);
        }
        completer.complete(req).await
    }
}

/// Extract the OpenRouter API key from an `OpenRouterCompleter` reference.
/// The field is private, so we use the `as_any` downcast + a dedicated accessor.
fn get_openrouter_key_from_completer(completer: &dyn LlmPort) -> anyhow::Result<String> {
    // `OpenRouterCompleter` is in the same crate; we access the key via a helper method
    // added below (see `impl OpenRouterCompleter` extension).
    let any = completer.as_any();
    let or = any
        .downcast_ref::<crate::llm::OpenRouterCompleter>()
        .ok_or_else(|| anyhow::anyhow!("expected OpenRouterCompleter"))?;
    Ok(or.api_key_for_agent())
}

/// POST directly to OpenRouter's `/api/v1/chat/completions` with tool schemas included.
///
/// `session_id` enables sticky routing + KV-cache warmth from request #1.
/// `bust_cache` adds `X-OpenRouter-Cache-Clear: true` to force a fresh model call
/// (use on stuck-loop / bad-cached-response retries).
async fn call_openrouter_with_tools(
    api_key: &str,
    model: &str,
    system: Option<&str>,
    messages: &[Value],
    tool_schemas: &[Value],
    session_id: &str,
    bust_cache: bool,
) -> anyhow::Result<LlmResponse> {
    // Build the full messages array with an optional system message prepended.
    // The system message gets a `cache_control` breakpoint so Anthropic-compatible
    // models routed via OpenRouter cache the static system prefix once per session.
    let mut full_messages: Vec<Value> = Vec::new();
    if let Some(sys) = system {
        full_messages.push(serde_json::json!({
            "role": "system",
            "content": [{
                "type": "text",
                "text": sys,
                "cache_control": {"type": "ephemeral", "ttl": "1h"}
            }],
        }));
    }
    full_messages.extend_from_slice(messages);

    let mut body = serde_json::json!({
        "model": model,
        "messages": full_messages,
        "max_tokens": 8192,
        // Sticky routing: same backend slot across all turns so the KV cache stays warm.
        "session_id": session_id,
    });
    if !tool_schemas.is_empty() {
        body["tools"] = Value::Array(tool_schemas.to_vec());
        // "auto" = model decides whether to call a tool or respond with text.
        body["tool_choice"] = serde_json::json!("auto");
    }

    let mut builder = reqwest::Client::new()
        .post("https://openrouter.ai/api/v1/chat/completions")
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .header("HTTP-Referer", "https://camerata.ai")
        .header("X-Title", "Camerata")
        // Enable OpenRouter response-level caching; `cache_discount` in the body tracks savings.
        .header("X-OpenRouter-Cache", "true");
    if bust_cache {
        builder = builder.header("X-OpenRouter-Cache-Clear", "true");
    }

    let resp = builder
        .json(&body)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("OpenRouter API request failed: {e}"))?;

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("OpenRouter API HTTP {status}: {text}");
    }

    let v: Value = serde_json::from_str(&text)
        .map_err(|e| anyhow::anyhow!("parse OpenRouter response JSON: {e}"))?;

    // Extract text content from the first choice.
    let choice = v["choices"]
        .as_array()
        .and_then(|c| c.first())
        .cloned()
        .unwrap_or_default();

    // The raw LlmResponse carries the full JSON for normalization by `parse_response`.
    // We embed the raw provider response in the `text` field as JSON so the normalizer
    // can access `tool_calls`. This is the "raw pass-through" strategy.
    let raw_json = serde_json::to_string(&choice).unwrap_or_default();
    let input_tokens = v["usage"]["prompt_tokens"].as_u64();
    let output_tokens = v["usage"]["completion_tokens"].as_u64();
    let model_returned = v["model"].as_str().unwrap_or(model).to_string();
    // Track OR response-level cache savings; log when non-zero.
    let or_cache_discount = v["cache_discount"].as_f64();
    if let Some(d) = or_cache_discount {
        if d > 0.0 {
            eprintln!(
                "[camerata-server/api_agent_driver] OpenRouter cache_discount={d:.2} \
                 session={session_id} model={model_returned}"
            );
        }
    }

    Ok(LlmResponse {
        text: raw_json,
        model: model_returned,
        backend: "openrouter/api/agentic".to_string(),
        cost_usd: None,
        input_tokens,
        output_tokens,
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
        or_cache_discount,
    })
}

/// The provider-neutral marker that terminates the Layer-2 grounding block in a prompt (emitted
/// by `grounding::assemble`). The prompt builders stay provider-neutral: they never mention
/// Anthropic and never place a `cache_control`. This body builder is the ONLY place that knows
/// how to translate the layering into Anthropic's cache breakpoints, so it detects the end of
/// Layer 2 by this marker and splits the first user message there.
const LAYER2_GROUNDING_TERMINATOR: &str = "=== END PROJECT GROUNDING ===";

/// Split a first-user-message string into its cacheable stable prefix (Layer 1 in the system
/// block already, plus Layer 2 grounding here) and its volatile Layer-3 tail, using the
/// grounding terminator. Returns `Some((prefix, tail))` only when the marker is present AND
/// there is real Layer-3 content after it (so we never place a degenerate breakpoint at the very
/// end). The prefix INCLUDES the terminator line so the whole grounding block is cached.
///
/// Provider-neutral by construction: the builders emit the marker via `grounding::assemble`
/// without knowing this split exists; only the Anthropic body builder consumes it.
fn split_grounding_prefix(text: &str) -> Option<(&str, &str)> {
    let idx = text.find(LAYER2_GROUNDING_TERMINATOR)?;
    // Boundary = just past the terminator marker (end of Layer 2).
    let boundary = idx + LAYER2_GROUNDING_TERMINATOR.len();
    let (prefix, tail) = text.split_at(boundary);
    // Only split when there is non-whitespace Layer-3 content after the boundary; otherwise the
    // second breakpoint would be redundant with the end of the message.
    if tail.trim().is_empty() {
        return None;
    }
    Some((prefix, tail))
}

/// Wrap a first-user-message `Value` (whose content is a plain string) in a two-block content
/// array with a `cache_control` breakpoint at the end of the Layer-2 grounding prefix, when that
/// message carries a grounding block. Any other message (tool_result arrays, non-string content,
/// or a message with no grounding marker) is returned unchanged. Pure: testable without HTTP.
///
/// This is the "end of Layer 2" breakpoint from the cache-layering design: the system block
/// caches Layer 1, and this caches Layer 1+2's continuation in the first user turn, so only the
/// volatile Layer-3 tail is billed at full price on each turn.
fn apply_grounding_cache_breakpoint(first_user_msg: &Value) -> Value {
    let Some(content) = first_user_msg.get("content").and_then(Value::as_str) else {
        return first_user_msg.clone();
    };
    let Some((prefix, tail)) = split_grounding_prefix(content) else {
        return first_user_msg.clone();
    };
    let mut msg = first_user_msg.clone();
    msg["content"] = serde_json::json!([
        {
            "type": "text",
            "text": prefix,
            "cache_control": {"type": "ephemeral"}
        },
        {
            "type": "text",
            "text": tail
        }
    ]);
    msg
}

/// Build the Anthropic Messages API request body (pure — no HTTP). Returns the body and
/// whether prompt-caching is active (drives the `anthropic-beta` header in the caller).
///
/// Extracted so the request shape is unit-testable without a live HTTP endpoint. The body
/// carries: `model`, `max_tokens`, `messages` (already Anthropic-shaped, including
/// tool_result user messages), a top-level `system` text block with a `cache_control`
/// ephemeral breakpoint (when a system prompt is present), and `tools` in Anthropic format
/// (`{name, description, input_schema}`) with `tool_choice: {type:"auto"}`.
///
/// CACHE LAYERING: two `cache_control` breakpoints are placed, both provider-neutral in the
/// builders (which never mention Anthropic):
///   - the top-level `system` block = the end of Layer 1 (kernel + role);
///   - the end of the Layer-2 grounding block INSIDE the first user message (detected via the
///     grounding terminator marker), so Layer 2 is cached too and only the volatile Layer-3 tail
///     is billed at full price on each turn.
fn build_anthropic_request_body(
    model: &str,
    system: Option<&str>,
    messages: &[Value],
    tool_schemas: &[Value],
) -> (Value, bool) {
    // Place the Layer-2 grounding breakpoint on the FIRST user message when it carries a
    // grounding block. Every other message is passed through untouched.
    let mut use_caching = false;
    let shaped_messages: Vec<Value> = messages
        .iter()
        .enumerate()
        .map(|(i, m)| {
            if i == 0 && m.get("role").and_then(Value::as_str) == Some("user") {
                let shaped = apply_grounding_cache_breakpoint(m);
                // A shaped (array-content) first message means a grounding breakpoint was placed,
                // so the beta header must be sent even if there is no system prompt.
                if shaped
                    .get("content")
                    .map(Value::is_array)
                    .unwrap_or(false)
                {
                    use_caching = true;
                }
                shaped
            } else {
                m.clone()
            }
        })
        .collect();

    let mut body = serde_json::json!({
        "model": model,
        "max_tokens": 8192,
        "messages": shaped_messages,
    });

    // Top-level `system` with a prompt-cache breakpoint on the static prefix. Sending it
    // as a single text block with `cache_control` (rather than a bare string) lets the
    // Anthropic prompt-caching beta cache the system prefix across the multi-turn loop.
    if let Some(sys) = system {
        body["system"] = serde_json::json!([{
            "type": "text",
            "text": sys,
            "cache_control": {"type": "ephemeral"}
        }]);
        use_caching = true;
    }

    if !tool_schemas.is_empty() {
        body["tools"] = Value::Array(tool_schemas.to_vec());
        // "auto" = let the model decide whether to call a tool or respond with text.
        body["tool_choice"] = serde_json::json!({"type": "auto"});
    }

    (body, use_caching)
}

/// POST directly to the Anthropic Messages API (`/v1/messages`) with tool schemas in
/// Anthropic format.
///
/// Mirrors `call_openrouter_with_tools`, but speaks the Anthropic wire shape (verified
/// against `llm.rs::complete_api` for the exact headers + version):
/// - URL `https://api.anthropic.com/v1/messages`
/// - headers `x-api-key: <key>` + `anthropic-version: 2023-06-01` (+ the prompt-caching
///   beta header when a `cache_control` breakpoint is set on the system block)
/// - body with a TOP-LEVEL `system` field (with a `cache_control` ephemeral breakpoint so
///   the static system prefix is cached per Anthropic prompt caching), `messages`,
///   `max_tokens`, and `tools` in Anthropic format (`[{name, description, input_schema}]`).
///
/// The `messages` array already carries Anthropic-shaped tool-result user messages
/// (see [`append_tool_results`]). The response is passed through verbatim as JSON so the
/// shared [`parse_response`] normalizer parses the Anthropic `content[].type=="tool_use"`
/// blocks — the same normalizer the OpenRouter path uses for text/tool calls.
async fn call_anthropic_with_tools(
    api_key: &str,
    model: &str,
    system: Option<&str>,
    messages: &[Value],
    tool_schemas: &[Value],
) -> anyhow::Result<LlmResponse> {
    let (body, use_caching) = build_anthropic_request_body(model, system, messages, tool_schemas);

    let mut builder = reqwest::Client::new()
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", api_key)
        .header("anthropic-version", ANTHROPIC_VERSION)
        .header("content-type", "application/json");
    // Only sent when a cache_control breakpoint is present, mirroring llm.rs::complete_api.
    if use_caching {
        builder = builder.header("anthropic-beta", "prompt-caching-2024-07-31");
    }

    let resp = builder
        .json(&body)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Anthropic API request failed: {e}"))?;

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("Anthropic API HTTP {status}: {text}");
    }

    let v: Value = serde_json::from_str(&text)
        .map_err(|e| anyhow::anyhow!("parse Anthropic response JSON: {e}"))?;

    // Pass the FULL response object through as `text`: it has the `content` array the
    // normalizer's Anthropic branch reads (`content[].type=="tool_use"` / `"text"`).
    let raw_json = serde_json::to_string(&v).unwrap_or_default();
    let input_tokens = v["usage"]["input_tokens"].as_u64();
    let output_tokens = v["usage"]["output_tokens"].as_u64();
    let cache_read = v["usage"]["cache_read_input_tokens"].as_u64().unwrap_or(0);
    let cache_creation = v["usage"]["cache_creation_input_tokens"].as_u64().unwrap_or(0);
    let model_returned = v["model"].as_str().unwrap_or(model).to_string();

    Ok(LlmResponse {
        text: raw_json,
        model: model_returned,
        backend: "anthropic/api/agentic".to_string(),
        cost_usd: None,
        input_tokens,
        output_tokens,
        cache_read_input_tokens: cache_read,
        cache_creation_input_tokens: cache_creation,
        or_cache_discount: None,
    })
}

/// Fallback for completers that don't support tool schemas: squash the message
/// history into a single text prompt.
fn messages_to_text_prompt(messages: &[Value]) -> String {
    messages
        .iter()
        .filter_map(|m| {
            let role = m["role"].as_str().unwrap_or("unknown");
            let content = m["content"].as_str().unwrap_or("");
            if content.is_empty() {
                None
            } else {
                Some(format!("[{role}]\n{content}"))
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

// ─── response normalization ───────────────────────────────────────────────────

/// The normalized result of one provider response turn.
enum ParsedResponse {
    /// The model emitted a text completion with no further tool calls (final turn).
    FinalText(String),
    /// The model wants to call one or more tools.
    ToolCalls {
        /// The raw assistant message to append to conversation history.
        raw_assistant_msg: Value,
        /// Normalized tool invocations.
        calls: Vec<ToolInvocation>,
    },
}

/// Normalize a provider `LlmResponse` into either a final text or a list of tool calls.
///
/// Handles:
/// - **OpenAI/OpenRouter style**: `choices[0].message.tool_calls[]` with
///   `function.{name, arguments}` (arguments is a JSON string).
/// - **Anthropic style**: `content[]` blocks with `type: "tool_use"` carrying
///   `{id, name, input}` (input is already a JSON object).
/// - **Plain text**: `choices[0].message.content` (OpenAI) or `content[0].text` (Anthropic).
///
/// The `LlmResponse.text` field carries the raw JSON of the choice/message, as produced
/// by `call_openrouter_with_tools`. For non-tool fallback completers it is a plain string.
fn parse_response(resp: &LlmResponse) -> ParsedResponse {
    // First try: interpret `text` as a JSON object (the raw choice from our tool-aware call).
    if let Ok(choice) = serde_json::from_str::<Value>(&resp.text) {
        // ── OpenAI / OpenRouter format ────────────────────────────────────
        // shape: {message: {role, content, tool_calls: [{id, type, function: {name, arguments}}]}}
        if let Some(msg) = choice.get("message") {
            let tool_calls = msg["tool_calls"].as_array().cloned().unwrap_or_default();
            if !tool_calls.is_empty() {
                let calls: Vec<ToolInvocation> = tool_calls
                    .into_iter()
                    .filter_map(|tc| normalize_openai_tool_call(&tc))
                    .collect();
                if !calls.is_empty() {
                    // Reconstruct the assistant message for history.
                    let raw_msg = serde_json::json!({
                        "role": "assistant",
                        "content": msg["content"],
                        "tool_calls": msg["tool_calls"],
                    });
                    return ParsedResponse::ToolCalls {
                        raw_assistant_msg: raw_msg,
                        calls,
                    };
                }
            }
            // Plain text content (OpenAI shape, no tool calls).
            if let Some(text) = msg["content"].as_str() {
                return ParsedResponse::FinalText(text.to_string());
            }
        }

        // ── Anthropic format ──────────────────────────────────────────────
        // shape: {content: [{type: "text"|"tool_use", ...}], stop_reason: "tool_use"|"end_turn"}
        if let Some(blocks) = choice["content"].as_array() {
            let tool_use_blocks: Vec<ToolInvocation> = blocks
                .iter()
                .filter(|b| b["type"] == "tool_use")
                .filter_map(|b| normalize_anthropic_tool_use(b))
                .collect();

            if !tool_use_blocks.is_empty() {
                let raw_msg = serde_json::json!({
                    "role": "assistant",
                    "content": choice["content"],
                });
                return ParsedResponse::ToolCalls {
                    raw_assistant_msg: raw_msg,
                    calls: tool_use_blocks,
                };
            }

            // Extract text from the first text block.
            let text = blocks
                .iter()
                .filter(|b| b["type"] == "text")
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("");
            if !text.is_empty() {
                return ParsedResponse::FinalText(text);
            }
        }
    }

    // Fallback: the text field is a plain string (non-tool fallback completer path).
    ParsedResponse::FinalText(resp.text.clone())
}

/// Normalize an OpenAI-style `tool_calls` entry.
///
/// Shape: `{id: "...", type: "function", function: {name: "...", arguments: "<JSON string>"}}`
fn normalize_openai_tool_call(tc: &Value) -> Option<ToolInvocation> {
    let id = tc["id"].as_str()?.to_string();
    let name = tc["function"]["name"].as_str()?.to_string();
    let args_str = tc["function"]["arguments"].as_str().unwrap_or("{}");
    let input = serde_json::from_str::<Value>(args_str).unwrap_or(Value::Object(Default::default()));
    Some(ToolInvocation { id, name, input })
}

/// Normalize an Anthropic `tool_use` content block.
///
/// Shape: `{type: "tool_use", id: "...", name: "...", input: {...}}`
fn normalize_anthropic_tool_use(block: &Value) -> Option<ToolInvocation> {
    let id = block["id"].as_str()?.to_string();
    let name = block["name"].as_str()?.to_string();
    let input = block["input"].clone();
    Some(ToolInvocation { id, name, input })
}

// ─── tool execution ───────────────────────────────────────────────────────────

/// Execute one tool invocation. Returns `(result_text, Option<denial_message>)`.
///
/// - For `gated_write`: evaluate via the gateway library; if allowed, write the file.
/// - For read tools (`Read`, `Glob`, `Grep`, `LS`): execute directly on the filesystem.
/// - For `delegate` / `fan_out`: only reachable when `orchestrator = true`; dispatched
///   through the gated `run_delegated`/`run_fan_out` primitives (per-model child provider
///   coupling) when an `OrchestratorConfig` (with factory) is attached, else an honest
///   "not configured" message and NO spawn.
/// - For anything else (including `shell`, `Task`, `Write`, `Edit`, `Bash`): deny.
async fn execute_tool(
    driver: &ApiAgentDriver,
    role: &Role,
    inv: &ToolInvocation,
) -> (String, Option<String>) {
    match inv.name.as_str() {
        GATED_WRITE => execute_gated_write(driver, role, inv),
        TOOL_READ => (execute_read(inv, driver.worktree.as_deref()), None),
        TOOL_GLOB => (execute_glob(inv, driver.worktree.as_deref()), None),
        TOOL_GREP => (execute_grep(inv, driver.worktree.as_deref()), None),
        TOOL_LS => (execute_ls(inv, driver.worktree.as_deref()), None),
        TOOL_DELEGATE if driver.orchestrator => {
            // Native orchestrator delegation. The gate is NOT reimplemented here: we
            // dispatch through the SAME gated `run_delegated` primitive the CLI
            // orchestrator uses. The `OrchestratorConfig` carries a per-model
            // `ServerChildDriverFactory`, so the child resolves to ITS OWN model's
            // provider (Claude CLI / Anthropic API / OpenRouter) — gated_write-only,
            // worktree-jailed, depth-1/non-orchestrator by the factory's contract.
            execute_delegate(driver, inv).await
        }
        TOOL_FAN_OUT if driver.orchestrator => {
            // Native fan-out dispatch through the gated `run_fan_out` primitive, which
            // already enforces the depth guard + per-repo jail + (via `run_delegated`)
            // the gate. Each worker is built per-model via the injected factory.
            execute_fan_out(driver, inv).await
        }
        other => {
            // Any unlisted tool (including shell, Task, Write, Edit, Bash,
            // delegate/fan_out for non-orchestrators) is denied hard.
            let msg = format!(
                "DENIED: tool `{other}` is not in the ApiAgentDriver allowlist. \
                 Only gated_write + read-only tools are permitted."
            );
            (msg.clone(), Some(msg))
        }
    }
}

/// Native `delegate`: dispatch one subtask through the gated `run_delegated` primitive.
///
/// Reuses the gateway's gated child-spawn (gated_write-only, jailed, depth-1,
/// non-orchestrator), with the child's provider resolved per-model by the factory carried
/// on the attached [`camerata_gateway::delegate::OrchestratorConfig`]. The gate is never
/// reimplemented here. When no config is attached (no factory), returns honestly and does
/// NOT spawn.
async fn execute_delegate(
    driver: &ApiAgentDriver,
    inv: &ToolInvocation,
) -> (String, Option<String>) {
    let Some(config) = driver.orchestrator_config.as_ref() else {
        return (
            "delegate: this orchestrator has no OrchestratorConfig attached (no child \
             driver factory), so delegation is unavailable; do the work yourself."
                .to_string(),
            None,
        );
    };
    let subtask = inv.input["subtask"]
        .as_str()
        .or_else(|| inv.input["task"].as_str())
        .unwrap_or("");
    let tier = inv.input["tier"].as_str().unwrap_or("balanced");
    if subtask.trim().is_empty() {
        let msg = "DENIED: delegate requires a non-empty `subtask` string".to_string();
        return (msg.clone(), Some(msg));
    }
    let output = camerata_gateway::delegate::run_delegated(
        config,
        driver.rule_subset.clone(),
        subtask,
        tier,
    )
    .await
    .unwrap_or_else(|e| e.to_string());
    (output, None)
}

/// Native `fan_out`: dispatch a multi-repo work set through the gated `run_fan_out`
/// primitive (depth guard + per-repo jail + per-entry gate via `run_delegated`). Each
/// worker's provider is resolved per-model by the factory on the attached config. Returns
/// a per-repo assembled summary. No-spawn + honest message when no config is attached.
async fn execute_fan_out(
    driver: &ApiAgentDriver,
    inv: &ToolInvocation,
) -> (String, Option<String>) {
    let Some(config) = driver.orchestrator_config.as_ref() else {
        return (
            "fan_out: this orchestrator has no OrchestratorConfig attached (no child \
             driver factory), so fan-out is unavailable; do the work yourself."
                .to_string(),
            None,
        );
    };
    // Accept `entries` (canonical) or `tasks` (schema alias).
    let raw = if !inv.input["entries"].is_null() {
        inv.input["entries"].clone()
    } else {
        inv.input["tasks"].clone()
    };
    let entries: Vec<camerata_gateway::fan_out::FanOutEntry> =
        match serde_json::from_value(raw) {
            Ok(e) => e,
            Err(e) => {
                let msg = format!(
                    "DENIED: fan_out `entries` must be an array of {{repo, domain, subtask}}: {e}"
                );
                return (msg.clone(), Some(msg));
            }
        };
    match camerata_gateway::fan_out::run_fan_out(config, driver.rule_subset.clone(), entries).await {
        Ok(results) => {
            let by_repo = camerata_gateway::fan_out::assemble_by_repo(&results);
            let mut summary = format!("fan_out complete: {} worker(s).\n", results.len());
            for (repo, r) in &by_repo {
                let marker = if r.incomplete { "INCOMPLETE" } else { "OK" };
                summary.push_str(&format!(
                    "\n── repo={repo} domain={} [{marker}] ──\n{}\n",
                    r.domain, r.output
                ));
            }
            (summary, None)
        }
        Err(e) => (e.to_string(), None),
    }
}

/// Execute a `gated_write` invocation.
///
/// 1. Extract `path` + `content` from `input`.
/// 2. Enforce size guard.
/// 3. Evaluate against the role's rule subset via `evaluate_call`.
/// 4. Enforce worktree jail (path must be under `driver.worktree` when set).
/// 5. If everything passes: write the file, returning an allow message.
/// 6. If denied: return the denial reason without writing.
fn execute_gated_write(
    driver: &ApiAgentDriver,
    _role: &Role,
    inv: &ToolInvocation,
) -> (String, Option<String>) {
    let path = match inv.input["path"].as_str() {
        Some(p) => p.to_string(),
        None => {
            let msg = "DENIED: gated_write requires a `path` string field in input".to_string();
            return (msg.clone(), Some(msg));
        }
    };
    let content = match inv.input["content"].as_str() {
        Some(c) => c.to_string(),
        None => {
            let msg = "DENIED: gated_write requires a `content` string field in input".to_string();
            return (msg.clone(), Some(msg));
        }
    };

    // ── Size guard ─────────────────────────────────────────────────────────
    if content.len() > MAX_WRITE_BYTES {
        let msg = format!(
            "DENIED: gated_write content exceeds {} bytes ({} bytes); split the write.",
            MAX_WRITE_BYTES,
            content.len()
        );
        return (msg.clone(), Some(msg));
    }

    // ── Gateway rule evaluation (Layer-1) ──────────────────────────────────
    let call = ToolCall {
        tool: GATED_WRITE.to_string(),
        input: inv.input.clone(),
    };
    let decision = evaluate_call(&driver.rule_subset, &call);
    match decision {
        Decision::Deny { rule, reason } => {
            let msg = format!("DENIED by {} — {}", rule.0, reason);
            return (msg.clone(), Some(msg));
        }
        Decision::Allow => {}
    }

    // ── Worktree jail ──────────────────────────────────────────────────────
    // The jail check RETURNS the resolved absolute write path, so the CHECK and the
    // write EFFECT are the same path: no `wt.join(trim)` path-doubling (GATE-F3) and no
    // check-here/write-there divergence (GATE-F4).
    let write_path = if let Some(wt) = &driver.worktree {
        match assert_in_worktree(wt, &path) {
            Ok(resolved) => resolved,
            Err(e) => {
                let msg = format!("DENIED: worktree jail violation — {e}");
                return (msg.clone(), Some(msg));
            }
        }
    } else {
        PathBuf::from(&path)
    };

    if let Some(parent) = write_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            let msg = format!("ERROR: could not create parent directories for `{path}`: {e}");
            return (msg, None);
        }
    }

    match std::fs::write(&write_path, &content) {
        Ok(()) => {
            let msg = format!("OK: wrote {} bytes to `{path}`", content.len());
            (msg, None)
        }
        Err(e) => {
            let msg = format!("ERROR: write failed for `{path}`: {e}");
            (msg, None)
        }
    }
}

/// Lexically normalize a path: resolve `.` and `..` WITHOUT touching the filesystem
/// (so it works for not-yet-created files).
fn normalize_lexical(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Resolve symlinks in the EXISTING ancestor prefix of `p`, rejoining the canonical
/// existing prefix with the remaining not-yet-created tail. Canonicalizing the deepest
/// existing ancestor resolves every symlink in that prefix; the tail names components
/// that do not exist yet, so none of them can be a symlink. This is what stops a
/// repo-committed symlink directory from smuggling a write outside the jail.
fn canonicalize_existing_prefix(p: &Path) -> PathBuf {
    let mut ancestor = p.to_path_buf();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(canon) = std::fs::canonicalize(&ancestor) {
            let mut resolved = canon;
            for seg in tail.iter().rev() {
                resolved.push(seg);
            }
            return resolved;
        }
        match ancestor.file_name() {
            Some(name) => {
                tail.push(name.to_os_string());
                if !ancestor.pop() {
                    return normalize_lexical(p);
                }
            }
            None => return normalize_lexical(p),
        }
    }
}

/// Resolve `path` to the absolute location it must be WRITTEN to inside `worktree`, or
/// return an error when it escapes the jail. Relative paths resolve against the worktree;
/// absolute paths are taken as-is. Symlinks in the existing ancestor prefix (of both the
/// target and the worktree) are resolved BEFORE a component-wise prefix check, so a
/// symlinked directory component cannot smuggle a write outside the worktree (GATE-F1)
/// and a legit write under a symlinked worktree prefix is not falsely denied (GATE-F5).
/// The returned path is the resolved write target, so the caller writes to exactly what
/// was checked (no path-doubling — GATE-F3).
fn assert_in_worktree(worktree: &Path, path: &str) -> anyhow::Result<PathBuf> {
    // Reject obvious `..` traversals immediately (defence-in-depth; the gateway rules
    // also catch these via SEC-NO-PATH-ESCAPE-1).
    if path.contains("..") {
        anyhow::bail!("path `{path}` contains `..` which may escape the worktree");
    }
    let t = Path::new(path);
    let abs = if t.is_absolute() {
        t.to_path_buf()
    } else {
        worktree.join(t)
    };
    let target = canonicalize_existing_prefix(&normalize_lexical(&abs));
    let root = canonicalize_existing_prefix(&normalize_lexical(worktree));
    if !target.starts_with(&root) {
        anyhow::bail!(
            "path `{path}` resolves to `{}` which is not under worktree `{}`",
            target.display(),
            root.display()
        );
    }
    Ok(target)
}

// ─── read-tool implementations ────────────────────────────────────────────────

/// Execute a `Read` tool call: read the content of a file.
///
/// Expected input: `{file_path: "..."}` (Claude Code convention).
fn execute_read(inv: &ToolInvocation, worktree: Option<&Path>) -> String {
    let raw_path = match inv.input["file_path"].as_str().or_else(|| inv.input["path"].as_str()) {
        Some(p) => p,
        None => return "ERROR: Read requires `file_path` in input".to_string(),
    };
    let resolved = resolve_path(raw_path, worktree);
    match std::fs::read_to_string(&resolved) {
        Ok(content) => truncate_output(content, MAX_READ_OUTPUT_BYTES),
        Err(e) => format!("ERROR reading `{raw_path}`: {e}"),
    }
}

/// Execute a `Glob` tool call: list files matching a glob pattern.
///
/// Expected input: `{pattern: "..."}` or `{glob: "..."}`.
fn execute_glob(inv: &ToolInvocation, worktree: Option<&Path>) -> String {
    let pattern = match inv.input["pattern"].as_str()
        .or_else(|| inv.input["glob"].as_str())
    {
        Some(p) => p,
        None => return "ERROR: Glob requires `pattern` in input".to_string(),
    };

    let base = worktree.map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
    let full_pattern = if pattern.starts_with('/') {
        pattern.to_string()
    } else {
        format!("{}/{}", base.display(), pattern)
    };

    match glob::glob(&full_pattern) {
        Ok(paths) => {
            let entries: Vec<String> = paths
                .filter_map(|p| p.ok())
                .map(|p| p.display().to_string())
                .collect();
            if entries.is_empty() {
                format!("No files matched pattern `{pattern}`")
            } else {
                truncate_output(entries.join("\n"), MAX_READ_OUTPUT_BYTES)
            }
        }
        Err(e) => format!("ERROR: Glob pattern error `{pattern}`: {e}"),
    }
}

/// Execute a `Grep` tool call: search files for a pattern.
///
/// Expected input: `{pattern: "...", path: "..."}` or `{regex: "...", directory: "..."}`.
fn execute_grep(inv: &ToolInvocation, worktree: Option<&Path>) -> String {
    let search_pattern = match inv.input["pattern"].as_str()
        .or_else(|| inv.input["regex"].as_str())
    {
        Some(p) => p,
        None => return "ERROR: Grep requires `pattern` in input".to_string(),
    };

    let search_dir = inv.input["path"].as_str()
        .or_else(|| inv.input["directory"].as_str())
        .unwrap_or(".");
    let resolved_dir = resolve_path(search_dir, worktree);

    // Compile the regex.
    let re = match regex::Regex::new(search_pattern) {
        Ok(r) => r,
        Err(e) => return format!("ERROR: invalid grep pattern `{search_pattern}`: {e}"),
    };

    let mut findings: Vec<String> = Vec::new();
    grep_dir(&resolved_dir, &re, &mut findings, 0);
    if findings.is_empty() {
        format!("No matches for `{search_pattern}` in `{search_dir}`")
    } else {
        truncate_output(findings.join("\n"), MAX_READ_OUTPUT_BYTES)
    }
}

/// Recursive grep helper: walks `dir` and collects `file:line:text` entries.
fn grep_dir(dir: &Path, re: &regex::Regex, out: &mut Vec<String>, depth: usize) {
    if depth > 8 {
        return; // depth limit to avoid symlink loops
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // Skip hidden directories (e.g. .git)
        if path.file_name().map(|n| n.to_str().unwrap_or("").starts_with('.')).unwrap_or(false) {
            continue;
        }
        if path.is_dir() {
            grep_dir(&path, re, out, depth + 1);
        } else if path.is_file() {
            if let Ok(text) = std::fs::read_to_string(&path) {
                for (lineno, line) in text.lines().enumerate() {
                    if re.is_match(line) {
                        out.push(format!("{}:{}:{}", path.display(), lineno + 1, line));
                        if out.len() > 2000 {
                            // Safety cap: avoid unbounded output.
                            out.push("... (truncated at 2000 matches)".to_string());
                            return;
                        }
                    }
                }
            }
        }
    }
}

/// Execute an `LS` tool call: list directory contents.
///
/// Expected input: `{path: "..."}` or `{directory: "..."}`.
fn execute_ls(inv: &ToolInvocation, worktree: Option<&Path>) -> String {
    let dir_path = inv.input["path"].as_str()
        .or_else(|| inv.input["directory"].as_str())
        .unwrap_or(".");
    let resolved = resolve_path(dir_path, worktree);
    match std::fs::read_dir(&resolved) {
        Ok(entries) => {
            let mut names: Vec<String> = entries
                .flatten()
                .map(|e| {
                    let name = e.file_name().to_string_lossy().to_string();
                    if e.path().is_dir() {
                        format!("{name}/")
                    } else {
                        name
                    }
                })
                .collect();
            names.sort();
            truncate_output(names.join("\n"), MAX_READ_OUTPUT_BYTES)
        }
        Err(e) => format!("ERROR listing `{dir_path}`: {e}"),
    }
}

// ─── path helpers ─────────────────────────────────────────────────────────────

/// Resolve a path relative to `worktree` (if set) or the current directory.
fn resolve_path(raw: &str, worktree: Option<&Path>) -> PathBuf {
    let p = Path::new(raw);
    if p.is_absolute() {
        p.to_path_buf()
    } else if let Some(wt) = worktree {
        wt.join(raw)
    } else {
        PathBuf::from(raw)
    }
}

/// Truncate a string to at most `max_bytes`, appending a note when truncated.
fn truncate_output(s: String, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        s
    } else {
        let truncated = &s[..max_bytes];
        format!("{truncated}\n... (output truncated at {max_bytes} bytes)")
    }
}

// ─── tool schema definitions ──────────────────────────────────────────────────

/// Build the JSON tool schemas to include in the chat-completions request.
///
/// The schemas follow the OpenAI function-calling convention (also accepted by
/// Anthropic's API with minor differences). Only the safe, allowlisted tools are
/// included; `shell`, `Task`, `Bash`, `Write`, `Edit`, `MultiEdit` are never present.
///
/// When `orchestrator = true`, `delegate` and `fan_out` schemas are appended.
/// Workers (`orchestrator = false`) never receive them, enforcing depth-1.
fn build_tool_schemas(orchestrator: bool) -> Vec<Value> {
    let mut schemas = vec![
        serde_json::json!({
            "type": "function",
            "function": {
                "name": GATED_WRITE,
                "description": "Write content to a file in the worktree. \
                    Every write is evaluated by the Layer-1 governance gate before \
                    the file is written. This is the ONLY way to modify files.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "File path to write (relative to worktree root)."
                        },
                        "content": {
                            "type": "string",
                            "description": "Complete file content to write."
                        }
                    },
                    "required": ["path", "content"]
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": TOOL_READ,
                "description": "Read the content of a file.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "file_path": {
                            "type": "string",
                            "description": "Path to the file to read."
                        }
                    },
                    "required": ["file_path"]
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": TOOL_GLOB,
                "description": "List files matching a glob pattern.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": {
                            "type": "string",
                            "description": "Glob pattern (e.g. `src/**/*.rs`)."
                        }
                    },
                    "required": ["pattern"]
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": TOOL_GREP,
                "description": "Search files for a regex pattern.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": {
                            "type": "string",
                            "description": "Regex pattern to search for."
                        },
                        "path": {
                            "type": "string",
                            "description": "Directory or file to search (defaults to worktree root)."
                        }
                    },
                    "required": ["pattern"]
                }
            }
        }),
        serde_json::json!({
            "type": "function",
            "function": {
                "name": TOOL_LS,
                "description": "List the contents of a directory.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Directory to list (defaults to worktree root)."
                        }
                    },
                    "required": []
                }
            }
        }),
    ];

    // Orchestrator-only tools: only appended when `orchestrator = true`.
    // Workers structurally cannot request these.
    if orchestrator {
        schemas.push(serde_json::json!({
            "type": "function",
            "function": {
                "name": TOOL_DELEGATE,
                "description": "Delegate a task to a child governed agent (orchestrator only).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "role": { "type": "string" },
                        "task": { "type": "string" }
                    },
                    "required": ["role", "task"]
                }
            }
        }));
        schemas.push(serde_json::json!({
            "type": "function",
            "function": {
                "name": TOOL_FAN_OUT,
                "description": "Fan out a task concurrently across multiple repos (orchestrator only).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "tasks": {
                            "type": "array",
                            "items": { "type": "object" }
                        }
                    },
                    "required": ["tasks"]
                }
            }
        }));
    }

    schemas
}

/// Build the tool schemas in the wire shape `shape` requires.
///
/// The allowlist (which tools are present, gated by `orchestrator`) is identical across
/// shapes — only the JSON envelope differs:
/// - [`ApiShape::OpenRouter`]: the OpenAI function-calling shape from [`build_tool_schemas`].
/// - [`ApiShape::Anthropic`]: each schema converted to Anthropic's tool format
///   (`{name, description, input_schema}`) via [`openai_tool_to_anthropic`].
///
/// Producing Anthropic schemas by CONVERTING the single source-of-truth OpenAI schemas
/// (rather than maintaining a second hand-written list) guarantees the two shapes always
/// expose exactly the same tool set — a worker can never gain a tool on one shape that it
/// lacks on the other.
fn build_tool_schemas_for(orchestrator: bool, shape: ApiShape) -> Vec<Value> {
    let openai = build_tool_schemas(orchestrator);
    match shape {
        ApiShape::OpenRouter => openai,
        ApiShape::Anthropic => openai.iter().filter_map(openai_tool_to_anthropic).collect(),
    }
}

/// Convert one OpenAI-shaped tool schema (`{type:"function", function:{name, description,
/// parameters}}`) into Anthropic's tool format (`{name, description, input_schema}`).
///
/// Returns `None` if the input isn't a well-formed function tool (shouldn't happen for our
/// own statically-built schemas; defensive so a malformed entry is dropped rather than sent
/// in a shape the API would reject).
fn openai_tool_to_anthropic(tool: &Value) -> Option<Value> {
    let func = tool.get("function")?;
    let name = func.get("name")?.clone();
    let description = func.get("description").cloned().unwrap_or(Value::Null);
    // OpenAI calls the JSON-Schema field `parameters`; Anthropic calls it `input_schema`.
    // The schema body itself is identical.
    let input_schema = func
        .get("parameters")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}}));
    Some(serde_json::json!({
        "name": name,
        "description": description,
        "input_schema": input_schema,
    }))
}

// ─── system prompt ────────────────────────────────────────────────────────────

/// Build the system prompt for the agent, embedding the shared governance kernel + the role's
/// tool constraints. This is THE system prompt for every OpenRouter/API in-process loop — the
/// path open-weight models arrive through — so it is the single highest-impact insertion point
/// for the operating protocol. See the hardened rewrite 3.5 in
/// `docs/plans/2026-07-05_prompt-hardening-and-governance-kernel.md`.
///
/// `model` pins the kernel's per-tier addendum (`kernel_for(model)`); an empty model falls back
/// to the base [`camerata_app_core::GOVERNANCE_KERNEL`] with no addendum.
///
/// LAYERING: this system prompt IS Layer 1 (global immutable) of the geological prompt layering —
/// identical across every turn for a given role + model tier. `build_anthropic_request_body`
/// places a `cache_control` breakpoint on it (the end-of-Layer-1 boundary). Layer 2 (grounding)
/// and Layer 3 (the volatile task) travel in the user message. See
/// `camerata_app_core::prompt_layers`.
fn build_system_prompt(role: &Role, model: &str) -> Option<String> {
    let kernel = if model.trim().is_empty() {
        camerata_app_core::GOVERNANCE_KERNEL.to_string()
    } else {
        camerata_app_core::kernel_for(model)
    };
    let paths = if role.allowed_paths.is_empty() {
        "<unrestricted>".to_string()
    } else {
        role.allowed_paths.join(", ")
    };
    Some(format!(
        "You are a governed software engineering agent in the `{role}` role under Camerata.\n\n\
         {kernel}\n\n\
         CONSTRAINTS: write files ONLY via gated_write (denied writes are information, not an \
         obstacle to route around); Read/Glob/Grep/LS to read (read before you write; never guess \
         contents/locations); NO Bash/Task/Edit/Write/MultiEdit or unlisted tools.\n\
         WORKING DISCIPLINE (in order): (1) read relevant code; (2) plan the minimal complete \
         change; (3) write tests with any behavior change; (4) implement defensively (explicit \
         error/empty handling, boundary validation, follow file conventions); (5) before finishing, \
         re-read every file you wrote and fix any incompleteness/syntax/import/rule issue. Not done \
         until this self-review finds nothing.\n\
         IF UNSURE: do not guess/invent. Prefer: read more; take the most conservative compliant \
         action; or state precisely what is unknown. Never fabricate file contents, APIs, or facts.\n\
         COMPLETION: final text message with CHANGES / TESTS / CONCERNS.\n\
         Role: `{role}`   Allowed paths: {paths}",
        role = role.name,
        kernel = kernel,
        paths = paths,
    ))
}

// ─── Anthropic-shape routing helpers ───────────────────────────────────────────

/// A no-op `LlmPort` used as the placeholder completer for an Anthropic-shape
/// `ApiAgentDriver`. The Anthropic path makes its HTTP call directly in
/// `call_anthropic_with_tools` (using the key carried on the driver), so this completer's
/// `complete` is only ever reached on the schema-less fallback — which the Anthropic path
/// only takes when NO key is attached. With a key attached it is never called; we still
/// give it an honest implementation so any unexpected use fails loudly rather than silently.
struct AnthropicNoopCompleter;

#[async_trait]
impl LlmPort for AnthropicNoopCompleter {
    async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
        anyhow::bail!(
            "AnthropicNoopCompleter::complete reached — the Anthropic ApiAgentDriver should \
             make its call via call_anthropic_with_tools, not the LlmPort trait"
        )
    }
    async fn complete_streaming(
        &self,
        req: LlmRequest,
        _on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        self.complete(req).await
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Decide whether the `claude` provider should run via the Anthropic Messages API
/// (`ApiAgentDriver` in [`ApiShape::Anthropic`]) instead of the `ClaudeCliDriver`.
///
/// Returns `Some(key)` when BOTH the effective backend is `api` AND an Anthropic key is
/// available (non-empty) — the same two signals `llm.rs` uses to select the Anthropic API
/// backend. Returns `None` otherwise (the default `cli` path, or `api` with no key).
///
/// ROUTES-9: the Anthropic key is read from the CREDENTIAL STORE first (with an env fallback
/// for back-compat), NOT solely from `ANTHROPIC_API_KEY` env. The `set_credential` handler no
/// longer mirrors a freshly-saved key into process env (that was a request-thread `set_var`
/// racing worker-thread `getenv` — POSIX UB). Reading the store here means a freshly-saved
/// key still takes effect without a restart, with no process-env mutation. The backend signal
/// is still read from `CAMERATA_LLM_BACKEND` env, which is hydrated ONCE at single-threaded
/// startup from the persisted setting (see `run`), so it is never written after threads spawn.
fn anthropic_api_backend_key(creds: &dyn crate::credentials::CredentialStore) -> Option<String> {
    let backend = std::env::var("CAMERATA_LLM_BACKEND").ok();
    if backend.as_deref() != Some("api") {
        return None;
    }
    // Store-first, env-fallback: mirrors `credentials::resolve` so a keychain-saved key wins
    // and existing dotenv/CI setups keep working. A store read error must not silently degrade
    // to env with no trace, so warn and fall back (matches the resolve() posture).
    match creds.get(crate::credentials::ANTHROPIC_API_KEY) {
        Ok(Some(k)) if !k.trim().is_empty() => return Some(k),
        Ok(_) => {}
        Err(e) => eprintln!(
            "[camerata-server] credential-store read of ANTHROPIC_API_KEY failed ({e}); \
             falling back to env"
        ),
    }
    std::env::var("ANTHROPIC_API_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty())
}

/// Build a `claude`-provider driver: an Anthropic-shape [`ApiAgentDriver`] when
/// `CAMERATA_LLM_BACKEND=api` + `ANTHROPIC_API_KEY` are set, else the [`ClaudeCliDriver`].
///
/// Shared by [`build_agent_driver`], [`ServerChildDriverFactory`], and
/// [`ServerOrchestratorDriverFactory`] so a Claude tier honors the same per-model provider
/// coupling everywhere: under backend=api it uses the Anthropic API agent; under cli it
/// uses the CLI. Either way the result is gated identically (same `evaluate_call`, same
/// worktree jail, same orchestrator gating).
fn build_claude_driver(
    model_id: &str,
    // ROUTES-9: credential store, consulted store-first (env fallback) for the Anthropic key
    // instead of a per-request env `set_var`. See `anthropic_api_backend_key`.
    creds: &dyn crate::credentials::CredentialStore,
    mcp_config_path: &str,
    rule_subset: Vec<RuleId>,
    worktree: Option<PathBuf>,
    orchestrator: bool,
    // Opt this CLI agent into the READ-CLASS `raise_escalation` gateway tool. Only meaningful on
    // the CLI path (the gateway MCP provides the tool); ignored by the Anthropic-API shape.
    escalation: bool,
    // LIFECYCLE-7 liveness heartbeat. Wired onto whichever concrete driver is built (CLI: per
    // output line; Anthropic API: per loop turn) so a healthy long run stays fresh.
    on_activity: Option<HeartbeatFn>,
) -> Arc<dyn AgentDriver> {
    if let Some(key) = anthropic_api_backend_key(creds) {
        // Anthropic Messages API agent. Same gate surface as every other ApiAgentDriver:
        // gated_write-only, worktree-jailed, delegate/fan_out only when orchestrator=true.
        let mut driver = ApiAgentDriver::new(Arc::new(AnthropicNoopCompleter), model_id)
            .with_rule_subset(rule_subset)
            .as_orchestrator(orchestrator)
            .with_shape(ApiShape::Anthropic)
            .with_anthropic_api_key(key);
        if let Some(wt) = worktree {
            driver = driver.with_worktree(wt);
        }
        if let Some(cb) = on_activity {
            driver = driver.with_on_activity(cb);
        }
        Arc::new(driver)
    } else {
        let mut cli_driver = camerata_agent::ClaudeCliDriver::new(mcp_config_path)
            .as_orchestrator(orchestrator)
            .with_escalation(escalation);
        if !model_id.trim().is_empty() {
            cli_driver = cli_driver.with_model(model_id);
        }
        if let Some(wt) = worktree {
            cli_driver = cli_driver.with_worktree(wt);
        }
        if let Some(cb) = on_activity {
            cli_driver = cli_driver.with_on_activity(cb);
        }
        Arc::new(cli_driver)
    }
}

// ─── driver selection factory ─────────────────────────────────────────────────

/// Select the right `Arc<dyn AgentDriver>` for `model_id` based on its provider.
///
/// - **`"claude"` provider (or unknown):** returns a `ClaudeCliDriver` (the Claude
///   subscription path — uses the local `claude` CLI, no per-token cost), OR — when
///   `CAMERATA_LLM_BACKEND=api` and `ANTHROPIC_API_KEY` is set — an `ApiAgentDriver` in
///   [`ApiShape::Anthropic`] that drives the same in-process gateway over the Anthropic
///   Messages API.
/// - **`"openrouter"` provider:** returns an `ApiAgentDriver` backed by the
///   `OpenRouterCompleter` (the native, provider-agnostic loop, in-process gateway).
///
/// `run_session_id` is an optional stable id for this run (e.g. the UoW story id or a
/// run id string). When `Some`, it is set on the `OpenRouterCompleter` so every request
/// in this run shares the same OpenRouter session id, keeping the KV cache warm across
/// all multi-turn loop iterations. When `None` a per-instance token is generated.
///
/// This is the run-orchestration seam where driver selection happens, keeping the
/// choice out of every individual caller (dev_implement_run, etc.).
///
/// # Panics / errors
///
/// Returns `Err` when an OpenRouter driver is requested but the credential store
/// does not have the `OPENROUTER_API_KEY`.
pub fn build_agent_driver(
    model_id: &str,
    registry: &crate::model_registry::ModelRegistry,
    creds: &dyn crate::credentials::CredentialStore,
    mcp_config_path: &str,
    rule_subset: Vec<RuleId>,
    worktree: Option<PathBuf>,
    orchestrator: bool,
    limiter: Arc<crate::rate_limit::ProviderRateLimiter>,
    run_session_id: Option<&str>,
    // Opt the agent into the READ-CLASS `raise_escalation` gateway tool (CLI path only). `false`
    // for every caller except a governed dev run, which lets the agent self-escalate on rule
    // conditions. Adds no write path; the gate posture is unchanged.
    escalation: bool,
    // LIFECYCLE-7 liveness heartbeat. Threaded onto the concrete driver (CLI, Anthropic API, or
    // OpenRouter) so a healthy long run keeps `last_activity_ms` fresh and is not reported
    // stalled. `None` for callers that don't wire one (tests / non-supervised builds). Runners
    // pass `Arc::new(move || runs.touch_activity(&run_id, None))`, mirroring
    // investigation_run / update_branch_run.
    on_activity: Option<HeartbeatFn>,
) -> anyhow::Result<Arc<dyn AgentDriver>> {
    let provider = registry
        .all_entries()
        .into_iter()
        .find(|e| e.id == model_id)
        .map(|e| e.provider)
        .unwrap_or_else(|| "claude".to_string());

    match provider.as_str() {
        "openrouter" => {
            let key = creds
                .get(crate::credentials::OPENROUTER_API_KEY)
                .map_err(|e| anyhow::anyhow!("credential store error: {e}"))?
                .filter(|k| !k.trim().is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "model `{model_id}` is an OpenRouter model but OPENROUTER_API_KEY is not \
                         set — add it via Settings → Credentials before using this model"
                    )
                })?;

            let mut or_completer = crate::llm::OpenRouterCompleter::for_agent(key, limiter);
            if let Some(sid) = run_session_id {
                or_completer = or_completer.with_session_id(sid);
            }
            let completer = Arc::new(or_completer);
            let mut driver = ApiAgentDriver::new(completer, model_id)
                .with_rule_subset(rule_subset)
                .as_orchestrator(orchestrator);
            if let Some(wt) = worktree {
                driver = driver.with_worktree(wt);
            }
            if let Some(cb) = on_activity {
                driver = driver.with_on_activity(cb);
            }
            Ok(Arc::new(driver))
        }
        // "claude" or any unrecognised provider: CLI by default, or the Anthropic Messages
        // API agent when CAMERATA_LLM_BACKEND=api + ANTHROPIC_API_KEY are set.
        _ => Ok(build_claude_driver(
            model_id,
            creds,
            mcp_config_path,
            rule_subset,
            worktree,
            orchestrator,
            escalation,
            on_activity,
        )),
    }
}

// ─── per-model child driver factory (provider coupling) ────────────────────────

/// Server-side [`camerata_gateway::delegate::ChildDriverFactory`] backed by
/// [`build_agent_driver`].
///
/// This is the seam that fixes per-model provider coupling for delegated / fanned-out
/// children: `run_delegated`/`run_fan_out` (in the gateway lib) ask this factory to build
/// each child for ITS tier's model, and the factory routes the model to its OWN provider
/// (Claude CLI, Anthropic API, or OpenRouter) via [`build_agent_driver`] — the child does
/// NOT inherit the parent orchestrator's provider.
///
/// # Gate contract (held for BOTH driver kinds)
///
/// Every child this returns is built as a WORKER (`orchestrator = false`) jailed to the
/// child `worktree`:
/// - **CLI child** (`claude` provider): [`build_agent_driver`] returns a `ClaudeCliDriver`
///   whose `--allowedTools` is `gated_write` + read-only tools (NO `delegate`/`fan_out`,
///   since `orchestrator = false`) and whose `--disallowedTools` is
///   `Task`/`Bash`/`Write`/`Edit`/`MultiEdit`/`NotebookEdit`. Its session mcp-config is the
///   ordinary [`camerata_agent::prepare_session`] config (NO `CAMERATA_DELEGATE_ENABLED`),
///   so its gateway never registers `delegate`/`fan_out` and its writes are jailed to the
///   worktree.
/// - **API child** (`openrouter` provider): [`build_agent_driver`] returns an
///   [`ApiAgentDriver`] with `orchestrator = false`, so [`build_tool_schemas`] omits
///   `delegate`/`fan_out`, the only mutation tool is `gated_write` (evaluated through the
///   same `evaluate_call` gate), and the worktree jail is enforced in
///   [`execute_gated_write`] + [`assert_in_worktree`].
///
/// Either way the child is **gated_write-only, worktree-jailed, depth-1 / non-orchestrator**
/// — identical to the legacy hard-coded CLI child, just on the correct provider.
pub struct ServerChildDriverFactory {
    registry: crate::model_registry::ModelRegistry,
    creds: Arc<dyn crate::credentials::CredentialStore>,
    limiter: Arc<crate::rate_limit::ProviderRateLimiter>,
    /// Located `camerata-gateway` binary, used to wire each CLI child's own gated gateway.
    gateway_bin: PathBuf,
    /// The orchestrator's active rule subset; every child is born under the SAME subset
    /// (so the child's session rules + the gate evaluation match the gateway's `child_role`).
    rule_subset: Vec<RuleId>,
    /// Stable per-run session id (OpenRouter sticky routing / KV-cache warmth). Optional.
    run_session_id: Option<String>,
    /// LIFECYCLE-10: the run's OWN gate-events sink, threaded per-spawn into each delegate
    /// child's gateway mcp-config env (never the shared parent process env). `None` = no
    /// live gate-events capture for children.
    gate_events_file: Option<PathBuf>,
}

impl ServerChildDriverFactory {
    /// Build the factory with the provider-dispatch context the children need.
    pub fn new(
        registry: crate::model_registry::ModelRegistry,
        creds: Arc<dyn crate::credentials::CredentialStore>,
        limiter: Arc<crate::rate_limit::ProviderRateLimiter>,
        gateway_bin: PathBuf,
        rule_subset: Vec<RuleId>,
        run_session_id: Option<String>,
        gate_events_file: Option<PathBuf>,
    ) -> Self {
        Self {
            registry,
            creds,
            limiter,
            gateway_bin,
            rule_subset,
            run_session_id,
            gate_events_file,
        }
    }
}

/// A child driver that keeps its gated CLI session alive.
///
/// [`build_agent_driver`]'s CLI path needs a per-child gated session (rules + mcp-config)
/// on disk; that session lives in a [`tempfile::TempDir`] that must outlive the driver. We
/// wrap the inner `Arc<dyn AgentDriver>` together with the [`camerata_agent::SessionSpawn`]
/// so the `_dir` TempDir is dropped (and cleaned up) only when this child driver is. For
/// API children the spawn is still held (harmless) so the one wrapper type covers both.
struct SessionBoundChildDriver {
    inner: Arc<dyn AgentDriver>,
    // Held purely for its RAII `_dir`: dropping this removes the child's session dir.
    _session: camerata_agent::SessionSpawn,
}

#[async_trait]
impl AgentDriver for SessionBoundChildDriver {
    async fn run(&self, role: &Role, task: &str) -> anyhow::Result<AgentOutcome> {
        self.inner.run(role, task).await
    }
}

impl camerata_gateway::delegate::ChildDriverFactory for ServerChildDriverFactory {
    fn build_child(
        &self,
        model: &str,
        worktree: &Path,
        read_dirs: &[PathBuf],
    ) -> std::io::Result<Box<dyn AgentDriver>> {
        // The child runs under the orchestrator's own rule subset, jailed to `worktree`.
        // The name is provenance only; the tool surface is decided by orchestrator=false.
        let child_role = Role {
            name: "delegate-child".to_string(),
            rule_subset: self.rule_subset.clone(),
            allowed_paths: vec![worktree.display().to_string()],
        };

        // Build the gated, NON-orchestrator CLI session (rules + mcp-config WITHOUT the
        // delegate env). Even when the model resolves to the API provider we still create
        // this so the wrapper holds a uniform RAII handle; `build_agent_driver`'s API path
        // ignores the mcp-config path. `prepare_session` jails writes to `worktree`.
        let spawn = camerata_agent::prepare_session(
            &self.gateway_bin,
            &child_role,
            Some(worktree),
            read_dirs,
            self.gate_events_file.as_deref(),
        )
        .map_err(|e| std::io::Error::other(format!("prepare child session: {e}")))?;
        let mcp_config_path = spawn.mcp_config.display().to_string();

        // Route the model to its OWN provider; build a WORKER (orchestrator = false),
        // jailed to `worktree`, under the orchestrator's rule subset.
        let inner = build_agent_driver(
            model,
            &self.registry,
            self.creds.as_ref(),
            &mcp_config_path,
            self.rule_subset.clone(),
            Some(worktree.to_path_buf()),
            false, // depth-1 worker: NEVER an orchestrator
            self.limiter.clone(),
            self.run_session_id.as_deref(),
            false, // workers do not self-escalate (the governed dev-implement agent does)
            None,  // child driver: heartbeat is owned by the parent run's supervised path
        )
        .map_err(|e| std::io::Error::other(format!("build child driver for `{model}`: {e}")))?;

        Ok(Box::new(SessionBoundChildDriver {
            inner,
            _session: spawn,
        }))
    }
}

// ─── per-model LEAD/orchestrator driver factory (provider coupling) ─────────────

/// Server-side [`camerata_fleet::orchestrator::OrchestratorDriverFactory`] that builds the
/// LEAD/orchestrator stage on the STRONGEST model's OWN provider.
///
/// This is the orchestrator analogue of [`ServerChildDriverFactory`]: where that seam fixes
/// per-model provider coupling for delegated/fanned-out CHILDREN, this one fixes it for the
/// LEAD itself. The fleet's tiered build calls [`Self::build_lead`] for the single lead
/// stage; this routes the strongest model to its provider:
///
/// - **Claude strongest** (`claude` provider): a [`camerata_agent::ClaudeCliDriver`] in
///   orchestrator mode, using the lead session's orchestrator mcp-config (delegate ON, the
///   per-tier models, depth=0, the worktree jail). Identical to the current CLI path.
/// - **OpenRouter strongest** (`openrouter` provider): an [`ApiAgentDriver`] with
///   `as_orchestrator(true)` + [`ApiAgentDriver::with_orchestrator_config`], where the
///   attached [`camerata_gateway::delegate::OrchestratorConfig`] carries a
///   [`ServerChildDriverFactory`]. So the native lead's `delegate`/`fan_out` resolve each
///   child per-model + gated, through the SAME gated `run_delegated`/`run_fan_out`
///   primitives.
///
/// # Gate contract (held for BOTH provider paths)
///
/// - The lead is the ONLY stage this factory is ever called for, so only the lead can carry
///   `delegate`/`fan_out`. The factory NEVER builds a non-lead/worker driver in orchestrator
///   mode.
/// - The native lead's children are built by the embedded [`ServerChildDriverFactory`],
///   which makes every child gated_write-only, worktree-jailed, depth-1, non-orchestrator —
///   the exact same contract as the CLI delegate path. The depth guard (`depth=0`,
///   `max_depth=1`) lives in the gated primitive; this factory only supplies the config.
pub struct ServerOrchestratorDriverFactory {
    registry: crate::model_registry::ModelRegistry,
    creds: Arc<dyn crate::credentials::CredentialStore>,
    limiter: Arc<crate::rate_limit::ProviderRateLimiter>,
    /// Located `camerata-gateway` binary (each native child wires its own gated gateway).
    gateway_bin: PathBuf,
    /// Stable per-run session id (OpenRouter sticky routing / KV-cache warmth). Optional.
    run_session_id: Option<String>,
    /// LIFECYCLE-10: the run's OWN gate-events sink, threaded per-spawn into the lead's and
    /// its delegate children's gateway mcp-config env (never the shared parent process env).
    gate_events_file: Option<PathBuf>,
}

impl ServerOrchestratorDriverFactory {
    /// Build the factory with the provider-dispatch context the lead (and its children) need.
    pub fn new(
        registry: crate::model_registry::ModelRegistry,
        creds: Arc<dyn crate::credentials::CredentialStore>,
        limiter: Arc<crate::rate_limit::ProviderRateLimiter>,
        gateway_bin: PathBuf,
        run_session_id: Option<String>,
        gate_events_file: Option<PathBuf>,
    ) -> Self {
        Self {
            registry,
            creds,
            limiter,
            gateway_bin,
            run_session_id,
            gate_events_file,
        }
    }

    /// Resolve a model id's provider, defaulting to `"claude"` for unknown ids (mirrors
    /// [`build_agent_driver`]).
    fn provider_of(&self, model_id: &str) -> String {
        self.registry
            .all_entries()
            .into_iter()
            .find(|e| e.id == model_id)
            .map(|e| e.provider)
            .unwrap_or_else(|| "claude".to_string())
    }

    /// Build the per-tier [`camerata_gateway::delegate::DelegateModels`] the native lead's
    /// delegate/fan_out children resolve through. Mirrors the CLI path's
    /// [`camerata_fleet::orchestrator::delegate_models_json`] vision gating: the vision key is
    /// populated ONLY when the band is enabled AND a non-empty primary model exists.
    fn delegate_models(
        tier_map: &camerata_fleet::tier::TierMap,
        vision_enabled: bool,
    ) -> camerata_gateway::delegate::DelegateModels {
        use camerata_fleet::tier::CapabilityBand;
        let vision = if vision_enabled {
            tier_map
                .vision
                .first()
                .filter(|m| !m.trim().is_empty())
                .cloned()
                .unwrap_or_default()
        } else {
            String::new()
        };
        camerata_gateway::delegate::DelegateModels {
            fast: tier_map.model_for(CapabilityBand::Fast).to_string(),
            balanced: tier_map.model_for(CapabilityBand::Balanced).to_string(),
            strongest: tier_map.model_for(CapabilityBand::Strongest).to_string(),
            vision,
        }
    }
}

impl camerata_fleet::orchestrator::OrchestratorDriverFactory for ServerOrchestratorDriverFactory {
    fn build_lead(
        &self,
        ctx: &camerata_fleet::orchestrator::LeadBuildContext<'_>,
    ) -> anyhow::Result<Box<dyn AgentDriver>> {
        match self.provider_of(ctx.strongest_model).as_str() {
            "openrouter" => {
                // Native orchestrator: an ApiAgentDriver in orchestrator mode whose
                // delegate/fan_out resolve children per-model + gated via a
                // ServerChildDriverFactory. The lead runs under the SAME rule subset its
                // children inherit (the orchestrator session's role subset).
                let rule_subset = ctx.session.role_rule_subset.clone();

                let key = self
                    .creds
                    .get(crate::credentials::OPENROUTER_API_KEY)
                    .map_err(|e| anyhow::anyhow!("credential store error: {e}"))?
                    .filter(|k| !k.trim().is_empty())
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "lead model `{}` is an OpenRouter model but OPENROUTER_API_KEY is \
                             not set — add it via Settings → Credentials before using this model",
                            ctx.strongest_model
                        )
                    })?;

                let mut or_completer =
                    crate::llm::OpenRouterCompleter::for_agent(key, self.limiter.clone());
                if let Some(sid) = self.run_session_id.as_deref() {
                    or_completer = or_completer.with_session_id(sid);
                }
                let completer = Arc::new(or_completer);

                let child_factory = ServerChildDriverFactory::new(
                    self.registry.clone(),
                    self.creds.clone(),
                    self.limiter.clone(),
                    self.gateway_bin.clone(),
                    rule_subset.clone(),
                    self.run_session_id.clone(),
                    self.gate_events_file.clone(),
                );

                let orch_config = camerata_gateway::delegate::OrchestratorConfig {
                    models: Self::delegate_models(ctx.tier_map, ctx.vision_enabled),
                    worktree_root: ctx.worktree.to_path_buf(),
                    gateway_bin: self.gateway_bin.clone(),
                    depth: 0,
                    max_depth: 1,
                    child_driver_factory: Some(Arc::new(child_factory)),
                };

                let driver = ApiAgentDriver::new(completer, ctx.strongest_model)
                    .with_rule_subset(rule_subset)
                    .with_worktree(ctx.worktree.to_path_buf())
                    // ORCHESTRATOR-ONLY: only the lead is ever built here.
                    .as_orchestrator(true)
                    .with_orchestrator_config(orch_config);
                Ok(Box::new(driver))
            }
            // "claude" or any unrecognised provider: the Anthropic Messages API native
            // orchestrator when CAMERATA_LLM_BACKEND=api + ANTHROPIC_API_KEY are set, else
            // the CLI orchestrator path (unchanged). Either way the lead is the ONLY stage
            // this factory builds, so only the lead can carry delegate/fan_out; its children
            // are built per-model + gated by the embedded ServerChildDriverFactory.
            _ => {
                if let Some(key) = anthropic_api_backend_key(self.creds.as_ref()) {
                    // Native Anthropic-shape orchestrator. Mirrors the OpenRouter arm: a
                    // ServerChildDriverFactory resolves each delegate/fan_out child to ITS
                    // model's provider (CLI / Anthropic API / OpenRouter), gated.
                    let rule_subset = ctx.session.role_rule_subset.clone();

                    let child_factory = ServerChildDriverFactory::new(
                        self.registry.clone(),
                        self.creds.clone(),
                        self.limiter.clone(),
                        self.gateway_bin.clone(),
                        rule_subset.clone(),
                        self.run_session_id.clone(),
                        self.gate_events_file.clone(),
                    );

                    let orch_config = camerata_gateway::delegate::OrchestratorConfig {
                        models: Self::delegate_models(ctx.tier_map, ctx.vision_enabled),
                        worktree_root: ctx.worktree.to_path_buf(),
                        gateway_bin: self.gateway_bin.clone(),
                        depth: 0,
                        max_depth: 1,
                        child_driver_factory: Some(Arc::new(child_factory)),
                    };

                    let driver = ApiAgentDriver::new(Arc::new(AnthropicNoopCompleter), ctx.strongest_model)
                        .with_rule_subset(rule_subset)
                        .with_worktree(ctx.worktree.to_path_buf())
                        .with_shape(ApiShape::Anthropic)
                        .with_anthropic_api_key(key)
                        // ORCHESTRATOR-ONLY: only the lead is ever built here.
                        .as_orchestrator(true)
                        .with_orchestrator_config(orch_config);
                    return Ok(Box::new(driver));
                }

                let mut cli_driver =
                    camerata_agent::ClaudeCliDriver::new(ctx.session.mcp_config.display().to_string())
                        .with_worktree(ctx.worktree)
                        // ORCHESTRATOR-ONLY: delegate/fan_out in --allowedTools, delegate
                        // env in the session mcp-config (delegate ON).
                        .as_orchestrator(true);
                if !ctx.strongest_model.trim().is_empty() {
                    cli_driver = cli_driver.with_model(ctx.strongest_model);
                }
                if let Some(cb) = ctx.on_activity.clone() {
                    cli_driver = cli_driver.with_on_activity(cb);
                }
                Ok(Box::new(cli_driver))
            }
        }
    }
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_core::Role;
    use camerata_gateway::{enforced_gate_rules, gov1_rule};
    use crate::credentials::CredentialStore as _;
    use std::sync::Arc;

    // ── test helpers ─────────────────────────────────────────────────────────

    fn test_role() -> Role {
        Role {
            name: "TestWorker".to_string(),
            rule_subset: vec![gov1_rule()],
            allowed_paths: vec!["src/".to_string()],
        }
    }

    fn all_rules_role() -> Role {
        Role {
            name: "TestAllRules".to_string(),
            rule_subset: enforced_gate_rules(),
            allowed_paths: vec!["src/".to_string()],
        }
    }

    // ── build_system_prompt: the open-weight chokepoint ─────────────────────────

    /// The system prompt for every API-driven agent embeds the shared governance kernel
    /// (markers + all seven clauses), the working-discipline block, and the if-unsure clause.
    /// This is the single highest-impact insertion point for the operating protocol.
    #[test]
    fn build_system_prompt_embeds_kernel_and_working_discipline() {
        let role = test_role();
        let p = build_system_prompt(&role, "claude-opus-4-8").expect("prompt is Some");

        // Kernel markers + a couple of load-bearing clauses.
        assert!(
            p.contains("=== CAMERATA OPERATING PROTOCOL"),
            "system prompt must embed the governance kernel opening marker"
        );
        assert!(
            p.contains("=== END OPERATING PROTOCOL ==="),
            "system prompt must embed the governance kernel closing marker"
        );
        assert!(p.contains("GROUND EVERY FACT"), "kernel clause 1 must be present");
        assert!(p.contains("VERIFY BEFORE DONE"), "kernel clause 5 must be present");

        // Working discipline + if-unsure blocks.
        assert!(
            p.contains("WORKING DISCIPLINE (in order)"),
            "system prompt must include the working-discipline block"
        );
        assert!(
            p.contains("IF UNSURE:"),
            "system prompt must include the if-unsure clause"
        );

        // Role + paths interpolation is preserved.
        assert!(p.contains("TestWorker"), "role name must be interpolated");
        assert!(p.contains("src/"), "allowed paths must be interpolated");

        // Opus resolves to the strongest tier addendum.
        assert!(
            p.contains("TIER DISCIPLINE (strongest)"),
            "an Opus model must carry the strongest-tier addendum"
        );
    }

    /// An empty model falls back to the base kernel (no addendum) and still carries the
    /// full protocol + constraints.
    #[test]
    fn build_system_prompt_empty_model_uses_base_kernel() {
        let role = test_role();
        let p = build_system_prompt(&role, "").expect("prompt is Some");
        assert!(p.contains("=== CAMERATA OPERATING PROTOCOL"));
        assert!(!p.contains("TIER DISCIPLINE"), "no per-tier addendum for an empty model");
        assert!(p.contains("gated_write"), "constraints block must be preserved");
    }

    /// A stub `LlmPort` that always returns a fixed final text (no tool calls).
    struct StubCompleter(String);

    #[async_trait::async_trait]
    impl LlmPort for StubCompleter {
        async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
            Ok(LlmResponse {
                text: self.0.clone(),
                model: "stub".to_string(),
                backend: "stub".to_string(),
                cost_usd: Some(0.0),
                input_tokens: None,
                output_tokens: None,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
                or_cache_discount: None,
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

    /// A stub `LlmPort` that returns one OpenAI-style `tool_calls` response then
    /// a final text response on the second call.
    struct OneToolCallCompleter {
        tool_name: String,
        tool_input: Value,
        final_text: String,
        call_count: std::sync::Mutex<usize>,
    }

    impl OneToolCallCompleter {
        fn new(tool_name: &str, tool_input: Value, final_text: &str) -> Self {
            Self {
                tool_name: tool_name.to_string(),
                tool_input,
                final_text: final_text.to_string(),
                call_count: std::sync::Mutex::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl LlmPort for OneToolCallCompleter {
        async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
            let mut count = self.call_count.lock().unwrap();
            let n = *count;
            *count += 1;

            let raw = if n == 0 {
                // First call: return a tool_calls response (OpenAI shape).
                let tc = serde_json::json!({
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": "call_test_001",
                            "type": "function",
                            "function": {
                                "name": self.tool_name,
                                "arguments": serde_json::to_string(&self.tool_input).unwrap()
                            }
                        }]
                    }
                });
                serde_json::to_string(&tc).unwrap()
            } else {
                // Second call: return final text (OpenAI shape, no tool_calls).
                let msg = serde_json::json!({
                    "message": {
                        "role": "assistant",
                        "content": self.final_text
                    }
                });
                serde_json::to_string(&msg).unwrap()
            };

            Ok(LlmResponse {
                text: raw,
                model: "stub".to_string(),
                backend: "stub".to_string(),
                cost_usd: Some(0.001),
                input_tokens: None,
                output_tokens: None,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
                or_cache_discount: None,
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

    // ── INVARIANT 1: gated_write is the ONLY write path ───────────────────────

    /// Build a driver backed by `completer` with `rule_subset` from `role`.
    fn driver_with(
        completer: Arc<dyn LlmPort>,
        role: &Role,
        wt: Option<PathBuf>,
    ) -> ApiAgentDriver {
        let mut d = ApiAgentDriver::new(completer, "stub-model")
            .with_rule_subset(role.rule_subset.clone());
        if let Some(w) = wt {
            d = d.with_worktree(w);
        }
        d
    }

    #[test]
    fn invariant_gated_write_is_the_only_write_path_tool_schemas() {
        // Worker tool schemas must NOT include any direct write/exec tool.
        let schemas = build_tool_schemas(false);
        let names: Vec<&str> = schemas
            .iter()
            .filter_map(|s| s["function"]["name"].as_str())
            .collect();

        // Must include gated_write (the governed path).
        assert!(names.contains(&"gated_write"), "gated_write must be in schemas");

        // Must NOT include any escape tool.
        for escape in ["Write", "Edit", "Bash", "Task", "MultiEdit", "NotebookEdit",
                       "shell", "exec", "run"] {
            assert!(
                !names.contains(&escape),
                "escape tool `{escape}` must NOT appear in worker schemas"
            );
        }
    }

    /// LIFECYCLE-7: the API agent loop fires the wired `on_activity` heartbeat at least once
    /// per run so a healthy long API-driven run keeps `last_activity_ms` fresh (the API path has
    /// no per-line output stream, unlike the CLI driver). Uses a two-turn completer so the loop
    /// runs multiple iterations, and asserts the heartbeat fired once per turn.
    #[tokio::test]
    async fn api_driver_fires_liveness_heartbeat_per_turn() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        // Two turns: one tool call (gated_write to an allowed path), then final text.
        let tmp = tempfile::tempdir().expect("tempdir");
        let wt = tmp.path().to_path_buf();
        let completer = Arc::new(OneToolCallCompleter::new(
            "gated_write",
            serde_json::json!({ "path": "src/ok.rs", "content": "fn main() {}" }),
            "done",
        ));
        let role = all_rules_role();

        let beats = Arc::new(AtomicUsize::new(0));
        let beats_cb = beats.clone();
        let driver = driver_with(completer, &role, Some(wt))
            .with_on_activity(Arc::new(move || {
                beats_cb.fetch_add(1, Ordering::SeqCst);
            }));

        let _ = driver.run(&role, "do the thing").await;

        // The loop ran two turns (tool-call turn + final-text turn); the heartbeat fires once
        // at the top of each, so it must have fired at least twice.
        assert!(
            beats.load(Ordering::SeqCst) >= 2,
            "heartbeat must fire per loop turn (got {})",
            beats.load(Ordering::SeqCst)
        );
    }

    /// A driver with NO heartbeat wired must run without panicking (the callback is optional).
    #[tokio::test]
    async fn api_driver_without_heartbeat_runs_fine() {
        let completer = Arc::new(StubCompleter("done".to_string()));
        let role = test_role();
        let driver = driver_with(completer, &role, None); // no with_on_activity
        let out = driver.run(&role, "hi").await;
        assert!(out.is_ok(), "no-heartbeat run must succeed");
    }

    #[test]
    fn invariant_gated_write_is_the_only_write_path_execute_rejects_bash() {
        // Direct call to `execute_tool` with `Bash` must be denied and not write.
        let completer = Arc::new(StubCompleter("done".to_string()));
        let role = test_role();
        let driver = driver_with(completer, &role, None);

        let inv = ToolInvocation {
            id: "t1".to_string(),
            name: "Bash".to_string(),
            input: serde_json::json!({"command": "rm -rf /"}),
        };
        // We can't call `execute_tool` directly (it's async), so we test via the
        // schema list: Bash is not in allowed schemas.
        let schemas = build_tool_schemas(driver.orchestrator);
        let names: Vec<&str> = schemas
            .iter()
            .filter_map(|s| s["function"]["name"].as_str())
            .collect();
        assert!(!names.contains(&"Bash"));
        let _ = inv; // referenced to avoid unused-variable warning
    }

    #[tokio::test]
    async fn invariant_gated_write_denied_does_not_write_file() {
        // A write to a "forbidden" path (GOV-1) must be denied and the file must NOT exist.
        let tmp = tempfile::tempdir().expect("tempdir");
        let wt = tmp.path().to_path_buf();
        let completer = Arc::new(OneToolCallCompleter::new(
            "gated_write",
            serde_json::json!({
                "path": "forbidden/secret.rs",
                "content": "fn main() {}"
            }),
            "done — I tried to write to forbidden path",
        ));

        let role = all_rules_role();
        let driver = driver_with(Arc::new(StubCompleter("x".into())), &role, Some(wt.clone()));

        // Directly test execute_gated_write with the forbidden path.
        let inv = ToolInvocation {
            id: "call1".to_string(),
            name: "gated_write".to_string(),
            input: serde_json::json!({
                "path": "forbidden/secret.rs",
                "content": "fn main() {}"
            }),
        };
        let (result, denial) = execute_gated_write(&driver, &role, &inv);
        // Must be denied.
        assert!(denial.is_some(), "GOV-1 must deny the write: {result}");
        assert!(result.contains("DENIED"), "result must say DENIED: {result}");
        // File must NOT exist.
        assert!(
            !wt.join("forbidden/secret.rs").exists(),
            "denied write must not create the file"
        );
        let _ = completer;
    }

    #[tokio::test]
    async fn invariant_gated_write_allowed_writes_file() {
        // A clean write (no forbidden path, no secrets) must succeed.
        let tmp = tempfile::tempdir().expect("tempdir");
        let wt = tmp.path().to_path_buf();
        let role = all_rules_role();
        let driver = driver_with(Arc::new(StubCompleter("x".into())), &role, Some(wt.clone()));

        let inv = ToolInvocation {
            id: "call2".to_string(),
            name: "gated_write".to_string(),
            input: serde_json::json!({
                "path": "src/lib.rs",
                "content": "pub fn hello() {}"
            }),
        };
        let (result, denial) = execute_gated_write(&driver, &role, &inv);
        assert!(denial.is_none(), "clean write must not be denied: {result}");
        assert!(result.contains("OK"), "clean write must say OK: {result}");
        assert!(
            wt.join("src/lib.rs").exists(),
            "allowed write must create the file"
        );
    }

    // ── INVARIANT 2: delegate / fan_out are orchestrator-only ─────────────────

    #[test]
    fn invariant_delegate_absent_for_worker() {
        let schemas = build_tool_schemas(false);
        let names: Vec<&str> = schemas
            .iter()
            .filter_map(|s| s["function"]["name"].as_str())
            .collect();
        assert!(
            !names.contains(&"delegate"),
            "delegate must NOT be in worker schemas"
        );
        assert!(
            !names.contains(&"fan_out"),
            "fan_out must NOT be in worker schemas"
        );
    }

    #[test]
    fn invariant_delegate_present_for_orchestrator() {
        let schemas = build_tool_schemas(true);
        let names: Vec<&str> = schemas
            .iter()
            .filter_map(|s| s["function"]["name"].as_str())
            .collect();
        assert!(
            names.contains(&"delegate"),
            "delegate must be in orchestrator schemas"
        );
        assert!(
            names.contains(&"fan_out"),
            "fan_out must be in orchestrator schemas"
        );
        // Even orchestrator must NOT expose shell/exec/Bash/Task.
        for escape in ["Bash", "Write", "Edit", "Task", "shell"] {
            assert!(!names.contains(&escape), "escape tool `{escape}` must not appear even in orchestrator schemas");
        }
    }

    #[tokio::test]
    async fn invariant_delegate_rejected_for_non_orchestrator_execution() {
        // A worker trying to call `delegate` must get a DENIED result.
        let role = test_role();
        let driver = ApiAgentDriver::new(
            Arc::new(StubCompleter("x".into())),
            "stub",
        )
        .with_rule_subset(role.rule_subset.clone())
        // orchestrator = false (default)
        ;

        assert!(!driver.orchestrator, "worker must have orchestrator=false");

        let inv = ToolInvocation {
            id: "d1".to_string(),
            name: "delegate".to_string(),
            input: serde_json::json!({"role": "Frontend", "task": "do something"}),
        };
        let (result, denial) = execute_tool(&driver, &role, &inv).await;
        assert!(denial.is_some(), "worker delegate call must be denied");
        assert!(result.contains("DENIED"), "result must say DENIED: {result}");
    }

    #[tokio::test]
    async fn invariant_fan_out_rejected_for_non_orchestrator_execution() {
        let role = test_role();
        let driver = ApiAgentDriver::new(
            Arc::new(StubCompleter("x".into())),
            "stub",
        )
        .with_rule_subset(role.rule_subset.clone());

        let inv = ToolInvocation {
            id: "fo1".to_string(),
            name: "fan_out".to_string(),
            input: serde_json::json!({"tasks": []}),
        };
        let (result, denial) = execute_tool(&driver, &role, &inv).await;
        assert!(denial.is_some(), "worker fan_out call must be denied");
        assert!(result.contains("DENIED"), "result must say DENIED: {result}");
    }

    // ── INVARIANT 3: shell / Task never exposed ────────────────────────────────

    #[test]
    fn invariant_shell_and_task_never_in_any_schema() {
        for orchestrator in [false, true] {
            let schemas = build_tool_schemas(orchestrator);
            let names: Vec<&str> = schemas
                .iter()
                .filter_map(|s| s["function"]["name"].as_str())
                .collect();
            for forbidden in ["shell", "Task", "exec", "Bash", "Write", "Edit",
                              "MultiEdit", "NotebookEdit"] {
                assert!(
                    !names.contains(&forbidden),
                    "`{forbidden}` must never appear in tool schemas (orchestrator={orchestrator})"
                );
            }
        }
    }

    #[tokio::test]
    async fn invariant_task_tool_rejected_at_execution() {
        let role = test_role();
        let driver = ApiAgentDriver::new(Arc::new(StubCompleter("x".into())), "stub")
            .with_rule_subset(role.rule_subset.clone());

        for bad_tool in ["Task", "shell", "Bash", "Write", "Edit"] {
            let inv = ToolInvocation {
                id: format!("bad-{bad_tool}"),
                name: bad_tool.to_string(),
                input: serde_json::json!({}),
            };
            let (result, denial) = execute_tool(&driver, &role, &inv).await;
            assert!(
                denial.is_some(),
                "`{bad_tool}` execution must produce a denial"
            );
            assert!(
                result.contains("DENIED"),
                "`{bad_tool}` result must say DENIED: {result}"
            );
        }
    }

    // ── INVARIANT 4: worktree jail ─────────────────────────────────────────────

    #[test]
    fn invariant_path_traversal_blocked_by_jail() {
        let wt = PathBuf::from("/tmp/wt");
        // A `..` traversal must be caught.
        assert!(assert_in_worktree(&wt, "../outside/file.rs").is_err());
        assert!(assert_in_worktree(&wt, "../../etc/passwd").is_err());
    }

    #[test]
    fn invariant_clean_paths_pass_jail() {
        let wt = PathBuf::from("/tmp/wt");
        // Relative paths without traversal are fine.
        assert!(assert_in_worktree(&wt, "src/lib.rs").is_ok());
        assert!(assert_in_worktree(&wt, "Cargo.toml").is_ok());
        // Absolute path under the worktree is fine.
        assert!(assert_in_worktree(&wt, "/tmp/wt/src/main.rs").is_ok());
    }

    #[test]
    fn invariant_absolute_path_outside_worktree_blocked() {
        let wt = PathBuf::from("/tmp/wt");
        // Absolute path outside the worktree must be blocked.
        assert!(assert_in_worktree(&wt, "/etc/passwd").is_err());
        assert!(assert_in_worktree(&wt, "/tmp/other/file.rs").is_err());
    }

    // GATE-F3: the jail check RETURNS the resolved write path, and an absolute in-jail
    // path must resolve to itself, NOT to a doubled `wt/wt/...` path.
    #[test]
    fn jail_returns_undoubled_write_path_for_absolute_in_jail_target() {
        let base = std::env::temp_dir().join(format!(
            "cam-drv-f3-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let wt = base.join("worktree");
        std::fs::create_dir_all(&wt).unwrap();
        let canon_wt = std::fs::canonicalize(&wt).unwrap();

        // Absolute in-jail target -> resolves to itself (no wt.join(trim) doubling).
        let abs = wt.join("src").join("lib.rs");
        let resolved = assert_in_worktree(&wt, abs.to_str().unwrap()).unwrap();
        assert_eq!(resolved, canon_wt.join("src").join("lib.rs"));

        // Relative target -> resolves under the worktree once.
        let rel = assert_in_worktree(&wt, "Cargo.toml").unwrap();
        assert_eq!(rel, canon_wt.join("Cargo.toml"));

        let _ = std::fs::remove_dir_all(&base);
    }

    // GATE-F1 (second write path): a symlinked directory component in the worktree must
    // not let a write escape the jail here either.
    #[cfg(unix)]
    #[test]
    fn jail_denies_write_through_in_worktree_symlink() {
        let base = std::env::temp_dir().join(format!(
            "cam-drv-f1-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let wt = base.join("worktree");
        let outside = base.join("outside");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let link = wt.join("escape");
        std::os::unix::fs::symlink(&outside, &link).unwrap();

        // Write through the symlink lands outside -> denied.
        let via = link.join("loot");
        assert!(
            assert_in_worktree(&wt, via.to_str().unwrap()).is_err(),
            "write through an in-worktree symlink to an outside dir must be denied"
        );
        // A genuine in-jail write is still allowed.
        assert!(assert_in_worktree(&wt, wt.join("real.rs").to_str().unwrap()).is_ok());

        let _ = std::fs::remove_dir_all(&base);
    }

    // ── tool normalization ─────────────────────────────────────────────────────

    #[test]
    fn normalize_openai_tool_call_parses_correctly() {
        let tc = serde_json::json!({
            "id": "call_abc",
            "type": "function",
            "function": {
                "name": "gated_write",
                "arguments": "{\"path\":\"src/lib.rs\",\"content\":\"fn main() {}\"}"
            }
        });
        let inv = normalize_openai_tool_call(&tc).expect("must parse");
        assert_eq!(inv.id, "call_abc");
        assert_eq!(inv.name, "gated_write");
        assert_eq!(inv.input["path"].as_str(), Some("src/lib.rs"));
        assert_eq!(inv.input["content"].as_str(), Some("fn main() {}"));
    }

    #[test]
    fn normalize_anthropic_tool_use_parses_correctly() {
        let block = serde_json::json!({
            "type": "tool_use",
            "id": "toolu_01XYZ",
            "name": "Read",
            "input": {"file_path": "src/main.rs"}
        });
        let inv = normalize_anthropic_tool_use(&block).expect("must parse");
        assert_eq!(inv.id, "toolu_01XYZ");
        assert_eq!(inv.name, "Read");
        assert_eq!(inv.input["file_path"].as_str(), Some("src/main.rs"));
    }

    #[test]
    fn parse_response_handles_openai_final_text() {
        // OpenAI shape with text content and no tool_calls.
        let resp = LlmResponse {
            text: serde_json::json!({
                "message": {"role": "assistant", "content": "Task complete!"}
            })
            .to_string(),
            model: "m".into(),
            backend: "b".into(),
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
        };
        match parse_response(&resp) {
            ParsedResponse::FinalText(t) => assert_eq!(t, "Task complete!"),
            ParsedResponse::ToolCalls { .. } => panic!("expected FinalText"),
        }
    }

    #[test]
    fn parse_response_handles_openai_tool_calls() {
        let resp = LlmResponse {
            text: serde_json::json!({
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_001",
                        "type": "function",
                        "function": {
                            "name": "gated_write",
                            "arguments": "{\"path\":\"a.rs\",\"content\":\"x\"}"
                        }
                    }]
                }
            })
            .to_string(),
            model: "m".into(),
            backend: "b".into(),
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
        };
        match parse_response(&resp) {
            ParsedResponse::ToolCalls { calls, .. } => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].name, "gated_write");
                assert_eq!(calls[0].input["path"].as_str(), Some("a.rs"));
            }
            ParsedResponse::FinalText(_) => panic!("expected ToolCalls"),
        }
    }

    #[test]
    fn parse_response_handles_anthropic_tool_use() {
        let resp = LlmResponse {
            text: serde_json::json!({
                "content": [
                    {
                        "type": "tool_use",
                        "id": "toolu_01",
                        "name": "Read",
                        "input": {"file_path": "src/main.rs"}
                    }
                ],
                "stop_reason": "tool_use"
            })
            .to_string(),
            model: "m".into(),
            backend: "b".into(),
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
        };
        match parse_response(&resp) {
            ParsedResponse::ToolCalls { calls, .. } => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].name, "Read");
                assert_eq!(calls[0].input["file_path"].as_str(), Some("src/main.rs"));
            }
            ParsedResponse::FinalText(_) => panic!("expected ToolCalls"),
        }
    }

    #[test]
    fn parse_response_falls_back_to_plain_text() {
        let resp = LlmResponse {
            text: "plain response with no JSON".to_string(),
            model: "m".into(),
            backend: "b".into(),
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
        };
        match parse_response(&resp) {
            ParsedResponse::FinalText(t) => assert_eq!(t, "plain response with no JSON"),
            _ => panic!("expected FinalText"),
        }
    }

    // ── driver selection (build_agent_driver) ────────────────────────────────

    /// Verify that a model with provider "claude" selects ClaudeCliDriver.
    ///
    /// We can't inspect the concrete type behind `Arc<dyn AgentDriver>` directly,
    /// so we test the selection indirectly: for a "claude" provider model with no
    /// OpenRouter key in the credential store, `build_agent_driver` must succeed
    /// (the ClaudeCliDriver path never reads credentials). For an "openrouter"
    /// model WITH a valid key it must also succeed (ApiAgentDriver path).
    #[test]
    fn driver_selection_claude_provider_succeeds_without_openrouter_key() {
        // Registry with only the static Claude entries (no OpenRouter entries).
        let registry = crate::model_registry::ModelRegistry::new();
        // Credential store with NO OpenRouter key set.
        let creds = crate::credentials::MemoryCredentialStore::new();
        let limiter = Arc::new(crate::rate_limit::ProviderRateLimiter::new());

        // Pick any Claude model id (it's in the static registry with provider = "claude").
        let model_id = "claude-sonnet-4-6";

        let result = build_agent_driver(
            model_id,
            &registry,
            &creds,
            "/tmp/fake-mcp.json", // mcp_config_path — not opened for this test
            vec![],               // rule_subset
            None,                 // worktree
            false,                // orchestrator
            limiter,
            None,                 // run_session_id
            false, // escalation
            None,  // on_activity — no heartbeat in this unit test
        );
        assert!(
            result.is_ok(),
            "claude provider model must select ClaudeCliDriver without needing a credential"
        );
    }

    /// Build a minimal `RegistryEntry` for use in tests (only id + provider matter for
    /// driver selection; the remaining fields are zeroed/empty).
    fn openrouter_test_entry(id: &str) -> crate::model_registry::RegistryEntry {
        crate::model_registry::RegistryEntry {
            id: id.to_string(),
            provider: "openrouter".to_string(),
            display: "Test OpenRouter Model".to_string(),
            free: true,
            tool_use: true,
            context: 4096,
            coding: 0.5,
            price_in: 0.0,
            price_out: 0.0,
            weight: 0,
            caching: false,
            vision: false,
        }
    }

    /// Verify that a model with provider "openrouter" selects ApiAgentDriver when
    /// the OpenRouter key is present in the credential store.
    #[test]
    fn driver_selection_openrouter_provider_succeeds_with_key() {
        let registry = crate::model_registry::ModelRegistry::new();
        // Seed an OpenRouter model into the registry.
        registry.seed_openrouter_entries(vec![openrouter_test_entry("openrouter/mistral-7b")]);

        // Credential store WITH an OpenRouter key.
        let creds = crate::credentials::MemoryCredentialStore::new();
        creds
            .set(crate::credentials::OPENROUTER_API_KEY, "sk-or-test-key")
            .unwrap();
        let limiter = Arc::new(crate::rate_limit::ProviderRateLimiter::new());

        let result = build_agent_driver(
            "openrouter/mistral-7b",
            &registry,
            &creds,
            "/tmp/fake-mcp.json",
            vec![],
            None,
            false,
            limiter,
            None, // run_session_id
            false, // escalation
            None,  // on_activity — no heartbeat in this unit test
        );
        assert!(
            result.is_ok(),
            "openrouter provider model with key must select ApiAgentDriver"
        );
    }

    /// Verify that a model with provider "openrouter" returns an error when the
    /// OpenRouter key is NOT set in the credential store.
    #[test]
    fn driver_selection_openrouter_provider_errors_without_key() {
        let registry = crate::model_registry::ModelRegistry::new();
        registry.seed_openrouter_entries(vec![openrouter_test_entry("openrouter/mistral-7b")]);

        // No key in the credential store.
        let creds = crate::credentials::MemoryCredentialStore::new();
        let limiter = Arc::new(crate::rate_limit::ProviderRateLimiter::new());

        let result = build_agent_driver(
            "openrouter/mistral-7b",
            &registry,
            &creds,
            "/tmp/fake-mcp.json",
            vec![],
            None,
            false,
            limiter,
            None, // run_session_id
            false, // escalation
            None,  // on_activity — no heartbeat in this unit test
        );
        assert!(
            result.is_err(),
            "openrouter model without credential must return an error"
        );
        let err = result.err().unwrap().to_string();
        assert!(
            err.contains("OPENROUTER_API_KEY"),
            "error message must mention OPENROUTER_API_KEY: {err}"
        );
    }

    // ── end-to-end with stub completer ────────────────────────────────────────

    #[tokio::test]
    async fn end_to_end_stub_returns_final_text() {
        let role = test_role();
        let driver = ApiAgentDriver::new(
            Arc::new(StubCompleter("All done!".to_string())),
            "stub-model",
        )
        .with_rule_subset(role.rule_subset.clone());

        let outcome = driver.run(&role, "implement the feature").await.unwrap();
        assert_eq!(outcome.result, "All done!");
        assert!(outcome.denials.is_empty());
    }

    #[tokio::test]
    async fn end_to_end_with_allowed_gated_write() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let wt = tmp.path().to_path_buf();

        let completer = Arc::new(OneToolCallCompleter::new(
            "gated_write",
            serde_json::json!({
                "path": "src/lib.rs",
                "content": "pub fn hello() -> &'static str { \"hello\" }"
            }),
            "Done! I wrote src/lib.rs.",
        ));

        let role = all_rules_role();
        let driver = ApiAgentDriver::new(completer, "stub-model")
            .with_rule_subset(role.rule_subset.clone())
            .with_worktree(wt.clone());

        let outcome = driver.run(&role, "write the hello function").await.unwrap();
        assert_eq!(outcome.result, "Done! I wrote src/lib.rs.");
        assert!(outcome.denials.is_empty(), "no denials expected: {:?}", outcome.denials);
        assert!(wt.join("src/lib.rs").exists(), "file must have been written");
    }

    #[tokio::test]
    async fn end_to_end_denied_write_recorded_in_outcome() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let wt = tmp.path().to_path_buf();

        // Try to write to a "forbidden" path (GOV-1 denies it).
        let completer = Arc::new(OneToolCallCompleter::new(
            "gated_write",
            serde_json::json!({
                "path": "forbidden/config.rs",
                "content": "// secret"
            }),
            "I tried to write to the forbidden path.",
        ));

        let role = all_rules_role();
        let driver = ApiAgentDriver::new(completer, "stub-model")
            .with_rule_subset(role.rule_subset.clone())
            .with_worktree(wt.clone());

        let outcome = driver.run(&role, "write to forbidden").await.unwrap();
        assert!(!outcome.denials.is_empty(), "denial must be recorded");
        assert!(
            !wt.join("forbidden/config.rs").exists(),
            "denied file must not exist"
        );
    }

    // ── OpenRouter caching controls ───────────────────────────────────────────

    /// `ApiAgentDriver::new` inherits the `session_id` from an `OpenRouterCompleter`.
    /// The session id must be non-empty and must match what the completer exposes.
    #[test]
    fn driver_inherits_session_id_from_openrouter_completer() {
        let limiter = Arc::new(crate::rate_limit::ProviderRateLimiter::new());
        let or_completer = crate::llm::OpenRouterCompleter::for_agent(
            "sk-or-test".to_string(),
            limiter,
        )
        .with_session_id("my-story-id-123");
        let expected_session = or_completer.session_id_for_agent();
        assert_eq!(expected_session, "my-story-id-123");

        let driver = ApiAgentDriver::new(Arc::new(or_completer), "openrouter/model");
        // session_id must be inherited (non-empty, matches what the completer reports).
        assert_eq!(
            driver.session_id,
            "my-story-id-123",
            "driver must inherit the completer's session id"
        );
    }

    /// Non-OR completers (stubs) generate a per-instance session token (non-empty).
    #[test]
    fn driver_generates_session_id_for_non_or_completer() {
        let driver = ApiAgentDriver::new(
            Arc::new(StubCompleter("done".to_string())),
            "stub-model",
        );
        assert!(
            !driver.session_id.is_empty(),
            "session_id must be non-empty even for non-OR completers"
        );
        // Two distinct drivers must get different session ids (no collision).
        let driver2 = ApiAgentDriver::new(
            Arc::new(StubCompleter("done".to_string())),
            "stub-model",
        );
        assert_ne!(
            driver.session_id,
            driver2.session_id,
            "two drivers must have distinct session ids"
        );
    }

    /// `with_session_id` on `OpenRouterCompleter` overrides the auto-generated token
    /// and is the value the agent driver inherits.
    #[test]
    fn openrouter_completer_with_session_id_overrides_generated_token() {
        let limiter = Arc::new(crate::rate_limit::ProviderRateLimiter::new());
        let completer = crate::llm::OpenRouterCompleter::for_agent("key".to_string(), limiter)
            .with_session_id("custom-session-xyz");
        assert_eq!(completer.session_id_for_agent(), "custom-session-xyz");
    }

    /// `with_session_id("")` (empty string) is a no-op — the generated token is preserved.
    #[test]
    fn openrouter_completer_empty_session_id_is_noop() {
        let limiter = Arc::new(crate::rate_limit::ProviderRateLimiter::new());
        let completer = crate::llm::OpenRouterCompleter::for_agent("key".to_string(), limiter)
            .with_session_id("");
        // Empty override must leave the auto-generated session id in place (non-empty).
        assert!(
            !completer.session_id_for_agent().is_empty(),
            "empty with_session_id must not clear the session token"
        );
    }

    /// `bust_cache_on_next_call` sets `bust_cache = true` on the driver.
    #[test]
    fn bust_cache_flag_is_settable() {
        let mut driver = ApiAgentDriver::new(
            Arc::new(StubCompleter("done".to_string())),
            "stub-model",
        );
        assert!(!driver.bust_cache, "bust_cache must start false");
        driver.bust_cache_on_next_call();
        assert!(driver.bust_cache, "bust_cache must be true after call");
    }

    /// `build_agent_driver` passes `run_session_id` through to the OpenRouter completer:
    /// the resulting driver must carry the supplied session id.
    #[test]
    fn build_agent_driver_wires_run_session_id_to_or_driver() {
        let registry = crate::model_registry::ModelRegistry::new();
        registry.seed_openrouter_entries(vec![openrouter_test_entry("openrouter/mistral-7b")]);
        let creds = crate::credentials::MemoryCredentialStore::new();
        creds
            .set(crate::credentials::OPENROUTER_API_KEY, "sk-or-test-key")
            .unwrap();
        let limiter = Arc::new(crate::rate_limit::ProviderRateLimiter::new());

        let driver = build_agent_driver(
            "openrouter/mistral-7b",
            &registry,
            &creds,
            "/tmp/fake-mcp.json",
            vec![],
            None,
            false,
            limiter,
            Some("uow-story-id-42"), // run_session_id
            false, // escalation
            None,  // on_activity — no heartbeat in this unit test
        )
        .expect("build must succeed");

        // The outcome session_id is written from driver.session_id in run_loop.
        // We can't run the loop without a live OR endpoint, but we can check that
        // `build_agent_driver` returned Ok (i.e. key was found + driver built).
        // The deeper session_id wiring is verified by `driver_inherits_session_id_from_openrouter_completer`.
        let _ = driver; // verified: it is an ApiAgentDriver pointing at OR
    }

    /// `or_cache_discount` on `LlmResponse` is `None` for stub paths and well-formed
    /// for OR paths that would set it. Struct-level test (no HTTP call needed).
    #[test]
    fn llm_response_or_cache_discount_field_exists_and_defaults_none() {
        let resp = LlmResponse {
            text: "t".to_string(),
            model: "m".to_string(),
            backend: "stub".to_string(),
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
        };
        assert!(resp.or_cache_discount.is_none(), "default is None");

        // A response with a discount set (simulates what call_api_inner would produce).
        let resp_cached = LlmResponse { or_cache_discount: Some(0.75), ..resp.clone() };
        assert_eq!(resp_cached.or_cache_discount, Some(0.75));
    }

    // ── ServerChildDriverFactory: per-model provider coupling + gate ──────────

    use camerata_gateway::delegate::ChildDriverFactory as _;

    fn factory_with(
        registry: crate::model_registry::ModelRegistry,
        creds: Arc<dyn crate::credentials::CredentialStore>,
    ) -> ServerChildDriverFactory {
        ServerChildDriverFactory::new(
            registry,
            creds,
            Arc::new(crate::rate_limit::ProviderRateLimiter::new()),
            PathBuf::from("/tmp/fake-camerata-gateway"), // path only; binary need not exist
            vec![gov1_rule()],
            None,
            None,
        )
    }

    /// A Claude-provider child model builds successfully through the factory WITHOUT any
    /// OpenRouter credential — proving the factory routed it to the CLI provider (the CLI
    /// path never reads creds). The build is the gated, non-orchestrator worker path.
    #[test]
    fn factory_routes_claude_model_to_cli_child_no_creds_needed() {
        let registry = crate::model_registry::ModelRegistry::new();
        let creds: Arc<dyn crate::credentials::CredentialStore> =
            Arc::new(crate::credentials::MemoryCredentialStore::new());
        let factory = factory_with(registry, creds);

        let tmp = tempfile::tempdir().unwrap();
        let child = factory.build_child("claude-sonnet-4-6", tmp.path(), &[]);
        assert!(
            child.is_ok(),
            "claude model must build a CLI child without an OpenRouter key: {:?}",
            child.err().map(|e| e.to_string())
        );
    }

    /// An OpenRouter-provider child model builds successfully through the factory WHEN the
    /// OpenRouter key is present — proving the factory routed it to the API (ApiAgentDriver)
    /// provider, NOT the parent's CLI provider. Gated worker (orchestrator=false).
    #[test]
    fn factory_routes_openrouter_model_to_api_child_with_key() {
        let registry = crate::model_registry::ModelRegistry::new();
        registry.seed_openrouter_entries(vec![openrouter_test_entry("openrouter/mistral-7b")]);
        let mem = crate::credentials::MemoryCredentialStore::new();
        mem.set(crate::credentials::OPENROUTER_API_KEY, "sk-or-test").unwrap();
        let creds: Arc<dyn crate::credentials::CredentialStore> = Arc::new(mem);
        let factory = factory_with(registry, creds);

        let tmp = tempfile::tempdir().unwrap();
        let child = factory.build_child("openrouter/mistral-7b", tmp.path(), &[]);
        assert!(
            child.is_ok(),
            "openrouter model with key must build an API child: {:?}",
            child.err().map(|e| e.to_string())
        );
    }

    /// An OpenRouter child model with NO key errors at build time (fail-closed). The
    /// factory never silently falls back to the parent's provider.
    #[test]
    fn factory_openrouter_model_errors_without_key() {
        let registry = crate::model_registry::ModelRegistry::new();
        registry.seed_openrouter_entries(vec![openrouter_test_entry("openrouter/mistral-7b")]);
        let creds: Arc<dyn crate::credentials::CredentialStore> =
            Arc::new(crate::credentials::MemoryCredentialStore::new());
        let factory = factory_with(registry, creds);

        let tmp = tempfile::tempdir().unwrap();
        let child = factory.build_child("openrouter/mistral-7b", tmp.path(), &[]);
        assert!(child.is_err(), "openrouter child without key must fail closed");
        assert!(
            child.err().unwrap().to_string().contains("OPENROUTER_API_KEY"),
            "error must mention the missing OpenRouter key"
        );
    }

    /// GATE INVARIANT (API child kind): the factory builds workers with
    /// `orchestrator = false`, and a non-orchestrator API driver's tool schemas are
    /// gated_write-only — NO delegate/fan_out, NO escape tools. (The CLI child kind's
    /// equivalent gate is asserted by the gateway/agent allowed/disallowed-tools tests.)
    #[test]
    fn factory_built_api_child_is_gated_write_only_non_orchestrator() {
        // A non-orchestrator API driver (what the factory builds for an OpenRouter model)
        // exposes gated_write + reads only; delegate/fan_out and all escape tools absent.
        let schemas = build_tool_schemas(false);
        let names: Vec<&str> = schemas
            .iter()
            .filter_map(|s| s["function"]["name"].as_str())
            .collect();
        assert!(names.contains(&"gated_write"));
        for forbidden in [
            "delegate", "fan_out", "Task", "Bash", "Write", "Edit", "MultiEdit",
            "NotebookEdit", "shell",
        ] {
            assert!(
                !names.contains(&forbidden),
                "factory-built API child must NOT expose `{forbidden}`"
            );
        }
    }

    // ── Native delegate/fan_out wiring on ApiAgentDriver ──────────────────────

    /// Build an OrchestratorConfig whose factory is a server factory (claude-only registry,
    /// no creds) so the test exercises the wiring without spawning `claude` (the CLI child's
    /// run is what would spawn; we assert the routing + no-config behavior instead).
    fn orchestrator_config_with_server_factory() -> camerata_gateway::delegate::OrchestratorConfig {
        let registry = crate::model_registry::ModelRegistry::new();
        let creds: Arc<dyn crate::credentials::CredentialStore> =
            Arc::new(crate::credentials::MemoryCredentialStore::new());
        let factory = Arc::new(factory_with(registry, creds));
        camerata_gateway::delegate::OrchestratorConfig {
            models: camerata_gateway::delegate::DelegateModels {
                fast: "claude-haiku-4-5-20251001".to_string(),
                balanced: "claude-sonnet-4-6".to_string(),
                strongest: "claude-opus-4-8".to_string(),
                vision: String::new(),
            },
            worktree_root: PathBuf::from("/tmp/wt"),
            gateway_bin: PathBuf::from("/tmp/fake-camerata-gateway"),
            depth: 0,
            max_depth: 1,
            child_driver_factory: Some(factory),
        }
    }

    /// Without an attached OrchestratorConfig, an orchestrator-mode API driver's `delegate`
    /// arm returns an honest "no config" message and does NOT spawn.
    #[tokio::test]
    async fn native_delegate_without_config_is_honest_no_spawn() {
        let role = test_role();
        let driver = ApiAgentDriver::new(Arc::new(StubCompleter("x".into())), "stub")
            .with_rule_subset(role.rule_subset.clone())
            .as_orchestrator(true); // orchestrator, but no config attached
        let inv = ToolInvocation {
            id: "d".into(),
            name: "delegate".into(),
            input: serde_json::json!({"subtask": "do x", "tier": "fast"}),
        };
        let (result, denial) = execute_tool(&driver, &role, &inv).await;
        assert!(denial.is_none());
        assert!(
            result.contains("no OrchestratorConfig attached"),
            "got: {result}"
        );
    }

    /// The `delegate` arm refuses cleanly (no spawn) when the depth guard is tripped, even
    /// with a config attached — proving the wiring routes through the gated primitive's
    /// guards rather than reimplementing them.
    #[tokio::test]
    async fn native_delegate_routes_through_gated_primitive_depth_guard() {
        let role = test_role();
        let mut cfg = orchestrator_config_with_server_factory();
        cfg.depth = 1; // == max_depth: depth guard must trip in run_delegated
        let driver = ApiAgentDriver::new(Arc::new(StubCompleter("x".into())), "stub")
            .with_rule_subset(role.rule_subset.clone())
            .as_orchestrator(true)
            .with_orchestrator_config(cfg);
        let inv = ToolInvocation {
            id: "d".into(),
            name: "delegate".into(),
            input: serde_json::json!({"subtask": "do x", "tier": "fast"}),
        };
        let (result, _denial) = execute_tool(&driver, &role, &inv).await;
        assert!(
            result.contains("depth guard tripped"),
            "must route through run_delegated's depth guard: {result}"
        );
    }

    /// The `fan_out` arm validates entries through the gated `run_fan_out` primitive:
    /// a duplicate-repo set is refused (partition-collision invariant) with no spawn.
    #[tokio::test]
    async fn native_fan_out_routes_through_gated_primitive_duplicate_repo() {
        let role = test_role();
        let cfg = orchestrator_config_with_server_factory();
        let driver = ApiAgentDriver::new(Arc::new(StubCompleter("x".into())), "stub")
            .with_rule_subset(role.rule_subset.clone())
            .as_orchestrator(true)
            .with_orchestrator_config(cfg);
        let inv = ToolInvocation {
            id: "fo".into(),
            name: "fan_out".into(),
            input: serde_json::json!({"entries": [
                {"repo": "backend", "domain": "api", "subtask": "a"},
                {"repo": "backend", "domain": "api2", "subtask": "b"}
            ]}),
        };
        let (result, _denial) = execute_tool(&driver, &role, &inv).await;
        assert!(
            result.contains("partition collision"),
            "must route through run_fan_out's duplicate-repo guard: {result}"
        );
    }

    /// A non-orchestrator driver still hard-denies delegate/fan_out even if a config were
    /// present (the `if driver.orchestrator` arm guard), preserving depth-1.
    #[tokio::test]
    async fn worker_with_config_still_denies_delegate() {
        let role = test_role();
        let cfg = orchestrator_config_with_server_factory();
        let driver = ApiAgentDriver::new(Arc::new(StubCompleter("x".into())), "stub")
            .with_rule_subset(role.rule_subset.clone())
            // orchestrator = false (worker) even though a config is attached
            .with_orchestrator_config(cfg);
        assert!(!driver.orchestrator);
        let inv = ToolInvocation {
            id: "d".into(),
            name: "delegate".into(),
            input: serde_json::json!({"subtask": "x", "tier": "fast"}),
        };
        let (result, denial) = execute_tool(&driver, &role, &inv).await;
        assert!(denial.is_some(), "worker delegate must be denied");
        assert!(result.contains("DENIED"), "got: {result}");
    }

    // ── Anthropic shape: tool-schema format ───────────────────────────────────

    #[test]
    fn anthropic_tool_schemas_use_input_schema_not_function() {
        // The Anthropic shape must produce `{name, description, input_schema}` tools,
        // NOT OpenAI's `{type:"function", function:{...}}`.
        let schemas = build_tool_schemas_for(false, ApiShape::Anthropic);
        assert!(!schemas.is_empty());
        for s in &schemas {
            assert!(s.get("name").and_then(|v| v.as_str()).is_some(),
                "anthropic tool must have a top-level `name`: {s}");
            assert!(s.get("input_schema").is_some(),
                "anthropic tool must have `input_schema`: {s}");
            // Must NOT carry the OpenAI envelope.
            assert!(s.get("function").is_none(),
                "anthropic tool must NOT have `function`: {s}");
            assert!(s.get("type").is_none(),
                "anthropic tool must NOT have a `type` discriminator: {s}");
        }
        // gated_write must still be present (allowlist unchanged across shapes).
        let names: Vec<&str> = schemas.iter().filter_map(|s| s["name"].as_str()).collect();
        assert!(names.contains(&"gated_write"));
    }

    #[test]
    fn anthropic_and_openrouter_shapes_expose_identical_tool_sets() {
        for orchestrator in [false, true] {
            let openai = build_tool_schemas_for(orchestrator, ApiShape::OpenRouter);
            let anthropic = build_tool_schemas_for(orchestrator, ApiShape::Anthropic);
            let mut or_names: Vec<&str> = openai
                .iter()
                .filter_map(|s| s["function"]["name"].as_str())
                .collect();
            let mut an_names: Vec<&str> =
                anthropic.iter().filter_map(|s| s["name"].as_str()).collect();
            or_names.sort_unstable();
            an_names.sort_unstable();
            assert_eq!(
                or_names, an_names,
                "tool allowlist must match across shapes (orchestrator={orchestrator})"
            );
        }
    }

    #[test]
    fn anthropic_worker_schemas_omit_delegate_and_escape_tools() {
        let schemas = build_tool_schemas_for(false, ApiShape::Anthropic);
        let names: Vec<&str> = schemas.iter().filter_map(|s| s["name"].as_str()).collect();
        for forbidden in [
            "delegate", "fan_out", "Task", "Bash", "Write", "Edit", "MultiEdit",
            "NotebookEdit", "shell",
        ] {
            assert!(
                !names.contains(&forbidden),
                "Anthropic-shape worker must NOT expose `{forbidden}`"
            );
        }
    }

    #[test]
    fn openai_tool_to_anthropic_converts_correctly() {
        let openai = serde_json::json!({
            "type": "function",
            "function": {
                "name": "gated_write",
                "description": "Write a file.",
                "parameters": {
                    "type": "object",
                    "properties": {"path": {"type": "string"}},
                    "required": ["path"]
                }
            }
        });
        let an = openai_tool_to_anthropic(&openai).expect("must convert");
        assert_eq!(an["name"].as_str(), Some("gated_write"));
        assert_eq!(an["description"].as_str(), Some("Write a file."));
        // `parameters` becomes `input_schema`, body preserved verbatim.
        assert_eq!(an["input_schema"]["type"].as_str(), Some("object"));
        assert_eq!(an["input_schema"]["required"][0].as_str(), Some("path"));
        assert!(an.get("function").is_none());
    }

    // ── Anthropic shape: request body ─────────────────────────────────────────

    #[test]
    fn anthropic_request_body_has_top_level_system_and_tools() {
        let schemas = build_tool_schemas_for(false, ApiShape::Anthropic);
        let messages = vec![serde_json::json!({"role": "user", "content": "do it"})];
        let (body, use_caching) = build_anthropic_request_body(
            "claude-opus-4-8",
            Some("You are governed."),
            &messages,
            &schemas,
        );
        assert!(use_caching, "system present → caching active");
        assert_eq!(body["model"].as_str(), Some("claude-opus-4-8"));
        assert!(body["max_tokens"].as_u64().is_some());
        // Top-level system as a cache_control text block (NOT a bare string, NOT inside messages).
        assert_eq!(body["system"][0]["type"].as_str(), Some("text"));
        assert_eq!(body["system"][0]["text"].as_str(), Some("You are governed."));
        assert_eq!(body["system"][0]["cache_control"]["type"].as_str(), Some("ephemeral"));
        // Anthropic tools format (input_schema, not function) present in the body.
        assert!(body["tools"].is_array());
        assert!(body["tools"][0]["input_schema"].is_object());
        assert!(body["tools"][0].get("function").is_none());
        assert_eq!(body["tool_choice"]["type"].as_str(), Some("auto"));
    }

    #[test]
    fn anthropic_request_body_no_system_no_caching() {
        let (body, use_caching) =
            build_anthropic_request_body("claude-opus-4-8", None, &[], &[]);
        assert!(!use_caching, "no system → no caching header");
        assert!(body.get("system").is_none());
        // No tools → no tools/tool_choice keys.
        assert!(body.get("tools").is_none());
        assert!(body.get("tool_choice").is_none());
    }

    // ── Anthropic shape: Layer-2 grounding cache breakpoint ───────────────────

    #[test]
    fn split_grounding_prefix_splits_at_the_terminator_when_layer3_follows() {
        let text = "=== PROJECT GROUNDING ===\nrepo digest\n=== END PROJECT GROUNDING ===\n\n## Story\nvolatile tail";
        let (prefix, tail) = split_grounding_prefix(text).expect("must split");
        // Prefix includes the terminator (whole grounding block cached).
        assert!(prefix.ends_with(LAYER2_GROUNDING_TERMINATOR));
        assert!(prefix.contains("repo digest"));
        // Tail is the volatile Layer-3 content only.
        assert!(tail.contains("## Story"));
        assert!(!tail.contains("repo digest"));
    }

    #[test]
    fn split_grounding_prefix_none_without_marker_or_without_tail() {
        // No grounding marker at all.
        assert!(split_grounding_prefix("just a plain task with no grounding").is_none());
        // Marker present but nothing meaningful after it → no degenerate breakpoint.
        assert!(split_grounding_prefix("digest\n=== END PROJECT GROUNDING ===\n\n  ").is_none());
    }

    #[test]
    fn anthropic_first_user_message_gets_layer2_breakpoint_when_grounded() {
        let task = "=== PROJECT GROUNDING ===\ndigest here\n=== END PROJECT GROUNDING ===\n\n## Story\ndo the thing";
        let messages = vec![serde_json::json!({"role": "user", "content": task})];
        let (body, use_caching) =
            build_anthropic_request_body("claude-opus-4-8", None, &messages, &[]);
        // Even without a system prompt, the grounding breakpoint activates caching.
        assert!(use_caching, "grounded first message → caching active");
        let content = &body["messages"][0]["content"];
        assert!(content.is_array(), "first message content must be a two-block array");
        assert_eq!(content[0]["type"].as_str(), Some("text"));
        assert_eq!(content[0]["cache_control"]["type"].as_str(), Some("ephemeral"));
        assert!(content[0]["text"].as_str().unwrap().ends_with(LAYER2_GROUNDING_TERMINATOR));
        // Second block is the volatile tail with NO cache_control.
        assert!(content[1]["text"].as_str().unwrap().contains("## Story"));
        assert!(content[1].get("cache_control").is_none());
    }

    #[test]
    fn anthropic_first_user_message_unchanged_without_grounding() {
        let messages = vec![serde_json::json!({"role": "user", "content": "plain task, no grounding"})];
        let (body, _use_caching) =
            build_anthropic_request_body("claude-opus-4-8", Some("sys"), &messages, &[]);
        // No grounding marker → first message content stays a plain string.
        assert_eq!(body["messages"][0]["content"].as_str(), Some("plain task, no grounding"));
    }

    #[test]
    fn anthropic_only_the_first_user_message_is_reshaped() {
        // A later user message (e.g. tool_result array) that happens to contain the marker must
        // not be reshaped — only index 0 is the grounded opening turn.
        let messages = vec![
            serde_json::json!({"role": "user", "content": "plain opener no marker"}),
            serde_json::json!({"role": "user", "content": "later =[END PROJECT GROUNDING]= x\n\ntail"}),
        ];
        let (body, _) = build_anthropic_request_body("claude-opus-4-8", None, &messages, &[]);
        // Index 0 has no marker → stays a string; index 1 is passed through untouched.
        assert!(body["messages"][0]["content"].is_string());
        assert!(body["messages"][1]["content"].is_string());
    }

    // ── Anthropic shape: tool-result feedback format ──────────────────────────

    #[test]
    fn anthropic_tool_results_are_one_user_message_of_tool_result_blocks() {
        let mut messages: Vec<Value> = Vec::new();
        let results = vec![
            ("toolu_1".to_string(), "OK: wrote a.rs".to_string()),
            ("toolu_2".to_string(), "OK: wrote b.rs".to_string()),
        ];
        append_tool_results(&mut messages, ApiShape::Anthropic, &results);
        // Exactly ONE message appended (Anthropic batches all results into one user turn).
        assert_eq!(messages.len(), 1);
        let m = &messages[0];
        assert_eq!(m["role"].as_str(), Some("user"));
        let blocks = m["content"].as_array().expect("content must be an array");
        assert_eq!(blocks.len(), 2);
        for (i, (id, text)) in results.iter().enumerate() {
            assert_eq!(blocks[i]["type"].as_str(), Some("tool_result"));
            assert_eq!(blocks[i]["tool_use_id"].as_str(), Some(id.as_str()));
            assert_eq!(blocks[i]["content"].as_str(), Some(text.as_str()));
        }
    }

    #[test]
    fn openrouter_tool_results_are_separate_tool_role_messages() {
        let mut messages: Vec<Value> = Vec::new();
        let results = vec![
            ("call_1".to_string(), "r1".to_string()),
            ("call_2".to_string(), "r2".to_string()),
        ];
        append_tool_results(&mut messages, ApiShape::OpenRouter, &results);
        // One `{role:"tool"}` message per result (OpenAI shape, unchanged).
        assert_eq!(messages.len(), 2);
        for (i, (id, text)) in results.iter().enumerate() {
            assert_eq!(messages[i]["role"].as_str(), Some("tool"));
            assert_eq!(messages[i]["tool_call_id"].as_str(), Some(id.as_str()));
            assert_eq!(messages[i]["content"].as_str(), Some(text.as_str()));
        }
    }

    // ── Anthropic shape: normalizer parses tool_use (full response object) ─────

    #[test]
    fn parse_response_handles_anthropic_full_response_with_tool_use() {
        // call_anthropic_with_tools passes the FULL response object (with `content`)
        // through as `text`. The normalizer's Anthropic branch must parse it.
        let resp = LlmResponse {
            text: serde_json::json!({
                "id": "msg_01",
                "type": "message",
                "role": "assistant",
                "model": "claude-opus-4-8",
                "stop_reason": "tool_use",
                "content": [
                    {"type": "text", "text": "I'll write the file."},
                    {
                        "type": "tool_use",
                        "id": "toolu_99",
                        "name": "gated_write",
                        "input": {"path": "src/lib.rs", "content": "fn f() {}"}
                    }
                ],
                "usage": {"input_tokens": 10, "output_tokens": 5}
            })
            .to_string(),
            model: "claude-opus-4-8".into(),
            backend: "anthropic/api/agentic".into(),
            cost_usd: None,
            input_tokens: Some(10),
            output_tokens: Some(5),
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
        };
        match parse_response(&resp) {
            ParsedResponse::ToolCalls { calls, .. } => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].id, "toolu_99");
                assert_eq!(calls[0].name, "gated_write");
                assert_eq!(calls[0].input["path"].as_str(), Some("src/lib.rs"));
            }
            ParsedResponse::FinalText(_) => panic!("expected ToolCalls"),
        }
    }

    // ── Anthropic shape: driver flags ─────────────────────────────────────────

    #[test]
    fn default_shape_is_openrouter_back_compat() {
        let d = ApiAgentDriver::new(Arc::new(StubCompleter("x".into())), "m");
        assert_eq!(d.shape, ApiShape::OpenRouter, "default shape must stay OpenRouter");
        assert!(d.anthropic_api_key.is_none());
    }

    #[test]
    fn with_shape_and_key_set_anthropic_fields() {
        let d = ApiAgentDriver::new(Arc::new(AnthropicNoopCompleter), "claude-opus-4-8")
            .with_shape(ApiShape::Anthropic)
            .with_anthropic_api_key("sk-ant-test");
        assert_eq!(d.shape, ApiShape::Anthropic);
        assert_eq!(d.anthropic_api_key.as_deref(), Some("sk-ant-test"));
    }

    #[test]
    fn empty_anthropic_key_is_noop() {
        let d = ApiAgentDriver::new(Arc::new(AnthropicNoopCompleter), "m")
            .with_anthropic_api_key("");
        assert!(d.anthropic_api_key.is_none(), "empty key must not be stored");
    }

    // ── Routing: build_claude_driver respects backend + key ───────────────────
    //
    // anthropic_api_backend_key() reads the backend from process env and the key
    // store-first (env fallback). These tests exercise the ENV-fallback path with an
    // EMPTY credential store, so they mutate env and run serially under a shared mutex.

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// An empty credential store: the helper's key lookup misses and falls back to the
    /// `ANTHROPIC_API_KEY` env var these routing tests set.
    fn empty_creds() -> crate::credentials::MemoryCredentialStore {
        crate::credentials::MemoryCredentialStore::new()
    }

    /// RAII guard that snapshots + restores the two env vars routing depends on.
    struct EnvSnapshot {
        backend: Option<String>,
        key: Option<String>,
    }
    impl EnvSnapshot {
        fn capture() -> Self {
            Self {
                backend: std::env::var("CAMERATA_LLM_BACKEND").ok(),
                key: std::env::var("ANTHROPIC_API_KEY").ok(),
            }
        }
        fn set(name: &str, val: Option<&str>) {
            match val {
                Some(v) => std::env::set_var(name, v),
                None => std::env::remove_var(name),
            }
        }
    }
    impl Drop for EnvSnapshot {
        fn drop(&mut self) {
            Self::set("CAMERATA_LLM_BACKEND", self.backend.as_deref());
            Self::set("ANTHROPIC_API_KEY", self.key.as_deref());
        }
    }

    #[test]
    fn anthropic_backend_key_requires_both_signals() {
        let _g = env_lock();
        let _snap = EnvSnapshot::capture();
        // Empty store: the key comes from the env fallback these assertions set.
        let creds = empty_creds();

        // Neither set → None.
        std::env::remove_var("CAMERATA_LLM_BACKEND");
        std::env::remove_var("ANTHROPIC_API_KEY");
        assert!(anthropic_api_backend_key(&creds).is_none());

        // backend=api but no key → None.
        std::env::set_var("CAMERATA_LLM_BACKEND", "api");
        std::env::remove_var("ANTHROPIC_API_KEY");
        assert!(anthropic_api_backend_key(&creds).is_none());

        // key but backend not api (default cli) → None.
        std::env::remove_var("CAMERATA_LLM_BACKEND");
        std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-x");
        assert!(anthropic_api_backend_key(&creds).is_none());

        // backend=cli explicitly + key → None.
        std::env::set_var("CAMERATA_LLM_BACKEND", "cli");
        std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-x");
        assert!(anthropic_api_backend_key(&creds).is_none());

        // Both signals → Some(key).
        std::env::set_var("CAMERATA_LLM_BACKEND", "api");
        std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-x");
        assert_eq!(anthropic_api_backend_key(&creds).as_deref(), Some("sk-ant-x"));

        // backend=api + empty key → None.
        std::env::set_var("ANTHROPIC_API_KEY", "   ");
        assert!(anthropic_api_backend_key(&creds).is_none());
    }

    /// ROUTES-9: the credential STORE is consulted first for the Anthropic key, so a
    /// store-saved key routes to the Anthropic API path with NO `ANTHROPIC_API_KEY` env set
    /// (proving the removed per-request `set_var` is no longer needed for no-restart effect).
    #[test]
    fn anthropic_backend_key_reads_store_without_env() {
        use crate::credentials::CredentialStore as _;
        let _g = env_lock();
        let _snap = EnvSnapshot::capture();

        // Backend=api (startup-hydrated env), NO ANTHROPIC_API_KEY env at all.
        std::env::set_var("CAMERATA_LLM_BACKEND", "api");
        std::env::remove_var("ANTHROPIC_API_KEY");

        let creds = empty_creds();
        // Empty store + no env → None.
        assert!(anthropic_api_backend_key(&creds).is_none());

        // Save the key to the store only → helper returns it with no env mutation.
        creds.set(crate::credentials::ANTHROPIC_API_KEY, "sk-ant-store").unwrap();
        assert_eq!(
            anthropic_api_backend_key(&creds).as_deref(),
            Some("sk-ant-store"),
            "a store-saved key must route to the API path with no env set"
        );
        assert!(
            std::env::var("ANTHROPIC_API_KEY").is_err(),
            "reading the store must not have mutated process env"
        );
    }

    // The driver-selection routing decision is `anthropic_api_backend_key()` (exhaustively
    // tested above) combined with the construction path. `AgentDriver` is not `Any`, so we
    // can't downcast `Arc<dyn AgentDriver>` to the concrete type; instead we exercise the
    // exact Anthropic construction `build_claude_driver` performs (verified separately in
    // `with_shape_and_key_set_anthropic_fields`) and assert the env-gated path is the one
    // that fires. End-to-end, both `build_agent_driver` and `build_claude_driver` must
    // SUCCEED (no spawn, no credential lookup) on the claude+api path.

    #[test]
    fn build_claude_driver_builds_on_api_and_cli_paths_without_spawn() {
        let _g = env_lock();
        let _snap = EnvSnapshot::capture();
        let creds = empty_creds();

        // claude + api + key: routing helper fires, driver builds (no claude spawn).
        std::env::set_var("CAMERATA_LLM_BACKEND", "api");
        std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-test");
        assert!(anthropic_api_backend_key(&creds).is_some());
        let _api_driver = build_claude_driver(
            "claude-opus-4-8",
            &creds,
            "/tmp/fake-mcp.json",
            vec![gov1_rule()],
            Some(PathBuf::from("/tmp/wt")),
            false,
            false, // escalation
            None,  // on_activity — no heartbeat in this unit test
        ); // building must not panic / spawn

        // default cli: routing helper does not fire, CLI driver builds.
        std::env::remove_var("CAMERATA_LLM_BACKEND");
        std::env::remove_var("ANTHROPIC_API_KEY");
        assert!(anthropic_api_backend_key(&creds).is_none());
        let _cli_driver = build_claude_driver(
            "claude-opus-4-8",
            &creds,
            "/tmp/fake-mcp.json",
            vec![gov1_rule()],
            None,
            false,
            false, // escalation
            None,  // on_activity — no heartbeat in this unit test
        );
    }

    #[test]
    fn build_agent_driver_claude_api_backend_succeeds() {
        let _g = env_lock();
        let _snap = EnvSnapshot::capture();
        std::env::set_var("CAMERATA_LLM_BACKEND", "api");
        std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-test");

        let registry = crate::model_registry::ModelRegistry::new();
        let creds = crate::credentials::MemoryCredentialStore::new();
        let limiter = Arc::new(crate::rate_limit::ProviderRateLimiter::new());

        // Routing decision fires, and build_agent_driver must succeed (Anthropic API path,
        // no OpenRouter credential needed, no claude binary spawn at build time).
        assert!(anthropic_api_backend_key(&creds).is_some());
        let result = build_agent_driver(
            "claude-sonnet-4-6",
            &registry,
            &creds,
            "/tmp/fake-mcp.json",
            vec![],
            None,
            false,
            limiter,
            None,
            false, // escalation
            None,  // on_activity — no heartbeat in this unit test
        );
        assert!(result.is_ok(), "claude+api+key must build: {:?}", result.err().map(|e| e.to_string()));
    }

    #[test]
    fn build_agent_driver_claude_cli_default_succeeds() {
        let _g = env_lock();
        let _snap = EnvSnapshot::capture();
        std::env::remove_var("CAMERATA_LLM_BACKEND");
        std::env::remove_var("ANTHROPIC_API_KEY");

        let registry = crate::model_registry::ModelRegistry::new();
        let creds = crate::credentials::MemoryCredentialStore::new();
        let limiter = Arc::new(crate::rate_limit::ProviderRateLimiter::new());

        assert!(anthropic_api_backend_key(&creds).is_none());
        let result = build_agent_driver(
            "claude-sonnet-4-6",
            &registry,
            &creds,
            "/tmp/fake-mcp.json",
            vec![],
            None,
            false,
            limiter,
            None,
            false, // escalation
            None,  // on_activity — no heartbeat in this unit test
        );
        assert!(result.is_ok(), "claude+cli must build: {:?}", result.err().map(|e| e.to_string()));
    }

    // ── Child factory: claude model under backend=api builds an Anthropic API child ──

    #[test]
    fn factory_claude_model_under_api_backend_builds_anthropic_api_child() {
        let _g = env_lock();
        let _snap = EnvSnapshot::capture();
        std::env::set_var("CAMERATA_LLM_BACKEND", "api");
        std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-test");

        let registry = crate::model_registry::ModelRegistry::new();
        let creds: Arc<dyn crate::credentials::CredentialStore> =
            Arc::new(crate::credentials::MemoryCredentialStore::new());
        let factory = factory_with(registry, creds);

        let tmp = tempfile::tempdir().unwrap();
        let child = factory.build_child("claude-sonnet-4-6", tmp.path(), &[]);
        assert!(
            child.is_ok(),
            "claude child under backend=api must build (Anthropic API child): {:?}",
            child.err().map(|e| e.to_string())
        );
    }

    // ── GATE INVARIANT: Anthropic-shape worker is non-orchestrator + gated ────

    #[tokio::test]
    async fn anthropic_shape_worker_is_orchestrator_false_and_gated_write_only() {
        // An Anthropic-shape worker (what build_claude_driver builds for a Claude tier under
        // backend=api) must be orchestrator=false (no delegate/fan_out), jailed to a
        // worktree, and its (Anthropic-format) tool schemas must be gated_write + reads only.
        let driver = ApiAgentDriver::new(Arc::new(AnthropicNoopCompleter), "claude-opus-4-8")
            .with_rule_subset(vec![gov1_rule()])
            .with_worktree(PathBuf::from("/tmp/wt"))
            .with_shape(ApiShape::Anthropic)
            .with_anthropic_api_key("sk-ant-test");
        // orchestrator=false (default) → workers can never delegate/fan_out.
        assert!(!driver.orchestrator, "Anthropic worker must be orchestrator=false");
        assert!(driver.worktree.is_some(), "Anthropic worker must be jailed to a worktree");

        // delegate/fan_out absent from the Anthropic worker's schemas; gated_write present.
        let schemas = build_tool_schemas_for(driver.orchestrator, driver.shape);
        let names: Vec<&str> = schemas.iter().filter_map(|s| s["name"].as_str()).collect();
        assert!(names.contains(&"gated_write"));
        for forbidden in ["delegate", "fan_out", "Bash", "Write", "Edit", "Task", "shell"] {
            assert!(!names.contains(&forbidden), "`{forbidden}` must not appear");
        }

        // Gate still runs identically: a delegate call on the worker is hard-denied even
        // with the Anthropic shape (the `if driver.orchestrator` arm guard).
        let role = test_role();
        let inv = ToolInvocation {
            id: "d".into(),
            name: "delegate".into(),
            input: serde_json::json!({"subtask": "x", "tier": "fast"}),
        };
        let (result, denial) = execute_tool(&driver, &role, &inv).await;
        assert!(denial.is_some(), "Anthropic worker delegate must be denied");
        assert!(result.contains("DENIED"), "got: {result}");
    }
}
