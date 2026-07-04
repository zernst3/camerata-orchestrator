# Fable 5 audit (COMPLETE) — 2026-07-04

Point-in-time correctness + gap audit run with Fable 5. Run in two passes:
- **Pass 1** (recursive general-purpose agents) covered cockpit.rs + workspace.rs/table.rs before
  the Fable limit was hit. Those findings live in `ARCH_AUDIT_2026-07-04_fable5-partial.md` and are
  summarized here under "Pass 1".
- **Pass 2** (this file) used 6 **bounded, read-only `Explore` agents** (no sub-agent spawning, no
  edits) over the remaining subsystems. All 6 returned complete reports.

Findings are Fable-reported; confidence is Fable's own unless marked VERIFIED. **Nothing here is
fixed yet** — this is the fix backlog. Fix order is in the "Priority" section.

## Coverage (now complete)

| Area | Pass | Status |
|---|---|---|
| UI cockpit.rs | 1 | done (7) |
| UI workspace.rs + table.rs | 1 | done (8) |
| Gate + enforcement (the moat) | 2 | done (7) |
| UoW lifecycle + dev/PR runners + async | 2 | done (13) |
| Server routes + client/server contract | 2 | done (9) |
| GitHub publish + rules emit + creds + llm | 2 | done (8) |
| UI: rules/uow/live_run/scan/design | 2 | done (16) |
| Functionality gaps vs vision | 2 | done (10) |

## Cross-agent confirmations (highest trust — found independently by 2 agents)

- **GitHub token keychain↔env split** — routes-agent #1 AND publish-agent #1. A token saved via
  the Credentials UI (keychain) is invisible to ~18 handlers that read `CAMERATA_GITHUB_TOKEN` env
  only (emit/arm/apply/clone/push/pull/ship/onboarding/release). HIGH.
- **Design "+ Add child" creates TWO nodes** — routes-agent #3 AND ui-agent #12. `api_design_blank`
  then `api_design_materialize` both insert. MED-HIGH.
- **`design_publish` aborts mid-tree, non-idempotent** — routes-agent #4 AND publish-agent #4. First
  create error `?`-returns 500, discarding already-created issues; retry duplicates. MED.
- **Symlink defeats the lexical jail** — gate-agent F1 (VERIFIED real) AND gap-agent Gap 10 (elevate).
  This is a gate bypass, i.e. the moat. HIGHEST priority.

---

## PRIORITY (suggested fix order)

**P0 — gate/safety + moat (do first):**
1. GATE-F1 symlink jail bypass (both write paths) — a repo-committed symlink lets a gated write land
   outside the worktree. VERIFIED. `gateway/src/main.rs:412-439,654`; `server/src/api_agent_driver.rs:1127-1142`.
2. GATE-F2 rules-file load failure falls back to `[GOV-1]` only, silently shedding the whole SEC-*
   floor for the session (fail-OPEN). `gateway/src/main.rs:522-565`. Fix: fall back to `enforced_gate_rules()`.
3. LIFECYCLE-1 Cancel is a no-op for every runner except investigation; worse, the executor later
   clobbers the Cancelled state and still commits+pushes. `run.rs:95-102`; runners have zero
   `is_cancelled` checks. Stop button lies + ships code.
4. GAP-2 PROCESS-*/VCS-action gate is configured + has a bypass endpoint but is **never enforced** at
   any commit/PR chokepoint (only caller is the bypass). `checks/src/vcs_action.rs` vs
   `workspace.rs:1278,1300`, `pr.rs`, `dev_implement_run.rs`. Governance hole.

**P1 — broken primary flows (high, user-visible):**
5. LIFECYCLE-2 `stamp_provenance_when_done` advances Development→AwaitingQa on ANY terminal run
   (failed/cancelled included) + attaches SOC-2 evidence for work that never happened. `lib.rs:1790-1830`.
6. LIFECYCLE-3 provenance watcher gives up after 5 min; real live runs outlive it → UoW stuck at
   Development, no provenance/evidence, sign-off breaks the stage invariant. `lib.rs:1789`.
7. ROUTES-1/PUBLISH-1 GitHub token keychain↔env split (see confirmations). HIGH.
8. ROUTES-2 Manual "Add decision (approved)" always 422s: UI posts empty `provenance`, server can't
   deserialize → the only manual path to satisfy the dev gate is dead. `uow.rs:6182-6191` vs
   `lib.rs:10953-10959` / `investigation.rs:279-280`.
