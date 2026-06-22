# Project CONFIG (transferable) vs project DATA (local) — keep them separate

**Date:** 2026-06-21 · **Decided by:** Zach

## The principle

Camerata storage splits into two categories, and they must stay separate:

- **Project CONFIG — transferable.** The portable definition of a project: its repos
  (`owner/repo`), ruleset, onboarded-state, and tier_map. Lives in `projects.json` (the
  `ProjectStore`) and is what project **export/import** carries (a single, path-free
  `camerata-project-<name>.json`). This is the only thing meant to move between machines/people.

- **Project DATA — local to the developer, never transferred.** Everything that is the
  state of *this* developer's local work: Units of Work (`uow.json`), the story spine
  (`stories.json`), the onboarding draft (`onboarding-draft.json`), scan caches, escalations,
  routines, and the local repo checkout paths. Lives in separate files under the data dir.

## Why UoWs must NOT be transferable

A UoW is **in-progress dev-lifecycle state** — its stage, branch, gate provenance, decision
records, run history, and sign-off. If it traveled with the project config, two developers who
import the same project would inherit each other's half-finished work and sign-offs, producing
**overlapping, inconsistent work**. UoWs are local the way a git working tree is local while the
repo config is shared. Same for the story spine (the locally-pulled snapshot of tracker issues).

## Consequence (no change needed)

The current build is correct: `projects.json` = transferable config; `uow.json` / `stories.json`
/ drafts / caches = local data. A proposed refactor to fold UoWs into the project store was
**rejected** for the reason above. Export stays config-only.

Relates to [[workitem_uow_governed_dev_architecture]] and the project-portability decision
(export = path-free, config-only).
