# UoW: project settings move to a gear popup + AI-assisted "Update branch"

**Date:** 2026-06-22 · **Decided by:** Zach. Build queued after the in-flight styling/bypass
edit to `cockpit.rs` integrates (avoid a same-file conflict).

## 1. Project-level settings live in a gear popup, not inside the UoW

Project-scoped settings must NOT be rendered inline in a UoW's dev controls (it made the
project-level loop-guard read like a per-UoW field). Add a **gear icon → project-settings popup**
at the project/cockpit level holding the project-scoped settings:
- **Loop-guard** (max revise iterations — `project.max_iterations`).
- **Default tier-map** (the project's fast/balanced/strongest defaults).
- **Feature flags** (e.g. SOC-2) where surfaced.

The UoW dev controls show ONLY per-UoW state. The per-UoW dev-run tier selects STAY in the UoW as
a **run override** that defaults from the project tier-map (the *default* is edited in the gear; the
*override* is per run). `GateSelfCheck` is a diagnostic, not a setting — it can stay in the UoW (or
move to the gear); decide during build, default to leaving it.

## 2. AI-assisted "Update branch" within the UoW

A UoW control to merge a chosen branch INTO the UoW's working branch, AI-assisted for conflicts —
the GitHub PR "Update Branch" pattern (merge base → head), but with an agent resolving conflicts.

- **Source branch is user-selectable, from LOCAL or ORIGIN.** Backend lists the repo's branches
  (local refs + `origin/*`); the UI offers a picker.
- **Mechanism:** in the UoW's local worktree/branch, `git fetch` (for an origin source) then
  `git merge <source>`. On a clean merge, commit. On conflicts, spawn a **gated** agent that
  resolves the conflict markers (edits via `gated_write`, makes it build), then commits the merge.
- **Gate preserved:** conflict-resolution writes go through `gated_write` (layer-1); layer-2 can run
  on the merged result. `Task` stays disallowed; the agent is spawned by the server/fleet, gated.
- **UI:** a branch picker (local / origin) + "Update branch (AI-assisted)" button + AgentActivity
  progress + result. Lives in the UoW dev controls (it IS per-UoW — it targets this UoW's branch).

Relates to [[workitem_uow_governed_dev_architecture]]; gate stays universal
([[camerata_gate_universal_enforcement]]).
