# Tech debt: subprocess + temp-file resource hygiene (reap children, clean scratch)

> **Status: STAGED — not yet filed as a GitHub issue.**
> On the next "GitHub push", create this as a **sub-issue of the Tech Debt Epic (#70)** using the title + body below.

**Title:** Reap spawned subprocesses (`kill_on_drop`) and RAII-clean temp files — stop orphaning processes/scratch

---

## Problem

The running machine accumulated **52 orphaned `osv-scanner` processes** (dep-audits over days) and leaked temp scratch on disk. Root cause: `tokio::process::Command` does not kill the child when its future is dropped unless `.kill_on_drop(true)` is set, and **only 2 of 45 subprocess spawn sites set it**. A timed-out dep-audit (120s `timeout()`) drops the child future and `osv-scanner` runs forever. Temp dirs/files aren't all RAII-cleaned (16 tempfile sites, clearly insufficient). Directive recorded: `docs/decisions/2026-06-24_runtime_resource_hygiene.md` (RUNTIME-SUBPROCESS-HYGIENE-1, RUNTIME-TEMP-HYGIENE-1).

## Fix

1. **`kill_on_drop(true)` at every `Command::new` spawn site (45).** Highest-priority offenders: `crates/server/src/dep_audit.rs` (osv-scanner, the timeout orphan), `scan_tools.rs` (clippy/ruff/eslint/semgrep via `run_capture_stdout`), `tool_provisioning.rs` (pip/npm/go installs), `crates/agent/src/lib.rs` (claude CLI), `crates/checks/src/subprocess.rs` (cargo runners), `workspace.rs`/`update_branch_run.rs` (git), `fleet`.
2. **Verify the dep-audit timeout actually kills `osv-scanner`** (with `kill_on_drop`, dropping the future on timeout kills the child) — add a test/assertion.
3. **Shutdown reaper:** track children that outlive a single await (or the embedded server) and kill them on app shutdown — ties into the window-close → process-exit lifecycle work (`fix/grounding-fallback-and-lifecycle`).
4. **Temp-file RAII audit:** review the 16 tempfile/tempdir sites; convert any manual temp paths to `TempDir`/`NamedTempFile` so `Drop` removes them; clean clone/scan scratch dirs after use.
5. **Promote the two RUNTIME-* rules to corpus principles** and regenerate `CONVENTIONS.md` so Camerata's own scan enforces `kill_on_drop` presence and temp-RAII going forward (dogfood).

## Immediate mitigation
`pkill -9 -f osv-scanner` clears the current 52 orphans (run in a normal terminal).

## Scope
45 subprocess sites + 16 tempfile sites + shutdown reaper + 2 corpus principles. Parent: **Tech Debt Epic #70**.
