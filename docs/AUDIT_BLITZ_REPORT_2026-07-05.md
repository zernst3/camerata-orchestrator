---
priority: high
needs_attention: true
headline: Camerata audit blitz — consolidated PR + what needs you
---

# Camerata audit blitz — consolidated PR

All six audit fix branches merged cleanly into one branch: **`fix/audit-consolidated`**
(off `origin/main`). The wrapper will push it; your assistant opens the PR in the morning.

## Build + test status (GREEN)

- `cargo build -p camerata-ui` — clean (warnings only)
- `cargo build -p camerata-server` — clean (warnings only)
- `cargo test -p camerata-server --lib` — **1082 passed, 0 failed**
- `cargo test -p camerata-ui` — **544 passed, 0 failed**
- `cargo test -p camerata-ui-core` — **105 passed, 0 failed**

All six branches merged; **none had to be left out.** Three merges had a conflict
only in the shared progress-tracker doc (`docs/plans/2026-07-04_audit-fix-plan.md`);
each was resolved by integrating both rows (independent status updates, no code conflicts).

---

## 1. FIXED (in fix/audit-consolidated)

Derived from `git log origin/main..fix/audit-consolidated`.

### Docs (branch fix/audit-docs)
- **GAP-9, GAP-5(doc)** — fixed crate/rule-count drift and retired-consumer-UI claims in
  README / RATIONALE / CONSUMER_UX.

### UI contract + reactivity (branch fix/audit-ui-contract)
- **cockpit-F1, cockpit-F4, cockpit-F5, cockpit-F6, UI-1,2,3,4,6,7,8,9,10,11,13,14,16** —
  dead `/api/release` banner repointed at `/api/updates/check`; row-click no longer rewrites
  the ruleset; repo filter reactive; bug-fix report persisted before run; parent-ID hydration
  guard; deep-report export uses real project_id; double-click guards; shared clarifications
  refresh; dedup option-pick save; toast-on-failure across silent dead ends; pull-work-items
  cache preserved on None; design "publishes N nodes" refresh; hook-order fix; false-success
  remove-from-repo fixed; export status check before Save.

### Workspace panel (branch fix/audit-workspace)
- **workspace-F1..F8** — Start-branch stale panel + Push/Pull target the current branch;
  dirty/ahead pills refresh after Commit/Ship; Clone-all / Export toast on failure;
  F6 drag-onto-branch restricted to the current branch + hint fixed (UI-only; server
  `cherry_pick` branch param deliberately NOT added — out of scope); cherry-pick in-flight
  guard + `dragged_sha` cleared after drop.

### Design double-create (branch fix/audit-design)
- **ROUTES-3 / UI-12** — "+ Add child" no longer creates two nodes (dropped the redundant
  materialize call).

### Server contract (branch fix/audit-server-contract)
- **ROUTES-2** — manual "Add decision" 422 fixed (provenance stamped + server-side default).
- **ROUTES-6** — credential error field aligned (UI reads `message`).
- (ROUTES-5/7/8/9 were deferred by that branch — see NEEDS YOUR ATTENTION.)

### Tonight's blitz — GitHub token / publish / gate / lifecycle (branch fix/audit-blitz)
- **PUBLISH-3** — price cache-read/creation tokens separately in cost estimate.
- **PUBLISH-6** — reject error-shaped CLI completions instead of empty success.
- **ROUTES-1 / PUBLISH-1 / PUBLISH-5** — route GitHub token through keychain-aware resolver;
  warn on keychain read errors.
- **PUBLISH-2** — normalize repos before persisting armed selections.
- **ROUTES-4 / PUBLISH-4 / PUBLISH-8** — make `design_publish` fail-soft; warn on dropped
  sub-issue links.
- **PUBLISH-7** — `emit_project` covers repos scoped by rules, not just `project.repos`.
- **GATE-F6** — gate self-check probes every enforced floor rule
  (`layer1_total == RULE_REGISTRY.len()`).
- **GATE-F2** — rules-file load failure fails closed onto the full enforced floor (not `[GOV-1]`).
- **GATE-F1 / F3 / F4 / F5** — resolve symlinks in the prefix + unify the jail check with the
  write path on both write paths (symlink-jail bypass closed, path-doubling removed,
  check==effect, symlinked-root false-deny fixed); each proved by a new test.
- **LIFECYCLE-11** — don't drop the escalation resume result (old run marked done, new run_id
  returned, 409 on resume Err).
- **LIFECYCLE-13** — terminal job guard (finish/fail no-op on cancelled); review-outcome
  disambiguation (idempotent already-reviewed vs 404 no-note); honest resume pause reason.

**The GATE fixes (F1..F6) are the moat.** Each landed only with a test proving the new
behavior, and the gateway + server-lib suites stay green.

---

