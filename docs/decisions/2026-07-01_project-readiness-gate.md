# ADR: Project readiness gate — portable identity, machine-local path, derived readiness

**Date:** 2026-07-01
**Status:** Accepted (target); implementation to follow
**Refines:** `2026-06-18` project export/import & portability decision

## Context

A project today records its repo **identity** (`owner/repo` + origin URL) in `projects.json`, but
the local **checkout** into the workspace folder is a separate, optional step (a manual "clone/update"
on the Workspace surface, or a lazy clone during `apply_local`). Neither project creation nor
onboarding materializes the repo. The result is a confusing, disconnected state: the project "knows"
its repo, while the Workspace shows nothing, and repo-dependent actions individually fail or no-op.

This is fine for portability (paths are machine-local and must never be exported), but the two facts
are not coupled, so a project can silently claim a repo it cannot act on. On transfer to another
machine, the same happens by construction: identity arrives, the local clone does not.

## Decision

A project is modeled as **three facts**, and the coupling lives in the third:

1. **Identity** — `owner/repo` + origin URL. Portable; exported on transfer; always known.
2. **Local materialization** — a resolvable local path / git clone. **Machine-local; never exported.**
3. **Readiness** — **derived** from (2): `Ready` | `Unlinked` | `Partial` (some repos resolve, others
   don't). Both the Workspace view and the project's operational state read this single derived value,
   so they can never disagree.

**Do not couple identity to local presence** (that breaks portability). **Couple operations to derived
readiness.** An `Unlinked` (or `Partial`) project loads **paused**: repo-dependent actions (scan, apply,
run, design-publish) are disabled behind a single "Link repo" affordance rather than each failing
independently — which is the same family as the dead-button problem (an action that can't succeed must
not look live).

### Resolve UX

Clicking into an `Unlinked` project shows a modal:

> *"This project is coupled with `owner/repo`, but no local match exists. Clone it now, or select a
> local clone you already have?"*

- **Clone** → native folder picker (`rfd`) for the destination → clone from the known origin into
  `<chosen>/<owner>/<repo>`.
- **Select existing** → native folder picker → **validate** `git remote get-url origin` matches the
  project's origin before accepting (records a per-repo path override); warn on mismatch instead of
  linking the wrong folder.
- **Neither** → the project stays **paused**.

On transfer: identity travels, path is absent → readiness = `Unlinked` → recipient sees the modal →
resolves → project activates. This is "the project is on pause until a repo is linked."

**Auto-resolution** (clone-on-select without the modal) is a convenience layered on top, taken only
when a workspace root + GitHub token are already present. It does not replace the gate; it is one
automatic path through it. (This subsumes the earlier standalone "auto-clone on select" idea.)

## Consequences

- The per-repo broken-path **health check** is promoted from a cosmetic banner to a **first-class gate**
  that the whole project keys off.
- Portability is preserved: export still carries only identity (path-free); readiness is recomputed on
  the receiving machine.
- Dead-end affordances shrink: repo-dependent actions gate on one readiness signal instead of each
  discovering failure at call time.

### Caveat

`rfd` native folder pickers are **desktop-only**. On a future **web** build of the cockpit, browsers
cannot pick arbitrary filesystem folders, so the "select existing local clone" path needs a different
resolution UX (e.g. clone-only, or a server-side path entry). Revisit when the web target ships.

## Implementation notes

- Add `ProjectReadiness { Ready | Unlinked | Partial }`, derived from the existing per-repo local-path
  resolution (`workspace::checkout_status`).
- Reuse `workspace::clone_or_pull` (Clone path) and a validated per-repo path override (Select path).
- Gate repo-dependent UI actions on readiness; render the resolve modal at project-entry when not `Ready`.
- Sequenced **after** the UI contract-mismatch fixes (same "make actions honest" theme).
