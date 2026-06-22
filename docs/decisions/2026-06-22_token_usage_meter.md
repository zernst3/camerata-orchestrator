# Provider-agnostic cumulative token/$ usage meter + rate-limited state

Date: 2026-06-22
Status: Accepted

## Context

Camerata had per-audit usage tracking only: `ai_audit::UsageMeter` sums input/output/cost/cache
tokens for ONE scan (its passes + calibration) to power the post-scan actual-vs-estimated readout.
There was no SESSION-WIDE view of what the process is spending across ALL model calls (the audit,
the research chat, story authoring, decomposition, clarification suggestion, routine-prompt
authoring, severity calibration, the escalation translator), and no explicit signal for when the
provider is rate-limiting us. Operators flew blind on cumulative spend and on throttling.

Camerata is also vendor-neutral by design (`llm.rs`): Anthropic is wired today, Gemini/OpenAI are
reserved arms. Any usage view had to work for Claude now and for a future Gemini arm WITHOUT change.

## Decision

### 1. Process/session-global ledger

A new `usage_ledger::UsageLedger` (Arc, interior-mutable: atomics for scalar counters, one `Mutex`
for the small per-model map + last rate-limit event) lives in `AppState`. It accumulates
`input_tokens`, `output_tokens`, `cache_read`, `cache_creation`, `total_cost_usd`, `calls`, and a
`by_model` breakdown (`{model, tokens, cost, calls}`). `record(model_id, &LlmResponse)` folds one
completion in. It is the cumulative counterpart to the per-audit `UsageMeter`; both coexist (the
audit still uses `UsageMeter` for its local readout).

### 2. Provider-agnostic cost-fallback rule

The ledger keys off the vendor-neutral `LlmResponse` usage fields, NOT any Anthropic-specific shape:

1. If `cost_usd` is present (the Anthropic CLI reports a dollar figure) -> add it verbatim.
2. Else if tokens are present and the model id is in `MODELS` -> DERIVE cost from list pricing
   (`input * price_in + output * price_out`, $/Mtok). This is the Gemini shape (tokens reported,
   no cost field): Gemini still yields a $ figure the moment its `MODELS` entries + `complete` arm
   land, with zero changes here.
3. Else (model id not in `MODELS`) -> tokens accumulate at `$0` cost. Never a panic.

### 3. Recording chokepoint (sees ALL calls)

Rather than sprinkle `ledger.record(...)` at every call site, the ledger is threaded INTO the `Llm`
seam itself as an `Option<Arc<UsageLedger>>`. `Llm::complete` and `Llm::complete_streaming` record
every successful response and run the rate-limit detector on every failure — one chokepoint, so the
ledger sees every call path. Handlers obtain the ledger-attached seam via `AppState::llm()`
(= `Llm::from_env_with_ledger`); the audit path receives an `Option<Arc<UsageLedger>>` parameter and
builds its `Llm` with it attached. Bare `Llm::from_env()` (no ledger) remains for tests and the few
metadata-only callers (`/api/models` backend label).

### 4. Rate-limited state

The ledger carries `rate_limited: bool` + `last_rate_limit: Option<{when_unix, detail}>`.
`note_failure(detail)` sets the flag + records the event when `is_rate_limit_signal(detail)` is true;
every successful `record(...)` CLEARS it (a served call proves we're no longer throttled).

`is_rate_limit_signal(&str)` is a small provider-agnostic helper. ANTHROPIC signals covered now:
HTTP `429`, `"overloaded"`, `"rate_limit"`/`"rate limit"`, and the CLI idle-timeout hang message
(which already attributes itself to "likely rate-limited/queued"). A `TODO(gemini)` notes Gemini's
`RESOURCE_EXHAUSTED` (gRPC) — its `429` is already matched — to be added when that arm is wired.

### 5. Endpoint

`GET /api/usage` -> `{ input_tokens, output_tokens, cache_read, cache_creation, total_cost_usd,
calls, by_model:[{model,tokens,cost,calls}], rate_limited, last_rate_limit }`. Read-only snapshot.

### 6. UI

A compact, persistent `UsageMeter` component pinned to the right of the cockpit nav row polls
`/api/usage` every ~4s: `<tokens> tok · $<cost> · <calls> calls`, click to expand a by-model
breakdown table. When `rate_limited`, it swaps to a distinct amber pulsing "Rate-limited — retrying"
badge instead of the normal readout. Reuses the existing `bff_get_json` fetch + `use_future` poll
loop patterns and the nav/style vocabulary in `style.rs`.

## Observability only — gate unchanged

This is purely observational. Nothing here changes the deny-before-execute gate, model selection,
prompt content, retry behavior, or any LLM call semantics. The ledger only WATCHES what already
flows through the `llm` seam; `is_rate_limit_signal` only classifies error text after the fact.

## Tests (camerata-server)

`usage_ledger` unit tests: accumulation across multiple `LlmResponse`s incl. cache tokens + call
count + by-model; cost fallback (Gemini shape: tokens + `cost_usd=None` + known model -> derived
cost; unknown model -> tokens at `$0`, no panic); provider-agnostic mixed accumulation (an
Anthropic-shaped response and a Gemini-shaped one both accumulate correctly); `is_rate_limit_signal`
true for 429/overloaded/rate_limit/idle-hang and false for normal text; `rate_limited` sets on
detection + clears on a successful record. A `lib.rs` endpoint test asserts the `/api/usage` payload
shape and that it reflects recorded calls (Anthropic + Gemini shapes).
