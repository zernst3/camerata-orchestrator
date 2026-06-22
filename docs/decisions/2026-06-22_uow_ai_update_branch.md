# AI-assisted "Update branch" for a UoW (gated conflict resolution)

Date: 2026-06-22

## Context

A Unit of Work (UoW) develops a story on its own git branch in a local clone. While
that branch is in flight, the base it forked from (a teammate's branch, `main`, an
integration branch) moves on. GitHub's PR view solves this with an "Update branch"
button that merges the base back INTO the PR branch. Camerata's UoW had no equivalent:
the developer had to drop to a terminal, and merge conflicts were entirely manual.

We want the same affordance, AI-assisted: pick a source branch and merge it into the
UoW branch, with a gated agent resolving any conflicts — without weakening the
governance gate that every Camerata agent runs behind.

## Decision

Add two server endpoints + one per-UoW UI control. The merge runs in the UoW's local
clone, on the UoW's branch. The gate is preserved exactly as the investigation runner
preserves it (single gated `claude -p` agent via `governed_role` + `prepare_session`).

### Endpoints

`POST /api/uow/:story_id/branches` → `{ "local": [...], "origin": [...] }`

Lists the branches this UoW can merge FROM, populating the picker. The repo is derived
from the story id (`owner/repo#num` → `owner/repo`) and resolved to its local clone dir
via `resolve_repo_dir` (per-repo path override, else workspace root). `local` =
`git branch --format=%(refname:short)`; `origin` = `git branch -r` with the `origin/`
prefix stripped and the `origin/HEAD` symbolic ref dropped. Token-less / no-clone /
unresolvable-repo → empty lists (graceful, never an error).

`POST /api/uow/:story_id/update-branch` body `{ source_branch, source, model? }`
→ `{ run_id }`

`source` is `"local"` or `"origin"`. Returns a 4xx (no run created) when: `source` is
malformed; `source_branch` is empty; the UoW has no branch yet (nothing to update); the
repo can't be derived; or the repo isn't resolved to a local clone. Otherwise it creates
a run (pollable via `GET /api/runs/:id`) and spawns the merge work.

### Merge → conflict → gated-agent flow

In the UoW's local clone (`crates/server/src/update_branch_run.rs`):

1. Check out the UoW branch (`switch_branch`).
2. If the source is an origin branch, `git fetch` it first. The GitHub token is injected
   ONLY into that fetch's transient authenticated URL (the existing `workspace.rs`
   token-handling rule); with no token, the already-fetched `origin/<branch>` is merged
   as-is. The merge ref is the branch name for a local source, `origin/<branch>` for an
   origin source.
3. `git merge --no-edit <ref>` into the UoW branch.
   - **Clean merge** → git auto-commits the merge commit; the run reports success.
   - **Conflict** → spawn ONE gated agent to resolve the conflict markers and `git add`
     the resolved files. The agent does NOT commit and does NOT push. The SERVER then
     completes the merge commit (`git commit --no-edit`).

A merge that fails for a non-conflict reason (unknown ref, dirty tree) is an error, not
a false conflict — `merge_source` distinguishes the two by whether the tree is mid-merge
with conflicted paths.

### Fail-closed

A merge that cannot be honestly completed never leaves a half-merged claimed-success
tree:

- Live mode off (`CAMERATA_LIVE_BUILD != 1`) + conflicts → abort the merge, report an
  honest "conflicts need the AI resolver" failure (a clean merge still succeeds — it is
  pure local git, no agent needed).
- Agent fails, or leaves any path still conflicted, or the merge commit won't complete →
  `git merge --abort` (tree restored) and the run reports failure.

The verification step re-runs `git diff --diff-filter=U` AFTER the agent finishes and
fails closed if any conflict remains, so a model that claims success without resolving is
caught.

## Gate preservation

The conflict-resolution agent is built from the SAME `camerata_fleet::governed_role` +
`camerata_agent::prepare_session` machinery the fleet and investigation runner use, so it
carries the identical `--allowedTools` = gated tools only and the identical denylist
(`Task`, `Write`, `Bash`, …). Its only mutation path is the governance gate
(`gated_write`, layer-1). It cannot spawn sub-agents (`Task` is disallowed). The repo dir
is passed as the session worktree, jailing the agent's writes to the repo being merged.
Spawning is the server's job, never the agent's. None of `crates/agent`, `crates/gateway`,
or `crates/fleet` internals were modified.

## UI

A per-UoW "Update branch (AI-assisted)" control in `UowDevControls`
(`crates/ui/src/cockpit.rs`): a `<select>` populated from the branches endpoint with
grouped "Local" / "Origin" `<optgroup>`s (origin values carry an `origin:` prefix so the
client knows the source kind), a model select, and a "▶ Update branch (AI-assisted)"
button styled with the existing `btn-run` variant. The button POSTs to the update-branch
endpoint, drives `AgentActivity` on the returned run, and refreshes the UoW when the run
completes. It uses `enc_seg(story_id)` for the `:story_id` path segment (story_id is
`owner/repo#num`) and owns its own active-run signal so it doesn't collide with the
lifecycle run control. A server 4xx raises a toast carrying the reason; no clone / no
branches shows a hint to set the repo path.

## Tests

Token-free, no real network/agent: origin-branch parsing (prefix strip + `origin/HEAD`
drop), `list_merge_sources` empty-for-non-repo, a real-git round-trip exercising clean vs
conflict path selection + abort + the unknown-ref hard error, the request-shape parsers
(`MergeSourceKind::from_wire`, `merge_ref`), the conflict-prompt shape (resolution-
oriented, forbids commit/push), and the runner end-to-end for both the clean-merge success
and the live-mode-off conflict fail-closed (merge aborted, honest error, tree restored).

## Scope

Confined to `crates/server/**` (`workspace.rs`, new `update_branch_run.rs`, `lib.rs`
routes + handlers) and `crates/ui/src/cockpit.rs` (+ one CSS rule in `style.rs`). No
changes to agent/gateway/fleet internals.
