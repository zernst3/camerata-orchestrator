# Development console: gap analysis against roadmap 2a-2d

Repo: camerata-orchestrator @ feat/audit-report-export. Read-only; nothing built or run. All paths under `crates/server/src/` unless noted. Verdict up front: the console is a well-tested single-agent "implement one story" pipeline with a GitHub-shaped spine. It is not the orchestrator/builder/reviewer loop 2a describes, and the brownfield path has one click-path break that likely means it has never run live end to end from the UI.

## 1. What exists today (flow, with file:line)

1. **Pull work items**: `POST /api/workitems/pull` (lib.rs:1505, handler ~11795) calls `github_issues::list_open_issues_with_parents`. Token-less returns an empty list.
2. **Create UoW**: `POST /api/uow/from-workitem` (lib.rs:1518, 12266). UoW key = `OWNER/REPO#N` (workitems.rs:152-170). Alternative: blank draft `POST /api/uow/blank` (12339) + AI authoring (`/author`) + `/publish` (12634), which must open a GitHub issue (12665) to get a real id.
3. **Intake**: context and repo/branch scope `POST .../intake/repos` (1576 -> uow.rs:1004). `BranchMode::{Existing, NewFromBase}` (api-types/uow.rs:219).
4. **Investigation**: `POST .../begin-investigation` (15252) -> one gated `Investigator` agent (investigation_run.rs), model = caller or `tier_map.strongest`. Live-only; off-mode records a placeholder. It writes a prose note. It does NOT emit `DecisionRecord`s (grep: none in investigation_run.rs). Decisions are typed by the architect in the UI ("Add decision (approved)", ui/cockpit/uow.rs:7137) and stored via `POST .../decisions` (15031); `approve-decisions` (15990).
5. **Run**: `POST /api/stories/:id/run` (2081): per-story active-run guard (2101), no-code-first gate `ensure_development_gate` (2145: >=1 decision, all approved, contract if cross-boundary), then `start_governed_run` (2616).
   - Brownfield iff a worktree resolves (2693-2735): requires `uow.branch` to be `Some`. **The only writer of `uow.branch` is `POST /api/uow/:id/branch` (13970), and the UI never calls it** (UI only hits `/branches` list and `/intake/repos`; `set_intake_repos` does not touch `branch`). A single-repo UoW therefore has `branch = None`, `worktree = None`, and falls to the **greenfield scaffolder in a temp dir** (live_fleet.rs header; `is_brownfield` dev_implement_run.rs:933). Multi-repo (>=2 intake repos) derives per-repo branches (2484-2500) but the primary-repo dir still comes from `uow.branch`. Tests set the branch directly (lib.rs:21360).
   - `spawn_brownfield_dev_run` (2388): grounding = project ruleset summary + repo digest (624-660); L3 bundle only if `l3_review.enabled` (2410-2431); then `execute_dev_implement_run` (dev_implement_run.rs:950).
   - Inside: `governed_role("BrownfieldImplementer")` (1169) -> rule subset from `DEFAULT_CORPUS_PATH` + `FLEET_DOMAINS = ["rust","agentic"]` (fleet/lib.rs:36,88; rules/lib.rs:1586, a compile-time path), **not the project's selected ruleset**. Driver built with `orchestrator = false` (1238): no `delegate`, no `fan_out` (comment at 1211 says orchestrator-via-API dispatch "not yet implemented"). Branch created in worktree (1136). Loop up to `max_iterations` (default 1): L1 gate during writes; L2 `runner_for_worktree` (1572); L3 reviewer if enabled, **advisory on error** (371-379); integration gate; test-tamper guard; `commit_all` (2025); push only if GitHub token (2060-2120); status `AwaitingQa` (2136). Watcher advances stage `Development -> AwaitingQa` on success (2864).
6. **PR**: `POST .../pr/open` (15671: token required, `repo_from_story_id` 15354, push, `open_pr_with_base`), `GET .../pr`, `/pr/comment`, `/pr/resolve` (15865: `PrFeedbackResolver` agent), multi-repo `intake/ship` (14093). PR title/body via `vcs_choke::compliant_machine_pr`.
7. **Merge**: no merge endpoint anywhere (grep `merge_pull_request|/merge` = none). Merge is manual in GitHub.
8. **Sign-off**: `POST /api/runs/:id/sign-off` (3548), blocked by a critical scoped-scan finding without a waiver; tears down the worktree (~3670); post-story doc hook (agent/post_story_hook.rs).

