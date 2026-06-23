# Stall UI: Warning Surface, Stop Button, Terminal States, Dual-Threshold Settings

**Date:** 2026-06-23
**Branch:** feat/stall-ui
**Commit:** 6e6a621

## Context

The backend gained stall-detection in the preceding `feat/stall-policy` commit (326dda2):
- `GET /api/runs/:id` now returns `idle_ms`, `stalled`, `stall_threshold_ms`, `stall_policy`, `failure_reason`.
- `POST /api/runs/:id/cancel` and `POST /api/onboard/audit/job/:id/cancel` exist and return 204.
- `GET /api/onboard/audit/job/:id` returns `{ job: JobState, idle_ms: Option<u128>, cancel_requested: bool }`.
- `RunStatus` has two new terminal variants: `Failed { reason: String }` and `Cancelled`.
- `Project` carries `stall_thresholds: StallThresholds { watched_secs, routine_secs }`.
- `ProjectStore::set_stall_thresholds` exists but had no HTTP endpoint yet.

This ADR documents the UI decisions made in `feat/stall-ui`.

## Decisions

### 1. Stall warning surface on dev runs (LiveRunPanel)

**Decision:** Render an amber `run-stall-warning` banner above the live-events stream when `stalled == true && !done`. The banner shows idle duration (formatted by `format_idle(idle_ms)`) and contains a prominent Stop button. This is distinct from the red `run-terminal-failed` div (hard failure) and the neutral `run-terminal-cancelled` div.

**Rationale:** A stall is not a failure — the run is still alive, just idle. Amber/warning treatment communicates "something may be wrong, act if you want" vs. red "it is definitively done and failed." The stall banner is conditional on `!done` so it never appears on a completed run even if the snapshot carries stalled=true (a race window at shutdown).

**Alternative rejected:** Showing the stall as a synthetic live-event row. Rejected because a row in the activity stream is not prominent enough to catch attention; a banner above the stream is harder to miss.

### 2. Always-available Stop button on dev runs

**Decision:** The Stop button (label `■ Stop`, CSS class `btn-stop`) appears in the `live-run-head` bar whenever `run_is_cancellable(status, done)` is true — i.e., any non-done, non-failed, non-cancelled state. It is NOT gated on stall.

**Rationale:** Stall detection has a window (the poller runs every 600ms). A user watching a run that looks stuck should not have to wait for the stall flag to appear before getting a Stop affordance.

**Implementation:** `cancel_run(run_id)` is fire-and-forget (ignores the 204 response), because the next poll cycle will reflect the cancelled state via `RunStatus::Cancelled` in the run response.

### 3. Terminal states: failed and cancelled

**Decision:** `run_status_badge` now maps `"failed"` → `("FAILED", "error")` and `"cancelled"` → `("CANCELLED", "neutral")`. `LiveRunPanel` renders a `run-terminal-failed` div showing `failure_reason` when status is `"failed"`, and a `run-terminal-cancelled` div for `"cancelled"`. Both use `#[serde(default)]` on `failure_reason` so old payloads without it don't break.

**Rationale:** Before this change both terminal states fell through to the `"RUNNING" / "active"` catch-all badge, which was wrong. The `failure_reason` field surfaces auto-cancel messages (e.g. "Stall timeout exceeded") so the user knows why the run stopped.

### 4. Stop button and stall warning on the onboarding scan

**Decision:** The `auditing()` spinner block gains a Stop button at all times (not just on stall). A stall warning (`scan-stall-warning`) appears when `scan_idle_ms() > 120_000 && auditing()`. The 120s threshold is hardcoded on the UI side (matching the server's default `watched_secs`). The Stop button calls `cancel_audit_job(job_id)`.

**Rationale:** The scan does not carry a project-scoped threshold in its response (the job API returns `idle_ms: Option<u128>` without `stall_threshold_ms`). Using the server default (120s) on the UI side keeps the warning consistent without adding a new field to the job API. If per-project scan thresholds are needed later, the job response can carry the value and the UI can be updated.

**Implementation detail:** `poll_job` gained a new `scan_idle_ms: Signal<Option<u128>>` parameter (seventh argument). All three call sites were updated. The `JobStatusEnvelope` wrapper struct was introduced to correctly deserialize the server's `{ job: ..., idle_ms, cancel_requested }` shape — the prior code was trying to deserialize this as a flat `JobStateView`, which was a latent bug.

**Terminal state:** `poll_job` now handles `"cancelled"` as a terminal status (same cleanup as `"failed"`: clear auditing, job_progress, det_progress, active_audit_job, break).

### 5. Dual-threshold settings in the project-settings gear

**Decision:** `StallThresholdsEditor` is a new component (batch-save pattern matching `TierMapEditor`) with two `<input type="number">` fields for `watched_secs` and `routine_secs`. It appears in both gear popup locations under a `SETTINGS: Stall thresholds` label. Values must be positive integers (zero is guarded client-side in the `oninput` handler and server-side in the endpoint).

**Save pattern:** Explicit "Save thresholds" button (not per-field auto-save). Rationale: both values are semantically coupled (you want to see the pair you're configuring before committing). Matches `TierMapEditor`'s approach for the same reason.

**Rationale for `#[serde(default)]` on `ProjectView.stall_thresholds`:** Existing project JSON that predates the stall-thresholds feature will not carry the field; `Default` fills in 120/600.

### 6. New server endpoint: POST /api/projects/:id/stall-thresholds

**Decision:** Handler `set_stall_thresholds_handler` + `SetStallThresholdsReq { watched_secs: u64, routine_secs: u64 }`. Mirrors `set_step_model` exactly: JSON body, returns `{ ok: true, project: ... }` or `{ ok: false, message: ... }`. Zero values are rejected (a threshold of 0 would immediately flag every run as stalled). Route placed at line 299 adjacent to the other project-config routes.

**Rationale for separate endpoint (not merged into an existing `/api/projects/:id` PATCH):** All project-config mutations in this codebase are scoped endpoint mutations (one concern per route). Consistent.

### 7. Pure helper functions + unit tests

Three pure functions were extracted for testability:
- `format_idle(idle_ms: u128) -> String` — human-readable idle duration
- `run_is_cancellable(status: &str, done: bool) -> bool` — true when run can be stopped
- `run_stall_banner_visible(stalled: bool, done: bool) -> bool` — true when stall banner should render

12 new unit tests cover: idle formatting, cancellable-state predicate, stall-banner predicate, `live_event_style` stall family, `RunView` back-compat defaults, `RunView` stall fields parsing, `RunView` failure reason parsing, `run_status_badge` terminal states, `StallThresholdsView` defaults, and `JobStatusEnvelope` wrapped-job parsing.

## Files Changed

- `crates/ui/src/cockpit.rs`: 461 lines changed
- `crates/server/src/lib.rs`: 29 lines added

## Test Results

- `cargo test -p camerata-ui`: 110 tests, all pass
- `cargo test -p camerata-server`: 2 doc-tests, all pass
- `cargo build --workspace`: clean (1 dead_code warning on `JobStatusEnvelope.cancel_requested`, which is deserialized for future use)
