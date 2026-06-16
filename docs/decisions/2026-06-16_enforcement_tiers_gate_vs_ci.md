# Enforcement tiers: the write-time gate (security) vs CI checks (consistency)

Status: accepted (2026-06-16); both wired.

## Context

Reviewing whether any of the 107 corpus rules should become deny-before-execute gate
rules surfaced a clean dividing line. The gate arm is `fn(path, content) -> allow/deny`
over ONE write — it has no build, no project config, no cross-file or AST view. So a rule
only belongs at the gate if it's deterministically decidable from a single write's path
or content with near-zero false positives.

Evaluating the 16 mechanical corpus rules against that bar: NONE fit. Their own
conformance text specifies CI mechanisms — `SQL-DB-INDEX-2` says "query-plan inspection
in CI", `UI-IMAGE-COMPONENT-1` / `UI-UTC-DATES-1` say "ESLint no-restricted-syntax … in
CI", `ARCH-EXACT-DECIMALS-1` says "a grep gate fails the build + a property test in CI".
They need build context and the ability to exempt the canonical component/helper — which
the write-time gate can't see. Forcing them into the gate would fire without that context
and produce the false-positive noise that kills adoption.

## Decision: two enforcement tiers, mapped by what the check needs

- **Gate tier (write-time, deny-before-execute) = SECURITY.** High strictness, near-zero
  false positives, decidable from one write. This is where secrets, path escapes, raw-SQL
  building, and secret URLs live — and now **secret FILES** (`SEC-NO-SECRET-FILES-1`:
  deny writing a real `.env`/`.pem`/`.key`/`id_rsa`/keystore; templates exempt). These
  are security invariants no agent should be able to talk past.
- **CI tier (integration stage) = CONSISTENCY / ARCHITECTURE.** Mechanical corpus rules
  enforce here, because their checks (lint, query-plan, migration audit, AST) need build
  context. Arming a mechanical rule emits `.camerata/ci-checks.json` (the declarative
  integration config — the parallel to the gate's `.camerata/rules.json`) plus a
  `.github/workflows/camerata-governance.yml` scaffold that surfaces each declared check
  for the team to wire to its stack's mechanism.

The rule of thumb: **gate = "could this leak a secret or escape the sandbox?" → deny
instantly; CI = "is this consistent with how we build?" → fail the PR.** Same strictness
philosophy, applied at the stage where each check can be precise.

## Surface

- `crates/gateway/src/lib.rs`: `SEC-NO-SECRET-FILES-1` arm + registry entry (auto-joins
  the enforced set). 6 gate rules now (5 real + GOV-1, a synthetic test fixture).
- `crates/server/src/arm.rs`: `arm_files_for_repo` emits `.camerata/ci-checks.json` +
  the governance workflow for mechanical rules. Tested.

## Wiring the CI rules is a governed dev task ("Add CI-enforced rules")

A config doesn't enforce itself — turning each declared check in `.camerata/ci-checks.json`
into a real CI mechanism (an ESLint rule, a migration/index audit, an AST lint) is
DEVELOPMENT work, not enforcement. So it's an explicit governed action, not a hidden
side effect: `POST /api/onboard/ci-rules` creates a story ("the agent reads the declared
checks, sees which are already enforced in CI, and implements the rest, wiring them into
the `camerata-governance` workflow") and runs it through `start_governed_run` — the SAME
worktree → gate → layer-2 → bounce pipeline as any dev task. Surfaced as an
"Add CI-enforced rules" button in both the brownfield onboarding results and the Rules
view. Same shape as the fix-audited action: a goal → a governed dev run.

## Next

A `camerata ci` runner that consumes `.camerata/ci-checks.json` + `.camerata/rules.json`
+ `.camerata/baseline.json` and runs the deterministic content checks over a PR's changed
files (failing only on new, non-baselined violations) — so the gate's content rules are
also enforced in CI, ratcheted by the baseline, without a write-time agent in the loop.