Stubbed/placeholder: everything agentic refuses without `CAMERATA_LIVE_BUILD=1` (7 files). Enforced corpus coverage is 6 L1 arms + 6 L2 runners (ENFORCEMENT.md:186); everything else in the subset is a no-op carried as data. No decision log, no parallel-run cap or machine-aware limit (grep `Semaphore|max_parallel` = none; only per-story single-flight), no autonomous dev runs (`RunKind::Autonomous` has no non-test caller). `docs/LIVE_RUN_VERIFICATION.md` proves the L1 deny/allow with a real agent (2026-06-13), not this console's flow.

## 2. Roles and model selection vs 2a

Today (fleet/tier.rs:146-169; api-types/project.rs:17,97): `TierMap` fast=`claude-haiku-4-5-20251001`, balanced=`claude-sonnet-5`, strongest=`claude-opus-5`; `StepModels` all `claude-sonnet-5`; `L3ReviewConfig` disabled, model empty -> balanced (review_agent.rs:526). Settings UI exists for all (cockpit.rs:1259, 2992). UI sends all three tiers per run (cockpit.rs:1476); brownfield consumes only `strongest` (lib.rs:2730).

| 2a role | Today |
|---|---|
| Orchestrator (plans, briefs, merges, no code) | Absent. Strongest model IS the implementer. Delegate/fan_out exist only on the greenfield scaffold path. |
| Builder (one brief, own worktree, runs gate, commits, never pushes) | Partially: one implementer per UoW in a worktree, server commits. Server also pushes. No brief/time-box/file-ownership. |
| Mechanical (search, rote) | Unused in brownfield. |
| Reviewer (adversarial, HIGH/MED/LOW, every finding resolved or decided) | L3 is opt-in, PASS/BOUNCE list, defaults to balanced tier, advisory on LLM error, feeds the same agent for a revise pass. No ranking, no decision record for dropped findings. |

## 3. GitHub Issues as a hard dependency (2b)

Structural: the UoW key is `OWNER/REPO#N`, and the repo is re-derived from it at `repo_from_story_id` (15354) and `rsplit_once('#')` sites: run dispatch 2705/2725, pr/open 15693, pr/resolve ~15887, update-branch 15355, vcs_choke.rs:242 (story number for commit messages). Board calls: pull 11795, refresh 11849, comment 11880, comments 11907, assignees 11934, assign 11973, updated-check 12054, set-parent 12107-12161, from-workitem 12301-12310, publish 12665-12714, decompose/commit 13718-13803. Token gates push (dev_implement_run.rs:2060), PR open/resolve, publish.

Making boards optional touches: a UoW identity independent of a tracker (`Provider::Native` exists in worktracker/lib.rs:137 but has zero server uses); explicit `repo` on the UoW instead of parsing the key; a tracker-less `source` field; push/PR via SSH or `gh` without a GitHub token; the Issue Management panel becoming one adapter; `vcs_choke::numeric_story_id` fallback.

## 4. Inspection findings -> units of work (2c)

Nothing connects them. "finding" in uow.rs refers only to the post-run scoped scan on the evidence record (uow.rs:262, 1574). No `from-findings` endpoint; dispositions (`baselines_from_findings` 7595) and delta retests (6960-6973) do not reference UoWs. The handoff needs: a UoW `source` carrying finding ids, rule id, file:line, inspection id and commit; a grouping step (per 2d); the rule set flowing into the agent's L1 subset (today compile-time corpus) and the L3 prose; and the delta retest writing its evidence onto the UoW.

## 5. Branch and PR handling vs 2d

One branch per UoW (`camerata/<story_id>` fallback 2727), one PR per UoW with caller-chosen base. No per-rule grouping, no size split, no severity/dependency ordering or rebase chaining, no finding-level traceability in the PR body. Multi-repo ship pushes and opens one PR per in-scope repo (14121-14180).

## 6. Smallest live run that would prove it

Fixture: `crates/server/tests/fixtures/e2e_report_repo` (Rust, 3 planted violations: SQL concat, AKIA secret, token-in-URL; not a git repo). Steps: copy to scratch, `git init` + commit, push to a throwaway GitHub repo `zernst/camerata-fixture`; `cargo build --release -p camerata-gateway`; `claude` CLI logged in; `CAMERATA_LIVE_BUILD=1`, `CAMERATA_GITHUB_TOKEN`; create project with that repo, `settings.repo_paths` override to the clone, L3 enabled; open issue #1 "remove the hardcoded AWS key in config.rs"; pull, create UoW, begin investigation, add one approved decision, approve, **`POST /api/uow/zernst%2Fcamerata-fixture%231/branch {"branch":"camerata/fix-1"}` via curl (the UI cannot)** (fixed 2026-10-28: Begin Development records it from the Intake scope), run with tier_map, open PR, merge by hand, sign off.

