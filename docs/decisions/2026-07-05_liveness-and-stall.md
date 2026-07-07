# ADR: Run liveness heartbeat and autonomous-only stall auto-cancel

**Date:** 2026-07-05
**Status:** Accepted (Batch 3b shipped on `fix/lifecycle-liveness`)

## Context

The Fable 5 audit (`docs/ARCH_AUDIT_2026-07-04_fable5-complete.md`) found two paired run-engine
liveness defects, stacked on Batch 3's bounce-loop / concurrency work (`fix/lifecycle-loop`,
`2026-07-05_lifecycle-loop-and-concurrency.md`).

- **LIFECYCLE-7: no liveness heartbeat on the two longest run paths.** `investigation_run` and
  `update_branch_run` wired an `on_activity` heartbeat onto their agent driver (fired per streamed
  output line), which keeps `last_activity_ms` fresh. But `dev_implement_run` and `pr_resolve_run` —
  the two longest-running paths — built their driver WITHOUT one. A healthy multi-minute
  implement/resolve run therefore read as "stalled" the moment it went quiet during a long tool call.
  Worse, `get_run` computed `stalled` for `done` runs and for human-PARKED runs (`AwaitingReview` /
  `AwaitingClarification`), which are intentionally idle, not wedged.
- **LIFECYCLE-6: stall enforcement was dead code.** `stall_decision`, `StallPolicy::Cancel`, and
  `RunKind::Autonomous` all existed and were unit-tested, but nothing ever constructed an `Autonomous`
  run or acted on a `Cancel` decision. Per-project stall thresholds (`Project::stall_threshold_ms`)
  were stored but `get_run` only ever read the process env default. So a wedged walk-away
  (routine-driven) run — the exact case with NO architect watching — would sit forever.

## Decision

### 1. Wire the liveness heartbeat everywhere a run drives an agent (LIFECYCLE-7)

- `build_agent_driver` and its `build_claude_driver` helper take an `Option<HeartbeatFn>` and wire it
  onto whichever concrete driver they build. `dev_implement_run` passes
  `Arc::new(move || runs.touch_activity(&run_id, None))`, mirroring `investigation_run` /
  `update_branch_run`. `pr_resolve_run` (which builds its driver from `prepare_session` directly, not
  via `build_agent_driver`) calls `.with_on_activity(...)` on that driver.
- **The API path had no heartbeat surface, so we added one.** `ApiAgentDriver` (the OpenRouter /
  Anthropic-API in-process agentic loop) gained an `on_activity` field + `with_on_activity` builder;
  the loop fires it once per turn (top of each iteration). The CLI driver beats per output line; the
  API loop has no line stream, so per-turn is its analogue. Both keep `last_activity_ms` fresh.
- `get_run` no longer reports `stalled` for a `done` run or a PARKED run
  (`RunStatus::is_parked()` — `AwaitingReview` / `AwaitingClarification`). Only a live, in-flight run
  can stall. `stall_decision` short-circuits the same cases to `StallDecision::Ok`, so the banner and
  the sweep agree.

### 2. Autonomous-only stall auto-cancel, wired through the active project threshold (LIFECYCLE-6)

- **Run kind is now constructed, not just declared.** `start_governed_run` takes a `RunKind`. The
  interactive HTTP start passes `Watched` (alert-only); the routine/scheduler-driven walk-away path
  passes `Autonomous` (which maps to `StallPolicy::Cancel` in `RunStore::create`).
- **`get_run` reads the ACTIVE project's per-kind threshold.** `Project::stall_threshold_ms(autonomous)`
  returns the watched or routine band; `get_run` uses it, falling back to the env/default only when no
  project is active. This is the SAME threshold the sweep applies.
- **A background sweep is the actor** (`crate::stall_sweep`). Spawned from `serve()` (not `router`, so
  router-only tests don't auto-cancel), it periodically scans every ACTIVE run and applies
  `stall_decision(run, project.stall_threshold_ms(kind), now)`. On `StallDecision::Cancel` it calls
  `runs.fail_with_reason` with an honest stall reason (idle seconds + threshold), so the terminal
  state records WHY it ended. `fail_with_reason` is idempotent-terminal, so a run that finished on its
  own between the snapshot and the cancel is left untouched.
- **Autonomous-only.** `StallDecision::Cancel` is only ever returned for a `Cancel`-policy run, which
  is set only for `Autonomous` runs. Watched runs that stall stay running (alert-only): the reported
  `stalled` flag is their signal, and the architect decides. Done / parked runs are never touched.

### 3. Generous default + the UI reflects it (Zach's explicit ask)

- **The routine (autonomous) default is now the generous 30-minute floor.** A walk-away run has no
  watcher and auto-cancels on stall, so the default grace period is deliberately long:
  `DEFAULT_ROUTINE_STALL_SECS = 1_800` (was an effective 600 s). The env override
  (`CAMERATA_RUN_STALL_THRESHOLD_SECS`) still takes precedence when set (scaled x5 off the watched
  base), so ops can tune it up or down; absent it, the generous default applies.
- The UI stall-thresholds control mirrors the same 1800 s routine default (`StallThresholdsView`) and
  its hint now states plainly: **Watched runs are alert-only when stalled; Routine (autonomous) runs
  auto-cancel on stall by default, after the generous grace period.**
- The project default (`StallThresholds::default()` → `default_routine_secs()`) and the UI default are
  the same value, so a fresh project and the fresh UI form agree.

## Consequences

- A healthy long dev-implement / pr-resolve run no longer false-reports as stalled — its per-line
  (CLI) or per-turn (API) heartbeat keeps it live.
- A wedged autonomous / routine run is now actually reaped: it auto-cancels to `Failed` with the stall
  reason, instead of sitting open forever with nobody watching.
- Interactive runs are unchanged in behavior (alert-only), and human-parked runs are never mistaken
  for stalled or auto-cancelled.

## Honest limits

- The sweep reads the in-memory `RunStore`; if the process restarts mid-run the run record (and thus
  the sweep's view of it) is lost with it — consistent with Batch 2/3's in-memory completion signal
  and single-flight guard.
- Routines do not yet spawn a real governed `RunStore` run in this codebase (live routine execution is
  latent until the GAP-8 structured-scope work). `start_governed_run` now carries the `RunKind` seam,
  so when that path lands it passes `Autonomous` and the sweep governs it with zero further change.
