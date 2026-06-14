# Camerata Orchestrator: Design Rationale

> Purpose: why this is built the way it is, not just what it is. The audience is a
> technical reader who wants the reasoning behind the architecture. Companion docs:
> [`ARCHITECTURE.md`](ARCHITECTURE.md) (the stack) and [`ENFORCEMENT.md`](ENFORCEMENT.md)
> (the gate in detail).

---

## 1. The problem: agents are probabilistic, and prompts are advisory

LLM coding agents are probabilistic. They cannot reliably verify their own output,
and a second model asked to review the first is still probabilistic. "Guardrails"
expressed as prompt instructions are advisory: the model can ignore them, and
nothing structural stops a disallowed action from executing. As an agent-generated
codebase grows, ungoverned output accumulates structural inconsistency that no
amount of model scaling removes the need to catch.

The engineering question that follows is the one this repository explores: can the
rules an agent must obey be enforced mechanically, outside the model, deterministically,
so the result is a binary pass/fail rather than a model's opinion?

---

## 2. The approach: two deterministic layers

Camerata answers that with two layers that make zero model calls:

- **Layer 1, a real-time MCP tool-gateway (deny-before-execute).** Every agent tool
  call (write a file, run a command, call an external API) routes through a Rust MCP
  server that allows or denies it BEFORE it executes. A violation a prompt-only setup
  would silently permit is rejected at the boundary.
- **Layer 2, post-task structural checks.** After an agent task completes, `cargo fmt`,
  `cargo clippy`, and `cargo test` run out-of-process; any failure bounces the work
  back for revision. Binary pass/fail, not "the model thinks this looks right."

The combination is what makes generated code stay coherent under an agent rather
than drifting into inconsistency: the gate is the deterministic floor the
probabilistic part has to clear.

---

## 3. What is proven, stated narrowly

The claim worth leading with is the narrow one that is fully reproducible:

> A deterministic, deny-before-execute MCP gate, written in Rust, locked a real
> `claude -p` agent to a single gated tool and blocked a forbidden write before it
> touched disk, in microseconds, in-process and fail-closed.

That is reproducible by running `cargo run -p camerata -- live-demo`
(see [`RUST_CORE_VERIFICATION.md`](RUST_CORE_VERIFICATION.md) and
[`LIVE_RUN_VERIFICATION.md`](LIVE_RUN_VERIFICATION.md)).

Two honest scoping notes belong right next to it:

- The gate today enforces five rules (a path-segment guard against `..` traversal
  and writes into `.git`/`.ssh`, a forbidden-path guard, and three regex content
  heuristics for secrets / raw-SQL-concat / secrets-in-URLs; no AST analysis yet),
  with the rest of the corpus catalogued but not yet given executable enforcement
  arms. The architecture is the point; deepening the rule set behind the seam is
  incremental, not architectural.
- The end-to-end consumer run ([`PO_MODE.md`](PO_MODE.md), the `po-demo`) takes a
  non-technical intake form through the lead engineer and a governed fleet to a
  passing `cargo build` / `cargo test`, not to a live deployed application.

---

## 4. Why this is hard to replicate (as engineering)

Two properties are load-bearing and architectural, not incidental:

- **Determinism and fail-closed behavior.** The verdict function is pure: the same
  `(rule_subset, tool_call)` always yields the same decision, and an unparseable or
  empty rule configuration fails closed onto the verified default rather than
  opening the gate. That is a property no probabilistic verifier can offer.
- **Provider neutrality by construction.** The gate sits at the MCP tool boundary and
  the agent runtime sits behind a seam, so a non-Claude model swaps in without
  touching the gate. This is a structural consequence of where the gate lives, not a
  feature bolted on, and it is demonstrated with a second, non-Claude driver
  ([`PROVIDER_NEUTRALITY.md`](PROVIDER_NEUTRALITY.md)). The gate does not depend on any
  one model vendor.

---

## 5. The interaction design: the non-technical user as requirements owner

A second design choice is who specifies the work. Instead of an engineer editing
YAML or a markdown "constitution," the non-technical user is interviewed before any
code is written: a structured intake form, then a refinement conversation with an AI
lead engineer that surfaces plain-language user stories, a climbing confidence score,
proactive suggestions, and honest limits. The user occupies the requirements-owner
role (the Product Owner role, in agile terms) and is treated as one.

The reasoning: a clarification-first interaction catches ambiguity at the cheapest
point (before generation), and the honest-limits behavior ("what you are describing is
essentially a standard CRM, you would likely be happier with an existing tool") draws
the boundary of what the approach should and should not attempt. Both are interaction-
design decisions in service of governed, predictable output, detailed in
[`CONSUMER_UX.md`](CONSUMER_UX.md) and [`PO_MODE.md`](PO_MODE.md).

---

## 6. Honest scope and limits

Stated directly, because omitting them would make this document less useful:

- **The hard part is generation reliability under governance, not the intake UI.** A
  clarification loop that produces a spec is straightforward. An agent that reliably
  generates code passing the deterministic gates on the first attempt, across diverse
  project types, is not. That is where the real engineering lives.
- **Clarification-first interaction is convergent, not unique.** Other tools are moving
  toward an editable plan before generation. The durable interest here is the
  determinism of the enforcement layer, not the idea of asking questions first.
- **The two surfaces have different maturity.** The engine drives two surfaces: an
  enterprise-facing orchestration surface (a human architect plus a requirements owner
  working through an existing tracker) and a consumer-facing app-builder surface. The
  enterprise surface is the most code and the most-tested crate, but its external
  adapters run against scripted fakes and have made no live API calls yet; the consumer
  surface's data-and-flow spine runs end to end while its default experience uses a
  deterministic stub reviewer and a timed build narrative. The README's Status section
  draws the proven-versus-staged line in full.

---

## 7. What this repository is

A working demonstration of a governance architecture for AI coding agents, built end
to end in Rust so the central claim can be run and checked, not just described. The
honest interior (code comments that say exactly what is mocked, what is opt-in, and
what is staged) is part of the demonstration: the audience is a technical reader who
will run the code, and the documentation is written to match what the code does at
runtime.
