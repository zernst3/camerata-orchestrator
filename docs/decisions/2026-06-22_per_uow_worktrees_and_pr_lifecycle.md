# Per-UoW worktrees + PR lifecycle in the dev console

**Date:** 2026-06-22 · **Decided by:** Zach (two AskUserQuestion picks: *per-UoW worktrees* + *worktrees first, then PR*).

Two linked asks: (1) work multiple UoWs at once without same-repo conflicts; (2) bring the
push/PR/feedback loop into each UoW's dev console.

## The concurrency problem

The dev run and update-branch operate *in the UoW's local clone, on the UoW's branch* via
`resolve_repo_dir`, which returns **one shared clone dir per repo**. Two UoWs on the SAME repo both
try to check out their branch in that one working tree → git refuses ("branch already checked out") /
clobbering. Different repos are already safe (separate clones).

## Decision 1 — per-UoW git worktrees (structural)

Each UoW operates in its **own git worktree** off the repo's shared `.git`, keyed by the UoW's
branch. Git worktrees are built for exactly this: N branches checked out at once, one object store.

- A repo clone stays the shared bare-ish source of truth (the `.git`); each UoW gets
  `…/<repo>/.camerata-worktrees/<sanitized-branch>` (or similar) via `git worktree add`.
- Resolve a UoW's working dir through a new `resolve_uow_worktree(uow)` (create-on-first-use, reuse
  if present); the dev run, update-branch, gate self-check, and ship/push all run THERE.
- A branch can be checked out in only one worktree → each UoW MUST have a distinct branch (already
  true; keyed by story).
- Cleanup: `git worktree remove` when a UoW is signed-off/torn down (best-effort; prune on startup).
- Caveat surfaced to the user: worktrees remove the **checkout** conflict; two UoWs editing the
  **same lines** still produce a normal merge conflict at PR/merge time (expected, resolved at merge).

Build order: **worktrees first**, so the PR push/ship/resolve operate in each UoW's own worktree
(no rework later).

### Phase 1 implementation note (reconciliation, 2026-06-22)

On building Phase 1 the dev-run path turned out to differ from the assumption above
("the dev run … operate[s] in the UoW's local clone, on the UoW's branch"):

- **`uow_update_branch` → `execute_update_branch_run` DOES operate on the clone/branch** —
  this was the real same-repo collision and is now routed through `resolve_uow_worktree`
  (each UoW's branch checked out in its own `…/<clone>/.camerata-worktrees/<sanitized-branch>`).
- **The live dev run (`execute_live_run` / `execute_live_run_tiered`) does NOT touch the
  clone or the UoW branch.** It scaffolds a brand-new app from a plan into a throwaway
  `temp_dir/camerata-live[-tiered]-<pid>` directory. It therefore needs **no git worktree**.
  It *did* have a latent same-process collision: the scaffold dir was keyed by `<pid>` only,
  so two concurrent dev runs in one process shared it. Fixed by keying the scaffold on
  `<pid>-<run_id>` (run ids are unique). No worktree was introduced here because there is no
  repo/branch checkout to conflict on.

Other call sites were classified and LEFT on the shared clone because they are repo-scoped,
not UoW-scoped: `resolve_local_sources` (scan), `onboard_apply` / `onboard_apply_preflight`
(the single `camerata/onboard-governance` governance branch), `resolve_git_dir` (the
Repository Workspace `git_*` endpoints), and `uow_list_branches` (read-only; refs are shared
across worktrees off the same `.git`, so the shared clone is the right place to list them).

So the worktree seam is: **clone-touching UoW runs go through `resolve_uow_worktree`; the
plan-scaffolding dev run gets a unique temp dir instead.** Ship/push/resolve-PR (Phase 2)
will run in the UoW worktree, which is now the canonical "where this UoW's code lives."

API added in `workspace.rs`: `resolve_uow_worktree(override, workspace_root, repo, branch)`
(+ internal `ensure_uow_worktree(clone, branch)`), `remove_uow_worktree(clone, branch)`,
`prune_worktrees(clone)`, and `sanitize_branch_segment(branch)`. Resolution is create-on-
first-use, reuse-if-present, and returns the existing worktree (never errors) when a branch
is already checked out elsewhere. Paths are returned canonicalized so a freshly-added
worktree's identity string-matches what `git worktree list` reports on the next resolve
(idempotent reuse across the macOS `/var`→`/private/var` symlink, etc.). Cleanup hook:
`remove_uow_worktree` on UoW sign-off (best-effort, non-fatal); `prune_worktrees` across all
known repo clones on server startup.

**Invariant (made explicit):** a git branch can be checked out in only one worktree, so each
UoW must have a distinct branch (already true — branches are keyed by story). If two UoWs ever
shared a branch they would, by definition, share one worktree; `ensure_uow_worktree` returns
the existing worktree for that branch rather than erroring, so the case degrades gracefully.

## Decision 2 — PR lifecycle in the dev console (per UoW)

On existing rails in `workspace.rs` (`push_branch`, `open_pr`, `ship`; `open_pr` already does
head-branch discovery via `pulls?head={owner}:{branch}`):

- **Push + open PR** — once dev is done/cleared, a button pushes the UoW branch and opens a PR with
  a **user-selected target/base branch** (picker in the console). Store the resulting PR number.
- **Pull PR info** — resolve the PR for the UoW: by stored `pr_number` → else search PRs where
  head = `uow.branch` (handles PRs opened directly in GitHub) → **store** the resolved number. Pull
  PR state + comments + CI/check status into the console.
- **Resolve with agent(s)** — feed PR feedback (review comments, failing CI) to a **gated** run (same
  layer-1/2 as the dev run; resolving feedback is still code-writing — gate unchanged).
- **Add a comment** — post a PR/issue comment from the console.

> Phase 2 (Decision 2) is NOT yet implemented — this doc records the Phase 1 worktree
> foundation only.

### Data model
- Add `pr_number: Option<u64>` (+ maybe `pr_url`) to the UoW; persisted (flush-on-set like `branch`).
- Discovery is idempotent: stored number wins; head-branch search is the fallback that backfills it.

## Gate note
The resolve run is governed exactly like the dev run — universal gate, `Task` disallowed, layer-2
bounce. Worktrees change WHERE the agent works, not WHETHER it's gated. Enforcement unchanged.

Relates to [[camerata_gate_universal_enforcement]] and the UoW dev-run architecture.
