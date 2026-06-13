# Camerata Orchestrator: PHASE0_TASKS.md

Status: Phase 0 build plan. Ordered, dependency-aware, estimated task breakdown that
satisfies the five VISION section 15 acceptance criteria, built against TECH_DESIGN.md.

Estimates use AI-orchestrated-build calibration (agent-built, not human-paced). They are
deliberately optimistic: an hour here is an hour of orchestrated agent work plus the
human's review-and-approve loop, not a human typing the code. Do not pad them into
human-week shapes.

The sequencing rule that overrides all others: **the governance gate firing on the
planted violation (criterion 3) is the thesis-proving moment, so it is made provable as
early as the dependency graph allows.** The check engine and the planted violation are
front-loaded; the polished investigation pass, provenance formatting, and onboarding
proposal land after the gate has already fired.

---

## OUT OF SCOPE (VISION section 4, hard boundaries for Phase 0)

Do NOT build any of these. They are the platform, not the thin slice.

- **No cockpit / web UI.** Phase 0 is a CLI. One input box (a Story string), text
  output, files on disk. No React, no dashboard, no live view.
- **No status dashboard / live telemetry.** Status is a flat record the CLI prints at
  the end. No streaming progress UI, no metrics surface.
- **No multi-feature orchestration.** Exactly ONE Story end to end. No backlog, no
  queue, no DAG beyond the two-node Backend -> Frontend chain.
- **No more than 2 roles.** EXACTLY two role agents: Backend and Frontend. No DB role as
  a third session (the DB boundary is enforced as a rule on the Frontend role, not a
  separate agent), no QA agent, no reviewer agent.
- **No full provenance audit system.** One provenance LINE per change (task_id, role,
  session_id, rules_passed). No queryable audit store, no history UI, no diff lineage
  graph.
- **No parallel agent execution.** Agents run SEQUENTIALLY (Backend to completion, then
  Frontend). Parallelism, rate-limit-aware scheduling, and lock-contention handling are
  P2 (TECH_DESIGN Q4).
- **No subagent spawning.** Two flat role sessions only. PreToolUse subagent gap
  (TECH_DESIGN Q2) is thereby avoided.
- **No full convention extraction or rule synthesis.** Brownfield onboarding maps the
  repo, proposes the should-be baseline, and INSTALLS the governance scaffolding (lint
  config, CI gate steps, agent rules, hooks) as a reviewable diff (T13). It does NOT
  AST-mine brand-new rules from observed patterns (TECH_DESIGN Q6, deferred). The line:
  install the should-be from the corpus = in scope; synthesize new rules = deferred.
- **No embedding / retrieval over the corpus.** All 106 rules fit in context as a
  6-field index (TECH_DESIGN Q5). No vector store.
- **No OAuth / Max-subscription-token plumbing.** Auth is a metered `ANTHROPIC_API_KEY`
  with Max credit auto-applied (TECH_DESIGN Q1, the refuted-thesis correction). Do not
  build an OAuth path.
- **No greenfield scaffolding generator.** Greenfield is a stub in Phase 0; brownfield
  (Agora) is the only onboarding mode exercised.

---

## Task graph (overview)

```
T0  scaffold + auth smoke-test ......... (crit 5)
      |
T1  corpus loader + 106-rule index ..... (crit 1)
      |
T2  three-state bucket classifier ...... (crit 1, 3)
      |
T3  CheckRunner + ESLint reuse + map ... (crit 3)   <-- gate engine
      |
T4  PreToolUse hook (layer 1) .......... (crit 3)
      |
T5  Agent SDK session wrapper .......... (crit 2, 5)
      |
T6  Role entity + scoping + boundaries . (crit 2)
      |
T7  worktree coordinator (sequential) .. (crit 2)
      |
T8  post-task gate + bounce loop ....... (crit 3)   <-- THESIS MOMENT
      |
T9  planted-violation acceptance run ... (crit 3)   <-- proves enforcement
      |
T10 investigation + rule-selection pass  (crit 1)
      |
T11 provenance line per change ......... (crit 4)
      |
T12 human-QA presentation .............. (crit 4)
      |
T13 brownfield onboarding proposal ..... (crit 1)
      |
T14 end-to-end CLI wire-up + run ....... (crit 1-5)
```

