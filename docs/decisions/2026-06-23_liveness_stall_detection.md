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

---

## Implemented (Phase 1) — 2026-06-24

Branch: `feat/liveness-phase1`. Four commits on top of `main`.

### What landed

**Step 1 — `camerata_core::liveness::LivenessTracker`** (`crates/core/src/liveness.rs`)

New module, pub from the crate (`pub use liveness::LivenessTracker`). Thread-safe
pure std-only component: `Arc<AtomicU64>` last-activity + `Arc<Mutex<Option<String>>>`
progress label. API: `tick()`, `record_progress(label)`, `idle_ms(now_ms: u64) -> u64`,
`is_stalled(threshold_ms, now_ms) -> bool`, `last_label() -> Option<String>`,
`last_activity_ms() -> u64`, `with_initial_ms(ms)` (test helper), `Clone`/`Default`.
5 unit tests: fresh idle, tick resets, record_progress resets + stores label, stall
strictly-greater-than, clone shares state.

**Step 2 — `camerata_agent::liveness`** (`crates/agent/src/liveness.rs`)

New module with two exports:
- `newest_mtime(dir: &Path) -> Option<SystemTime>` — pure std::fs walk returning the
  newest modified timestamp under a dir tree. Runs in `spawn_blocking`. 4 unit tests.
- `spawn_mtime_probe(dir: PathBuf, on_heartbeat: HeartbeatFn, interval: Duration) -> JoinHandle<()>`
  — starts a tokio::spawn loop polling `newest_mtime` every `interval` (default 15s via
  `MTIME_PROBE_INTERVAL`). Fires `on_heartbeat` when mtime advances. Self-terminates after
  `MTIME_PROBE_MAX_DURATION` (= `DEFAULT_AGENT_TOTAL_TIMEOUT_SECS` = 1h). Fail-soft (missing
  dir, stat failures silently skip). 1 async test.

Re-exported from crate root: `spawn_mtime_probe`, `newest_mtime`, `MTIME_PROBE_INTERVAL`.

**Step 3 — scan-path liveness wiring (the rivet fix)** (`crates/server/src/scan_tools.rs`)

`run_capture_stdout` gained `on_progress: Option<&HeartbeatFn>`. With `Some(cb)`: streaming
path reads stdout line-by-line via `AsyncBufReadExt`, firing `cb()` per line. With `None`:
falls back to `.output().await` (unchanged behaviour for silent callers).

`run_one_tool` gained `on_progress: Option<HeartbeatFn>`. Starts a `spawn_mtime_probe`
against `dir/target/` for the duration of each tool run (dropped via `_mtime_probe` on exit).
Passes `on_progress.as_ref()` to all 4 `run_capture_stdout` call sites.

`run_scan_tools` builds `on_progress: Option<HeartbeatFn>` from its existing `progress:
Option<(&JobStore, &str)>` param: `Arc::new(move || store.touch_activity(&id))`. Threads
it through `run_one_tool`. `JobStore::touch_activity` bumped to `pub(crate)`.

2 new tests: streaming path fires heartbeat per output line; None path still works.

Net effect: `cargo clippy` compiling rivet keeps `last_activity_ms` fresh throughout the
compile via both signals → `JobStore::idle_ms` stays low → UI banner stops false-firing.

**Step 4 — dev-run heartbeat wiring** (fleet + server)

`camerata-fleet`: two new `_and_activity` variants of the build_from_plan functions:
- `build_from_plan_with_model_iterations_layer2_and_activity` — accepts
  `on_activity: Option<HeartbeatFn>` and wires it via `.with_on_activity(cb.clone())`
  into every driver in the single-model driver-construction block.
- `build_from_plan_with_tier_map_layer2_and_activity` — same for the tiered path.
Old functions become shims that pass `None` (backwards-compatible).

Server wiring sites:
- `live_fleet.rs execute_live_run` → calls `_and_activity` variant with
  `Arc::new(move || store.touch_activity(&run_id, None))`.
- `live_fleet.rs execute_live_run_tiered` → same.
- `investigation_run.rs` (~line 468) → `spawn.driver.with_model().with_clarification(true).with_on_activity(cb)`.
- `update_branch_run.rs resolve_conflicts_and_commit` → `spawn.driver.with_model().with_on_activity(cb)`.

### What is deferred to Phase 1b

The following were explicitly out of scope for Phase 1 per the task brief:

1. **`JobMeta`/`Run` field unification onto `LivenessTracker`** — both stores
   (`JobStore` in `jobs.rs`, `RunStore` in `run.rs`) still carry their own
   `last_activity_ms: u128` fields and their own `idle_ms`/`is_stalled` logic.
   Consolidating them to delegate to `LivenessTracker` is clean-up, not a bug fix,
   and requires touching the UI's polling endpoints. Deferred.

2. **UI changes** — the scan stall banner at `scan.rs:3084` still compares
   `idle > 120_000` directly against `JobStore::idle_ms`. No UI change made.
   After unification (Phase 1b) the banner simply reads the same field.

3. **Dep-audit liveness signal** — `dep_audit.rs` and the `lib.rs` dep-audit
   call sites (`lib.rs:2465, 2927`) still use buffered `.output().await` with no
   heartbeat. They are `HIGH` priority but blocked by no urgency issue; deferred.

4. **Tool-provisioning liveness** — `tool_provisioning.rs` semgrep/eslint
   provisioning runs for minutes before the scan even starts; no heartbeat. Deferred.

5. **`checks`/cargo-runner adoption** — `crates/checks/src/subprocess.rs` cargo
   runners (layer-2) have no heartbeat. Requires the crate-topology decision
   (new `camerata-liveness` micro-crate, routed to Zach per ROUTE-1). Deferred to Phase 2.

6. **`sysinfo` CPU probe** — descendant-CPU signal. Not added (`sysinfo` = new dep).
   Deferred to Phase 2.

7. **`StallThresholds` unification bug** — `get_run` at `lib.rs:1166` reads the env
   threshold, ignoring the stored per-project value. Not fixed here. Deferred.
