# Scan execution modes: two axes, three presets, auto-select with override

**Status:** decided (design); not yet built.
**Date:** 2026-06-16

## Context

The brownfield audit today runs as **one synchronous, fully-sequential sweep**: the UI fires
`POST /api/onboard/audit` and awaits the whole job; the server loops repos one at a time, and
within each repo loops file-chunks one at a time, each chunk a single blocking LLM call against
the full ruleset, plus a calibration call.

That is fine for a tiny repo on a fast model (seconds). It breaks down badly at scale. Napkin
math for the "thorough enterprise" case — 5 very large repos (~5M chars each → ~14 context-sized
chunks each → ~70 chunks + calibration ≈ ~85 LLM calls), on the strongest model (~3–5 min/chunk
for an exhaustive audit), **sequential**:

> **~85 calls × ~4 min ≈ 5–6 hours**, one synchronous request.

Two *independent* failures hide in that number, and they need different fixes.

## The two axes

The reason a single "make it faster" doesn't cover it is that the design has two orthogonal axes:

| Axis | Options | What it controls |
|------|---------|------------------|
| **Execution** | sequential ↔ parallel | how many LLM calls run at once |
| **Delivery**  | synchronous-blocking ↔ async-job | how results get back to the UI |

- **Execution = sequential** is the wall-clock problem. Fix: run independent calls concurrently
  (this is literally `ARCH-PARALLEL-INDEPENDENT-1`, our own rule). With a concurrency cap of ~16,
  the 5-hour sweep above drops to roughly `total / 16` ≈ **~20–30 min**.
- **Delivery = synchronous-blocking** is a *separate* problem that parallelization does **not**
  fix. Today the UI holds one HTTP request open for the entire job. A request open for 20–30 min
  (let alone hours) is fragile: HTTP/proxy idle timeouts, a frozen-looking app with no incremental
  findings, and all-or-nothing loss if anything drops mid-way. You only get the findings table at
  the very end.

## The three presets

The two axes collapse into a sensible gradient of three presets:

| Mode | Execution | Delivery | Best for |
|------|-----------|----------|----------|
| **1. Single synchronous** (today) | sequential | blocking | tiny repos, debugging, gentlest on rate limits |
| **2. Parallel** | parallel | blocking | medium repos audited in one sitting |
| **3. Job / streaming** | parallel | async + incremental | huge / multi-repo, walk-away thoroughness |

Two honest notes on these:

- **Mode 1 is strictly dominated by Mode 2 for *results*** — same findings, just slower. Its only
  real reason to exist is "simplest / one call at a time / gentlest on rate limits." Keep it as a
  *simplicity/safety* mode, not as a quality mode.
- **Mode 2 still holds the HTTP request open** for its (shorter) duration. Fine for minutes; it
  only breaks at the multi-hour scale, which is exactly Mode 3's reason to exist.

## What "job / streaming" actually means (plain English)

This is the part that's easy to lose, so here it is as an analogy.

**Today (synchronous) is like a phone call you can't hang up.** You press "Audit," the app dials
the server and **holds the line** — phone pressed to your ear — until the *entire* audit (every
repo, every chunk) is completely finished. Then you hear the whole result at once and hang up. If
it takes an hour, you stand there holding the phone for an hour. If the call drops, you got
nothing.

**The job model is like submitting a ticket.** You press "Audit," the server says *"Got it —
here's ticket #1234"* and **hangs up immediately**. It goes off and does the work in the
background. Your app periodically checks back: *"How's ticket #1234?"* — and each check returns the
**progress** ("47 of 85 passes done") and any **findings discovered so far**, which populate the
table **live**. You can close your laptop, come back later, and check the ticket again. If one part
fails, the server notes it on the ticket and keeps going — nothing else is lost.

So the job model is three things:
1. **Submit → get an id** (the request returns instantly, not after hours).
2. **Workers run in the background**, in parallel, writing findings + progress to the job as they go.
3. **The UI polls/streams the job** — findings appear incrementally, a progress bar moves, and the
   run survives you walking away or a transient drop.

### Is this a CLI feature or API-only?

**Neither — it's *our* architecture, and the CLI can absolutely do it.** Two different things both
get called "streaming," and only one is a backend capability:

- **Token streaming** (per-token deltas from the model) — both the CLI (`--output-format
  stream-json`, which we already use) and the API (SSE) support it. Not a differentiator.
- **The job model** (submit → background → poll findings/progress) — this lives entirely *inside
  camerata-server*. It has nothing to do with how each individual LLM call is made. We already have
  the exact pattern: the **transcript store** streams each agent's live output while the work runs
  in a `tokio::spawn`, and the Agent-activity drawer polls `/api/runs/scan-audit/agents`. The job
  model just generalizes that from "transcript text" to "findings + progress + persistence."

The only place the backend matters is how heavy parallelism *feels*: with the CLI, 16 concurrent
calls = 16 `claude` subprocesses (more per-call overhead, more processes to manage); with the API
it's 16 lean HTTP requests. Both work; the API is just tidier at high concurrency. That's a reason
backend and scan-mode interact, not a blocker.

