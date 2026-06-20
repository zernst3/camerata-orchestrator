# Message Batches API execution mode (#61)

**Date:** 2026-06-20
**Status:** Implemented (dev3/batches)
**Issue:** #61

## What

A new `batch` scan mode that routes all (chunk x rule-batch) requests through the
Anthropic Message Batches API (`POST /v1/messages/batches`) instead of real-time
per-request calls. The discount is a flat 50% off all input and output tokens, applied
uniformly by the provider — no additional logic required on our side.

## Why

The standard parallel mode fires N real-time requests concurrently (N = chunks x rule-
batches, typically 6-50 for a normal repo scan). Each request contributes to the
per-minute token rate limit and incurs full per-token pricing. For large or multi-repo
scans, both constraints bite: rate-limit 429s force retries/backoff, and the dollar
figure grows linearly with the repo size.

The Message Batches API sidesteps both: requests are submitted in one POST, Anthropic
schedules and processes them asynchronously out-of-band (no rate-limit pressure on the
caller), and the effective price is half the list rate. The trade-off is asynchronous
delivery: results arrive when the batch is done (typically seconds to a few minutes for
small scans, up to 24h for very large ones). This is acceptable for scans where
wall-clock time is secondary to total cost — the "submit and check back later" pattern.

## How

### New types in `llm.rs`

- `BatchItem` / `BatchItemParams` / `BatchMessage` — the per-request objects the batch
  API expects. Shape mirrors `/v1/messages` body.
- `BatchSubmitResult` — returned by `submit_batch`: the `batch_id` + initial item counts.
- `BatchStatus` — returned by `poll_batch_status`: `processing_status` + live counts.
- `BatchResultRow` — one JSONL line from the results endpoint: `custom_id`, an
  `Option<LlmResponse>` (succeeded), and an `Option<String>` error (failed).
- `build_batch_item(custom_id, req, model) -> BatchItem` — constructs a batch item from
  an `LlmRequest`, forwarding the prompt-caching `cache_prefix_len` breakpoint exactly
  as `complete_api` does (two-block content array with `cache_control`).
- `parse_batch_results_jsonl(jsonl) -> Vec<BatchResultRow>` — parses the results JSONL
  stream, robust to malformed lines (skip + continue).
- `reassemble_batch_results(rows) -> HashMap<String, LlmResponse>` — converts the flat
  unordered result list into a `custom_id -> LlmResponse` map. This is the key step that
  undoes unordered delivery: since Camerata assigns deterministic `custom_id`s of the
  form `c{ci}-b{bi}` (chunk index, rule-batch index), the map lookup is O(1).

Three new methods on `Llm`:

- `submit_batch(items) -> Result<BatchSubmitResult>` — POST to
  `/v1/messages/batches`.
- `poll_batch_status(batch_id) -> Result<BatchStatus>` — GET the batch status.
- `fetch_batch_results(batch_id) -> Result<Vec<BatchResultRow>>` — GET + parse the
  results JSONL.

### `ScanMode::Batch` in `ai_audit.rs`

Added as a third variant alongside `Sequential` and `Parallel`. `ScanMode::parse`
recognises `"batch"` on the wire. `ScanMode::tuning()` returns the same values as
`Parallel` (the same chunking and rule-batching apply; the Batch mode only differs in how
the requests are delivered).

`run_passes_batch` is the new execution function. It:

1. Builds the full cartesian product of `(ci, bi)` pairs, constructing one `BatchItem`
   per pair with `custom_id = "c{ci}-b{bi}"`.
2. Enforces the 100k item cap by splitting into sub-batches (each submitted separately,
   their results unioned).
3. Submits each sub-batch, records the `batch_id` on the job store (surfaced by the UI),
   and polls at `CAMERATA_BATCH_POLL_SECS`-second intervals (default 10s) until
   `processing_status == "ended"`.
4. Fetches + reassembles results by `custom_id`, feeds each succeeded response into
   `parse_ai_findings` + `parse_needs_files`, streams findings into the job store, and
   counts progress toward the job total.

`audit_repo` dispatches to `run_passes_batch` when `mode == ScanMode::Batch`. The
resolution round (for `needs_files`) always uses the real-time parallel engine even in
batch mode: the resolution set is typically 1-5 files, and the polling overhead of a
separate batch submission outweighs the marginal discount.

### `JobState.batch_id` in `jobs.rs`

A new optional field (`Option<String>`) added to `JobState` and exposed in the serialized
poll response. Set by `JobStore::set_batch_id` immediately after each `submit_batch`
call; cleared by `finish` (the id is not informative once the job is done). The UI can
display "batch in flight: msgbatch_01..." in the status line.

### Cockpit changes (`cockpit.rs`)

- A fourth option `"Batch (50% off — async, API key required)"` added to the scan mode
  `<select>` element.
- `estimate_audit_cost` applies a `batch_discount = 0.5` multiplier to both the audit
  and calibration per-token prices when `mode == "batch"`. The pass count, chunking,
  prompt-caching structure, and FUDGE factor are unchanged.
- `JobStateView.batch_id` mirrors the server-side field so the UI can display it.

## Constraints honoured

- **100k / 256MB caps**: split into sub-batches of at most 100k items each; each sub-
  batch is submitted and polled independently.
- **Unordered results**: `custom_id`s are deterministic (`c{ci}-b{bi}`); reassembly is a
  map lookup by id, not positional.
- **Prompt caching**: `build_batch_item` forwards the `cache_prefix_len` breakpoint
  exactly as `complete_api` does — the cached prefix and suffix blocks are present in the
  batch item content.
- **API key required**: `run_passes_batch` returns an early error with a clear message if
  no API key is present, so the caller (or the user in the cockpit) can switch to
  parallel mode.
- **Calibration pass**: runs on the real-time API path (a single call over the aggregated
  findings) — not batched. The batch discount does not apply to calibration.

## Usage

```bash
# API backend (required for batch mode)
CAMERATA_LLM_BACKEND=api ANTHROPIC_API_KEY=sk-ant-... cargo run

# Submit a batch scan via the cockpit or directly:
curl -X POST http://localhost:3001/api/onboard/audit/start \
  -H 'Content-Type: application/json' \
  -d '{"repos":["org/repo"],"mode":"batch","model":"claude-sonnet-4-6"}'

# Poll interval override (default 10s):
CAMERATA_BATCH_POLL_SECS=5 cargo run
```

## Decision log

- **Resolution round uses real-time parallel, not batch**: polling overhead for a ~5-file
  resolution set would cost 10+ seconds of sleep with no meaningful savings. The
  resolution set never exceeds the CHUNK_DIGEST_CHARS budget by design.
- **Calibration is not batched**: the calibration pass is a single call over the
  aggregated findings — no chunking, no batching needed. Batching one request is a no-op.
- **Sub-batch splitting is sequential**: Anthropic's batch API is async (no real-time
  rate limit pressure), so sequential sub-batch submissions are safe and simpler than
  concurrent ones. The sub-batch scenario only arises at >100k items (rare in practice).
- **`batch_id` cleared on finish**: once the batch is done and the report is set, the
  batch id is not informative and is cleared to keep the job state tidy.
- **50% discount applies to scan passes only, not calibration**: `estimate_audit_cost`
  applies `batch_discount = 0.5` to the scan audit prices but NOT to the calibration
  prices. Calibration always runs real-time (a single call over aggregated findings) even
  in batch mode, so its cost is not discounted.
