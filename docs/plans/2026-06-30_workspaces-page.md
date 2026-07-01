# Workspaces Page — a consolidated repo / branch / commit surface

Status: DRAFT (consolidation + gaps; ~80% already built)
Date: 2026-06-30
Owner: Zach
Related: `2026-06-20_repository_workspace_git_controls.md`, `2026-06-21_project_config_vs_data_separation.md`, `2026-06-23_split_cockpit_modules.md`, `docs/TECHNICAL.md`, companion `2026-06-30_epic-design-page.md`

## 1. The idea

A single cockpit page that is, in Zach's words, "glorified repo/branch/commit management." Unlike the epic page, almost every primitive here is **already built**; this doc mostly **consolidates** the pieces into one coherent page and calls out the few real gaps. The one genuinely interesting interaction, drag-and-drop cherry-pick, is also already shipped.

## 2. What already exists (inventory)

### 2.1 Workspace model
- `settings.json` (`SettingsStore`, `crates/server/src/settings.rs`): `workspace_root: Option<String>`, `repo_paths: HashMap<repo, String>` (machine-local per-repo overrides), `bombe_enabled`. Credentials live in the system keychain, not here.
- Clone convention: `<workspace_root>/<owner>/<repo>`; per-repo override in `repo_paths` wins. Resolved by `workspace::resolve_repo_dir`.

### 2.2 Git controls (all shipped)
Endpoints (`crates/server/src/lib.rs:751`), UI in `GitPanel` (`crates/ui/src/workspace.rs:630`):

| Endpoint | Method | Purpose |
|---|---|---|
| `/api/projects/:id/git/status?repo=<owner%2Frepo>` | GET | `{ ok, branch, dirty, ahead, behind, detail }` |
| `/api/projects/:id/git/branches` | GET | branch list |
| `/api/projects/:id/git/log` | GET | commit log |
| `/api/projects/:id/git/checkout` | POST | switch branch |
| `/api/projects/:id/git/commit` | POST | commit staged changes |
| `/api/projects/:id/git/push` | POST | push branch |
| `/api/projects/:id/git/pull` | POST | pull/fetch |
| `/api/projects/:id/git/cherry-pick` | POST | cherry-pick a SHA |
| `/api/projects/:id/repo-health` | GET | broken-path health |
| `POST /api/settings/repo-path` | POST | set/clear a per-repo local path override |

`RepoGitStatus`: `branch`, `dirty`, `sync: AheadBehind { ahead: Option<u32>, behind: Option<u32> }` (None = no upstream), `detail`. Parsed from `git rev-list --left-right --count HEAD...@{u}`.

Guardrails already enforced: no auto-commit/auto-push; token passed only to transient network commands, never written to `.git/config`; destructive ops (reset, force-push except on the governance branch) are not exposed.

### 2.3 Drag-and-drop cherry-pick (shipped — this is the "move a commit between branches" feature)
- Commit rows in the log are draggable (`ondragstart` sets `dragged_sha`); branch rows are drop targets (`ondragover`/`ondrop`) — `crates/ui/src/workspace.rs:728`.
- On drop: `api_git_cherry_pick(project_id, repo, sha)` → `POST /api/projects/:id/git/cherry-pick` → `workspace::cherry_pick(dir, sha)` runs `git cherry-pick <sha>` (`crates/server/src/workspace.rs:1264`).
- A per-commit button is the non-drag fallback; hint text: "drag a row onto a branch to cherry-pick it, or use the button."

Note the semantics: this is **cherry-pick (copy)**, not a move. The commit is copied onto the target branch; the source branch is untouched.

### 2.4 Repo health / broken-path (shipped)
- `GET /api/projects/:id/repo-health` → `{ ok, repos: [<resolution>] }` via `workspace::repo_resolution(override, workspace_root, repo)`. Called on load, after import, after a resolve.
- `POST /api/settings/repo-path` sets/clears an override and returns the post-set resolution (no second round-trip).

### 2.5 Project export / import (shipped, thin)
- `GET /api/projects/:id/export` → path-free JSON `camerata-project-<name>.json` (repos, `ProjectRuleset`, `onboarded` set, `TierMap`, `DesignerBand`, `StepModels`, `ModelProfile`, `L3ReviewConfig`, stall thresholds, loop guard). `settings.json` and run data do NOT travel.
- `POST /api/projects/import` → upsert, with a same-name overwrite warning. After import the architect sets `workspace_root` and any per-repo overrides via repo-health.

