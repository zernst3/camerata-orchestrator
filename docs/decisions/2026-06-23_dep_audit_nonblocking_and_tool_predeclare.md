# Decision: dep-audit is non-blocking, last-in-pipeline, hard-bounded; full tool set is pre-declared

**Date:** 2026-06-23
**Status:** Implemented on `fix/dep-audit-nonblocking`

---

## The regression

The deterministic onboarding scan's progress box ("Deterministic scan — X/N tools") had two
bugs introduced when always-on dep-audit was added:

1. **dep-audit BLOCKED the preview linters.** `audit_repos` in `onboard.rs` ran the security
   floor, then called `run_dep_audit(...)` (a potentially-slow osv-scanner subprocess including
   provisioning), and THEN returned.  The preview linters (clippy/ruff/eslint/semgrep) run
   AFTER `audit_repos` returns, in `lib.rs` via `merge_scan_preview`.  When dep-audit stalled
   (user saw 2m19s hang), `audit_repos` never returned and the linters never ran.

2. **The tool count showed "1/2" instead of "1/4" (or whatever the true total is).**  The
   job's "N tools" was derived from tools-seen-so-far (each tool called `det_tool_running` when
   it started executing).  The floor + dep-audit registered inside `audit_repos`; the preview
   linters registered later inside `merge_scan_preview`.  Mid-scan the denominator was 2 and the
   user could not see that linters were still pending.

---

## Three fixes

### Fix 1: Pre-declare the full deterministic tool set at scan start

Before `audit_repos` runs, `onboard_audit_start` (lib.rs) now computes the complete tool list
the scan WILL run:

- `floor` (always, when `run_deterministic`)
- preview linter tool ids derived from the selected mechanical rules via `preview_tool_ids_for_rules`
  (new pure function in `scan_tools.rs` — same `group_by_tool` logic as `run_scan_tools`,
  without executing anything)
- `dep-audit` (unless `CAMERATA_DISABLE_DEP_AUDIT`)

All are registered upfront via the new `JobStore::declare_tools(&jid, &[ids])` method (added to
`jobs.rs`).  From the very first poll the UI sees the correct "N" — e.g. "0/4 tools" rather
than "1/2 tools".

### Fix 2: dep-audit runs LAST — after the floor AND the preview linters

dep-audit was removed from `audit_repos` entirely.  The sequence is now:

1. `audit_repos` — runs the floor (and all AI review passes)
2. `merge_scan_preview` — runs preview linters (clippy/ruff/eslint/semgrep)
3. dep-audit loop — runs per source repo, results appended to report, progress updated in job

dep-audit can NEVER block the linters again because it executes after them.

### Fix 3: Hard 60-second total timeout on the entire dep-audit step

The inner `run_dep_audit` subprocess already had a 120-second scan cap, but provisioning
(`ensure_osv_scanner`) was bounded separately, so the total could reach ~2.5 minutes.  The
call site in `onboard_audit_start` (and in the synchronous `onboard_audit` handler) now wraps
the entire `run_dep_audit` call in `tokio::time::timeout(Duration::from_secs(60), ...)`.  On
timeout: fail-soft, emit `CoverageNote("dependency audit (osv-scanner) timed out after 60 s
for {spec}")`, mark `dep-audit` done on the job, and continue.

dep-audit can never stall the scan more than 60 seconds total, and the linters are already done
by the time dep-audit runs.

---

## Constraints preserved

- **Fail-soft everywhere.** Timeout, provisioning failure, subprocess error, parse error — all
  paths produce a `CoverageNote` and empty findings.  Never panic, never abort the scan.
- **`CAMERATA_DISABLE_DEP_AUDIT` still works.** Pre-declaration skips the `dep-audit` tool id
  when the env var is set.  `run_dep_audit_with_tooling` still fast-exits on the env var.
  Existing tests that set this var continue to pass.
- **Enforcement-ledger capture unaffected.** The ledger capture is in the synchronous
  `onboard_audit` handler and fires AFTER dep-audit (which now also runs there, after
  `merge_scan_preview`).  The floor findings that the ledger captures are in `report.findings`
  before the capture point.
- **The synchronous `onboard_audit` handler** (no job, no live progress) also runs dep-audit
  last with the same 60-second hard timeout and appends findings/notes to the report.

---

## Files changed

| File | Change |
|------|--------|
| `crates/server/src/jobs.rs` | Added `declare_tools(&self, id, &[&str])` batch pre-declaration method |
| `crates/server/src/scan_tools.rs` | Added `preview_tool_ids_for_rules` pure function for upfront tool-id derivation |
| `crates/server/src/onboard.rs` | Removed dep-audit block from `audit_repos`; removed `dep_audit_coverage_notes` local; updated comments |
| `crates/server/src/dep_audit.rs` | Made `DISABLE_ENV_VAR` `pub` so lib.rs can reference it for pre-declaration gating |
| `crates/server/src/lib.rs` | `onboard_audit_start`: added upfront `declare_tools` + dep-audit last with 60s timeout; `onboard_audit` (sync path): added dep-audit last with 60s timeout |

## Tests added (8 new)

- `jobs::tests::declare_tools_predeclares_full_pipeline` — total == declared count before any tool runs
- `jobs::tests::declare_tools_idempotent_on_known_tools` — re-declaration does not reset status or double-count
- `scan_tools::tests::preview_tool_ids_returns_distinct_tool_names` — two rules same tool → one id
- `scan_tools::tests::preview_tool_ids_empty_when_no_mechanical_rules` — architectural rules yield nothing
- `scan_tools::tests::preview_tool_ids_includes_unrouted_for_unknown_linter` — Checkstyle → "unrouted"
- `onboard::tests::audit_repos_does_not_call_dep_audit` — ordering regression guard: floor only in the job after audit_repos
- `dep_audit::tests::hard_timeout_on_slow_dep_audit_produces_coverage_note` — timeout pattern fail-soft
- `dep_audit::tests::disable_env_var_skips_all_network_activity` — DISABLE_ENV_VAR fast-exit contract
