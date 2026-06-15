# Cross-agent rules: the integration gate (a third enforcement tier)

Date: 2026-06-15
Status: Accepted (design); NOT built.
Deciders: Zach (architect), Claude (architect)

Companion docs: [`ENFORCEMENT.md`](../ENFORCEMENT.md) (the two tiers that exist today),
[`RATIONALE.md`](../RATIONALE.md), [`VISION.md`](../VISION.md) (contract handoffs).

## Context: the per-agent gates have a blind spot

A single story is decomposed into role agents (UI, API, DB, ...) that each work in an
isolated worktree. The enforcement that exists today is entirely **per agent**:

- Layer 1 (MCP tool-gateway): evaluates one write, for one agent, before it executes.
- Layer 2 (check runner): evaluates one agent's diff (fmt / clippy / test) after a task.

Both can pass green for every agent while the **seam between them is broken**. The API
agent ships a clean diff exposing `POST /members/export`; the UI agent ships a clean
diff calling `POST /members/csv`. Each agent only ever sees its own worktree, so
nothing in the per-agent tiers can catch the mismatch. The contract drift surfaces only
when the pieces are combined.

Language matters here, and it sharpens the design:

- In a Rust monorepo with a **shared type across the boundary**, the compiler IS the
  cross-agent gate: both sides import the same type, and a mismatch simply does not
  compile. Drift is impossible.
- A fullstack JavaScript app has no shared compiler holding the seam. The frontend and
  backend can disagree about the contract silently, and ship.

Since the generated apps can be JavaScript, this gate is load-bearing, not optional.

## Decision: a cross-agent integration gate, run before the branch ships

Add a third enforcement tier that runs **once on the integrated tree** (all role
agents' worktrees combined), **after** per-agent execution and **before** the branch is
pushed anywhere. It is a pre-PR gate: the human (and any remote) only ever sees a
branch that is already cross-agent-consistent.

Pipeline position:

```
execute role agents (each gated by Layer 1 + Layer 2)
        |
        v
integrate the worktrees (dependency order)
        |
        v
[ CROSS-AGENT INTEGRATION GATE ]   <- new; runs on the WHOLE tree
        |  pass                         fail -> bounce to the responsible
        v                                       agent(s), or escalate to the Architect
push branch / open PR
        |
        v
human QA
```

## What it enforces (cross-cutting invariants)

- **API contract conformance:** the endpoints, methods, request/response shapes, and
  status codes the consumer calls match what the producer actually exposes.
- **Shared schema / type consistency:** DTOs, enums, and serialization formats agree
  across the boundary.
- **Interface / port conformance:** where one agent defines a port and another
  implements or consumes it.
- **DB schema vs code agreement:** migrations match the entities the code uses.
- **No dangling references** across agents (a call with no corresponding handler, a
  consumed field never produced).

## The principle: prefer compiled contracts; check explicitly where you cannot

"Rust makes it impossible to get wrong" generalizes to a rule: **make the contract a
compiled artifact whenever the stack allows**, so the seam is enforced for free and the
integration gate has nothing to do. Where the stack cannot (JavaScript), the gate runs
an explicit, deterministic contract check: derive the contract from the producer (e.g.
an OpenAPI document or a generated typed client) and verify the consumer conforms, or
run contract tests across the boundary. Deterministic cross-agent checks first; fuzzy
semantic ones (does this endpoint mean what the story intended) stay human-QA.

## Connection to contract handoffs

VISION already has contract handoffs: an upstream task emits a contract (API / type
definitions) that downstream tasks consume, and the coordinator passes it forward. This
gate is the enforcement half of that: the contract is **declared at handoff** (the
producer emits it) and **enforced at integration** (the gate verifies the consumer
matches it). Declare at handoff, enforce at integration.

## Mechanism

A new integration-scoped check runner that operates on the assembled tree rather than a
single agent's diff (the per-agent `CheckRunner` is scoped to one worktree; this is its
cross-agent sibling). A failure bounces back to the responsible agent(s) with the
specific mismatch (same bounce-and-revise loop as Layer 2), or escalates to the
Architect when the conflict is a genuine design fork rather than one side being wrong.

Rule category: a new `INTEGRATION-*` family (e.g. `INTEGRATION-API-CONTRACT-1`,
`INTEGRATION-SCHEMA-MATCH-1`), distinct from the per-write (path/content) and per-task
(fmt/clippy/test) rules, because its scope is the integrated whole.

## Honest current state

Not built. Today there is Layer 1 + Layer 2, both per-agent. The `FleetCoordinator`
integrates completed-and-gated tasks in dependency order and conceptually "re-runs gates
at integration," but no cross-agent contract checks exist, and there is no
`INTEGRATION-*` rule family. This ADR defines the third tier; building it is future
work (and it pairs with making the deterministic contract-derivation step real per
stack).

## Open questions

- Contract derivation for non-Rust stacks: generate OpenAPI from the producer and
  validate the consumer? Generate a typed client the consumer must use? Both?
- Which cross-agent invariants are deterministically checkable now vs which need new
  tooling (API-contract diff is tractable; full DB-schema-vs-code is harder).
- Bounce vs escalate routing: when is a mismatch one agent's bug (bounce) vs a real
  design conflict the Architect must resolve (escalate)?
