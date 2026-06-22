# UoW development run: orchestrator-led tier delegation (gate-preserving)

**Date:** 2026-06-21 · **Decided by:** Zach (directed) · Status: approved design, implementation pending his go on the gate-interaction approach.

## The model (Zach's direction)

The UoW is the only place AI runs. Runs attach to the **lifecycle steps**, not a standalone
"Run this work" button:

- **Investigation phase** ("Begin investigation"): runs the investigation agent with **one**
  selectable model (defaults to the top tier; customizable).
- **Development phase**: a **three-tier** model selection (top / mid / low), customizable
  per-UoW, mirroring the routine model-tiering. Shown at the Development step, in place of the
  investigation button (the active step's run control swaps by phase).

The three tiers are NOT a mechanical per-task complexity classifier. They are an
**orchestrator + two delegate pools**: the **top-tier model is the parent orchestrator** — it
does the complex work itself AND decides which subtasks to hand to the mid/low-tier models.
Mirrors the parent-agent → subagent pattern (Claude Code: keep the hard reasoning, delegate the
mechanical parts).

## The gate constraint (why this needs care)

The fleet spawns every agent with `--allowedTools` = gated tools only, and **explicitly
disallows `Task`** (subagent spawning) — see `crates/agent/src/lib.rs` (`disallowed_builtins`
includes `Task`). That is the capability-based, prompt-proof core of the gate
([[camerata_gate_universal_enforcement]]). Letting the orchestrator call `Task` directly would
let it spawn a child that escapes the gate — NOT acceptable.

## Decision: fleet-mediated delegation (gate-preserving)

Delegation is **fleet-mediated**, not agent-spawned:

1. The top-tier (orchestrator) agent does the complex work AND emits a structured
   **delegation plan** (which subtasks → which tier).
2. The **fleet** spawns the mid/low-tier agents for those subtasks — each still gated with only
   `gated_write`. `Task` stays disallowed for every agent.

This yields Zach's "parent decides delegation" model while every agent stays capability-confined
and the gate invariant holds. The fleet already has the tier infra (`TierMap`, `classify_task`,
`build_from_plan_with_tier_map` in `crates/fleet/src/tier.rs`); the change is making the
orchestrator's plan drive delegation instead of a fixed complexity heuristic.

## Implementation scope (FE + BE)

- **Backend:**
  - `begin-investigation` kicks an investigation agent run (single chosen model), not just a
    stage transition.
  - The dev run accepts a per-UoW **tier-map override** (top/mid/low) and runs the
    orchestrator-led delegation (orchestrator on top tier emits the delegation plan; fleet
    dispatches gated subagents on the mid/low models).
- **Frontend:**
  - Move run controls out of the top of `UowDevControls` into the lifecycle steps.
  - Investigation step: 1 model select + run. Development step: 3 tier selects (default from the
    project `tier_map`, customizable per-UoW) + governed run. Remove the standalone "Run this
    work (governed)".
  - Remove the now-dead clarify subsystem in this pass.

Builds on [[workitem_uow_governed_dev_architecture]]. The gate stays universal
([[camerata_gate_universal_enforcement]]).