T3 and T4 (the two gate layers) are intentionally reachable before the investigation
pass (T10) and before provenance/QA polish (T11/T12), so the gate can be exercised in
isolation the moment T8 lands.

---

## Tasks

### T0: Scaffold the orchestrator and prove auth end to end

- **id:** T0
- **title:** Project scaffold + `ANTHROPIC_API_KEY` auth smoke test
- **description:** Initialize the TypeScript/Node project with the TECH_DESIGN section 8
  module layout (empty stubs for each directory). Add `.claude/worktrees/` and
  `.claude/coordination/` to `.gitignore`. Wire one trivial `query()` call through
  `agents/session.ts` authenticated with `ANTHROPIC_API_KEY` (NOT OAuth), confirm a
  round-trip completes, and confirm the spend draws from the Max monthly Agent SDK credit
  pool. This is the first place the refuted Max-OAuth thesis is replaced with the
  verified API-key mechanism, so it is proven before anything depends on it. Confirm live
  (TECH_DESIGN risk 11): credit pool draw, and set a console spend cap as the safety
  valve. **Establish the two-tier config-as-code layout (TECH_DESIGN section 10):**
  project config (selected rules, roles, gate definitions, the approved RuleSet + its
  version/hash, retry/ceiling settings, the project's tracker binding) in an in-repo
  `.camerata/config.toml` that is committed and git-tracked (portable + auditable via
  PRs); secrets (`ANTHROPIC_API_KEY`, tracker tokens) in a local user dir
  (`~/.config/camerata/`) or the OS keychain, NEVER committed. **Stand up
  `persistence/store.ts` (the Story spine) here:** a minimal SQLite-or-flat-file store for
  Stories, RuleSets, Provenance, and FeatureStatus (TECH_DESIGN section 8). It is
  foundational and read/written by T10, T11, T12, and T14, so it is built in T0 rather than
  left implicit. Schema follows the VISION section 8 entities; no queryable audit store or
  lineage graph (that is deferred platform scope).
- **depends_on:** none
- **estimate:** 2.5h (adds the persistence store to the scaffold + auth smoke test)
- **advances:** criterion 5

### T1: Corpus loader and the 106-rule index

- **id:** T1
- **title:** Load camerata-ai TOML corpus, build the 6-field RuleIndexEntry set
- **description:** In `rules/corpus.ts`, load and parse all 106 TOML files from the
  camerata-ai corpus. The corpus location is a **configured value** (project config /
  env var, resolved relative to a configured corpus root), NEVER a hardcoded absolute
  path, so the build runs in a worktree or on another machine without breaking. In
  `rules/index.ts`, build the `RuleIndexEntry` per TECH_DESIGN Q5: `id`, `domain`,
  `layer` map 1:1; `statement` is DERIVED from the directive of the option named by
  `decision.default` (use `directive`, not `label`); fall back to `decision.question`
  with `unresolved = true` when no default exists; `role_scope` computed from the
  `domain -> scope` lookup. Assert the parsed count is exactly 106 (16 mechanical, 67
  structured, 23 prose) as a fixture guard so corpus drift is caught.
- **depends_on:** T0
- **estimate:** 2h
- **advances:** criterion 1

### T2: Three-state enforcement_kind classifier

- **id:** T2
- **title:** Bucket each rule into deterministic-active / deterministic-declared / review-heuristic
- **description:** In `rules/bucket.ts`, implement the three-state classifier
  (TECH_DESIGN Q5): `mechanical` AND a resolvable `check_ref` -> `deterministic-active`;
  `mechanical` with no shipping check -> `deterministic-declared` (degrade to review at
  runtime, never silently pass); structured/prose -> `review-heuristic`. Encode the
  verified premise collapse ("mechanical => deterministic"; `qualifies` adds no
  discriminating info) and flag the charter-vs-corpus count discrepancy (17 vs 16) in a
  comment so a future corpus change cannot silently reintroduce the off-by-one.
- **depends_on:** T1
- **estimate:** 1h
- **advances:** criteria 1, 3

### T3: CheckRunner interface, ESLint reuse, and the rule-id map (GATE ENGINE)

- **id:** T3
- **title:** Pluggable LanguageCheckRunner + TypeScriptCheckRunner shelling to Agora's ESLint
- **description:** In `checks/CheckRunner.ts`, define the `LanguageCheckRunner` interface
  (`applies()` / `run()`) and a registry. In `checks/TypeScriptCheckRunner.ts`, shell
  `npx eslint . --format json` in a target worktree, parse the JSON, and surface
  violations as `CheckViolation` records. In `rules/ruleMap.ts`, map the ESLint
  `no-restricted-syntax` violation (with the layering message signature) to the Camerata
  id `ARCH-STRICT-LAYERING-1`, and the `next/image` ban to `UI-IMAGE-COMPONENT-1`. Filter
  violations to those whose Camerata id is in the active RuleSet. Stub `RustCheckRunner`
  (registered, not implemented). **The check engine runs Camerata's CANONICAL checks (the
  "should-be" state), not the target repo's pre-existing config.** The canonical checks are
  owned by Camerata / sourced from the corpus and INSTALLED into the worktree during
  onboarding (T13); the engine then shells `eslint` against that installed config. Where a
  target repo already ships an equivalent check, reuse is opportunistic, NOT a dependency:
  Camerata does not assume the repo already enforces anything (a drifted or bare repo is
  the normal case, and bringing it to the should-be state is the point). This is the
  deterministic heart of the gate.