9. UI-1 Table-2 (All rules) row-click to READ silently rewrites the ruleset (writes `default_option`
   into `chosen_ctx` → watcher saves): resets a non-default option to default, or adopts an unadopted
   process/cross-repo rule. `cockpit/rules.rs:1408-1414` + watcher 1045-1077. (Interacts with today's
   `apply_chosen_option` adopt fix — check for regression.)
10. UI-2 "Filter by repo" in Project Rules table is dead (rows minted once at mount, filter not in
    remount key) → also nullifies "View in project rules". `cockpit/rules.rs:970-991,2977`.
11. UI-3 Bug-fix loop silently drops the user's bug report (read, non-empty-checked, never sent or
    appended to transcript). `cockpit/uow.rs:4750-4795`.
12. UI-6 Deep-report export passes a repo name as project_id → export always 404s with a misleading
    toast. `cockpit/scan.rs:3338-3346`. (Also ROUTES-5: server `latest_deep_report` returns an
    arbitrary job, not project-scoped/latest → cross-project leak. `lib.rs:12150-12188`.)
13. UI-5 Live-run polling is component-scoped: switching phase tab / UoW kills the poll, drops the
    LoadingGuard, `active_run=None`, no rehydration → run "vanishes" while still executing server-side.
    `cockpit/uow.rs:5434-5483,2947`.
14. UI-4 Draft story's saved Parent ID never hydrates (seed flag set on first render before resource
    resolves); tabbing through the field then persists `""`, wiping the real parent. `cockpit/uow.rs:2597-2604`.
15. LIFECYCLE-4 Resumed governed runs (Approve/Amend after pause) never get a provenance watcher →
    no provenance/evidence/stage-advance. `lib.rs:1314-1368`.
16. LIFECYCLE-5 Bounce-and-revise loop re-runs the agent with the IDENTICAL original prompt; bounce
    reasons (clippy/L3/contract) are logged then dropped → blind retries, no convergence.
    `dev_implement_run.rs:948-964` + bounce `continue`s.

**P2 — medium:**
17. GATE-F3 in-process gate: an allowed absolute in-jail write is path-doubled (`wt.join(trim)`) →
    write vanishes to a nested garbage path, Layer-2 checks wrong tree. `api_agent_driver.rs:1127-1133`.
18. GATE-F4 stdio gate validates `root.join(path)` but writes at process-cwd → check≠effect unless the
    MCP cwd equals the jail root (undocumented external dependency). `main.rs:431-439` vs `:654`.
19. LIFECYCLE-6 Stall enforcement is dead code: `StallPolicy::Cancel` never fires, `RunKind::Autonomous`
    never constructed, per-project stall thresholds stored but never read. `app-core/src/run.rs:388-403`,
    `project.rs:730-738`.
20. LIFECYCLE-7 No liveness heartbeat on dev-implement + pr-resolve agents → guaranteed false
    "stalled" on the two longest paths; `stalled` also computed for done/parked runs. `api_agent_driver.rs:1678`,
    `dev_implement_run.rs:899-915`, `pr_resolve_run.rs:206`.
21. LIFECYCLE-8 ROUTE-B half of the no-code-first gate ("investigation note reviewed") is never
    enforced; `reviewed` flag + endpoint exist but no transition reads them. `app-core/src/lifecycle.rs:156-174`.
22. LIFECYCLE-9 No single-flight guard: `ensure_development_gate` returns Ok regardless of stage +
    discards `start_development` result → two runs share one worktree; `sign_off_run` has no `run.done`
    check → can tear down a live run's worktree. `lib.rs:1255-1270,2090-2206`.
23. LIFECYCLE-10 Process-wide `set_var` for the gate-events sink cross-contaminates concurrent live
    runs' provenance/SOC-2 attribution. `live_fleet.rs:162-181`.
24. PUBLISH-2 `save_armed_to_project` persists RAW request repos before `normalize_repos` → the
    `\u{0}__single_repo__` sentinel can be written into `project.repos` and selections (fresh project
    from sentinel-only arm → all rules normalize to empty → "nothing to emit" forever). `lib.rs:6091-6149`.
25. PUBLISH-3 API cost estimate bills cache-read tokens at full input price (~10x overstatement on
    cached scans) — the cost meter says caching saved nothing. `llm.rs:979-988,440-447`.
