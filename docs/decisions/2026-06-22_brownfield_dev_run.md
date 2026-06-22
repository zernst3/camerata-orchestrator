# Brownfield Dev Run: In-Place Story Implementation

**Date:** 2026-06-22
**Status:** Implemented
**Crate:** `camerata-server`
**Module:** `crates/server/src/dev_implement_run.rs`

---

## Context

The existing live-fleet path (`live_fleet::execute_live_run` /
`execute_live_run_tiered`) scaffolds a NEW app from a plan in a throwaway
temp dir. That greenfield path is correct when there is no local repo to
work on, but once a repo is cloned locally and the UoW's branch exists
(the normal state for a real story), we want the agent to edit the
EXISTING codebase on the UoW's branch — not scaffold a blank app elsewhere.

## Decision

### In-place implement path (`dev_implement_run`)

`execute_dev_implement_run` is a new gated runner that implements a story
by editing the existing repo in the UoW's git worktree. It mirrors
`pr_resolve_run` structurally:

- One governed agent per run.
- The agent reads the existing codebase and makes minimal correct changes
  to satisfy the story + every approved decision.
- Layer-2 checks (real toolchain via `camerata_checks::runner_for_worktree`)
  run post-task; failing checks bounce the agent for a revise pass up to
  `max_iterations` (the active project's loop-guard ceiling).
- The SERVER commits and optionally pushes (agent is explicitly forbidden
  from committing or pushing).
- Events surface StageStarted/Finished, layer-1 gate decisions, layer-2
  pass/fail/revise, and the final commit/push outcome.

### Dispatch predicate: brownfield vs. greenfield

`start_governed_run` (lib.rs) determines which path to take:

```
live && resolve_uow_worktree(...) == Some(dir)
  → execute_dev_implement_run (brownfield, in-place)

live && resolve_uow_worktree(...) == None
  → execute_live_run / execute_live_run_tiered (greenfield, scaffold)
```

`is_brownfield(worktree: Option<&Path>) -> bool` is the pure predicate,
tested independently. A UoW with no local clone (no workspace root, no
per-repo override, or the clone doesn't exist on disk yet) falls through
to the greenfield scaffolder unchanged.

### Layer-2: real toolchain

`camerata_checks::runner_for_worktree(dir)` is used for layer-2 checks.
This is the same function the CLI's `po-demo` and the fleet use:
language-detected (Rust/JS/Go/Python/Ruby/Java/C#), real subprocess
runners (cargo fmt/clippy/test, npm/jest, go test, etc.). A worktree with
no detected language degrades to `NoopChecks` (visible, not silent).

### Gate is unchanged (priority #1)

The brownfield path reuses the IDENTICAL gate machinery:

- `camerata_fleet::governed_role("BrownfieldImplementer")` — same function
  as pr_resolve_run (`PrFeedbackResolver`) and update_branch_run
  (`ConflictResolver`).
- `camerata_agent::prepare_session(&session_dir, &gateway_bin, &role, Some(worktree_path))` —
  same call signature, same worktree jail.
- `gated_write` is the agent's ONLY write path (layer-1).
- `Task`, `Write`, `Bash`, `Edit`, `MultiEdit`, `NotebookEdit` are
  DISALLOWED (the `governed_role` deny list, unchanged).

Worktrees change WHERE the agent works, not WHETHER it is gated.

The no-code-first decisions gate (`ensure_development_gate`) runs in the
caller before this function is ever invoked — that gate is also unchanged.

### Prompt: approved decisions are the spec

`implement_prompt` builds the agent's task from:

1. The story title + description.
2. Every APPROVED `DecisionRecord` on the UoW (label + question +
   rationale), in order. Only `Approved` decisions appear; `Pending` and
   `Rejected` decisions are excluded.

The prompt explicitly forbids the agent from committing or pushing
(`"Do NOT run \`git commit\`"`, `"Do NOT push"`).

### Branch handling

`create_branch_at(dir, target_branch)` is called before the agent runs.
This creates the UoW branch if absent (new story) or switches to it if it
already exists (resume) — the same pattern `update_branch_run` uses via
`switch_branch` after the clone is known to have the branch.

### Server commits / pushes

After a clean layer-2 pass, `workspace::commit_all(dir, msg)` commits all
changes with a `"feat: implement story …"` message. `workspace::push_branch`
pushes when a GitHub token is available (`CAMERATA_GITHUB_TOKEN`); without
a token the commit stays local and the operator pushes manually.

### Model selection

The tiered path (`tier_map` given) uses `tier_map.strongest` for the
implementer. The single-model path uses `model` from the caller (or
`default_strongest_model()` when not specified).

### Token-free fallback

When `CAMERATA_LIVE_BUILD != 1`, the run reports an honest
"live mode is off" error and completes `AwaitingQa`. Nothing is faked.

## Consequences

- Brownfield stories with a local clone now implement in-place on their
  branch, not in a throwaway temp dir.
- Greenfield behaviour (scaffold from plan) is preserved for repos without
  a local clone.
- The gate is unchanged: every controlled path uses the same machinery.
- Layer-2 runs the real toolchain, so violations are caught before commit.
- No new permissions or tool surface: the gate's `governed_role` governs
  everything the agent can do.
