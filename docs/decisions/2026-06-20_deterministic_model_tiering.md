# Decision: Deterministic Model-Tiering for the Governed Fleet

**Date:** 2026-06-20
**Status:** Implemented (ORCH-MODEL-TIERING-1)
**Branch:** dev2/model-tiering

---

## What

Adds a deterministic, per-stage model-selection mechanism to the governed fleet.
Instead of every agent in a build run sharing one single model choice, each
`PlanTask` is classified into a capability band, and the band is resolved to a
concrete model id from a user-configurable map stored on the `Project`.

Three deliverables:

1. **`camerata_fleet::tier` module** — `CapabilityBand`, `TierMap`, `classify_task`,
   and their unit tests (in `crates/fleet/src/tier.rs`).
2. **`build_from_plan_with_tier_map`** in `camerata_fleet` — a new fleet entry point
   that uses the tier map for per-stage model resolution.
3. **`Project::tier_map`** in `camerata_server::project` — the persisted tier map,
   serde-defaulted for back-compat, exposed via `camerata_server::model_tier`.

---

## Why

### The existing single-model design

`build_from_plan_with_model_and_iterations` threads ONE operator-supplied model id
to EVERY agent in the fleet. That was the right first cut: "a single operator
intent, no mixing". But it forces a false choice: either pay Opus rates for a test
scaffold, or accept Haiku quality for a domain-level type design.

### The cost argument

A typical 4-task plan (schema + backend + frontend + tests) at 3 API calls per
stage and ~1000 tokens each:

| All-Opus | Tiered (Balanced/Strongest/Balanced/Fast) |
|----------|------------------------------------------|
| ~$0.90   | ~$0.21 (~77% cheaper) |

At project scale (dozens of runs per day) this is a meaningful operational lever.

### The correctness argument

Test generation is mechanical — the model reads the types and writes assertions.
Backend domain modelling is a one-way-door — the field types and newtype IDs the
first agent picks constrain every downstream stage. Using Opus for the mechanical
pass and Haiku for the architectural pass is exactly backwards. Tiering flips this.

---

## How

### CapabilityBand

Three vendor-neutral labels, defined in `crates/fleet/src/tier.rs`:

- `Fast` — mechanical / high-throughput. Default: Claude Haiku 4.5.
- `Balanced` — structured implementation. Default: Claude Sonnet 4.6.
- `Strongest` — architectural / one-way-door. Default: Claude Opus 4.8.

The labels are stable across model generations. Upgrading from "Haiku 4.5" to
"Haiku 5" is a `TierMap` config change, not a code change.

### TierMap

Serde-serialisable struct mapping each `CapabilityBand` to a concrete model id.
Every field has a serde default, so a `Project` JSON written before this feature
deserialises correctly — `serde` fills in the defaults. Same pattern as
`max_iterations` on `Project`.

Stored on `Project::tier_map` in `camerata_server::project`. Exposed to server
consumers via `camerata_server::model_tier`, which re-exports from fleet.

### classify_task

Pure, deterministic function with no I/O. Maps `TaskKind` to `CapabilityBand`:

| `TaskKind` | Band | Rationale |
|---|---|---|
| `Test` | `Fast` | Mechanical; fluency over depth. |
| `Database` | `Balanced` | Structured; mid-tier correct at lower cost. |
| `Frontend` | `Balanced` | Bounded reasoning over view/screen code. |
| `Backend` | `Strongest` | Domain logic + API surface: one-way-door. |

**Per-task override prefix**: a task description starting with `[TIER:fast]`,
`[TIER:balanced]`, or `[TIER:strongest]` (case-insensitive, leading whitespace
stripped) overrides the heuristic for that task. Unrecognised tier labels fall
through to the heuristic. This makes tiering overridable without a schema change.

### build_from_plan_with_tier_map

New entry point in `camerata_fleet::lib`. Calls `tier_map.model_for_task(task)`
for each `PlanTask` to get a per-stage model id, then threads it into the stage's
driver via `with_model(id)`. Identical fleet governance, identical bounce-and-revise
loop — only the per-stage model selection differs.

All existing entry points (`build_from_plan`, `build_from_plan_with_model`,
`build_from_plan_with_model_and_iterations`) are unchanged.

---

## Where things live

| Artifact | Location |
|---|---|
| `CapabilityBand`, `TierMap`, `classify_task` | `crates/fleet/src/tier.rs` |
| `build_from_plan_with_tier_map` | `crates/fleet/src/lib.rs` |
| `Project::tier_map` | `crates/server/src/project.rs` |
| Server re-exports + Project integration tests | `crates/server/src/model_tier.rs` |
| Fleet Cargo.toml | Added `serde` dependency |

---

## Constraints honoured

- **ROUTE-1 (additive only)**: no new crates, no moved module boundaries, no
  cross-crate public API removals. All existing entry points compile and pass.
- **Back-compat**: `Project::tier_map` serde-defaults to `TierMap::default()`.
  Legacy projects load with no migration.
- **No circular deps**: `camerata_fleet` does not depend on `camerata_server`.
  The tier types live in fleet (where they are used). Server imports from fleet.
- **`crates/server/src/lib.rs` change is minimal**: one `pub mod model_tier;` line.

---

## What this does NOT do (routed decisions)

- **Wire `build_from_plan_with_tier_map` into `live_fleet.rs`**: `live_fleet` still
  calls `build_from_plan_with_model_and_iterations`. Routing `live_fleet` to the
  tier-map path requires a handler change that reads `project.tier_map` — a
  straightforward follow-up once the UI surfaces the tier-map editor.
- **UI editor for the tier map**: the decision doc routes this to the next sprint.
  The config + fleet wiring are shipped; the editor is optional per the task.
- **Non-Anthropic models in the default map**: the defaults use Anthropic model ids
  today. The config is vendor-neutral; a user can point any band at any model id.

---

## Testing

- `crates/fleet/src/tier.rs`: 23 unit tests covering classification heuristic,
  override prefix parsing (case insensitive, leading whitespace, unknown bands),
  `TierMap` serde defaults, and end-to-end `model_for_task` resolution.
- `crates/fleet/src/lib.rs`: 2 tests — API signature compile test for
  `build_from_plan_with_tier_map`, and a model-resolution correctness test over a
  4-task mixed plan.
- `crates/server/src/model_tier.rs`: 4 integration tests — new project gets default
  tier map, tier map survives store round-trip, legacy JSON deserialises with
  defaults, custom values survive project JSON round-trip.

All 29 new tests pass. No existing tests broken (223 total in fleet + server, all green).