26. ROUTES-3/UI-12 Design "+ Add child" creates two nodes (see confirmations). `design.rs:683-699`.
27. ROUTES-4/PUBLISH-4 `design_publish` aborts mid-tree, non-idempotent (see confirmations). `lib.rs:9701-9807`.
28. UI-7 "Begin Development" has no in-flight guard → double-click starts two governed runs.
    `cockpit/uow.rs:4581-4615`.
29. UI-8 NEEDS-YOU queue and phase-view clarifications use divergent private refresh signals →
    answered clarification stays showing "NEEDS YOU (1)". `live_run.rs:437-485` vs `uow.rs:3903-3913`.
30. UI-10 Silent-failure dead ends (toast-less no-ops, some destroy typed input): ClarifyQuestion
    submit, review resolve, custom-rule delete/save, design authoring send. `live_run.rs:414-421,140-156`,
    `rules.rs:208-217,3205-3215`, `design.rs:875-899`.
31. UI-11 "Pull work items" failure writes an empty list into the app-lifetime cache and renders
    "No open work items" (false). `cockpit/uow.rs:2037-2043`.

**P2 — low / minor:**
32. GATE-F5 jail-root canonicalized but target only lexical → false-DENY of legit absolute writes under
    a symlinked prefix (e.g. macOS `/tmp`→`/private/tmp`). `main.rs:443-450`.
33. GATE-F6 gate self-check plants violations for 6 of 13 enforced arms → GO can hide a broken arm.
    `fleet/src/gate_probe.rs:153-188,237`.
34. GATE-F7 test-scope Waive lets the agent switch off SQL-concat / disabled-TLS / unsafe-deser by
    naming a file `*.spec`/`examples/` etc. (policy choice, undisclosed in ENFORCEMENT.md). `gateway/src/lib.rs:209-214`.
35. PUBLISH-5 keychain READ errors swallowed everywhere (resolve/github_token/anthropic hydration) →
    silent degrade to env/absent, undiagnosable. `credentials.rs:205-216`, `lib.rs:266-277,626-632`.
36. PUBLISH-6 `complete_cli` accepts an error-shaped CLI JSON (exit 0, `is_error:true`/no `result`) as a
    successful empty completion → masks backend failure as low-quality result. `llm.rs:688-701`.
37. PUBLISH-7 `emit_project` iterates `project.repos`, silently omitting selections scoped to a removed
    repo (the other two emit paths use rule-repo union). `lib.rs:6316-6323`.
38. PUBLISH-8 `design_publish` drops a promised sub-issue link with no warning when the parent was
    skipped in that repo. `lib.rs:9768-9788`.
39. ROUTES-6 credential save error detail lost: UI reads `error`, server sends `message`. `credentials.rs:113-118`.
40. ROUTES-7 `AppError` maps every failure (incl. not-found / 4xx-class) to HTTP 500. `lib.rs:12238-12251`.
41. ROUTES-8 Side-effectful GETs: `GET /api/uow/:id` (+ attachments/diagram/mockup-parent) use
    `get_or_create` → a typo'd id persists a junk UoW. `lib.rs:7991-7996` et al.
42. ROUTES-9/UI (dup) runtime `std::env::set_var` in request handlers of a multithreaded server
    (`set_llm_backend`, `set_credential`) races concurrent `getenv` (UB on POSIX). `lib.rs:7473,7548`.
43. UI-9 Option pick fires a duplicate save + duplicate "Option saved." toast (immediate handler AND
    watcher). `cockpit/rules.rs:1676-1682,2926-2950,1045-1077`.
44. UI-13 Design node "Publishes N nodes" summary goes stale after materialize (`tree_res` no refresh
    dep). `design.rs:794-799`.
45. UI-14 `IntakePhaseView` registers ~10 hooks after a conditional early return (hooks-order fragility
    + no hydrate when mounted finished). `cockpit/uow.rs:3408-3422`.
