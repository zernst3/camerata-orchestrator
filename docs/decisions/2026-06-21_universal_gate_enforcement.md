# Decision: the gate is enforced universally, at the execution seam

**Date:** 2026-06-21
**Status:** Invariant (one-way door). Do not weaken.

## Decision

Camerata's governance gate (deny-before-execute) MUST apply to **every** code-writing agent,
identically, regardless of how the work was initiated:

- the **Development screen** (a human working a story),
- a **routine** (a scheduled run: bug-triage, maintenance, anything that writes code),
- any **future dev automation**.

There is **no ungated code-writing path**, and a user **cannot prompt their way out of the gate.**

## Why

The deterministic gate is Camerata's differentiating wedge: strict, binary, fail-closed
enforcement of what an agent may write, not an LLM grading an LLM. If a routine (or any
alternate entry point) could write code *outside* the gate, the entire claim collapses — the
guarantee would depend on which button you pressed. The guarantee must hold by construction.

## How it is enforced (capability, not instruction)

The seam is the spawn, not the prompt:

- Every code-writing agent is launched with `--allowedTools` = **only** the gated MCP
  `gated_write` tool (`camerata_agent::GATED_WRITE_TOOL`), via the fleet
  (`crates/fleet/src/lib.rs`). The agent has no other write capability.
- `gated_write` is deny-before-execute, **worktree-jailed** (refuses any target outside the
  worktree, in code), and **fail-closed**.
- Because the restriction is a *capability* the agent doesn't possess — not an instruction it
  is asked to follow — no prompt can reach an ungated write. The generic (research/clarify)
  agent is spawned with **no** write tools at all.
- Routines are scheduled **governed runs** that reuse the same run engine + gate
  (`crates/server/src/routine.rs`), so they inherit the seam by construction.

## Obligations on future work

1. Any code path that spawns a code-writing agent MUST go through the fleet's gated spawn.
   Spawning a writer without the gated `allowedTools` set is forbidden.
2. When routine-driven **live** dev execution is wired, it MUST route through that same gated
   path — never a generic/ungated writer.
3. Guard it mechanically (Camerata's own principle): the fleet test asserts the gated tool is
   in the spawn, and the generic-agent test asserts it has no write tools. Extend these so the
   routine live-dev path is covered when it lands.

This is not subject to the feature freeze — it is the core security invariant, and the only
work it implies is *preserving* an existing guarantee as new surfaces are wired.
