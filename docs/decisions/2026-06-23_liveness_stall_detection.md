# Reusable liveness / stall detection

**Date:** 2026-06-23
**Status:** Accepted (design) — implementation phased
**Context:** The scan stall warning fires on *elapsed idle time*, which false-positives a
legitimately-slow task. Proof: scanning the `rivet` repo, `cargo clippy` spent 8+ minutes
cold-compiling native deps (rocksdb/zstd/lz4/bzip2 via `clang++`) — healthy and progressing,
but flagged "stalled" because no scan-level progress event fired during the compile. A fixed
timeout would have *killed a working build*. We need to measure **liveness**, not elapsed time.

## Principle

> Stalled = no sign of **progress** on ANY liveness signal for a threshold — never
> elapsed-since-start. Progress signals reset the idle clock; a generous backstop ceiling is a
> last-resort kill, far above any real workload. Detection must scale with repo size for free
> (more work → more progress → clock keeps resetting) and must not false-positive a busy task.

## Current state (from the code map)

Two **duplicated** stall implementations, no shared abstraction:

| | Scans | Dev runs |
|---|---|---|
| Activity field | `JobMeta.last_activity_ms` (`jobs.rs:26`) | `Run.last_activity_ms` (`run.rs:115`) |
| Touch | `JobStore::touch_activity` (`jobs.rs:152`) | `RunStore::touch_activity` (`run.rs:310`) |
| Idle calc | `JobStore::idle_ms` (`jobs.rs:278`) | `run::idle_ms` (`run.rs:380`) |
| Stall decision | none (UI compares `idle > 120_000`, `scan.rs:3084`) | `stall_decision()` + `StallPolicy` (`run.rs:400`) |
| Threshold | literal in UI only | `DEFAULT_RUN_STALL_THRESHOLD_MS` + per-project `StallThresholds` |
| Signals | `det_tool_running`/`det_tool_done` only (start/end) | gate events + `touch_activity` |

The scan path touches activity ONLY at a tool's start/end — nothing during execution → a long
tool is silent → false stall. The agent crate already has the right primitive
(`HeartbeatFn` = `crates/agent/src/lib.rs:85`; `stream_subprocess` ticks per stdout line,
`:100-186`; `with_on_activity()` builder on the drivers) but **no server call site wires it**
into `RunStore`/`JobStore`. `sysinfo` is NOT a dependency; `std::fs::metadata().modified()` is
the zero-dep path for build-dir liveness.

## The component

1. **`LivenessTracker` (pure, std-only) — `camerata-core::liveness`.** One abstraction that
   replaces both `JobMeta.last_activity_ms` and `Run.last_activity_ms` (each delegates to it):
   - `tick()` / `record_progress(label)` — bump `last_progress_ms` (atomic).
   - `idle_ms(now)`, `is_stalled(threshold, now)`, `decision(thresholds, now) -> Ok|Alert|Cancel`.
   - Carries an optional progress label ("compiling rocksdb-sys", "agent: <line>").
   Single source of truth → both UI banners and both stores read the same idle/stall math.
2. **Async liveness helpers — `camerata-agent::liveness`** (extends the existing primitives):
   - **Output-line signal:** reuse `stream_subprocess`'s `HeartbeatFn` — tick per stdout/stderr
     line. Wire it into `run_capture_stdout` (give it an `on_progress` callback) and the drivers.
   - **Build-dir mtime probe:** a `tokio::spawn` loop polling the newest mtime under a dir
     (`std::fs`, no new dep) every ~15s; tick on advance. Covers the cargo cold-compile case —
     `target/` rlibs/objects are written continuously even before clippy emits a single line.
   - **Backstop ceiling:** reuse the `DEFAULT_AGENT_TOTAL_TIMEOUT_SECS = 3600` pattern; a kill
     safety net only.
   - Signal priority: output line > build-dir mtime > explicit tick. (Descendant-CPU via
     `sysinfo` is a future option — new dep, deferred; mtime already covers the rivet case.)
3. **Thresholds unified:** keep the 120s default + per-project `StallThresholds`
   (watched/routine) + env override, but route BOTH scans and runs through one source. (Bug to
   fix in passing: `get_run` at `lib.rs:1166` reads the env threshold, ignoring the stored
   per-project value.)

## Home / topology — ROUTE note

Pure tracker → `camerata-core` (new module); async helpers → `camerata-agent` (new module).
Both are **new modules in existing crates** (low structural impact, reversible). Adopting the
component in `camerata-checks` (its layer-2 cargo runners) would require a `checks → agent/core`
dep OR extracting a `camerata-liveness` micro-crate — that is a crate-topology decision and per
ROUTE-1 it **routes to Zach**, deferred to Phase 2. Do NOT auto-create a new crate.

## Adoption list (prioritized, from the map)

- **CRITICAL** — `run_capture_stdout` (`scan_tools.rs:561`): all scan preview tools. The rivet fix.
- **HIGH** — dep-audit call sites (`lib.rs:2465,2927`, `dep_audit.rs:340`); wire the agent
  driver `with_on_activity` → `RunStore::touch_activity` (slot exists, unwired:
  `agent/lib.rs:440`, `generic.rs`); `llm.rs` `complete_cli` (no timeout at all);
  provisioning (`tool_provisioning.rs` semgrep venv/pip, eslint npm — minutes, no heartbeat,
  runs BEFORE the idle clock’s first tick).
- **MEDIUM/LOW** — `checks/src/subprocess.rs` cargo runners (pending the routed crate decision);
  `fleet/lib.rs:163` cargo build; `manifest_runner.rs:207`.

## Phasing

- **Phase 1 (the rivet fix + unification):** `LivenessTracker` in core; `JobMeta`/`Run` delegate
  to it; wire `run_capture_stdout` (output-line + build-dir mtime heartbeat) into `JobStore`;
  wire the agent driver heartbeat into `RunStore`; give dep-audit a liveness signal. UI banners
  read the unified idle. Result: a busy clippy never false-flags; a truly silent run (no output,
  no disk writes for the threshold) does.
- **Phase 2:** provisioning + `checks` adoption (after the crate-topology decision) + optional
  `sysinfo` descendant-CPU probe.
