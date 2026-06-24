# Runtime resource hygiene is required app behavior

**Date:** 2026-06-24
**Status:** Accepted (directive)
**Context:** A scan of the running machine found **52 orphaned `osv-scanner` processes** (from dep-audits over many days) and accumulating temp scratch on disk. The app was not reaping what it spawned or cleaning what it wrote.

## Directive

Camerata manages the full lifecycle of everything it spawns or writes, and cleans up what it is no longer actively using. This is **assumed app behavior, not optional**. Two rules (to be promoted to corpus principles so they regenerate into `CONVENTIONS.md`):

### RUNTIME-SUBPROCESS-HYGIENE-1 — Spawned subprocesses are reaped, never orphaned
Every spawned subprocess (osv-scanner, cargo/clippy/ruff/semgrep, the agent CLI, git, etc.) sets `kill_on_drop(true)` on its tokio `Command`, so a dropped / timed-out / cancelled future kills the child instead of orphaning it. Children that outlive a single await (or the embedded server itself) are tracked and killed on app shutdown. A dep-audit that times out kills `osv-scanner`; closing the app leaves no surviving child.

### RUNTIME-TEMP-HYGIENE-1 — Temp files and dirs are RAII-cleaned, never leaked
Every temp file/dir the app creates (clone dirs, scan scratch, subprocess input) uses RAII auto-cleanup (`tempfile`'s `TempDir`/`NamedTempFile`, whose `Drop` removes it) or is explicitly removed when its operation ends. Running the app repeatedly does not accumulate scratch.

## Why it was happening (root cause)

`tokio::process::Command` does **not** kill the child when its future/handle is dropped unless `.kill_on_drop(true)` is set. Survey: **45 subprocess spawn sites, only 2 set `kill_on_drop`.** So a timed-out dep-audit (the 120s `timeout()` resolves and drops the child future) leaves `osv-scanner` running forever, reparented to launchd. Same shape for temp files: created but not RAII-cleaned, so they pile up. The OS cleans neither — the app must.

## Consequences (the fix — tracked in the tech-debt issue)

1. Set `kill_on_drop(true)` at every `Command::new` spawn site (45).
2. Track long-lived / detached children and kill them on app shutdown (ties into the window-close → process-exit lifecycle work).
3. Audit the 16 tempfile/tempdir sites for RAII cleanup; convert any manual temp paths to `TempDir`.
4. Promote RUNTIME-SUBPROCESS-HYGIENE-1 + RUNTIME-TEMP-HYGIENE-1 to corpus principles and regenerate `CONVENTIONS.md` (dogfood: Camerata's own scan then enforces them).
5. Immediate: `pkill -9 -f osv-scanner` to clear the current 52 orphans (must be run in a normal terminal; the sandboxed assistant can't signal them).

## Implemented (branch: fix/resource-lifecycle, 2026-06-24)

### Step 1 — kill_on_drop(true) on every tokio Command spawn site (commit bef7942)

Set `kill_on_drop(true)` at all 29 previously-unset tokio spawn sites across 10 files.
Sites set in helpers inherit to all call sites:

| Location | What changed |
|---|---|
| `checks/subprocess.rs` | `run_with_heartbeat()` helper — covers 4 callers (fmt, clippy, test, run_command) |
| `checks/manifest_runner.rs` | `run_manifest_check()`, `check_tool_version()` |
| `server/dep_audit.rs` | osv-scanner inside 120s timeout — **the root bug** |
| `server/tool_provisioning.rs` | 9 sites: semgrep, pip, eslint, npm, osv-scanner probe, go env, go install, `interpreter_available` |
| `server/workspace.rs` | `git()` helper — covers all git ops |
| `server/scan_tools.rs` | `run_capture_stdout()` helper — both branches (None + Some) |
| `fleet/lib.rs` | `run_cargo()` |
| `intake/engine.rs` | `ClaudeLeadEngineer::evaluate()` |
| `intake/review.rs` | `ClaudeRefinementReviewer::review()` |
| `agent/lib.rs` | `stream_subprocess()` — defense-in-depth (already calls `.kill()` on stall) |

`server/llm.rs` already had `kill_on_drop` on both its sites. `std::process::Command` (sync git in tests/greenfield) not applicable.

### Step 2 — dep-audit reap test (commit ac78be1)

Added `kill_on_drop_reaps_child_on_timeout` (#[cfg(unix)]) to `dep_audit::tests`. Proves two invariants:
1. `Child::drop()` with `kill_on_drop=true` kills the process (PID gone after drop + 50ms grace).
2. `tokio::time::timeout(dur, Command::new("sleep").kill_on_drop(true).output())` is cancellable — timeout fires and does not block indefinitely on an orphaned child.
Normal-exit path verified: `kill_on_drop` is a no-op when the child exits before the future drops.

### Step 3 — detached children audit (none needed)

All `tokio::spawn` blocks in the app either (a) run complete execution flows that internally `await` every subprocess call, or (b) drain I/O pipes (not subprocess owners). No truly detached subprocess handles found. No speculative reaper built.

### Step 4 — temp-file RAII audit (commit 33a2d02)

Converted every per-session and per-run scaffold temp dir to `tempfile::TempDir` RAII.

| File | What changed |
|---|---|
| `agent/session.rs` | `prepare_session()` creates `TempDir` internally; `SessionSpawn._dir` holds it |
| `fleet/orchestrator.rs` | `prepare_orchestrator_session()` same; `OrchestratorSession._dir` |
| `fleet/lib.rs` | Two callers updated (non-tiered + tiered path); tiered path holds spawns |
| `gateway/delegate.rs` | Per-delegation `session_dir` → `TempDir` |
| `server/live_fleet.rs` | Per-run root scaffold → `TempDir` (plain + tiered) |
| `server/dev_implement_run.rs` | Manual `session_dir` removed |
| `server/update_branch_run.rs` | Manual `session_dir` removed |
| `server/investigation_run.rs` | Manual `session_dir` removed; `_session_dir` held across post-run path |
| `server/pr_resolve_run.rs` | Manual `session_dir` removed |
| `cli/build_demo.rs` | Manual session dirs removed |
| `cli/live_demo.rs` | `root` param removed from `run_one()`; sandbox (demo output) left as-is |

`tempfile` promoted from `[dev-dependencies]` to `[dependencies]` in: agent, fleet, gateway, server.

`std::process::Command` sites (non-test production code): all are instant signal probes (`kill -0`) or synchronous git ops in test helpers. None are long-running; no action required.

### Step 5 — verification (commit below)

`cargo build --workspace` clean. Tests: `camerata-server` 789 passed, `camerata-checks` all passed, `camerata-agent` 56 passed. CAMERATA_DISABLE_DEP_AUDIT=1 honored throughout.
