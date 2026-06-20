# Prompt Caching in the Scan Request Path

**Date:** 2026-06-20
**Branch:** dev1/prompt-caching
**Status:** Implemented

## What

Anthropic's prompt-caching API feature lets a caller mark a prefix of the user
message with `cache_control: {type: "ephemeral"}`. The provider caches that
prefix for 5 minutes; subsequent calls that share the identical prefix read it
from cache at ~0.1x the normal input token price instead of re-sending it at
full price. The one-time write costs 1.25x the input price (amortised over the
subsequent reads).

Camerata's parallel scan mode already orders each prompt with the static
content (repo map + chunk digest) leading and the varying content (batch
number + rules block) trailing — specifically to enable a stable cached prefix.
This phase wires the actual API-level breakpoint.

## Why

The dominant scan cost is the codebase digest, which is re-sent at full price
for every rule-batch over a chunk. With 15 rules and 2 batches per chunk, the
digest (the largest token block, ~80k tokens for a 350k-char chunk) is billed
twice at full price. With caching it is written once at 1.25x and then read at
0.1x — a net ~5x saving on the digest portion for a 2-batch scan, more for
larger rule sets.

The minimum cacheable prefix is 2048 tokens (Sonnet) / 4096 tokens (Opus/Haiku).
Our chunk digests are ~87k tokens (350k chars / 4 chars/tok), far above the
floor, so the minimum is always met.

## How It Works

### Prompt structure (unchanged from before; already cache-aware by design)

```
System prompt (always stable across batches — Anthropic caches this automatically)

User message:
  "Repository: {repo} ({label} {n}/{total})\n\n"  <- stable header
  {repo_map}                                        <- stable across batches for a chunk
  {digest}                                          <- stable across batches for a chunk
  "\n\n"
  ─── CACHE BREAKPOINT ───
  {task_line}                                       <- varies by batch number
  "\n\n"
  {rules_block}                                     <- varies by batch
```

### `LlmRequest::cache_prefix_len` (new field in `crates/server/src/llm.rs`)

`LlmRequest` grows a `cache_prefix_len: Option<usize>` field. When set to `Some(n)`,
`complete_api()` splits the user message at byte offset `n` into two content blocks:

```json
{
  "messages": [{
    "role": "user",
    "content": [
      {"type": "text", "text": "<prefix>", "cache_control": {"type": "ephemeral"}},
      {"type": "text", "text": "<suffix>"}
    ]
  }]
}
```

The `anthropic-beta: prompt-caching-2024-07-31` header is sent only when
`cache_prefix_len` is set; callers that don't set it get the identical
plain-string path as before (no structural change, no extra header).

The builder method `LlmRequest::with_cache_prefix_len(n)` clamps `n` to the
prompt length (defensive against oversized values) and is a no-op for `n == 0`.

The CLI backend ignores `cache_prefix_len` entirely (the CLI manages its own
context and does not accept a multi-block content format via flags). This is
correct: caching only matters on the API path where token billing is explicit.

### Breakpoint placement in `run_passes` (in `crates/server/src/ai_audit.rs`)

```rust
let static_prefix = format!(
    "Repository: {repo} ({label} {}/{n_c})\n\n{repo_map}{digest}\n\n",
    ci + 1,
);
let cache_prefix_len = static_prefix.len();
let prompt = format!("{static_prefix}{task_line}\n\n{rb}");
```

`cache_prefix_len` is passed to `audit_pass()`, which sets it on the request.
The `task_line` (which contains the batch index and changes per batch) is the
first byte of the suffix — so the cache boundary is exactly before the first
varying token, giving every rule-batch over the same chunk an identical prefix
to hit the cache.

### Cache token tracking (`UsageMeter` / `ActualUsage`)

`LlmResponse` now carries two new fields:
- `cache_read_input_tokens: u64` — tokens served from cache (~0.1x price)
- `cache_creation_input_tokens: u64` — tokens written to cache (~1.25x price)

