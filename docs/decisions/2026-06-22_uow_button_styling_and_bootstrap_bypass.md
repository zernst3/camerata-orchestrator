# UoW dev-control button styling reconciliation + layer-2 bootstrap bypass wiring

**Date:** 2026-06-22 · **Decided by:** Zach

Two changes to Camerata's UoW governed-development surface, implemented together because
they touch the same control (the `DecisionsApproved` development-run control).

Companion to
[[2026-06-22_ci_wiring_both_layers_and_layer2_bootstrap_bypass]], which decided the
behaviour; this doc records the wiring.

## A. Button styling reconciliation (one design language)

The UoW dev-control buttons had drifted into an ad-hoc mix of bespoke classes
(`uow-stage-btn`, `uow-seg-btn`, plus the already-shared `btn-run` / `btn-edit-sm`) that
didn't read as one system with the onboarding page. They are now reconciled onto the
onboarding page's established button variant system (no change to the onboarding page; the
UoW buttons were brought into line with it).

The canonical variant system (learned from the onboarding flow):

| Variant        | Class(es)                       | Geometry                                  |
|----------------|---------------------------------|-------------------------------------------|
| Primary action | `.btn-run` (`.onboard-cta`)     | accent bg, white, 13px/700, 9px 16px, r8  |
| Secondary      | `.btn-secondary`                | bordered surface, 13px/700, 9px 16px, r8  |
| Small inline   | `.btn-edit-sm`                  | bordered surface, 12px/600, 5px 11px, r8  |

How the UoW buttons now map:

- **Run / Begin** (Begin investigation, Run development, Create CI story, Comment submit):
  already `.btn-run` — the onboarding PRIMARY. Kept; semantically these are the primary
  action, so the accent style is correct.
- **Approve decisions** (a stage TRANSITION): was the bespoke `.uow-stage-btn`; now
  `.btn-secondary` — the onboarding SECONDARY variant (bordered, same geometry as the
  accent primary). The orphaned `.uow-stage-btn` rules were removed. Added a
  `.btn-secondary:disabled` rule (opacity .45 / not-allowed), matching the primary's
  disabled treatment, since this button disables outside the Investigating stage.
- **Dev-status segmented control** (`.uow-seg` / `.uow-seg-btn`): a genuinely distinct
  pattern (one control, mutually-exclusive options), kept as a segmented control but
  aligned to the shared system — radius bumped to 8px (the system radius), font/weight to
  the secondary's 12px/600, and the active segment already uses the primary accent. It now
  reads as the same family, not a one-off.
- Small inline buttons (open modal, browse, edit) stay `.btn-edit-sm` — the small variant.

The recently-added comfortable row spacing is preserved: `.run-control-row` keeps its
`gap: 12px`; we only dropped `btn-run`'s standalone 14px bottom margin INSIDE that row
(`.run-control-row .btn-run { margin-bottom: 0; }`) so the primary's baseline aligns with
the model select beside it — the exact pattern the onboarding action rows already use.

Files: `crates/ui/src/cockpit.rs` (the `class:` on the Approve button), `crates/ui/src/style.rs`.

## B. Layer-2 bootstrap bypass wiring (one-time, explicit, layer-1 still applies)

Layer 2 is fail-closed: a repo with a manifest but no lint/test wired returns
"could-not-run" (a hard failure), which deadlocks the very run that would INSTALL the
tooling layer-2 needs. We add an explicit, default-OFF, per-run skip of ONLY layer-2 for
that bootstrap run.

**THE GATE IS NEVER BYPASSED.** The bootstrap option skips only the post-task layer-2
lint/test bounce. Layer 1 (the MCP deny-before-write gate — agents are still spawned with
`gated_write` only, `Task` disallowed) and the no-code-first decisions gate
(`ensure_development_gate`) are UNCHANGED in both the on and off cases.

Wiring (server → fleet):

- `crates/fleet/src/lib.rs`: new private `layer2_runner(worktree, skip_layer2)` selects the
  existing `NoopChecks` when `skip_layer2`, else the real language-matched
  `runner_for_worktree`. Two new additive public entry points,
  `build_from_plan_with_model_iterations_and_layer2` and
  `build_from_plan_with_tier_map_and_layer2`, take the flag; the existing
  `build_from_plan_with_model_and_iterations` / `build_from_plan_with_tier_map` now delegate
  with `skip_layer2 = false`, so every existing caller and the public API are unchanged.
- `crates/server/src/lib.rs`: `StartRunReq` gains `skip_layer2: Option<bool>` (`#[serde(default)]`
  → absent = off). `start_run` reads it (`unwrap_or(false)`) and threads it through
  `start_governed_run` into both live executors. The response shape stays
  `{ run_id, story_id, mode }`. The scripted path has no layer-2 bounce, so the flag is a
  no-op there.
- `crates/server/src/live_fleet.rs`: both `execute_live_run` and `execute_live_run_tiered`
  take `skip_layer2`, call the `_and_layer2` fleet entry points, and emit a visible cockpit
  info event when the bypass is active ("Bootstrap run: layer-2 checks SKIPPED … the
  security gate (layer 1) still applies …").

UI (`crates/ui/src/cockpit.rs`):

- The `DecisionsApproved` development-run control gains a clearly-labeled, default-OFF
  checkbox: **"Bootstrap run — skip layer-2 checks"** with the hint "For the run that
  installs the linters/checkers layer-2 needs. The security gate (layer 1) still applies.
  Turn off afterward." The toggle is a per-run `use_signal(|| false)` — not persisted, not
  sticky.
- `dev_run_body(tier_map, skip_layer2)` includes `"skip_layer2": true` in the POST body
  ONLY when the toggle is on; when off, the body is byte-for-byte today's contract.
  `start_dev_run` threads the flag through.

## Tests (token-free)

- Server: `start_run_req_parses_skip_layer2_and_defaults_off` (parse + absent = None).
- Fleet: `layer2_runner_skips_when_bootstrap_and_runs_real_otherwise` — on a JS worktree
  with no lint/test, the real runner fails closed (the deadlock) while the no-op runner
  returns `Ok(empty)`; network-/token-free (bails before install).
- UI: `dev_run_body_includes_skip_layer2_only_when_on` + the existing frozen-contract test
  updated to assert the flag is absent by default.
