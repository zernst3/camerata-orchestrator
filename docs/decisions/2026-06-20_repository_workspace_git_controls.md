# Repository Workspace: Fuller Local Git Controls (issue #37)

**Date:** 2026-06-20
**Status:** Implemented
**Issue:** #37

---

## What

Added richer local git inspection and branch-management controls to the Repository Workspace:

1. **Ahead/behind counting** (`AheadBehind`, `parse_ahead_behind`, `git_status`) -- the UI now shows how many commits HEAD is ahead of or behind the upstream tracking branch.
2. **`RepoGitStatus` struct** -- a single snapshot combining branch name, dirty flag, ahead/behind counts, and a human-readable one-liner; returned by the new `/api/projects/:id/git/status` endpoint.
3. **`build_status_detail` helper** -- pure function that formats the detail string from components; tested independently of git.
4. **Status bar in the GitPanel UI** -- a status-bar row at the top of each repo's git panel showing branch, dirty badge, and sync badges (in sync / N ahead / N behind).
5. **CSS for the status bar** -- `git-status-bar`, `git-status-detail`, `git-status-badge`, and four state variants (`dirty`, `sync`, `ahead`, `behind`) added to `GLOBAL_CSS` in `style.rs`.

---

## Why

The existing `checkout_status` exposed `branch` and `dirty` but nothing about the repo's sync state relative to origin. An architect working through the "run it locally before you push" loop needs to know at a glance:

- Which branch they are on.
- Whether there are local edits not yet committed.
- How many commits are waiting to push (ahead) or pull (behind).

Without this, the cockpit forced the architect to drop to a terminal to answer those questions.

---

## How

### Server (`crates/server/src/workspace.rs`)

All new code is in the `// Local git controls (issue #37)` section, additive only:

- **`AheadBehind`** -- serde-round-trippable struct with `Option<u32>` fields. `None` means no upstream tracking ref (freshly created local branch, detached HEAD, etc.); `Some(0)` means in sync.
- **`parse_ahead_behind(raw: &str) -> AheadBehind`** -- pure parser for the tab-separated output of `git rev-list --left-right --count HEAD...@{u}`. Exported for unit tests; gracefully returns `default()` for any non-conforming input.
- **`RepoGitStatus`** -- bundles `branch: String`, `dirty: bool`, `sync: AheadBehind`, `detail: String`.
- **`git_status(dir: &Path) -> anyhow::Result<RepoGitStatus>`** -- runs three cheap local git commands (rev-parse, status --porcelain, rev-list --left-right) in sequence. The ahead/behind query is best-effort: a non-zero exit code (no upstream, detached HEAD) is silently mapped to `None` counts rather than an error.
- **`build_status_detail(branch, dirty, sync) -> String`** -- pure formatter producing strings like `"on main · 2 ahead · uncommitted changes"`. Exported for unit tests.

### Server (`crates/server/src/lib.rs`)

One new route, appended at the end of the `// Local git controls` block:

```
GET /api/projects/:id/git/status?repo=<owner%2Frepo>
```

Response shape:
```json
{ "ok": true, "branch": "feature/x", "dirty": false, "ahead": 2, "behind": 0, "detail": "on feature/x · 2 ahead" }
```

The handler re-uses `resolve_git_dir` (the shared path-resolution helper) and delegates to `crate::workspace::git_status`. No `AppState` additions.

### UI (`crates/ui/src/workspace.rs`)

- **`GitStatusView`** -- local deserialization struct matching the endpoint's response.
- **`api_git_status(project_id, repo) -> Option<GitStatusView>`** -- fetch helper, same pattern as `api_git_branches`.
- **`GitPanel`** -- added a third `use_resource` (`status_res`) alongside the existing `branches_res` and `log_res`. All three subscribe to `git_refresh` so they refresh together after any mutating git op.
- **Status bar** -- rendered above the "Branches" section. Hidden when the status fetch returns nothing (repo not resolved locally). Shows the detail string plus coloured pill badges for the four states (dirty, in-sync, ahead, behind).

### CSS (`crates/ui/src/style.rs`)

New rules at the end of the `// Git panel (issue #37)` block inside `GLOBAL_CSS`:

| Class | Purpose |
|---|---|
| `.git-status-bar` | Flex container for the one-line status row |
| `.git-status-detail` | Monospace detail string, grows to fill remaining width |
| `.git-status-badge` | Shared pill base (font, padding, border-radius) |
| `.git-status-dirty` | Amber pill for uncommitted changes |
| `.git-status-sync` | Green pill when exactly in sync with upstream |
| `.git-status-ahead` | Terracotta pill (matches app accent) when commits to push |
| `.git-status-behind` | Purple pill when commits to pull |

---

## Usage

The status bar is passive -- it updates when any git operation refreshes the panel. No user action is needed to see it. The ahead/behind counts reflect locally-fetched state; an explicit "Pull" button triggers a fetch that updates the counts.

---

## Guardrails upheld

- No auto-commit or auto-push. Every mutating operation requires an explicit button press.
- Destructive ops (reset, force-push except on Camerata's own governance branch) are not exposed anywhere in the workspace surface.
- Token is passed only to transient network commands (`push_branch`, `pull_branch`) and is never written to `.git/config`.

---

## ROUTE-1 routing decisions

All code is additive within `crates/server/src/workspace.rs`, `crates/server/src/lib.rs`, `crates/ui/src/workspace.rs`, and `crates/ui/src/style.rs`. No new crates, no module boundary moves, no cross-crate public API changes.

---

## Test coverage

All new pure functions have synchronous unit tests that run without a real git process:

- `parse_ahead_behind`: 8 cases (clean sync, ahead-only, behind-only, diverged, empty, malformed, non-numeric, trailing newline).
- `build_status_detail`: 6 cases (in sync clean, dirty+ahead, behind-only, diverged, no-upstream dirty, no-upstream clean).

The existing `clone_branch_and_status_round_trip` async test continues to cover the end-to-end `checkout_status` + `create_branch` + dirty-detection path.