`UsageMeter` accumulates both across all calls in the audit via two new
`AtomicU64` fields. `ActualUsage` exposes them in its snapshot so the UI's
actual-vs-estimated readout can show cache savings next to the billed total.

The `usage_tokens()` helper in `llm.rs` was updated from a 2-tuple to a
4-tuple `(input, output, cache_read, cache_creation)`. `input` still folds in
both cache fields (so it reflects all input-side billing), and the breakdowns
are returned separately for the meter.

### Cost estimate discount (`estimate_audit_cost` in `crates/ui/src/cockpit.rs`)

The pre-scan estimate now models the cache discount for parallel mode
(where `batches > 1`):

- **Batch 0 per chunk:** digest tokens billed at full price + 1.25x write surcharge
- **Batches 1..N per chunk:** digest tokens read from cache at 0.1x

Sequential mode (`batches == 1`) has no reuse across batches and pays the
pre-caching full price (unchanged). The `FUDGE = 1.4` multiplier keeps the
estimate conservative even after the discount, since the calibration pass and
resolution round are modelled at full price.

## Files Changed

| File | Change |
|------|--------|
| `crates/server/src/llm.rs` | `LlmRequest.cache_prefix_len` field + `with_cache_prefix_len()` builder; `LlmResponse.cache_read_input_tokens` + `.cache_creation_input_tokens`; `usage_tokens()` returns 4-tuple; `complete_api()` splits message into two blocks + sends beta header when caching active; `complete_cli_streaming()` captures per-call cache fields |
| `crates/server/src/ai_audit.rs` | `UsageMeter` tracks `cache_read_input_tokens` + `cache_creation_input_tokens`; `ActualUsage` exposes them; `audit_pass()` accepts `cache_prefix_len` param; `run_passes()` computes breakpoint and passes it through |
| `crates/ui/src/cockpit.rs` | `estimate_audit_cost()` models cache discount for multi-batch parallel scans |

## Tests Added

- `llm.rs`: `cache_prefix_len_builder` — builder clamps, zero is no-op, normal case
- `llm.rs`: `usage_tokens_parses_cache_fields` — 4-tuple, folding, absent fields
- `llm.rs`: `request_builder` updated to assert `cache_prefix_len == None` by default
- `cockpit.rs`: `sequential_mode_no_cache_discount` — 1 batch, no discount
- `cockpit.rs`: `parallel_multi_batch_cheaper_than_sequential_sum` — 2 batches < naive 2x
- `cockpit.rs`: `parallel_single_batch_no_discount` — 1 batch, near-equal to sequential
- `cockpit.rs`: `thorough_mode_costs_more_than_default` — calibration cost scaling

## Non-goals / Out of Scope

- **CLI caching:** The `claude -p` CLI manages its own caching internally; no
  changes are needed or possible there. This feature is API-path only.
- **System prompt caching:** The Anthropic API automatically caches the system
  prompt for all models when the prompt-caching beta is active. Our system
  prompt (`audit_system_prompt()`) is ~5k chars and stable across calls, so
  it benefits automatically without any explicit breakpoint.
- **Non-Anthropic vendors:** The `cache_prefix_len` field is ignored for OpenAI
  / Google (not wired). When those vendors are wired, they'll need their own
  caching protocol in their `complete_*` arms.
- **User-visible toggle:** Caching is always active on the API path for audit
  passes. There is no UI toggle; it is a pure cost reduction with no
  correctness tradeoff.

## How to Verify

1. Set `CAMERATA_LLM_BACKEND=api` and `ANTHROPIC_API_KEY=sk-...`.
2. Run an audit on a repo with 16+ adopted rules (forces 2+ batches in parallel
   mode).
3. Watch `ActualUsage` in the scan report: `cache_read_input_tokens` should be
   nonzero after the second batch, and `cache_creation_input_tokens` should be
   nonzero after the first.
4. The actual cost should be noticeably below the (conservatively high) estimate.
