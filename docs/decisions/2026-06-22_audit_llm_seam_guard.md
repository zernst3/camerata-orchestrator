# 2026-06-22 — Audit LLM seam + AI-failure guard test

## Context

`crates/server/src/ai_audit.rs` drives Camerata's brownfield audit. Roughly ten of its
functions took a concrete `&Llm` (`crates/server/src/llm.rs`) directly. That made the
single most important safety behavior of the audit impossible to unit-test without a live
model:

> When the LLM is unavailable in an AI-review ("both": deterministic + AI) scan, every
> audit pass errors, and `audit_repo` must **surface** that the AI review was skipped —
> never report a silent clean `Ok([])`. The deterministic floor findings still return
> independently.

That behavior lives in `audit_repo`'s `ok_passes == 0` branch (it returns `Err(last_err)`),
and at the merge level `onboard::audit_repos` turns that `Err` into a
`"{spec}: AI audit skipped ({e})"` note while the deterministic floor findings survive.
Untested, a future refactor could silently swap the surface for a clean result.

## Decision

### The `Completer` seam

Introduced a minimal trait in `llm.rs`:

```rust
#[async_trait::async_trait]
pub trait Completer: Send + Sync {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse>;
    async fn complete_streaming(
        &self,
        req: LlmRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse>;
    fn as_any(&self) -> &dyn std::any::Any;
}
```

`impl Completer for Llm` delegates both methods straight to the existing inherent
`Llm::complete` / `Llm::complete_streaming` — no wrapping, no behavior change. The real
`Llm` remains the production implementation everywhere.

The audit's completion/streaming functions now take `&dyn Completer` instead of `&Llm`:
`verify_findings`, `audit_pass`, `run_passes`, `run_routed_passes`, `audit_repo`,
`run_prose_lens`, `run_security_lens`, `run_security_passes`, `run_deep_tier`.

### Why object-safe (`&dyn`) over generic `<C: Completer>`

The audit threads one client reference down through ~10 functions. A generic parameter
would monomorphize each of them and force a `<C>` annotation through the whole call tree;
a `&dyn Completer` passes by reference with zero signature churn. Object-safety is what
makes that possible, which is why the streaming method takes the delta callback as
`&mut (dyn for<'a> FnMut(&'a str) + Send)` (a `&mut dyn`, explicitly higher-ranked so
`async_trait`'s desugaring matches the inherent method's `for<'a>` signature) rather than
a generic `F: FnMut`.

### `as_any` downcast for the batch path

The Message-Batches path (`run_passes_batch`, which calls `Llm::api_key`,
`submit_batch`, `poll_batch_status`, `fetch_batch_results`) is API-key-gated and is **not**
part of this minimal seam — it stays on the concrete `&Llm`. `audit_repo`'s batch branch
recovers the concrete client via `llm.as_any().downcast_ref::<Llm>()`. In production the
value is always a real `Llm`, so the downcast always succeeds and the batch path is byte
unchanged; a non-`Llm` stub (tests) can only drive the non-batch real-time path, which is
where the all-fail→surface logic lives anyway.

## The guard test

In `ai_audit::tests`:

- `FailingCompleter` — both `complete` and `complete_streaming` return `Err` (LLM
  unavailable). Token-free, no env mutation, no network.
- `audit_repo_surfaces_ai_failure_not_silent_clean` — drives `audit_repo` in
  `ScanMode::Parallel` (the AI-review path), `feedback: None` (non-streaming `complete`).
  Asserts the result is `Err` (NOT `Ok([])`) and the surfaced error carries the underlying
  LLM failure — i.e. the audit does not fabricate success or a silent clean.
- `audit_repo_surfaces_ai_failure_streaming` — same guard through the streaming path
  (`feedback: Some((&store, …))` → `complete_streaming`).
- `StubCompleter` + `audit_repo_with_stub_completer_returns_findings` — happy-path proof
  the seam works in the other direction: a stub returning canned audit JSON lets
  `audit_repo` complete without a live model and yields the parsed finding.

Note: "deterministic findings survive an AI failure" is observable at the
`onboard::audit_repos` merge level (the floor findings come from `audit_files`, the AI
`Err` becomes a note). The unit guard here asserts the `Err`-surface at `audit_repo`; the
existing `deterministic_only_runs_floor_and_skips_ai` test in `onboard.rs` already covers
the floor producing findings independently of the AI path.

## Behavior-preserving note

This is a seam plus tests only. `Llm` is still the real (and only production) implementor;
the audit logic, the `ok_passes == 0` → surface behavior, scoring/calibration, and the
batch path are all unchanged. `cargo build --workspace` is green and the full
`camerata-server` suite passes (497 tests, +3 new, plus doctest).