## Decision: auto-select by scale, with manual override

Rather than force a cold three-way choice on every scan, **the system picks the recommended mode by
scale and tells the user which it chose, with a manual override** (same philosophy as the model
picker — the user owns the trade-off, but isn't burdened by default):

- tiny repo (fits one context, fast) → **Mode 1**
- medium repo → **Mode 2**
- huge / multi-repo → **Mode 3**

The three modes still exist under the hood; the UI just defaults intelligently and exposes an
override select for power users. This removes the "which do I pick?" burden for the common case
while preserving control.

## Build order

1. **Parallel execution** (Modes 1→2) — **BUILT** (commit `08d95bf`). Rule-batches × file-chunks run
   concurrently via `futures::buffer_unordered` under a cap; `ScanMode::{Sequential, Parallel}`;
   `run_passes()` is the shared engine. Sequential = `(1 call, all rules)` (the gentle floor);
   Parallel `(6, 15)` is the default.
2. **Job / delivery layer** (Mode 3) — **BUILT** (commits `adc51b7` server, `3825463` UI). An
   in-memory `JobStore`; `POST /audit/start` spawns the audit in a `tokio` task and returns a
   `job_id` immediately; `GET /audit/job/:id` returns progress + incremental findings + the final
   report. The audit threads a job sink (deterministic floor up front, each semantic pass streams as
   it completes). The UI's "Background job" mode submits, then polls with a live progress bar.

### Mode 3 v1 limitations (refinements, not blockers)

- **In-memory, app-session-scoped** — a job lives in the running server's memory; it does not
  survive an app restart. (Persisting jobs to disk would make them survive restarts.)
- **Poll-while-on-screen** — the *work* is decoupled from the request (survives a dropped poll), but
  the UI only resumes polling if it still holds the `job_id`. Navigating away and back starts a fresh
  `ScanResults` that doesn't re-attach. Fix: stash the active `job_id` in a context/persisted setting
  and offer "resume" on mount.
- **Live findings are a raw preview** — incremental findings are pre-final-dedup/calibration; the
  `report` set on completion is authoritative (the UI switches the table to it on `done`). The live
  view today shows a progress bar + count, not a streaming table, to avoid remount flicker.
- **Cached-prefix cost optimization not yet applied** — the digest/repo-map are re-sent per rule-batch
  rather than ordered as a stable cached prefix; a reorder would cut Parallel/Job input cost.
- **Auto-select-by-scale not yet wired** — the UI defaults to Parallel and lets the user pick; it does
  not yet auto-recommend Job for huge/multi-repo scans. The mode infrastructure is in place for it.

## Pricing / tiering principles (the dials must add to a complete floor, not gate it)

Customer-controlled cost/speed/thoroughness is the right product shape — enterprises like
holding the dial, and it answers the cost worry (the buyer chooses their spend). Three rules keep
it a feature and not a trap:

1. **Three orthogonal dials, not one "max everything" slider.** They're independent and each buys
   a different thing — expose and explain them separately:
   - **Mode** (sequential / parallel / async) → **speed & scale**. Does NOT change *what* is found.
   - **Model tier** (Haiku … Opus) → **detection quality** (fewer semantic misses, sharper findings).
   - **Rule selection** → **coverage** (which conventions are checked).
   The real value is letting a user *mix* — top-tier quality on a small fast scan, or a cheap model
   on a huge async job. "Max on all three" is one valid point, not the only useful one.

2. **The deterministic floor is free, instant, and runs in EVERY config.** `audit_files` (local
   regex/AST, no LLM, no model, no mode) runs unconditionally on every scan — it does not depend on
   the selected rules, the model, or the mode. The dials apply ONLY to the **semantic/LLM pass**:
   that is what gets faster (parallel), better (top model), or scale-graceful (async). When the
   parallel-execution work lands, **parallel must be the DEFAULT efficient floor of the semantic
   pass, not a paywalled escape from a slow sequential baseline.** Sequential (Mode 1) is a
   debug/simplicity fallback, not the thing users pay to get out of.

3. **The critical checks can never be a paid tier.** Hardcoded secrets, injection, secret-in-URL
   — the highest-severity, deterministic-cheap defects — are table stakes on every tier, free,
   always-on. (Verified: they run in `audit_files` independent of `selected`/model/mode, and now
   rank Critical.) A governance tool that misses a hardcoded secret unless you upgrade is broken at
   any price. The premium dials buy the *expensive* things: deep semantic/architectural review (top
   model) and walk-away multi-repo scale (async mode).

The resulting pitch is also the stronger one: *"every scan catches the critical stuff fast and
cheap; pay more for deeper architectural review and walk-away multi-repo runs"* — not *"pay up or
we might miss your secrets."*

## Related

- `ARCH-PARALLEL-INDEPENDENT-1` — the rule that mandates the parallel execution axis.
- `docs/decisions/2026-06-16_two_domain_audit_and_two_phase_flow.md` — the audit's overall shape.
- The transcript store + Agent-activity drawer — the existing precedent for the job/poll pattern.