46. UI-15 Ship panel shares step-state across repos + "Push branch" actually opens a PR (marked
    TODO(#105)). `cockpit/uow.rs:4456-4463,4878-4917`.
47. UI-16 "Remove selected from repo" always claims success even when nothing was removed. `cockpit/rules.rs:1191-1213`.
48. LIFECYCLE-11 `answer_escalation` drops the resume result + new run id; old run parked at
    AwaitingReview forever. `lib.rs:7104-7124`.
49. LIFECYCLE-12 Reject-after-bounce doesn't revert committed snapshot commits (`git checkout -- .`
    leaves snapshots; should `git reset --hard checkpoint.base_commit`). `lib.rs:1292-1307`.
50. LIFECYCLE-13 (minor group) resume directive hardcodes wrong pause reason + doesn't restore
    iteration; `ensure_development_gate` reads inline cache not `decisions_for`; `JobStore.finish/fail`
    clobber a cancelled job; `mark_investigation_reviewed` conflates already-reviewed with no-note.

### Pass 1 (already captured in the -partial.md file)
- cockpit.rs F1-F7 (headline: dead `/api/release` update banner; test mocks the non-existent route).
- workspace.rs F1-F8 (headline: "Start branch" leaves panel stale → Push targets old branch;
  drag-onto-branch cherry-picks onto HEAD not the dropped-on branch).

---

## FUNCTIONALITY GAPS vs the vision (analysis, not bugs)

1. **No machine-consumable capability contract; adapter ladder has no rung 1.** 165 ad-hoc
   `serde_json` routes in one 18.6k-line file, no shared api-types crate, zero MCP binding of the
   verbs. Blocks chat→voice→MCP AND is already causing the contract-drift bugs above. Blocks-endgame.
2. **PROCESS-*/VCS gate never enforced** (see P0 #4). Blocks-endgame (moat).
3. **Headless-core extraction is a beachhead and regressing.** #116 Phase-2 state lift not started
   (no `SurfaceState`/`apply` in ui-core; hook count grew 573→614); #117 `LlmPort` trait unbuilt
   (llm.rs still adapter-locked); onboarding/arm/grounding/run-engines live only as axum handlers.
   New features land hooks-first. Blocks rungs 2+.
4. **Govdev console phase-chat panels are canned-string stubs** ("Investigation/Development agent
   response — coming soon. TODO #105") + stale TODOs claiming missing backend gates that DO exist.
   `cockpit/uow.rs:3885,4291`. Degrades the demo path + blurs proven/staged.
5. **Consumer (requirements-owner) UI surface deleted** — `crates/ui/src/screens/` no longer exists;
   `CONSUMER_UX.md` + the ui crate manifest still describe it. Doc-integrity break.
6. **Cross-agent integration gate: 1 of 4 promised categories built** (contract-conformance only;
   wiring/convention/cross-cutting unbuilt); ADR status now under-states it. `gateway/src/integration_gate.rs`.
7. **CLI is a demo harness, not an HTTP adapter** — the architecture diagram draws it as a surface;
   it links the libs directly. High-leverage as the cheap proof of adapter-readiness.
8. **Routine "permission scope" is decorative prose**, not a structured enforced boundary; becomes a
   governance gap the day live routine execution lands. `app-core/src/routine.rs:116-118`.
9. **Doc/claim drift:** README says 16 crates (actually ~19, and the mermaid omits app-core/ui-core);
   RATIONALE says the gate enforces 5 rules (actually ~13). Erodes the honesty differentiator.
10. **Elevate the symlink lead out of the salvage file** — it is the only gate-bypass among the audit
    findings and should be triaged first (now P0 #1, VERIFIED).

---

## Notable CLEAN areas (verified by the agents — no defects)

- Pure `UowStage` transition table (`app-core/src/lifecycle.rs`) — exhaustively tested; the lifecycle
  bugs are all in the callers bypassing it.
- `plan_design_publish` cross-repo linking rule + its 3 tests — correct (today's feature).
- The gate's `evaluate_call`/`RULE_REGISTRY`/rule arms, fail-closed session binding, the cage denylist,
  Layer-2 coordinator (model-free, check-errors-are-hard-errors), verification_gate.
- `normalize_repos`/`resolve_selection_repos` in isolation (the hole is the persist boundary, PUBLISH-2).
- corpus loader, arm.rs emit builder, github_issues.rs parsers, credentials masking/allowlist.
- Bombe LoadingGuard invariant across all audited AI spawns — clean, no violations.
- Full contract match for live_run/uow/scan/rules/routines/chat request+response shapes (bugs were the
  specific shape mismatches called out, not systemic).

## Method note (for next time)

The bounded read-only `Explore` agents (no `Agent`/`Edit`/`Write` tools) delivered full coverage on a
predictable budget with zero recursive spawning. Use this pattern for future Fable audits; do NOT use
general-purpose agents that can spawn their own sub-agents.