- **depends_on:** T2
- **estimate:** 3h
- **advances:** criterion 3

### T4: PreToolUse hook (layer-1 real-time gate)

- **id:** T4
- **title:** Layer-1 gate: Claude PreToolUse binding behind the provider-neutral GovernanceGateway
- **description:** Build the layer-1 real-time gate (TECH_DESIGN Q2) behind a
  **provider-neutral `GovernanceGateway` interface** (the gate-layer analogue of the
  `session.ts` auth seam), so model-agnosticism lives at the gate layer too. Phase 0
  implements the **Claude PreToolUse binding** in `agents/hooks.ts`: return
  `permissionDecision: "deny"` to hard-block, AND emit a top-level `systemMessage`
  carrying the rule id and an actionable reason (the verified correction:
  `permissionDecisionReason` is audit-only and may not reach the model). Implement the two
  hard boundaries for the Frontend role: (1) no raw DB commands (`Bash` matching
  `\b(psql|pg_dump)\b`) -> deny with `ROLE-PATH-BOUNDARY-FE-1`; (2) writes confined to the
  role's `path_boundaries`. The gate LOGIC (rule -> allow/deny) must NOT assume Claude
  hooks; the hook is one binding. The model-agnostic binding is the MCP tool-gateway
  (TECH_DESIGN Q2), built later, which also closes the subagent-deny gap. No subagents are
  spawned in Phase 0, so that gap does not bite here (comment it as a one-way-door
  constraint for later phases).
- **depends_on:** T0
- **estimate:** 2h
- **advances:** criterion 3

### T5: Agent SDK session wrapper

- **id:** T5
- **title:** query() wrapper with cwd, allowed_tools, permission mode, hook attachment
- **description:** In `agents/session.ts`, wrap the Agent SDK `query()` with the verified
  options (TECH_DESIGN Q1/Q4): `cwd` (NOT `workingDirectory`) set to the role's worktree,
  scoped `allowed_tools`, permission mode, injected rule subset in the system prompt, the
  T4 `GovernanceGateway` binding attached, and `ANTHROPIC_API_KEY` auth. This is the ONLY
  module in `src/` that opens an LLM session; everything else stays deterministic (the
  load-bearing responsibility boundary from TECH_DESIGN section 8). `session.ts` is also
  the auth seam (TECH_DESIGN Q1: API-key binding primary, `claude -p` CLI-OAuth binding
  for solo dogfood) AND the model seam: nothing outside it may assume a specific provider,
  so a future Gemini/Codex binding is a swap here, not a rewrite. Default the agent model
  to Opus 4.8 where reasoning depth matters; allow a cheaper tier for mechanical passes.
- **depends_on:** T0, T4
- **estimate:** 2h
- **advances:** criteria 2, 5

### T6: Role entity, scoping, and path boundaries

- **id:** T6
- **title:** Backend and Frontend Role definitions with scoped boundaries
- **description:** In `roles/role.ts`, define the `Role` entity (`system_prompt`,
  `allowed_tools`, `path_boundaries`, `rule_subset`). In `roles/scoping.ts`, implement the
  `domain -> role_scope` lookup and slice the active RuleSet into each role's
  `rule_subset` (FE gets ui / dioxus / javascript-next rules; BE gets api-layer / rust /
  permissions rules). Define the two concrete roles: Backend (may write under the API
  paths, gets migration/DB tooling) and Frontend (may write only under the UI paths,
  raw-DB boundary enforced by the T4 hook). EXACTLY two roles, no third DB agent.
