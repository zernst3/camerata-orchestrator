# ADR: Provider-agnostic agent runtime

**Date:** 2026-06-27
**Status:** Accepted (target architecture)

## Context

Camerata must become **fully provider-agnostic** — any model (Anthropic, OpenRouter free
models, local, future providers) should be able to back a tier or a worker. There are two
distinct model-invocation layers, and they have different shapes:

1. **Bare-LLM calls** — story authoring, audit, decompose, clarification drafting, the **L3
   reviewer**, the **integration-gate** check. These are single request/response and already go
   through an `Llm`/`Completer` trait.
2. **Agentic workers** — the gated development / investigation / `fan_out` agents. These are
   **multi-turn tool-use loops** (read → reason → tool-call → observe → repeat) and today run via
   the `claude -p` CLI (`ClaudeCliDriver` behind the `AgentDriver` seam). The CLI provides the
   agent **loop** *and* the MCP integration.

Pointing the `claude` CLI at OpenRouter (via `ANTHROPIC_BASE_URL`) reaches other models, but it
keeps a **Claude-shaped dependency** — it is not provider-agnostic.

The key distinction: **calling an API is one request/response (easy); running an *agent* means
owning the multi-turn tool-use loop.** The CLI bundles that loop for Claude. Provider-agnosticism
requires Camerata to own it.

## Decision

1. **Bare-LLM → direct provider APIs via `Completer` impls.** Add provider-neutral `Completer`
   implementations (Anthropic, OpenRouter, …) and call the HTTP API directly — no CLI. This is
   immediately agnostic and covers a large share of model calls. **Do this first (small).**
2. **Agentic workers → a native, MCP-speaking agent driver behind the `AgentDriver` seam.**
   Camerata owns the agent loop itself: model → parse tool-call → execute via the gateway's MCP
   tool → feed the result back → repeat. It calls **any** provider's API directly.
   `ClaudeCliDriver` stays as one impl; the new **`ApiAgentDriver`** is the provider-neutral one.
   **This is the target architecture.**
3. **CLI-via-proxy is interim only.** Acceptable as a fast path to reach non-Anthropic models
   before the native driver lands — never the destination. Any CLI-proxy usage is transitional
   and labeled as such.

## What does NOT change

The driver only changes *who runs the loop*. **Layer-1 invariants are untouched:** `gated_write`
is the only write path; `fan_out`/`delegate` stay orchestrator-only + depth-1; workers stay
worktree-jailed; Camerata remains the sole git committer. The `AgentDriver` seam was designed for
exactly this swap (a non-Claude driver stub already exists in the live demo).

## Consequences

- **Provider-neutral:** any model with tool-use + an HTTP API can back a tier or a domain
  worker. Unlocks free-model tiers, local models, per-tier / per-domain model choice, and
  **request-level fallback** (primary-free → paid-backup).
- **New responsibility:** Camerata maintains the agent loop — per-provider function-call
  parsing, retries, streaming, tool-result feedback. Bounded runtime; the gate already mediates
  every tool effect, so the loop carries no authority of its own.
- **Reliability of weaker/free models is already covered:** malformed tool-calls are caught by
  the existing gate + escalate-on-failure path (L2/L3 bounce, `INCOMPLETE` → orchestrator).

## Sequencing

1. **Now (small):** `Completer`-direct OpenRouter/Anthropic for all bare-LLM calls.
2. **Target:** native `ApiAgentDriver` (owns the MCP tool-use loop) — the provider-agnostic
   destination; the `claude` CLI becomes optional.
3. **Separate concern:** the *cost-optimization policy* (prompt caching, Batch API for routines,
   free-as-primary-with-paid-fallback) is orthogonal to this runtime decision — to be banked in
   its own note once chosen.

## Update (2026-06-27): domain-aware bands (Designer/vision)

Provider-agnosticism enables **domain-aware routing**, now decided: the dev fleet's bands split
into a **logic ladder** (`Strongest`/`Balanced`/`Fast`, orchestrator = `Strongest`) plus an
optional, **project-wide**, flat **`Designer` (vision)** band. Routing is **domain first** (visual
→ Designer) **then hierarchy within logic** (by difficulty). The vision model produces an
HTML/Tailwind **IR** that a logic tier translates into Dioxus `rsx!` — the vision model never emits
Rust. Build the **routing seams** (per-band/per-domain model selection + the IR handoff), never
hard-code model→domain assignments (models churn; the routing architecture is durable). The
registry flags vision-capable models via OpenRouter `architecture.input_modalities`. Full design:
`docs/plans/2026-06-27_model-efficiency-and-provider-agnostic-plan.md` §10. Orchestrator tiering is
unchanged (escalation already routes to `Strongest`).
