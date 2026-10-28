# Stale remote branches (2026-10-28)

Listing for Zach's approval. Nothing has been deleted.

Method: full (unshallowed) fetch of `origin` on 2026-10-28; 48 remote branches.
"Merged" means the branch tip is an ancestor of `origin/main` (`git merge-base --is-ancestor`).
"Stale" means the newest commit on the branch is older than 60 days (before 2026-08-29).
Dates are committer dates. Commit subjects are shown as written, with long dashes normalised to "-".

## Fully merged into `main` (safe to delete; every commit is already on `main`)

All of these are also older than 60 days.

| Branch | Last commit | Subject |
|---|---|---|
| `dev/integration` | 2026-06-24 | docs: decision record for chat-grounds-on-viewed-project fix |
| `fix/gate-hardening` | 2026-07-05 | feat(gate): wire branch-creation gating + record refinements in docs (GAP-2 Batch 1 #3) |
| `fix/lifecycle-provenance` | 2026-07-05 | docs(run-engine): ADR + TECHNICAL for lifecycle/provenance batch 2 |
| `fix/lifecycle-loop` | 2026-07-05 | fix(LIFECYCLE-12): reject-after-bounce reverts committed snapshots |
| `fix/lifecycle-liveness` | 2026-07-05 | fix(test): update model_selection_e2e for the build_agent_driver on_activity arg (LIFECYCLE-7) |
| `fix/checkrunner-diagnostics` | 2026-07-05 | feat(LIFECYCLE-5): CheckRunner returns CheckOutcome - full toolchain diagnostics feed the L2 bounce |
| `fix/gap4-chat` | 2026-07-05 | feat(govdev): wire Investigation/Development phase chats to real LLM (GAP-4) |
| `fix/gap8-routine-scope` | 2026-07-05 | feat(routine): structured, enforced scope boundary (GAP-8) |
| `fix/gap6-integration-gate` | 2026-07-05 | feat(integration-gate): stack-generalized cross-agent gate (GAP-6) |
| `fix/routes-correctness` | 2026-07-05 | docs(ROUTES): ADR + TECHNICAL (error model, read/write access, deep-report scoping) for Batch 4 |
| `fix/prompt-cache-layering` | 2026-07-05 | feat(prompt): cache-layering + kernel v2 (3-layer assembly, stable prefix, provider-neutral cache_control, reasoning block + state-machine addenda) |
| `consolidate/audit-hardening` | 2026-07-07 | test(server): seed workitems_comment test token via the store, not process env |
| `feat/uow-assignee-and-updates` | 2026-07-07 | fix(uow): re-baseline change-poll last-seen on assign so assign doesn't self-flag |
| `feat/adapter-ladder-headless-core` | 2026-07-09 | docs: refresh ARCHITECTURE/TECHNICAL/README + governance & gate-verification docs + diagram |
| `feat/camerata-workspace-git-onboarding-ux` | 2026-07-09 | feat(onboarding): reset-onboarding action + current-branch scan note (fixes 4-5) |
| `spike/dioxus-live-preview` | 2026-07-10 | docs(design): resolve PWA/auto-capture/per-app-auth/web-only + feedback-loop design |
| `feat/feedback-ingestion` | 2026-07-10 | feat(feedback): defect-report ingest model, store, endpoints, and adapter verbs |
| `feat/scaffolder` | 2026-07-10 | feat(scaffold): target enum + custom-rule conventions + create-app endpoint with GitHub repo auto-create (Part 2) |

## No commit in 60 days and NOT merged into `main` (needs a decision; deleting loses these commits)

`git cherry origin/main <branch>` marks every commit below as "+" (no equivalent patch on `main`).

| Branch | Last commit | Subject | Commits not on `main` |
|---|---|---|---|
| `wave2/lint-test-hardening` | 2026-06-19 | test: add focused unit tests across three crates (suppression, rules, vcs_action) | 3: `cf487d9` clippy fixes across the workspace, `9ef32e5` cargo fmt across the workspace, `2722610` unit tests (suppression, rules, vcs_action) |
| `docs/overnight-reconciliation` | 2026-06-19 | docs: overnight reconciliation guide for the 2026-06-19 branch set | 1: `f947343` the reconciliation guide itself |
| `camerata/onboard-governance` | 2026-06-24 | chore(camerata): governance - .camerata/baseline.json | 5: `99e7d23`, `3106e82`, `c203fba`, `e8f3ca3`, `da993cf`, each "chore(camerata): governance - .camerata/baseline.json" (app-generated baseline commits) |

The first two are four months behind `main`; the clippy/fmt sweep would conflict heavily and the reconciliation guide describes a branch set that has since been merged. The third holds output written by the app's own onboarding flow against this repo.

## Not stale (for completeness)

The remaining 27 branches (`main`, `feat/audit-report-export`, `fix/github-ci`, `briefs/hardening`, `wip/rank7`, `wip/rank9-base` and the `cloud/*` session branches) all have commits from 2026-10-27 or 2026-10-28 and are not listed above.
