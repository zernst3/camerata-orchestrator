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

## Decision: fleet-mediated delegation (gate-preserving) — TARGET design B, built in two increments

Delegation is **fleet-mediated**, never agent-spawned. `Task` (the CLI's ungoverned subagent
spawner) stays disallowed for EVERY agent; spawning is done by the fleet, which gates every child
with only `gated_write`. Two wirings were considered:

- **A — plan, then dispatch:** the orchestrator emits a full delegation plan as output; the fleet
  spawns the tier agents from it. Fully deterministic, but the plan is committed up front, so a
  subtask that turns out harder than estimated has **no escalation path**.
- **B — governed `delegate` tool + parent-driven escalation (TARGET):** the orchestrator gets ONE
  extra capability, a Camerata-owned `delegate(subtask, tier)` MCP tool (NOT `Task`). It delegates
  mid-run; the gateway routes the call; the fleet spawns a gated child on the chosen tier and
  returns the result to the orchestrator, which stays in the loop.

**Why B:** dynamic escalation. A delegate that hits work above its tier just **returns**
("incomplete / above my tier / reason") — it never calls "up." The **orchestrator** reads that
return and reroutes (does it itself, or re-delegates higher). Escalation is therefore
**parent-driven**, with no child→parent up-calls to design, and the delegation-depth guard
collapses to a **trivial counter** (max re-delegations per task). The gate holds throughout — every
spawned agent at every tier is born with only `gated_write`.

**Build order (nothing throwaway — A's machinery is B's foundation):**
- **Increment 1 (deterministic core):** fleet spawns gated tier agents; per-UoW tier-map override;
  runs move onto the lifecycle steps; orchestrator emits a plan and the fleet dispatches. Airtight
  and independently useful.
- **Increment 2:** the live `delegate` MCP tool + parent-driven escalation + the depth counter.

The fleet already has the tier infra (`TierMap`, `classify_task`, `build_from_plan_with_tier_map`
in `crates/fleet/src/tier.rs`). The change in increment 1 is making the orchestrator's plan drive
delegation instead of a fixed complexity heuristic; increment 2 makes delegation live + escalatable.

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
