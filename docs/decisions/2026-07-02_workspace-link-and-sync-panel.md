# ADR: Workspace page — link an existing clone, override-aware resolution, and a per-repo sync panel

**Date:** 2026-07-02
**Status:** Accepted (implemented)
**Refines:** `2026-07-01` project readiness gate (`2026-07-01_project-readiness-gate.md`); consolidates the `2026-06-30_workspaces-page.md` plan

## Context

The readiness gate (2026-07-01) modeled a project as three facts: portable identity
(`owner/repo` + origin), machine-local materialization (a resolvable clone), and a
derived readiness value both the Workspace and the operational state key off. That ADR
specified the resolve UX (clone, or select an existing clone) at project entry, but the
Workspace surface itself had not caught up to those primitives, and two real bugs showed
through.

1. **Status was override-blind and layout-rigid.** The Workspace status endpoint only
   inspected the DERIVED path `<workspace_root>/<owner>/<repo>`. A clone living anywhere
   else (a flat `.../repo`, or the workspace folder itself) was reported "not cloned"
   forever, and the Workspace offered no way to point a repo at a clone the user already
   had. Status resolution disagreed with the readiness gate, which already resolved through
   the per-repo override.

2. **Resolution was case-sensitive on one side only.** `repo_resolution` compared the
   folder's parsed origin `owner/repo` case-sensitively, while `validate_link_target`
   compared case-insensitively. A repo stored as `Owner/Repo` with an origin of
   `owner/repo` passed the link validation but then failed the resolution check, leaving
   the project stuck `Unlinked`/`Partial` forever. A companion UI bug left the resolve
   modal rendering with an empty repo list after the last repo resolved.

3. **The page stacked one card per repo.** For a multi-repo project the checkout,
   branches, commits, and sync controls stacked vertically, and there was no way to search
   a long branch or commit list. The Pull/Push/Ship button row was also geometrically
   ragged.

## Decision

### Link an existing clone (the materialization half of the gate)

The Workspace can point a project's repo at a folder the user already has. On a
not-cloned row, a **"Link to this folder…"** affordance opens a native `rfd` folder
picker; the chosen folder is sent to `POST /api/projects/:id/repos/:repo/link`
(`:repo` percent-encoded). The server **validates before recording anything**: the folder
must be a git clone whose `origin` matches the project's identity for that repo (both
`https://github.com/owner/repo(.git)` and `git@github.com:owner/repo(.git)` forms,
normalized case-insensitively). On a match it records the folder as this repo's
machine-local per-repo path **override** (via `settings.set_repo_path`, never on
`Project`, so it is never exported) and returns the freshly derived readiness so the UI
activates without a second round trip. On any failure (bad path, not a git folder, origin
mismatch) NOTHING is recorded and a 400 surfaces the specific reason inline. This endpoint
never clones: Clone stays a separate action (`POST .../checkout`). This is the "select
existing local clone" resolve path from the readiness gate, now reachable directly from the
Workspace.

### Override-aware, case-insensitive resolution

Status and readiness now resolve through the same primitives:

- `checkout_status_resolved(override_path, workspace_root, repo)` resolves via the per-repo
  override first (`resolve_repo_dir`), else the derived `<workspace_root>/<owner>/<repo>`
  layout. A folder counts as cloned only when it is a git checkout whose `origin` matches
  `owner/repo`; a wrong-origin folder reports not-cloned with a clear "different repo"
  reason. Branch/dirty enrichment runs on the RESOLVED dir. When neither an override nor a
  workspace root is present, that repo reports not-cloned with a helpful reason rather than
  hard-erroring the whole endpoint.
- The status handler calls this per repo, so repo rows render even with no workspace root
  (an override can still resolve a repo).
- The Clone action skips any repo that ALREADY resolves (override-aware git checkout with a
  matching origin), so it never clones a second copy into the derived path.
- `repo_resolution` now compares the parsed origin `owner/repo` with `eq_ignore_ascii_case`,
  matching the invariant `validate_link_target` already enforced. A clone whose origin
  casing differs from the stored identity resolves correctly instead of stranding the
  project as `Unlinked`/`Partial`. The resolve modal early-returns when there are no
  unresolved repos and closes on the final successful resolve, so an empty contradictory
  modal is impossible.

### A per-repo sync panel

A `<select>` at the top of the Workspace lists every repo in the active project and holds
the choice in a signal (defaults to the first repo). Only the selected repo's card renders,
so the whole page (checkout, branches, commits, sync) is one repo's slice rather than a
vertical stack; the selector renders even for a single-repo project so the page is
consistent. Branch and commit lists each get a case-insensitive substring filter: an empty
query shows all. The matching decisions are pure predicates (`branch_matches`,
`commit_matches` over short-sha, subject, author) in `camerata-ui-core::git`
(RUST-HEADLESS-CORE-1), unit-tested with no VirtualDom; the adapter calls them in its
render-time `.filter(...)`. The Pull/Push/Ship button row geometry was corrected so Pull
and Push are a pixel-matched segmented pair and Ship aligns on the same baseline.

## Consequences

- The Workspace status path and the readiness gate now share one resolution primitive, so a
  real clone is never mislabeled "not cloned", and clones outside the strict
  `<owner>/<repo>` layout are first-class.
- Linking is reachable from the Workspace, not only from the entry modal, and it is the
  machine-local materialization half of the gate: identity was always known, the clone is
  now bindable in place. Portability is preserved: the bound path is an override in
  `settings.json`, never in the exported project.
- A single case-insensitivity invariant (`eq_ignore_ascii_case` on the parsed `owner/repo`)
  now governs both linking and resolution, closing the stuck-paused class of bug.
- The page scales to multi-repo projects by scoping to one repo at a time with searchable
  branch and commit lists, instead of an unbounded vertical stack.

### Caveat

The `rfd` native folder picker inherited from the readiness gate is desktop-only. A future
web build of the cockpit needs a different "select existing clone" UX (clone-only, or a
server-side path entry). Revisit when the web target ships.