Expect to break: (a) without the curl, greenfield scaffold runs instead of editing the fixture (fixed 2026-10-28: such a run is now refused with the reason); (b) the agent's L1 subset is the Rust/agentic corpus slice, so a non-Rust client repo gets Rust rules; (c) L2 `cargo clippy/test` on the fixture may fail "could-not-run" (bootstrap toggle exists, cockpit.rs:1484); (d) L3 bounce with `max_iterations = 1` fails the run rather than iterating; (e) path params carrying `/` and `#` need percent-encoding in every client, verify axum matching; (f) Opus cost and stall thresholds (120 s watched) on a slow first run.

## 7. Ranked gaps

| # | Gap | Size | Files | Status |
|---|---|---|---|---|
| 1 | UI never sets `uow.branch`; single-repo runs fall to greenfield | S | lib.rs 2693-2735, 13970; ui/cockpit/uow.rs intake scope | FIXED 2026-10-28 (`cloud/console-gaps`). Begin Development (and the bug-fix re-run) now calls `POST /api/uow/:id/branch` with the branch derived from the Intake repo scope when none is set (`ui/cockpit/uow.rs` `ensure_run_branch`; derivation shared with the server as `api-types` `BranchMode::branch_for_story`). The server decides the target in `server/src/run_target.rs`: a unit that should work in a repository but has no branch, no local clone, or an issue repo outside its scope is refused with the reason (409 on `POST /api/stories/:id/run`, a failed run for direct callers); greenfield only for a project with no repositories, announced in the run events. Also covers multi-repo when the issue repo is in scope. Correction: `uow.branch` had a second writer, `orchestrator_message.rs` (~345). |
| 2 | Agent L1 rule subset is compile-time corpus + fixed domains, not the project's selected rules | M | fleet/lib.rs:36,88; rules/lib.rs:1586; dev_implement_run.rs:1169 | FIXED 2026-10-28 for the brownfield implementer (`cloud/console-gaps`). Its subset is now the project's selected rules, resolved by `resolve_project_selection_rules` (the resolver the inspection's delta rescan uses), plus every enforced gate rule (`server/src/governed_rules.rs`, `fleet` `governed_role_from_rules`); no selection or no active project falls back to the old corpus subset and logs once per project. The run's events name the source. Corrections: (1) the enforced gate rules were already always unioned in (`governed_role`), and no gate arm is Rust-specific, so L1 enforcement did not actually differ by language; the fixed slice only added inert Rust ids. (2) Only rules with a gateway arm (`RULE_REGISTRY`, 13 universal content/path rules incl. `SEC-NO-RAW-SQL-CONCAT-1`) deny at write time; selected SQL/Supabase-domain rules such as `SUPABASE-RLS-ENABLED-1` are now carried in the subset but have no arm, so they are not enforced by L1. Still open: (a) the PR-feedback, conflict and investigation agents have no project context and keep the fallback subset; (b) selected rules without a gateway arm are carried but inert at Layer 1: only the arms in `RULE_REGISTRY` deny at write time, so a selected rule such as `SUPABASE-RLS-ENABLED-1` rides along as grounding and enforces nothing until it gets an arm. The run's events now say how many selected rules are enforced at write time and name the inert ones. |
| 3 | No orchestrator/builder/reviewer loop in brownfield; strongest model implements directly, no delegate | L | dev_implement_run.rs:1211,1238; live_fleet.rs; gateway/delegate.rs | Open |
| 4 | Reviewer not adversarial by contract: opt-in, balanced default, PASS/BOUNCE, advisory on error | M | review_agent.rs:35-135,371; api-types/project.rs:97 | Open |
| 5 | GitHub-shaped UoW identity and token-gated push/PR (2b) | L | workitems.rs; lib.rs 15354 + sites above; worktracker Provider::Native | Open |
| 6 | No findings -> UoW handoff, no delta-retest evidence on UoW (2c) | L | lib.rs 7595, 6960; uow.rs; new endpoint | Open |
| 7 | Per-rule grouping, split, ordering, traceable PR body (2d) | M | new planner; vcs_choke compliant_machine_pr | Open |
| 8 | No merge step, no decision log, no parallel cap or resource guards | M | lib.rs run start; pr.rs; new | Open |
| 9 | Investigation emits prose only; decisions are hand-typed | S | investigation_run.rs; ui/cockpit/uow.rs:7098 | Open |
| 10 | Live-run proof of the console flow does not exist (LIVE_RUN_VERIFICATION covers the gate only) | S | docs/LIVE_RUN_VERIFICATION.md; run of section 6 | Open |
