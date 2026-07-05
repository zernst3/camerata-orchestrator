# ADR: Gate hardening Batch 1 (GAP-2, LIFECYCLE-10, GATE-F7)

**Date:** 2026-07-05
**Status:** Accepted

## Context

The 2026-07-05 audit blitz (`docs/ARCH_AUDIT_2026-07-04_fable5-complete.md`) and the
escalation-decisions document (`docs/plans/2026-07-05_escalation-decisions.md`) identified three
P0 gate/moat defects that were approved for immediate remediation.

### GAP-2: commit/PR gate configured but never enforced

`crates/checks/src/vcs_action.rs` and its `gate` / `gate_or_bypass` functions existed, and the
manual bypass endpoint at `POST /api/vcs-action-gate/bypass` called them. But no server-side
commit or PR operation ever called the gate. A project could configure process rules (conventional
commit shape, a required story-id, an `AB#<id>` ADO link, branch naming) and watch the server
generate commits and PRs that violated them silently.

### LIFECYCLE-10: per-run gate-events sink set via `std::env::set_var` (process-global)

The gate-events JSONL sink path that lets each run's cockpit display gate decisions from spawned
gateway subprocesses was published by writing `std::env::set_var(GATE_EVENTS_FILE_ENV, ...)` on
every run start. Because `set_var` mutates the OS process environment globally, two concurrent runs
would clobber each other's sink path, cross-contaminating gate provenance records.

### GATE-F7: test-scope Waive let the agent disable three floor rules by filename alone

The gateway's `test_scope_policy` function waived three rules for any path containing `test`,
`fixture`, or `examples`:
- `SEC-NO-DISABLED-TLS-1` (Waive)
- `SEC-NO-UNSAFE-DESERIALIZATION-1` (Waive)
- The `examples/` prefix was also treated as a Waive scope for `SEC-NO-RAW-SQL-CONCAT-1`

The `examples/` waive was the critical gap: example code ships to production and is read by users.
A TLS-verification bypass or unsafe pickle/yaml.load call under `examples/` is a real
vulnerability and a dangerous teaching pattern. Naming a file `examples/foo.py` was enough to
prevent the gate from blocking it.

## Decision

### GAP-2: enforce at every server-side chokepoint via `vcs_choke`

A new module, `crates/server/src/vcs_choke.rs`, is the single shared funnel every server-side VCS
action passes through. It exposes two families of entry points:

- `gated_commit` / `gated_pr` / `gated_branch`: **HARD-BLOCK** on any process-rule violation. The
  caller aborts the action. No commit, no PR, no branch. Use these for actions whose metadata a
  human or the fleet authored and is expected to satisfy the project's conventions.
- `gated_commit_or_bypass` / `gated_pr_or_bypass`: auditable bypass for orchestration-internal
  actions (machine-generated merge commits, onboarding governance PRs) that legitimately cannot
  satisfy the rule at the time of creation. A non-empty `reason` is mandatory; an empty reason is
  itself rejected (`ChokeError::BypassReasonRequired`). On bypass a record summary is returned for
  the audit trail.

Enforcement points as landed:

| Server action | Entry point |
|---|---|
| `POST /api/git/commit` (workspace commit_all, human-initiated) | `gated_commit` (HARD-BLOCK) |
| dev_implement_run final snapshot commit | `gated_commit_or_bypass` (machine; auditable bypass) |
| pr_resolve update-branch merge commit | `gated_commit_or_bypass` (machine; auditable bypass) |
| `POST /api/pr/open` (human-initiated PR) | `gated_pr` (HARD-BLOCK) |
| governance PR from onboarding apply | `gated_pr_or_bypass` (machine; auditable bypass) |

The bypass endpoint at `POST /api/vcs-action-gate/bypass` is unchanged; it remains the explicit
override path for callers that intentionally need to bypass with an audit record.

**Open design point (flagged for Zach):** machine-generated commits currently carry an auditable
bypass rather than being made format-compliant. This is correct in the short term (the machine
cannot always produce a conventional-commit subject for a merge commit), but the right long-term
answer is for the server's commit-authoring paths to produce rule-compliant metadata and take the
`gated_commit` (hard-block) path. That refactor is deferred to a follow-on story.

### LIFECYCLE-10: thread the sink path per-spawn

`start_gate_observability` in `crates/server/src/live_fleet.rs` now creates the per-run JSONL
sink, returns its path in the `LiveObservability` struct, and passes it explicitly to each
`build_from_plan_*` call via the `sink_path` parameter. Every spawned gateway subprocess receives
the sink path in its own per-session `Command::env`, not from the OS process environment. The
`std::env::set_var` call has been removed. Two concurrent runs now write to completely separate
sink files; they cannot read or overwrite each other's gate provenance.

### GATE-F7: `examples/` removed from test scope; disabled-TLS and unsafe-deserialization require explicit waiver

In `crates/gateway/src/lib.rs`:

1. `is_test_scope_path` no longer treats any `examples/` path component as test scope. Example
   code ships to production and is read by users; the gate treats it exactly as production code.
2. `test_scope_policy` for `SEC-NO-DISABLED-TLS-1` and `SEC-NO-UNSAFE-DESERIALIZATION-1` is
   changed from `Waive` to `Downgrade` (a violation in test scope demotes to low severity and logs
   but does not block). The sole write-time escape hatch for either rule in any scope is an explicit
   `// camerata:allow <RULE-ID> <reason>` annotation on or above the offending line. A filename
   alone no longer silences either rule.
3. `SEC-NO-RAW-SQL-CONCAT-1` retains `Waive` in true test scopes (test code commonly builds
   SQL strings for fixture/assertion purposes with no injection risk); `examples/` is no longer
   counted as a test scope for this rule either.

## Consequences

- The commit/PR gate is now a real enforcement gate, not a configured-but-advisory setting.
  Non-compliant commits from human-initiated operations are rejected before git is touched.
- Machine-generated commits use an auditable bypass; the bypass reason is logged for the evidence
  trail. The open design point above tracks the path to making those commits format-compliant too.
- Concurrent live runs can no longer cross-contaminate each other's gate-event streams. Gate
  provenance is per-run and accurate.
- Example code is now subject to the same security floor as production code. Placing a TLS bypass
  or unsafe deserialization call in `examples/` produces a gate deny, not a silent pass.
- Test code retains a targeted relief: `SEC-NO-HARDCODED-SECRETS-1`, `SEC-NO-RAW-SQL-CONCAT-1`,
  `ARCH-NO-SECRETS-IN-URL-1`, `SEC-NO-PRIVATE-KEY-1`, and `SEC-NO-VENDOR-TOKEN-1` remain
  path-waivable in genuine test scopes (test files and `#[cfg(test)]` blocks). The two high-risk
  rules (`SEC-NO-DISABLED-TLS-1`, `SEC-NO-UNSAFE-DESERIALIZATION-1`) require an explicit
  `camerata:allow` annotation everywhere, including test files.
