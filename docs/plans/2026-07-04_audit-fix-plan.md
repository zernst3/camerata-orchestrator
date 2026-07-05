# Audit fix plan — 2026-07-04

Classifies every finding in `docs/ARCH_AUDIT_2026-07-04_fable5-complete.md` (and the -partial.md
cockpit/workspace findings) as **AUTO** (an autonomous routine may fix it now) or **ZACH** (needs
Zach's attention: a security/moat, behavioral, structural, policy, semantic, or feature decision).

**This file is the work list AND the progress tracker for the scheduled fix routine.** The routine
fixes every AUTO item, skips every ZACH item, and updates the Progress table below.

## Classification criteria

- **AUTO** = the correct fix is clear and low-risk; no design, behavioral, security, structural, or
  policy decision. Mostly UI/contract/silent-failure/doc/local-logic fixes.
- **ZACH** = touches the governance gate / moat, changes runtime behavior in a way a human should sign
  off, is a structural/topology change (per ROUTE-1, structural changes route to Zach), is a policy or
  semantic decision, or is net-new feature work.

## Routine protocol (for the scheduled agent)

1. Work on a fresh branch PER AREA (e.g. `fix/audit-ui-contract`, `fix/audit-publish`,
   `fix/audit-docs`). Do NOT push to `main`.
2. For each AUTO item: make the fix, add/adjust a test where practical, and keep the build GREEN.
   Test per-crate (`cargo test -p camerata-ui` / `-p camerata-server` / `-p camerata-ui-core`), not the
   whole workspace, to stay within time.
3. Open ONE PR per area (no auto-merge label — Zach reviews these). Reference the finding IDs in the PR body.
4. Update the **Progress** table below (status + PR link + commit) as you go, and commit that update.
5. **Skip every ZACH item.** If an AUTO item turns out to need a judgment call, STOP that item, mark it
   `deferred-needs-zach` in the table with a one-line reason, and move on. Do not guess on the gate,
   lifecycle semantics, or structural boundaries.
6. If the build cannot be made green for an item, revert that item and mark it `blocked` with the error.

---

## AUTO — fix these (grouped by area → one PR per group)

### PR: UI contract + reactivity (crates/ui)
- **cockpit-F1** dead `/api/release` banner → point at `/api/updates/check` + match its response shape
  (`current_version`/`latest_version`/`release_url`); fix the test that mocks the phantom route.
- **UI-1** Table-2 (All rules) row-click must NOT write `default_option` into `chosen_ctx` (reading
  currently rewrites the ruleset). Only write `chosen_ctx` on an explicit option-button click. VERIFY
  this didn't regress from today's `apply_chosen_option` work.
- **UI-2** "Filter by repo" dead: include `repo_filter` in the Project-Rules-table remount key (or filter
  reactively via the handle).
- **UI-3** bug-fix loop drops the report: persist the report (`append_development_chat`) before starting
  the run.
- **UI-4** draft Parent ID never hydrates: gate the `parent_seeded` flag on the resolved resource
  (mirror the send-model seeding), so tabbing the field can't wipe the real parent.
- **UI-6** deep-report export passes a repo as project_id: pass the real `project_id` from `ScanResults`.
- **UI-7** "Begin Development" double-click: add a `starting` guard (mirror Begin-investigation).
- **UI-8** NEEDS-YOU vs phase-view clarifications: share one clarifications-refresh signal via context.
- **UI-9** duplicate option-pick save + toast: dedup (watcher skips entries already matching saved state).
- **UI-10** silent-failure dead ends: toast on the failure branch and clear inputs only on success
  (ClarifyQuestion submit, review resolve, custom-rule delete/save, design authoring send).
- **UI-11** "Pull work items" cache wipe: keep the old cache + toast on `None`; overwrite only on `Some`.
- **UI-13** design "Publishes N nodes" stale: give the panel a refresh dependency.
- **UI-14** `IntakePhaseView` hooks after early return: move the early return below all hook registrations.
- **UI-16** "Remove selected from repo" false success: compare `repos.len()` before/after `retain`.
- **cockpit-F4** export writes body without status check: check HTTP status before the Save dialog.
- **cockpit-F5** pending resource renders "No projects yet": show a loading state until resolved.
- **cockpit-F6** silent-failure affordances: toast on failure (Create&open, Open, Import, Memory actions).

### PR: Workspace panel (crates/ui/workspace.rs)
- **workspace-F1** "Start branch" leaves panel stale + Push targets old branch: bump the shared refresh
  and re-fetch; ensure Push/Pull read the current branch after a switch.
- **workspace-F2/F3** dirty/ahead pills stale after Commit/Ship: refresh the RepoCard/health state.
- **workspace-F4/F5** Clone-all / Export silent failure: toast on the discarded `Option`/`bool`.
- **workspace-F6** drag-onto-branch cherry-picks onto HEAD: pass the target branch to the server (needs a
  small server `cherry_pick` branch param) OR restrict the drop target to the current branch + fix the hint.
- **workspace-F7/F8** cherry-pick in-flight guard + clear `dragged_sha` after drop.

### PR: Design canvas double-create (crates/ui + server)
- **ROUTES-3 / UI-12** "+ Add child" creates TWO nodes: drop the redundant `api_design_materialize` call
  in the Add-child handler (the blank call already parents the node).

### PR: Server contract + handlers (crates/server)
- **ROUTES-2** manual "Add decision" 422: stamp `provenance {actor:"user", at: now}` (UI side) AND make
  `RevisionProvenance` default server-side.
- **ROUTES-5** `latest_deep_report` returns an arbitrary job: filter by project id + max by timestamp
  (add a completion timestamp + project id to `JobState`).
- **ROUTES-6** credential error field: UI reads `message` (server sends `message`, not `error`).
- **ROUTES-7** `AppError` all-500: add `NotFound`/`BadRequest` variants and use 404/400 where handler docs promise 4xx.
- **ROUTES-8** side-effectful GETs: read paths use a non-creating `get` (keep `get_or_create` for writes).
- **ROUTES-9** runtime `set_var` in handlers: read the effective backend/key from `AppState`/settings
  store instead of round-tripping the process env.

### PR: GitHub token + publish + rules emit (crates/server)
- **ROUTES-1 / PUBLISH-1** GitHub token keychain↔env split: route all handlers through
  `state.github_token()` (or hydrate `CAMERATA_GITHUB_TOKEN` from the keychain at startup + on save,
  mirroring the Anthropic key). Fix the lying docstring on the free `github_token()`.
- **PUBLISH-2** sentinel persist boundary: normalize repos BEFORE `save_armed_to_project` (and filter NUL
  entries out of `all_repos`/`projects.create`).
- **ROUTES-4 / PUBLISH-4** `design_publish` aborts mid-tree: make per-node create failures fail-soft
  (warning + continue), always return `{nodes, warnings}`; report already-created refs.
- **PUBLISH-8** `design_publish` drops a sub-issue link silently: warn when the parent wasn't published in
  that repo.
- **PUBLISH-3** cost estimate over-bills cache-read tokens: price the three input components separately
  (cache_read 0.1x, cache_creation 1.25x).
- **PUBLISH-5** keychain read errors swallowed: `tracing::warn!` on `Err` from `store.get` in resolve /
  github_token / startup hydration.
- **PUBLISH-6** `complete_cli` accepts an error-shaped payload as empty success: bail on `is_error:true`
  or absent `result`.
- **PUBLISH-7** `emit_project` omits selections scoped to a removed repo: derive the emit repo set
  consistently across the three emit paths; warn on out-of-project selections.
- **GATE-F6** gate self-check probes only 6/13 arms: derive the planted set from `RULE_REGISTRY` and assert
  `layer1_total() == RULE_REGISTRY.len()`. (Test-coverage improvement, low risk → AUTO.)

### PR: Lifecycle minor fixes (crates/server) — SAFE subset only
- **LIFECYCLE-11** `answer_escalation` drops the resume result: mark the old run terminal + return the new
  `run_id`; surface the resume `Err` as 409.
- **LIFECYCLE-13** minor group: `JobStore.finish/fail` must not clobber a cancelled job (terminal guard);
  `mark_investigation_reviewed` disambiguate already-reviewed vs no-note; fix the resume directive's
  hardcoded pause reason.

### PR: Doc accuracy sweep (docs + crate comments)
- **GAP-9 / GAP-5(doc part)** README crate count (16 → actual ~19) + add app-core/ui-core to the mermaid;
  RATIONALE rule count (5 → ~13); update `CONSUMER_UX.md` UI-status + `crates/ui/Cargo.toml` header to
  "engine spine only; consumer UI retired"; delete the stale TODO(#105) comments that claim missing
  backend gates which DO exist (`cockpit/uow.rs:4337-4341`).

---

## ZACH — do NOT auto-fix (needs your attention)

**P0 gate / moat (security — review before changing):**
- **GATE-F1** symlink defeats the lexical jail (VERIFIED gate bypass, both write paths). Careful fix +
  review. TOP priority.
- **GATE-F2** rules-file load failure fails OPEN to `[GOV-1]`. Fix is clear (fall back to
  `enforced_gate_rules()`) but it is the security floor — you sign off.
- **GATE-F3/F4/F5** in-process path-doubling, stdio check≠effect cwd, canonicalization false-deny. Gate
  path handling — review together with F1.
- **GATE-F7** test-scope Waive lets the agent switch off 3 floor rules via filename. POLICY decision.

**Lifecycle behavior/semantics:**
- **LIFECYCLE-1** Cancel is a no-op + clobbers Cancelled + still commits/pushes. Behavioral + safety;
  fix design (terminal-state guard + is_cancelled checks + abort registration) needs your call.
- **LIFECYCLE-2** provenance/stage advances on failed+cancelled runs (attaches SOC-2 evidence for no work).
  Semantic decision on what AwaitingQa means.
- **LIFECYCLE-3** 5-min provenance watcher outlived by real runs. Needs a design (completion notification
  vs longer poll).
- **LIFECYCLE-4** resume path spawns no provenance watcher (bundle with 2/3).
- **LIFECYCLE-5** bounce loop re-runs the identical prompt (reasons dropped). Behavioral.
- **LIFECYCLE-6** stall enforcement dead code (auto-cancel is behavioral).
- **LIFECYCLE-7** no liveness heartbeat on dev-implement/pr-resolve (false "stalled"). Touches runner
  internals; review.
- **LIFECYCLE-8** ROUTE-B "investigation reviewed" gate never enforced. Behavioral (blocks a transition).
- **LIFECYCLE-9** no single-flight guard (concurrent runs share a worktree; sign-off tears down live run).
  Behavioral.
- **LIFECYCLE-10** process-wide `set_var` gate-events cross-contamination. Provenance integrity + concurrency.
- **LIFECYCLE-12** reject-after-bounce doesn't revert committed snapshots (`git reset --hard`). Risky.

**Structural / feature (ROUTE-1 → route to Zach):**
- **GAP-1** machine-consumable capability contract + first MCP adapter rung. New crate / API surface.
- **GAP-2** PROCESS-*/VCS commit gate configured but never enforced. Behavioral (changes commit behavior).
- **GAP-3** headless-core state lift (#116 Phase 2) + `LlmPort` trait (#117 D2). Structural refactor.
- **GAP-4** govdev phase-chat panels are canned-string stubs. Feature work (wire to LLM plumbing).
- **GAP-6** cross-agent integration gate: 3 of 4 categories unbuilt. Feature.
- **GAP-7** CLI is a demo harness, not an HTTP adapter. Feature.
- **GAP-8** routine "permission scope" is decorative prose, not enforced. Structural (before live routines).

---

## Progress

| Area / PR | Findings | Status | PR / commit |
|---|---|---|---|
| UI contract + reactivity | cockpit-F1, UI-1,2,3,4,6,7,8,9,10,11,13,14,16, cockpit-F4,5,6 | not started | — |
| Workspace panel | workspace-F1..F8 | not started | — |
| Design double-create | ROUTES-3/UI-12 | not started | — |
| Server contract + handlers | ROUTES-2,5,6,7,8,9 | partial | branch fix/audit-server-contract (commit 0388420); ROUTES-2, ROUTES-6 done (worktracker+ui green). ROUTES-5/7/8/9 deferred-needs-zach: ROUTES-7 (per-handler 4xx status-code judgment across ~40 sites), ROUTES-8 (per-handler read-vs-write classification of ~25 get_or_create sites + new store getter), ROUTES-5 (JobState data-model change), ROUTES-9 (concurrency/env semantics) |
| GitHub token + publish + emit | ROUTES-1/PUBLISH-1, PUBLISH-2,3,4,5,6,7,8, GATE-F6 | not started | — |
| Lifecycle minor (safe) | LIFECYCLE-11,13 | not started | — |
| Doc accuracy sweep | GAP-9, GAP-5(doc) | not started | — |

_Routine updates this table as PRs land._
