# Tech debt: cargo lock-wait can starve the stall heartbeat (Rust targets, same-repo concurrency)

**Status:** RESOLVED 2026-06-28 (fix option #1). `run_with_heartbeat`
(`crates/checks/src/subprocess.rs`) now reads stdout AND stderr concurrently and fires the
heartbeat on a line from either stream, so cargo's "Blocking waiting for file lock" notice (stderr)
keeps the run alive during a lock-wait. Covered by `streaming_path_fires_heartbeat_on_stderr_lines`.
sccache (#2) and per-worktree target dirs (#3) remain optional future levers if same-repo
concurrency grows. Original analysis below.

## The edge

Concurrent Units of Work that build the **same Rust target repo** serialize on Cargo's
`target/` file lock (we route them all to one `<clone>/.camerata-shared-target` for disk safety;
see TECHNICAL.md §3). This is a deliberate "correctness over parallelism" tradeoff and is fine on
its own. The residual risk is the interaction with stall detection:

- `run_with_heartbeat` (`crates/checks/src/subprocess.rs`) fires the liveness heartbeat **only on
  stdout lines** (`BufReader::new(stdout).lines()` -> `cb()`). stderr is read once at the end
  (buffered), so stderr output does NOT advance the heartbeat mid-run.
- A genuine cold compile is covered by the **mtime probe** (`spawn_mtime_probe` against the target
  dir), which fires heartbeats from target-dir writes even with no stdout.
- BUT a **pure lock-wait** (cargo blocked on the file lock, not compiling) writes nothing to the
  target dir, and cargo prints `Blocking waiting for file lock on build directory` to **stderr**.
  So during the wait: no stdout lines, no target writes -> the heartbeat is silent.

If that wait exceeds the run's stall threshold (`watched_secs` 120 / `routine_secs` 600), the
run-level stall detector could fire: an amber alert for a Watched run (human decides), or an
auto-cancel for an Autonomous routine. Narrow, but a same-repo build pile-up on a routine could
self-cancel.

## Scope

- **Rust target repos only** (Cargo mechanism). JS/Python/Go/etc. targets have no shared cargo
  target and no lock. Bites Camerata-on-Camerata because that repo is Rust.
- **Same-repo only.** Different repos have separate shared targets; they never contend.

## Fix options (when built), cheapest first

1. **Make the lock-wait heartbeat-safe** (preferred): fold stderr into the heartbeat (read stderr
   lines concurrently and `cb()` on each), or special-case cargo's "Blocking waiting for file
   lock" as activity, or have the mtime probe also watch the lock file. Kills the residual edge
   cheaply; no architecture change.
2. **sccache via `RUSTC_WRAPPER`**: shortens the lock-hold window (caches crate compilations).
   Note: sccache does NOT remove the `target/` lock; it eases contention, it does not cure it.
3. **Per-worktree target dirs** behind the existing disk-headroom guard: real parallelism for
   same-repo builds, at ~5 GiB/worktree. Only if true parallel same-repo builds are needed.

Recommended first step is #1 alone; #2/#3 only if same-repo concurrency actually grows.
