# Project settings moved to a gear-icon popup in Governed Development

**Date:** 2026-06-22
**Implemented by:** feat/project-settings-gear

## What moved

| Setting | Before | After |
|---------|--------|-------|
| Loop guard (max revise iterations) | Inline in `UowDevControls` | Gear popup only |
| Default tier-map (fast / balanced / strongest model ids) | Rules view (`RulesView`) | Gear popup + Rules view (see below) |

## Where the gear button lives

A `ProjectSettingsGear` component renders a small "Settings" button (with a gear glyph) in a
`govdev-gear-row` div at the top of the Governed Development left nav (`GovernedDevPage`). It is
always visible regardless of which UoW is selected. Clicking it opens a `rule-modal-overlay` /
`rule-modal proj-settings-modal` popup (the same modal pattern used by rule-detail and import-overwrite
modals throughout the app). The popup closes on backdrop click or the X button.

## What the popup contains

1. **Loop guard** — the existing `LoopGuardControl` component, now rendered only inside the popup.
   Labeled as a project-wide setting. Reads/writes `project.max_iterations` via `set_max_iterations`.

2. **Default tier-map** — the existing `TierMapEditor` component, surfaced in the popup beneath
   the loop guard with a section divider. Reads/writes `project.tier_map` via `set_project_tier_map`.

## What stays per-UoW (unchanged)

- Pull latest work item button
- Gate self-check (`GateSelfCheck`)
- Lifecycle steps (Intake → Investigating → Developing → Done)
- Per-run model overrides: `invest_model`, `dev_strongest`, `dev_balanced`, `dev_fast`
  (these default FROM the project tier-map but are per-run overrides — they remain in `UowDevControls`)
- Agent activity, UoW panel, live run panel, comment-to-issue box

## TierMapEditor in the Rules view

`TierMapEditor` remains in `RulesView` (line ~2051) as a second surface for discoverability.
Both instances talk to the same backend endpoint and save to the same project row, so they stay
in sync via the toast + the user re-fetching. Removing it from the Rules view was considered
risky (it sits inside a large `match` arm); leaving it is the conservative choice per the task
instructions ("don't break its existing location if removing it is risky, note your choice").

## CSS additions (style.rs)

- `.govdev-gear-row` — flex row pinned to the right of the nav to host the gear button.
- `.govdev-gear-btn` — minor sizing override for the button (inherits `btn-edit-sm`).
- `.proj-settings-modal` — 560px-max popup, wider than the rule-detail modal to accommodate
  the tier-map input rows.
- `.proj-settings-scope-note` — italic hint text below the modal title.
- `.proj-settings-section` — top-border divider between loop-guard and tier-map sections.

## No new warnings

`cargo check -p camerata-ui` passes clean.