## 3. The page (consolidation)

The work here is mostly **assembling** the above into one page rather than having git controls, health, and export/import scattered. Proposed layout:

```
  ┌── WORKSPACES ─────────────────────────────────────────────┐
  │  workspace_root: /Users/…/repos   [change]                 │
  │                                                            │
  │  ┌─ repo: owner/repo ──────────────────────────────────┐   │
  │  │  ● healthy   on feature/x · 2 ahead   [dirty]        │   │
  │  │  ┌ branches ┐   ┌ log (drag a commit → a branch) ┐   │   │
  │  │  │ main     │   │ a1b2  fix: …           ⋮ pick   │   │  │
  │  │  │ feature/x│   │ c3d4  feat: …          ⋮ pick   │   │  │
  │  │  └──────────┘   └─────────────────────────────────┘   │  │
  │  │  [new branch]  [commit all]  [push]  [pull]          │   │
  │  └──────────────────────────────────────────────────────┘  │
  │                                                            │
  │  ┌─ repo: owner/other ─ ✗ path missing  [locate…] ─────┐   │
  │                                                            │
  │  [ import project… ]   [ export this project ]             │
  └────────────────────────────────────────────────────────────┘
```

- **Per-repo card** = the existing `GitPanel` (status bar, branch list, log, new-branch, commit/push/pull) plus the health badge.
- **Broken-path recovery**: a repo whose path does not resolve shows a "locate" affordance wired to `POST /api/settings/repo-path`, then re-runs repo-health (the endpoints already return the post-set resolution).
- **Import/export** live at the page footer.

Most of this is re-parenting components that already exist (`GitPanel`, `RepoHealthPanel`) under one page shell.

## 4. Gaps (what is NOT built)

1. **Interactive rebase / drag-to-reorder / move (NON-GOAL).** Reordering commits within a branch, or a true move (cherry-pick onto target + drop from source), is not built and is an explicit non-goal (see section 5). `git rebase` is already blocked by the gateway guard `is_git_state_mutation` (`crates/gateway/src/lib.rs:170`), which aligns with this. Listed here only so the boundary is clear, not as backlog.
2. **Multi-commit drag.** Dragging a range to cherry-pick as a set. Minor; deferrable.
3. **Import overwrite-warning UX.** The server upserts with a same-name warning, but the exact UI confirmation flow is not specified in a decision doc. Small spec gap.
4. **Consolidated page shell itself.** The components exist but are not yet assembled into one "Workspaces" page (they currently live within project/settings surfaces per `2026-06-23_split_cockpit_modules.md`).

## 5. Decisions and open questions

**DECIDED (Zach, 2026-06-30): cherry-pick (copy) only.** The shipped drag-and-drop cherry-pick is exactly the intended contract. No true "move" (source-branch drop), no reorder, no interactive rebase. Rationale: cherry-pick is the most complex git operation most developers ever perform in practice, and history-rewrite operations carry risk (and an undo story) that this page does not want to own. The gateway already blocks `rebase` (`is_git_state_mutation`), which aligns with this. Sections 4.1 and 6/Increment 3 are therefore **non-goals**, not backlog.

Open:

1. **Page home**: is Workspaces a top-level nav item, or a tab within the project surface? (Given it is per-project repo management, a tab per project or a top-level page filtered by active project both work; recommend top-level, active-project-scoped, to match the mental model of "manage my checked-out repos.")

## 6. Phasing

- **Increment 1 — assemble the page.** Put the existing `GitPanel` + `RepoHealthPanel` + import/export under one Workspaces shell, active-project-scoped. Almost no new backend.
- **Increment 2 — broken-path recovery polish.** First-class "locate / resolve" flow on unhealthy repos, health badges, onboarded status.
- **~~Increment 3 — move/reorder~~ (NON-GOAL).** Cut per section 5. Cherry-pick (copy) is the intended ceiling for this page.

## 7. Relationship to the epic page

These two ship in tandem but are near-opposites in effort: the epic page is mostly **new** design over built primitives; the workspaces page is mostly **assembly** of already-built components with one small guarded enhancement. Doing them together gives the cockpit a coherent "design work (epic page) → manage the repos that work lands in (workspaces page)" pairing.
