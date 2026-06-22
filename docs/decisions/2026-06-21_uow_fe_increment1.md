# 2026-06-21 — UoW governed-dev UI, Increment 1: step-bound run controls + per-phase model selection

## Context

The governed-development UI ran AI work from a single standalone "▶ Run this work
(governed)" button at the top of `UowDevControls`, with one model `<select>`. The
lifecycle steps (Intake → Investigating → DecisionsApproved → Development → Awaiting
QA → Signed off) were a passive read-out below it. That split the "what phase am I in"
signal from the "run this phase" action, and offered no way to choose models per
phase or per tier.

A dead "clarify" subsystem (ask-the-team questions + AI-suggested questions + answer
thread) was still compiled in but no longer rendered anywhere; it emitted warnings and
carried four unused client fns, a view struct, two components, and a context provider.

## Decision

### Runs live ON the steps, with per-phase model selection

The control for the **active** phase renders inline with the lifecycle strip and
**replaces** the prior phase's control (it does not stack). Implemented in a new
`UowStepRunControls` component (the old `UowLifecycleStrip` was folded into it):

- **Intake → Investigating.** A single model `<select>` (options from
  `fetch_audit_models`, default = the active project's `tier_map.strongest`,
  user-customizable for this run) beside a **▶ Begin investigation** button. The
  button calls `begin_investigation_run(story_id, model)` and drives the live
  `AgentActivity` on the returned run id. The server performs the stage transition.
- **Investigating → DecisionsApproved.** The architect's **Approve decisions**
  transition stays where it was (enabled only at the Investigating stage; the server
  gates and 409s with a reason if decisions aren't all approved).
- **DecisionsApproved (ready to run dev).** Three per-tier model `<select>`s —
  **Strongest** (orchestrator / complex work), **Balanced** (mid), **Fast** (simple)
  — defaulted from the project `tier_map` and editable per-UoW for this run, plus a
  **▶ Run development (governed)** button calling `start_dev_run(story_id, tier_map)`.
  A one-line hint states the strongest tier leads and delegates simpler work to the
  others. `AgentActivity` + `LiveRunPanel` follow as before.

Per-UoW model edits do **not** mutate the saved project tier map; they apply only to
the run.

`UowDevControls` now fetches the UoW itself (keyed on the shared `uow_refresh` tick)
to learn the current `stage`; `UowPanel` re-fetches the same UoW for its post-run
read-out (dev status, branch, gate provenance, sign-off, history). The two stay in
sync via the shared tick without sharing a fetch. The lifecycle strip moved out of
`UowPanel` into `UowStepRunControls`.

Kept unchanged: Pull latest, `GateSelfCheck`, `LoopGuardControl`, the UoW panel
read-out, sign-off, and the comment + @mention box.

### Frozen client/backend contract

- **Development run:** `POST /api/stories/:id/run` with body
  `{ "tier_map": { "strongest": "<id>", "balanced": "<id>", "fast": "<id>" } }`.
  Response `{ "run_id", "story_id", "mode" }`; a 409 carries a `reason`
  (Blocked/Failed handling preserved from the old `start_run`).
- **Investigation run:** `POST /api/uow/:story_id/begin-investigation` with body
  `{ "model": "<id>" }`. Response `{ "run_id", "story_id" }`.

New client fns: `start_dev_run`, `begin_investigation_run`, plus the pure
`dev_run_body` helper (unit-tested against the frozen shape) and a shared
`poll_run_to_done` run-polling helper. The old `start_run(model)` fn was removed
(it had no other callers).

### Removed: the dead clarify subsystem

Deleted with zero remaining references (no warnings):

- `ClarificationView` struct
- client fns `fetch_clarifications`, `post_clarification`, `suggest_clarifications`,
  `answer_clarification`
- components `ClarifySection`, `ClarificationCard`
- the `clarify_refresh` `use_context_provider` in `CockpitApp`

The comment + @mention box (which reuses some `clarify-*` CSS class names) stays; it
is the live replacement for ask-the-team and is unrelated to the deleted code.

## Status

`cargo check -p camerata-ui` is green with no warnings. New unit tests:
`dev_run_body_matches_frozen_contract`, `default_tier_map_seeds_all_three_tiers`.

Scope: UI only (`crates/ui/cockpit.rs` + `style.rs`). The backend builds the matching
endpoints separately.
