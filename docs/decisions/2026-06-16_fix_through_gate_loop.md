# The fix-through-gate loop: fixing audited items is governed dev, and verified

Status: accepted (2026-06-16); pipeline wired, verification function built, live-exec opt-in.

## Context

Remediating audited violations is the single most differentiating path — "it fixes the
problem AND proves the fix didn't create a new one" is a claim no advisory tool can make.
It's also the riskiest: an AI rewrite can (a) not actually resolve the violation,
(b) introduce a new one, or (c) silently break behavior. So a fix cannot be a one-shot
edit.

## Decision: a fix is a governed dev task, run through the full loop

Fixing audited items is NOT a special path. It is a development task (the first one,
usually), so it runs the exact same loop as any dev work:

1. **Generate** — the governed fleet writes the fix on a branch in an isolated worktree.
2. **Gate (Layer 1)** — every write passes the deny-before-execute MCP gate; a fix that
   itself violates a rule is denied before a byte hits disk.
3. **Checks (Layer 2)** — `RustCheckRunner` (fmt / clippy / test) runs over the worktree;
   a fix that breaks the build or tests **bounces back to the agent to revise**.
4. **Reviewable diff** — the result is a branch + PR; nothing is applied to the default
   branch without review. The diff is the artifact you approve.
5. **Verify** — compare the findings BEFORE and AFTER by violation identity (rule +
   content fingerprint): did it resolve the target, and did it introduce anything new?

Implemented: the fix endpoint (`POST /api/onboard/fix`) turns findings into a
remediation story and starts a run via `start_governed_run` — the same call `start_run`
makes, so steps 1-4 are the existing `build_from_plan` loop (gate + layer-2 + bounce +
PR). Step 5 is `fix.rs::verify`, a pure, tested function returning
`FixOutcome { resolved, remaining, introduced }` with `clean()` (introduced nothing —
non-negotiable) and `complete()` (introduced nothing AND resolved everything). The
agent-activity drawer surfaces each agent's prompt + output during the run.

## What this guards against (the test targets)

- **Did nothing** — `remaining` non-empty: the target violation survived.
- **Created new debt / regressed** — `introduced` non-empty: `clean()` is false; the fix
  must not pass even if it resolved the target.
- **Over-edit** — out of scope for `verify` (it's about violations, not diff size); the
  reviewable-diff step + a minimal-change directive in the fix prompt address it, and the
  diff review is where an over-broad rewrite is caught.

## Status / next

The pipeline + verification logic are wired and tested; live execution is opt-in
(`CAMERATA_LIVE_BUILD=1`, needs the gateway binary + `claude`). Next: run `verify`
automatically at run completion (re-audit the changed files) and show the
`FixOutcome` + diff in the cockpit before the PR is surfaced for merge.