## 2. NEEDS YOUR ATTENTION

Reproduced from `~/Library/Logs/claude/audit-escalations.md`. Each needs a human
design / behavioral / semantic / policy call. **No branches were left unmerged** — every
item below was intentionally held back by the blitz, not a merge failure.

### P0 — gate / moat + provenance integrity (do these first)

- **GAP-2 — commit gate configured but never enforced.** `checks/src/vcs_action.rs` +
  its bypass endpoint exist, but no commit/PR chokepoint calls it
  (`workspace.rs:1278,1300`, `pr.rs`, `dev_implement_run.rs`). Wiring it changes commit
  behavior at every chokepoint. *Approach:* decide enforcement points + failure mode, then
  call the gate at each commit/PR chokepoint.
- **LIFECYCLE-10 — process-wide `set_var` for gate-events cross-contaminates concurrent runs.**
  Per-run gate-event sink is delivered via a process-global env var; concurrent runs can read
  each other's sink path — a provenance-integrity + concurrency bug. *Approach:* thread the
  sink path through per-run state instead of a process env var; must not race the gateway
  subprocess contract.
- **GATE-F7 — test-scope Waive policy** was out of this blitz's scope and remains an open
  policy decision.

### P1 — lifecycle behavioral / semantic (bundle 2/3/4 together)

- **LIFECYCLE-1 — Cancel is a no-op in the runners; executor still commits/pushes.** Store
  layer is sound; runner loops don't check `is_cancelled` between steps or before commit/push.
  *Approach:* add `is_cancelled(run_id)` guards at each between-step boundary and immediately
  before commit and push; on cancel, stop before mutating git state.
- **LIFECYCLE-2 — provenance/stage advances on failed AND cancelled runs.**
  `stamp_provenance_when_done` (lib.rs:1793) fires on ANY `run.done`, so a Failed/Cancelled run
  still advances Development→AwaitingQa and attaches SOC-2 evidence. *Approach:* gate the
  stage-advance + evidence-attach on a successful terminal; decide separately whether to still
  freeze gate provenance for a failed run (recommended: yes — honest record of what the gate saw).
- **LIFECYCLE-3 — provenance watcher gives up after ~5 min** (MAX_POLLS = 600 * 500ms,
  lib.rs:1804); real long runs never get provenance/evidence and stick at Development.
  *Approach:* completion notification (run signals done) or a much longer/adaptive poll; write
  the "on completion" path once and share with LIFECYCLE-2.
- **LIFECYCLE-4 — resume path spawns no provenance watcher.** `resume_governed_run`
  (lib.rs:1317) never spawns `stamp_provenance_when_done`. Fix after 2/3 are decided, then
  mirror the spawn (bolting the current watcher on would propagate the failed/cancelled bug).
- **LIFECYCLE-5 — bounce loop re-runs the identical prompt** (revise reasons dropped); can loop
  on the same mistake. *Approach:* thread the denied rule ids + reasons from the bounce into the
  next iteration's grounding.
- **LIFECYCLE-7 — no liveness heartbeat on dev-implement / pr-resolve;** a healthy long run can
  be reported as "stalled". Review with the (dead) stall enforcement (LIFECYCLE-6).
- **LIFECYCLE-9 — no single-flight guard per story/worktree;** two concurrent runs can share a
  worktree and a sign-off can tear down a live run. *Approach:* concurrency design (where the
  lock lives, granularity, reject-vs-queue on a second start).
- **LIFECYCLE-12 — reject-after-bounce does not revert committed snapshots** (would need
  `git reset --hard`). Risky/destructive; needs an explicit rollback-semantics + safety decision.

### P2 — feature / structural (pre-designated skips)

- **GAP-4 — govdev phase-chat panels are canned-string stubs** (wire to real LLM plumbing — feature).
- **GAP-6 — cross-agent integration gate: 3 of 4 categories unbuilt** (feature).
- **ROUTES-5/7/8/9** (deferred by fix/audit-server-contract): ROUTES-7 per-handler 4xx
  status-code judgment (~40 sites), ROUTES-8 per-handler read-vs-write classification of
  ~25 `get_or_create` sites + a new non-creating store getter, ROUTES-5 `JobState` data-model
  change, ROUTES-9 concurrency/env semantics.
- **GAP-1** machine-consumable capability contract + first MCP adapter rung (new crate / API surface).
- **GAP-3** headless-core state lift (#116 Phase 2) + `LlmPort` trait (#117 D2) (structural refactor).
- **GAP-7** CLI is a demo harness, not an HTTP adapter (feature).
- **GAP-8** routine "permission scope" is decorative prose, not enforced (structural, before
  live routines).

### Branches that could not be merged

None. All six branches (docs, ui-contract, workspace, design, server-contract, blitz) merged
cleanly and the consolidated branch is green.
