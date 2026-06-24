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
