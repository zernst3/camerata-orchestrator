# UoW governed-dev redesign — Backend Increment 1 (deterministic core)

Date: 2026-06-21
Status: Implemented
Scope: `crates/server/**`, `crates/fleet/**` (no `crates/ui` changes)

## Summary

Increment 1 delivers two backend capabilities for the UoW governed-dev redesign,
behind a frozen contract the UI builds against:

1. The development run can use a **per-UoW three-tier model map**, with the strongest
   tier acting as the orchestrator/lead.
2. **"Begin investigation"** runs a **single-model investigation agent** that analyzes
   the issue/story and records an investigation note onto the UoW.

Both reuse the existing run-store, fleet, and agent-spawn machinery. The universal
governance gate is unchanged and was not weakened anywhere.

## Endpoint 1 — development run (`POST /api/stories/:id/run`)

### Request (extended, back-compatible)

```jsonc
{
  "model": "<string|null>",          // single operator-wide model (existing)
  "tier_map": {                       // NEW, optional
    "strongest": "<id>",
    "balanced":  "<id>",
    "fast":      "<id>"
  } | null
}
```

- An absent body, an absent `tier_map`, or a `tier_map: null` all take the existing
  single-`model` path unchanged (full back-compat; no-body callers still work).
- When `tier_map` is present it takes precedence over `model`.

### Response (unchanged)

```json
{ "run_id": "<id>", "story_id": "<id>", "mode": "live" | "scripted" }
```

### Tiered dev-run wiring

`StartRunReq` gained `tier_map: Option<TierMap>` (re-exported
`camerata_fleet::tier::TierMap`). `start_run` extracts both fields and passes them to
`start_governed_run(state, story_id, model, tier_map)`.

In the live branch of `start_governed_run`:

- `Some(map)` → spawns `live_fleet::execute_live_run_tiered(...)`, which builds a plan
  whose **lead implementer task (Backend → `Strongest`)** owns the complex/one-way-door
  work and acts as orchestrator, plus a follow-on **test task (Test → `Fast`)** for the
  mechanical verification. It calls the fleet's existing
  `build_from_plan_with_tier_map`, which classifies each task via
  `tier::classify_task` and threads the band's model id into that stage's driver via
  `with_model(id)`. So each task runs on its band's model, strongest leading.
- `None` → spawns the existing `execute_live_run(...)` single-model path, unchanged.

The scripted (token-free) path ignores both `model` and `tier_map`, exactly as before.

`live_fleet.rs` was refactored to share the `BuildEvent → GateEvent` recording closure
(`record_build_event`) and the terminal step (`finish_live_run`) between the
single-model and tiered functions, so both report progress identically.

### Gate preservation (dev run)

`ensure_development_gate` runs **before** either path is chosen, identically for both.
A `tier_map` does not bypass the no-code-first gate: a run with a tier map but no
approved decisions still returns `409` (`dev_run_accepts_tier_map_and_still_enforces_the_gate`).
Every spawned tier delegate is built by the same `build_from_plan_*` machinery, so each
keeps `--allowedTools` = gated tools only and `Task` on the disallowed list.

## Endpoint 2 — investigation run (`POST /api/uow/:story_id/begin-investigation`)

### Request

```jsonc
{ "model": "<id>" | null }   // absent body also accepted
```

`null`/blank/absent defaults to the active project's `tier_map.strongest` (falling back
to the shipped strongest default when there is no active project).

### Response

```json
{ "run_id": "<id>", "story_id": "<id>" }
```

so the UI can poll `GET /api/runs/:id` and watch AgentActivity.

### Behavior

The handler:

1. Transitions the stage **Intake → Investigating** via the existing
   `state.uow.begin_investigation`. If that transition is illegal (e.g. the UoW is not
   at Intake) it returns `409` with the precise reason and **starts no run**.
2. Resolves the model (caller's choice → project `tier_map.strongest` → default).
3. Pulls the story title/description for the agent prompt (best-effort).
4. Creates a run (`mode = "investigation"`) in the existing `RunStore` and spawns
   `investigation_run::execute_investigation_run(...)`.

### The investigation runner (`crates/server/src/investigation_run.rs`)

A **single** gated agent — NOT the development fleet. Investigation analyzes; it does
not scaffold or write code. It is built from the SAME machinery the fleet uses
(`camerata_fleet::governed_role("Investigator")` + `camerata_agent::prepare_session`),
so it carries the identical universal tool gate: `--allowedTools` = gated tools only,
`Task`/`Write`/`Bash`/… on the disallowed list. The agent's only mutation path is the
governance gate; it cannot spawn sub-agents.

- **Live mode on** (`CAMERATA_LIVE_BUILD=1`): one real `claude -p` agent runs on the
  resolved model with a read-oriented prompt that asks it to restate the story, list
  ambiguities, surface the decisions/tradeoffs the architect must resolve, and state
  what is out of scope. Its output is recorded verbatim as an `InvestigationArtifact`
  note on the UoW (attributed to the AI, `reviewed = false`, awaiting the architect),
  plus a `note` history entry. Honest: the note IS the model's output, never seeded.
- **Live mode off** (default; CI): no `claude` process is spawned. The runner records a
  clearly-labelled placeholder note ("investigation pending — live mode is off") and
  completes to AwaitingQa. This mirrors the dev run's scripted/live split and keeps CI
  token-free. Nothing is faked — the placeholder is explicitly labelled, never invented
  findings.

This is the smallest **real** single-agent investigation: one gated agent that reads
the issue context and emits a real note into the UoW. The decision-record extraction
(parsing the note into structured `DecisionRecord`s) is left to the architect via the
existing `POST /api/uow/:story_id/decisions` endpoint; the agent surfaces the decisions
in prose within the note.

## Scope notes / what was deliberately not done

- The fleet's `build_from_plan_with_tier_map`, `TierMap`, `CapabilityBand`, and
  `classify_task` already existed (ORCH-MODEL-TIERING-1); this increment **wired** the
  per-UoW map through the run path rather than rebuilding tier infrastructure.
- The investigation agent emits its decisions/tradeoffs as prose inside the note. It
  does not yet auto-create structured `DecisionRecord` artifacts; that remains an
  architect action through the existing decisions endpoint. Surfacing-only is the
  honest, read-oriented behavior for Increment 1.
- No `crates/ui` changes (out of scope for the backend increment).

## Tests added

- `start_run_req_parses_tier_map_when_present`
- `start_run_req_tier_map_absent_is_back_compat_single_model`
- `dev_run_accepts_tier_map_and_still_enforces_the_gate` (gate is universal)
- `dev_run_tiered_path_starts_once_decisions_are_approved`
- `begin_investigation_is_model_aware_returns_run_id_and_transitions_stage`
- `begin_investigation_accepts_absent_model_body`
- `begin_investigation_409s_when_not_at_intake`
- `investigation_run::investigation_prompt_is_read_oriented_and_names_the_story`
- `investigation_run::investigation_run_token_free_records_placeholder_note_and_completes`

`cargo build --workspace -j2` and `cargo test --workspace` are green.
