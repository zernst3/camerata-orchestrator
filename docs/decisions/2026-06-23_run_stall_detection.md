# 2026-06-23 — Run stall detection: progress-not-wallclock

## Problem

`ClaudeCliDriver::run` and `GenericCliDriver::run` called `cmd.output().await`,
which buffers ALL subprocess output until the process exits. A hung agent goes
completely silent: no error, no timeout, no heartbeat — the whole dev run hangs
forever. Total wall-clock is wrong as the stall signal because legitimately long
work keeps emitting output; a hang goes silent.

## Design

Three layers:

**Layer 1 — Streamed, bounded subprocess.**
Both drivers now stream stdout line-by-line via `tokio BufReader::lines()` with a
per-line `tokio::time::timeout(INACTIVITY_WINDOW, ...)`. Each line received fires
an injected `on_activity: Option<HeartbeatFn>` callback and resets the inactivity
clock. If no line arrives within `INACTIVITY_WINDOW`, the child is killed and
`AgentError::Stalled { idle_secs, last_line }` is returned — a fail-soft error,
not a panic. A separate total hard-ceiling (`TOTAL_TIMEOUT`) kills runaway
processes that keep trickling output.

Env overrides:
- `CAMERATA_AGENT_INACTIVITY_SECS` (default: 120)
- `CAMERATA_AGENT_TOTAL_TIMEOUT_SECS` (default: 3600)

The callback is `Option<HeartbeatFn>` (`Option<Arc<dyn Fn() + Send + Sync>>`), a
new field on both drivers. Existing callers that don't set it pass None and see
no behavior change except the subprocess is now bounded.

**Layer 2 — Heartbeat -> last_activity_ms on the run.**
`Run` gains `last_activity_ms: u128` (epoch-ms, initialized at creation) and
`last_progress_label: String`. `RunStore::push_event` now auto-updates both on
every gate event. Direct callers of the driver can wire the heartbeat callback
to call `RunStore::touch_activity` so per-line progress advances the clock.

Pure functions for stall derivation:
- `idle_ms(last_activity_ms, now_ms) -> u128`
- `is_stalled(idle_ms, threshold_ms) -> bool`

Env override:
- `CAMERATA_RUN_STALL_THRESHOLD_SECS` (default: 120, i.e. 120_000ms)

**Layer 3 — API contract.**
`GET /api/runs/:id` now returns a `RunStatusResponse` (flattens `Run` + adds
computed stall fields):

```json
{
  "id": "run-1",
  "story_id": "CAM-7",
  "status": "executing",
  "events": [...],
  "done": false,
  "mode": "live",
  "last_activity_ms": 1750000000000,
  "last_progress_label": "stage info",
  "idle_ms": 4200,
  "stalled": false,
  "stall_threshold_ms": 120000
}
```

The UI polls this endpoint and can show a stall banner when `stalled: true`.

## Rationale

Progress-not-wallclock is the correct stall signal: a legitimately long build
that keeps emitting lines is NOT stalled. A process that goes silent IS. The
inactivity window (per-line timeout) is the discriminator.

The `on_activity` callback is optional and back-compat: all existing callers and
tests that don't set it continue to work. The only behavioral change is that the
formerly-unbounded `output().await` is now bounded.

## Env-overridable thresholds

| Env var | Default | Controls |
|---|---|---|
| `CAMERATA_AGENT_INACTIVITY_SECS` | 120 | Per-line inactivity window in subprocess driver |
| `CAMERATA_AGENT_TOTAL_TIMEOUT_SECS` | 3600 | Hard total ceiling in subprocess driver |
| `CAMERATA_RUN_STALL_THRESHOLD_SECS` | 120 | `is_stalled` threshold on the run API response |
