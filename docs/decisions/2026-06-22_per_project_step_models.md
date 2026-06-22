# Per-project, per-step model configuration (`StepModels`)

Date: 2026-06-22
Status: Accepted

## Context

Every NON-FLEET AI step in Camerata (the brownfield audit, the calibration pass, the
research chat, story authoring, decomposition, escalation translation, and clarification
authoring) chose its model in an ad-hoc way: some steps had no model at all (falling
through to `Llm`'s env-resolved `default_model`), some pulled a model from an unrelated
config (escalation read the *routine's* model), and the UI-picked steps (audit /
calibration / chat) only had the per-run user pick with an env/const floor underneath.

There was no per-project home for these models. The governed development FLEET already had
one (`TierMap`, `ORCH-MODEL-TIERING-1`), but the fleet's tier map is a *different axis*: it
maps each task's capability band (fast / balanced / strongest) to a model for governed
runs. It does not, and should not, govern the non-fleet steps.

## Decision

Add a `StepModels` struct stored ON each `Project`, with one model-id slot per non-fleet
step. It mirrors the `TierMap` pattern exactly so the codebase stays uniform:

- `#[serde(default)]` on the `Project.step_models` field (legacy-JSON back-compat: a
  project persisted before this field existed deserializes to defaults, no migration).
- `#[serde(default = "default_model")]` on every field of `StepModels` (a *partial* blob,
  e.g. only `audit` present, still loads — the rest default).
- A `Default` impl seeding every slot with `DEFAULT_MODEL` (`claude-sonnet-4-6`).
- Per-project storage: mutated only through `ProjectStore::set_step_model(id, step, model)`,
  which goes through `update()` — it borrows ONE project's `&mut` and saves. A change to
  project A can never touch project B (the isolation guarantee, see below).

### `StepKind`

The enum of non-fleet steps, one variant per `StepModels` field:

`Audit`, `Calibration`, `ResearchChat`, `StoryAuthoring`, `Decomposition`, `Escalation`,
`Clarification`.

`Project::model_for_step(StepKind) -> &str` resolves the slot;
`Project::set_model_for_step(StepKind, String)` sets one slot in place.

### The no-fallback rule

**Once a project exists, there is NO runtime env/const fallback for these steps.** The
project's per-step value is authoritative — it is seeded to `DEFAULT_MODEL` at creation and
changed only through the setter. Call sites resolve the model from the active project and
put it explicitly on the `LlmRequest` (via `.with_model(...)`), so `Llm`'s internal
env-default (`CAMERATA_LLM_MODEL` / `DEFAULT_MODEL`) is never reached for a non-fleet step
when a project is active.

### The default-at-creation rule

Every project-construction site seeds `step_models: StepModels::default()` (create,
import/overwrite-create). So the moment a project exists, every step already has a concrete
model. There is no "unset" state to reason about.

### Per-project isolation guarantee

`set_step_model` mutates exactly the named project (the `update` closure borrows that one
project's `&mut`). It is structurally impossible for a change to one project to leak into
another. The isolation test (`set_step_model_is_per_project_isolated`) proves it: it
creates A and B, sets A's audit model to `claude-opus-4-8`, then asserts B's audit model is
still `DEFAULT_MODEL` and A's *other* steps are still `DEFAULT_MODEL`.

### UI-picked vs. fallback steps

Two resolution patterns, both rooted in the project's per-step model:

- **Fallback steps** (story authoring, decomposition, escalation, clarification): read the
  project step model directly via the `step_model(state, step)` helper. No per-run override.
- **UI-picked steps** (audit, calibration, research chat): the explicit per-run request
  model still WINS when non-empty; otherwise the project step model is the default. Resolved
  via `step_model_or(state, step, requested)`.

Both helpers live in `crates/server/src/lib.rs`. `step_model_or` delegates to `step_model`
for the no-override case, so there is one resolution point.

## The step → call-site mapping (every site rewired)

| Step            | Kind                    | Call site                                                  | Pattern        |
|-----------------|-------------------------|-----------------------------------------------------------|----------------|
| Audit           | `Audit`                 | `onboard_audit` + `onboard_audit_start` (lib.rs)          | UI-picked      |
| Calibration     | `Calibration`           | `onboard_audit` + `onboard_audit_start` (lib.rs)          | UI-picked      |
| Research chat   | `ResearchChat`          | `chat` (lib.rs)                                            | UI-picked      |
| Story authoring | `StoryAuthoring`        | `uow_author` (lib.rs)                                      | fallback       |
| Decomposition   | `Decomposition`         | `decompose_propose` → `decompose::propose_ai(.., model)`  | fallback       |
| Escalation      | `Escalation`            | `answer_escalation` → `escalation::translate_answer_ai`   | fallback       |
| Clarification   | `Clarification`         | `suggest_clarifications` (lib.rs)                          | fallback       |

Notes:

- The audit/calibration models are now resolved to a concrete id (`Some(step_model_or(..))`)
  rather than `None` when the user didn't pick — the downstream `ai_audit` signature
  (`audit_model: Option<&str>`) is unchanged; we simply always pass `Some`.
- `decompose::propose_ai` gained a `model: &str` parameter; the handler resolves it.
- `answer_escalation` previously pulled the model from the *routine* config; it now uses the
  project's `Escalation` step model (the routine model is no longer consulted for this).

## The project-less floor (only remaining `DEFAULT_MODEL` usage)

The ONLY place `DEFAULT_MODEL` is still a runtime floor for a non-fleet step is when there
is **no active project at all** (e.g. the smoke-test chat fired before any project is
created). `step_model` returns `DEFAULT_MODEL` only in that branch. Once a project exists,
its per-step value is used unconditionally. This is documented on `step_model` and on
`StepModels`.

## API

`POST /api/projects/:id/step-models` with body `{ "step": "audit", "model": "claude-opus-4-8" }`.
Patch semantics: one step per call; the others are untouched. Mirrors the tier-map route /
handler. Unknown step or blank model is a no-op error response (never a silent mutation).
The step key is tolerant of dash/space/case (`research-chat` == `research_chat`).

## UI

A "Step models" section (`StepModelsEditor` + `StepModelRow`) in the project-settings panel,
next to the tier-map editor (both the Rules-window settings block and the gear popup). One
labeled `<select>` per step, options from `GET /api/models`, current value from the
project's `step_models`, saving on change to `POST /api/projects/:id/step-models` with the
scoped project id (never a global).

## This is config only

`StepModels` changes which MODEL a step runs on. It does not touch any gate or enforcement
path — no governance change. The fleet tier map (`TierMap`) is untouched and remains the
governed-run axis.

## Scope deliberately excluded (fleet steps)

The investigation / update-branch / pr-resolve runs are FLEET-driven (driver `.with_model`
off `tier_map.strongest`), not non-fleet steps. They are out of scope for `StepModels` by
design — they belong to the tier-map axis.