- **depends_on:** T2, T4
- **estimate:** 1.5h
- **advances:** criterion 2

### T7: Worktree coordinator (sequential)

- **id:** T7
- **title:** git worktree add/remove, cwd wiring, sequential execution, dependency-order integration
- **description:** In `coordinator/worktree.ts`, drive `git worktree add
  .claude/worktrees/<task> -b <task>` and explicit `git worktree remove` cleanup (SDK runs
  do NOT auto-clean, TECH_DESIGN Q4 correction). In `coordinator/handoff.ts`, pass the
  Backend contract artifact forward to the Frontend session via file copy into
  `.claude/coordination/` and prompt context (NOT a premature merge). **The contract
  artifact is a SINGLE TYPED CONTRACT (shared type exports + schema) that is the one source
  for BOTH defining the API endpoint and calling it from the client**, so client/server
  drift is structurally impossible, not hand-synced (the enforced-contract idea applied to
  the FE/BE seam). It is the ONLY surface the two roles share; because roles have
  non-overlapping `path_boundaries`, Backend and Frontend cannot write the same file, so
  there is no overlapping-file merge to resolve, the contract flows one way. In
  `coordinator/integrate.ts`, merge completed-and-gated tasks in dependency order and
  re-gate at integration. `coordinator/schedule.ts` runs Backend to completion, then
  Frontend (sequential; P2 hook for parallel left as a stub). This delivers two agents in
  two isolated worktrees with scoped boundaries.
- **depends_on:** T5, T6
- **estimate:** 2.5h
- **advances:** criterion 2

### T8: Post-task gate and bounce-and-revise loop (THESIS MOMENT)

