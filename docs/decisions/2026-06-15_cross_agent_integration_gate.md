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

## What it enforces: any invariant that spans agents

The defining property of a cross-agent rule: **it holds across two or more agents'
outputs, so no per-agent gate can see it; it is only checkable on the assembled
whole.** Litmus test: if catching the violation requires looking at more than one
agent's worktree at once, it belongs in this tier. API contracts are the obvious first
example, not the category. The space is broad; a non-exhaustive map:

**1. Contract conformance (interface shapes agree across the seam)**
- API contracts: the endpoints, methods, request/response shapes, and status codes a
  consumer calls match what the producer exposes.
- Shared DTOs / types / enum value sets agree on both sides.
- The error and status-code shapes the consumer handles match what the producer returns.

**2. Wiring completeness (no dangling ends)**
- Every event/message emitted has a consumer; every subscribed event is actually emitted.
- Every config / env var one agent reads is declared or provided by another.
- DI / module wiring: a service registered by one agent is the one injected by another.
- Every entity the code references has a migration that creates it (DB agent vs code).

**3. Convention coherence (the same decision made the same way everywhere)**
- Serialization casing across the wire (snake_case JSON vs a camelCase consumer).
- The same concept named consistently across agents (`member_id` / `memberId` / `user_id`).
- Pagination/filtering conventions, date/timezone handling, money/units representation,
  and i18n key sets, agreed across the boundary. (The earlier intake currency and
  timezone clarifications are exactly these, now enforced ACROSS agents, not just within
  one.)

**4. Cross-cutting policy holds end to end (true of the whole, not per file)**
- Authorization actually enforced server-side for every action the UI gates. The
  classic gap, the UI hides a button but the endpoint is open (or 403s), is a
  cross-agent inconsistency no per-agent gate catches: the UI agent's diff is clean,
  the API agent's diff is clean, and the SEAM is wrong. The integration tier is the
  only place positioned to enforce "every gated affordance maps to a guarded endpoint."
- A write-path audit / provenance convention is honored by every agent's write path.
- Referential / soft-delete behavior is consistent across the agents that touch an entity.

**5. The seam is tested**
- An integration / contract test exists for each cross-agent boundary, not just
  per-agent unit tests. The tier can require that the seam itself be covered.

Same deterministic-first principle as the other tiers: lead with the invariants that are
mechanically checkable (a contract diff, a casing lint across the wire, "every gated UI
action maps to a guarded endpoint," migration-vs-entity reconciliation), and leave the
genuinely semantic ones to human QA.

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