- **id:** T8
- **title:** Run deterministic-active checks on the produced diff, bounce violations back
- **description:** In `gate/postTask.ts`, after a Task produces a diff, run the active
  `deterministic-active` checks (via T3's CheckRunner) in the worktree. In `gate/bounce.ts`,
  on any fail, format the specific violated Camerata rule id + `file:line` + the rule
  message + a fix suggestion ("move the query into a repository, call it from the
  service") and send it back to the agent session (T5) for revision; re-run the check;
  integrate only on green. **Bound the loop with a CONFIGURABLE max-revision ceiling**
  (from `.camerata/config.toml`, T0): after N failed bounce-and-revise cycles, stop and
  escalate to the human (mark the task BLOCKED with the persisting violation) rather than
  looping forever. This is the deterministic layer-2 backstop AND the moment the thesis
  becomes provable: a violation in the diff is caught mechanically and bounced with the
  exact rule, not just observed. Build this BEFORE the investigation polish so the gate
  can be exercised standalone.
- **depends_on:** T3, T7
- **estimate:** 2.5h
- **advances:** criterion 3

### T9: Planted-violation acceptance run (PROVES ENFORCEMENT)

- **id:** T9
- **title:** Run the Frontend agent against a planted direct-DB violation and prove the catch
- **description:** Drive the Frontend role to produce a diff containing a planted direct
  `db.select(...)` in a service-shaped file (the `ARCH-STRICT-LAYERING-1` violation) and,
  as the second already-shipping check, a planted `next/image` use (the
  `UI-IMAGE-COMPONENT-1` violation, TECH_DESIGN Q5: plant TWO, across API and UI layers).
  Assert the full enforcement path fires: (a) where the agent attempts a raw DB command,
  the T4 PreToolUse hook denies it in real time with the rule id; (b) where a structural
  violation lands in the diff, the T8 post-task gate catches it and bounces the specific
  rule id back; (c) the agent revises; (d) the re-run passes; (e) the diff integrates
  clean. This is the single most important Phase 0 task: it converts "orchestration" into
  "governed orchestration." Defense-in-depth is demonstrated (layer 1 blocks the live
  call, layer 2 catches anything that reaches the diff). **Should-be framing:** the gate
  being enforced is the canonical check Camerata INSTALLED during onboarding (T13), not a
  check the repo happened to already ship, so the demo proves Camerata ADDS governance to a
  repo, the strongest form of the thesis. **Test hygiene:** the planted violations live in
  a fixture / disposable worktree (or are reverted on teardown), so the acceptance run never
  pollutes the real target repo.
- **depends_on:** T8
- **estimate:** 2h
- **advances:** criterion 3

### T10: Investigation driver and rule-selection pass

- **id:** T10
- **title:** Investigation agent: codebase findings + recommended RuleSet + clarifying questions
- **description:** In `investigation/runner.ts`, drive one investigation agent session
  (opened via T5, so the orchestrator itself still makes ZERO model calls) that takes the
  Story and produces: codebase findings, a recommended RuleSet selected from the REAL
  106-rule index (T1), and 2-3 product clarifying questions. **FIRST step is a blast-radius
  triage:** classify the Story's scope (UI-only / API-only / full-stack / config-only),
  then read ONLY the relevant slice of the codebase and plan ONLY the relevant roles. A
  button-color change reads the UI layer, skips the API, and spawns one role; a full
  feature reads both. This keeps trivial changes ceremony-free AND bounds the investigation
  token budget (never load the whole monorepo for a small change). In
  `investigation/ruleSelection.ts`, do deterministic post-processing with NO LLM: validate
  every selected id exists (reject hallucinated ids, bounce), re-derive `enforcement_kind`
  and `role_scope` from the index (trust the index, not the agent's echo), split into the
  gate plan (`deterministic-active` -> post-task check list; `deterministic-declared` +
  `review-heuristic` -> surfaced to human at QA), and slice by `role_scope` into each
  role's `rule_subset`. `investigation/panels.ts` assembles the clarifying questions.
- **depends_on:** T1, T2, T5
- **estimate:** 3h
- **advances:** criterion 1

### T11: Provenance line per change

- **id:** T11
- **title:** Emit one provenance line per governed change
- **description:** In `provenance/trail.ts`, emit exactly ONE provenance line per change:
  `task_id`, `role`, `session_id`, `rules_passed[]` (the deterministic-active rule ids the
  diff cleared). No queryable audit store, no lineage graph (that is the deferred platform
  feature). The line is attached to the diff for QA presentation.
- **depends_on:** T8
- **estimate:** 1h
- **advances:** criterion 4

### T12: Human-QA presentation of the governed diff

- **id:** T12
- **title:** Present the governed diff for human QA with provenance and surfaced review items
- **description:** In the CLI, present the integrated, governed diff for human QA: the
  diff itself, the per-change provenance line (T11), and the `deterministic-declared` +
  `review-heuristic` rules surfaced for human attention (the rules that could not be
  auto-gated). The human owns the final accept. This closes criterion 4: a governed diff
  is produced AND presented with provenance.
- **depends_on:** T11
- **estimate:** 1.5h
- **advances:** criterion 4

### T13: Brownfield onboarding: propose AND install the should-be governance

- **id:** T13
- **title:** Map an in-progress repo, propose the should-be RuleSet, and INSTALL the governance scaffolding as a reviewable diff
- **description:** In `onboarding/brownfield.ts`, onboard an in-progress repo that may have
  NONE of Camerata's bindings (a drifted or bare repo is the normal case; the point is to
  bring it to the should-be state, not to assume it is already there). Three steps:
  (1) **MAP + PROPOSE.** Parse root + `apps/api` + `apps/ui` CLAUDE.md for stated
  conventions; scan existing lint configs to note what (if anything) is already enforced;
  cross-reference the corpus; propose the should-be baseline RuleSet (auto-select clear
  matches, mark applicable-but-unenforced as "recommended", exclude inapplicable rules like
  RUST-* in a TS repo); flag conflicts with the three options (adopt + migrate / keep +
  exception / synthesize variant). (2) **INSTALL (as a reviewable proposal, NEVER a silent
  rewrite).** Generate the governance scaffolding to bring the repo to the should-be state:
  the canonical lint config (the checks T3 will run), the CI/CD gate steps (teardown/rebuild
  the workflow to run the linter + gates at build time, e.g. the missing `npm run lint` the
  Agora API CI never ran), the AI-agent rule files (`.claude` rules / CONVENTIONS), and the
  hooks. (3) **GOVERN THE INSTALL ITSELF.** Emit all of (2) as a human-approvable DIFF/PR
  that the architect reviews and commits; tearing down and rebuilding CI is powerful and
  destructive, so the onboarding changes are themselves proposed -> approved -> committed,
  never applied silently. Output: the proposal doc, the machine-readable baseline RuleSet,
  and the install diff. `onboarding/greenfield.ts` stays a stub (greenfield bakes the same
  scaffolding in from commit zero, deferred).
- **depends_on:** T1, T2
- **estimate:** 4h (the install-scaffolding generation + the reviewable-diff packaging are
  the added cost over a pure detect-and-propose pass)
- **advances:** criterion 1

### T14: End-to-end CLI wire-up and full acceptance run

- **id:** T14
- **title:** Wire intake -> onboarding -> investigation -> two-agent run -> gate -> QA into one CLI run
- **description:** In `cli/main.ts`, wire the full Phase 0 flow: seed one Story (one input
  box / CLI prompt), run brownfield onboarding (T13) to seed the baseline RuleSet, run the
  investigation pass (T10) to produce findings + recommended RuleSet + clarifying
  questions, spawn EXACTLY the two role agents sequentially in two worktrees (T7), exercise
  the planted-violation gate (T9 path), produce the governed diff with provenance (T11),
  and present it for human QA (T12). Run the whole thing on a single Story against the
  Agora monorepo on the metered-key/Max-credit auth (T0). This is the integration task
  that proves all five criteria fire together in one run.
- **depends_on:** T9, T10, T12, T13
- **estimate:** 3h
- **advances:** criteria 1, 2, 3, 4, 5

---

## Estimate roll-up

| Task | Estimate |
|------|----------|
| T0   | 2.5h (scaffold + auth smoke test + persistence store) |
| T1   | 2h   |
| T2   | 1h   |
| T3   | 3h   |
| T4   | 2h   |
| T5   | 2h   |
| T6   | 1.5h |
| T7   | 2.5h |
| T8   | 2.5h |
| T9   | 2h   |
| T10  | 3h   |
| T11  | 1h   |
| T12  | 1.5h |
| T13  | 4h   |
| T14  | 3h   |
| **Total** | **~33.5h orchestrated-build** |

The gate-proving critical path (T0 -> T1 -> T2 -> T3 -> T4 -> T5 -> T6 -> T7 -> T8 -> T9)
reaches the thesis-proving moment in ~20h of build, before the investigation polish,
provenance, QA presentation, and onboarding land. That ordering is deliberate: prove
enforcement first, dress the rest after.

---

## Definition of done

Phase 0 is done when all five VISION section 15 acceptance criteria are satisfied by a
single CLI run against the Agora monorepo, on metered-key/Max-credit auth.

| # | Acceptance criterion | Satisfied by |
|---|----------------------|--------------|
| 1 | System outputs an Investigation: codebase findings + a recommended RuleSet from the REAL corpus + 2-3 product clarifying questions. | T1 (corpus + index), T2 (classification), T10 (investigation driver + selection pass), T13 (brownfield baseline), surfaced via T14. |
| 2 | Spawns EXACTLY TWO role agents (Backend + Frontend) in separate worktrees with scoped boundaries. | T5 (session wrapper), T6 (two Role defs + boundaries), T7 (worktree coordinator, sequential), exercised in T14. |
| 3 | A PLANTED VIOLATION (FE agent attempts a direct DB call) is CAUGHT by the gate and bounced with the specific rule, then resolved. Proves enforcement. | T2 (bucket), T3 (check engine + ESLint reuse), T4 (real-time hook), T8 (post-task bounce loop), T9 (the planted-violation run that proves the catch + resolve). |
| 4 | A governed diff is produced and presented for human QA with a provenance line per change. | T11 (provenance line), T12 (QA presentation), integrated in T14. |
| 5 | Runs entirely on the Max subscription (or the fallback chosen in TECH_DESIGN: metered API key with Max credit auto-applied). | T0 (auth smoke test on `ANTHROPIC_API_KEY` + Max credit), T5 (session auth), confirmed by the full T14 run. |

Criterion 3 is the one that distinguishes this from a generic multi-agent runner, so its
proof (T9) is reachable on the critical path before any of the cosmetic or convenience
tasks. If T9 passes and T14 ties the rest together, Phase 0 has demonstrated governed,
not merely orchestrated, multi-agent development.
