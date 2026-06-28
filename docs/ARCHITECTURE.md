# ARCHITECTURE.md

The Camerata Orchestrator stack, top to bottom and sideways. This reflects the
**verified all-Rust core** (see `RUST_CORE_VERIFICATION.md`). It supersedes the
TypeScript-core / Rust-BFF shape in the earlier `TECH_DESIGN.md` and `UI_DESIGN.md`
diagrams; the design *reasoning* in those docs still holds, only the language
boundary moved.

---

## Glossary (read this first)

| Term | What it is | Familiar analogy |
|---|---|---|
| **LLM** | Claude itself, the raw model | a contractor you pay per message |
| **Agent** | an LLM given a job, tools, and boundaries, running in a loop | an LLM wrapped in a loop that can call functions |
| **`claude -p` / the CLI** | Claude Code run headlessly; the orchestrator spawns it | `child_process.spawn`; this *is* the agent runtime |
| **Agent SDK** | Anthropic's in-process agent library (TS/Python only, no Rust) | the library we deliberately do NOT use; replaced by `ApiAgentDriver` |
| **`ApiAgentDriver`** | Camerata's own in-process agent driver; owns the MCP tool-use loop for any API-reachable provider | the in-house equivalent of the Agent SDK, written in Rust |
| **Orchestrator** | the "staff engineer" brain; deterministic Rust, **zero model calls** | a CI server / job scheduler, for agents |
| **MCP** (Model Context Protocol) | open standard for giving a model tools via a separate "tool server" | a plugin protocol; the model uses only the plugins you expose |
| **MCP tool-gateway** | *our* Rust MCP server that checks every tool call against the rules before running it | authorization middleware for agent actions |
| **Governance gate** | the checkpoint every agent action passes through (deny-before-execute) | auth middleware, but for what agents do |
| **`PreToolUse` hook** | Claude Code's older "block a tool before it runs" script | replaced by the MCP gateway (stronger) |
| **BFF** (Backend-for-Frontend) | thin server shaping core data for the UI | in the old TS design it bridged Rust-UI ↔ TS-core; all-Rust, the cross-language **boundary** disappears (the BFF survives as the embedded `camerata-server`) |
| **Axum** | Rust web framework | Express, for Rust |
| **Dioxus** | Rust UI framework (desktop/web) | React, for Rust |
| **worktree** | multiple working dirs from one git repo, each on its own branch | lets each agent work isolated without collisions |
| **rule corpus** (camerata-ai) | the library of 100+ coding conventions | the law book |
| **rule-subset** | the few rules relevant to *this* task, selected from the corpus | load the 6 rules that matter, not all 100 |
| **provenance** | the audit trail: who did what, under which rules, with what result | git-blame for agent actions |
| **task DAG** | the plan as a dependency graph (task B waits on A) | a build graph / dependency tree |
| **two-layer gate** | layer 1 = block bad tool calls live; layer 2 = structural check after the task | a pre-commit hook + a CI check |

---

## The vertical stack (top = you, bottom = the model)

```
+--------------------------------------------------------------+
|  YOU - Product Owner / Principal Architect                   |
|  Answer clarifying questions, approve plans, judge QA.       |
+------------------------------+-------------------------------+
                               | steer from one screen
+------------------------------v-------------------------------+
|  1. COCKPIT UI  -  Dioxus desktop app (Rust)                 |
|     intake | investigation panels | clarify loop | live plan |
|     | agent status | QA review with provenance               |
|     Dashboard grids dogfood rust-chorale.                    |
+------------------------------+-------------------------------+
                               | localhost HTTP + WebSocket
                               | (NO cross-language boundary: the BFF is now embedded all-Rust)
+------------------------------v-------------------------------+
|  2. ORCHESTRATOR CORE  -  Rust, makes ZERO model calls       |
|     - Intake driver        (your request -> work)            |
|     - Investigation        (product + tech tradeoff panels)  |
|     - Rule selection       (pick the rule-subset per task)   |
|     - Planner -> Task DAG  (dependency graph of work)        |
|     - Worktree manager     (isolated git workdir per agent)  |
|     - Coordinator / merge  (sequence agents, merge results)  |
|     - Persistence + provenance  (SQLite + audit logs)        |
+------------------------------+-------------------------------+
                               | spawns + supervises
+------------------------------v-------------------------------+
|  3. GOVERNANCE GATEWAY  -  Rust MCP server   [VERIFIED]      |
|     Every agent tool call routes through here. It looks up   |
|     session -> role -> rule-subset and ALLOWS or DENIES      |
|     before anything executes. Deny-before-execute.          |
+------------------------------+-------------------------------+
                               | agent's ONLY tools are the gated ones
+------------------------------v-------------------------------+
|  4. AGENT LAYER  -  `claude -p` subprocesses, one per role   |
|     (Backend agent, Frontend agent, ...) each scoped by:     |
|     system prompt | allowed tools | path boundaries | rules  |
+------------------------------+-------------------------------+
                               | tool calls + responses
+------------------------------v-------------------------------+
|  5. THE LLM  -  Claude (provider/tier/model-agnostic seam)   |
+--------------------------------------------------------------+
```

The verification's structural win: layers 2 and 3 were TypeScript with a Rust BFF
bolted on to reach the UI. They are now Rust, and the cross-language BFF *boundary*
vanishes — the BFF survives as the embedded `camerata-server` (Axum), now a
same-language component rather than a cross-stack bridge.

---

## The sideways pieces (supporting systems)

```
   camerata-ai            rust-chorale           Work Tracker
   (rule corpus)          (table library)        (async bridge)
        |                      |                      |
        | feeds                | renders the          | a REMOTE Product
        | rule-selection       | dashboard grids      | Owner's tasks/replies
        v                      v                      v
   ORCHESTRATOR  --------->  COCKPIT UI  <-------  ORCHESTRATOR
                                                   (no shared cloud; the tracker
                                                    is the asynchronous handoff)

   Persistence: SQLite (state) + JSON provenance logs (audit)
   sits directly under the orchestrator.
```

- **camerata-ai** = the law book the rule-selection reads from.
- **rust-chorale** = the grid component the dashboards are built on.
- **Work tracker** = how a *remote* product owner participates with no shared
  server: the orchestrator reads/writes tasks there asynchronously (see
  `WORKTRACKER_INTEGRATION.md`).
- **Persistence/provenance** = the memory and audit trail under everything.

---

## The infrastructure as it runs (processes on the machine)

```
  Dioxus desktop process  --localhost ws-->  ONE Rust orchestrator process
                                              |  +- embeds the Axum HTTP/WS server (the UI talks to this)
                                              |  +- embeds the Rust MCP governance gateway
                                              |  +- owns the SQLite db + provenance logs
                                              |  +- spawns, per task:
                                              |       claude -p (Backend role)   -+
                                              |       claude -p (Frontend role)  -+- all tool calls
                                              |       ... in isolated worktrees  -+   route back through
                                              |                                       the gateway
                                              +------------------------------------>  (deny-before-execute)
```

Essentially **one Rust binary** (the UI is optionally a second process) that *is*
the server, the brain, and the gate, fanning out short-lived `claude -p` agents
into isolated git worktrees, with every action they take passing back through its
governance gate.

---

## The language boundary

| Piece | Language | Why |
|---|---|---|
| Cockpit UI | Rust (Dioxus) | native, dogfoods chorale |
| Orchestrator core | Rust | deterministic, zero model calls |
| Governance gateway | Rust (rmcp/MCP) | **verified**; MCP has a first-party Rust SDK |
| Persistence | Rust (sqlx/serde) | language-agnostic, Rust-native |
| Agent runtime | `claude -p` subprocess (`ClaudeCliDriver`) OR in-process `ApiAgentDriver` | two drivers behind one `AgentDriver` trait; both are Rust; the CLI subprocess is used for the subscription path, the in-process driver for any API provider |
| `ts-morph` sidecar | TypeScript | *optional*, P2+; only for TS-AST checks the linter can't express; a subprocess, not core |

Everything load-bearing is Rust. The single optional TS piece is a sidecar for
governing TypeScript *target* code, not part of the engine.

---

## Two-layer governance (how the gate actually works)

- **Layer 1 (real-time):** the Rust MCP gateway. Denies a tool call before it
  executes, per the role's rule-subset. Proven in `RUST_CORE_VERIFICATION.md`.
- **Layer 2 (post-task):** a structural check after the agent finishes (lint /
  AST / rule audit). On a violation, the specific failed rule is bounced back to
  the agent for revision. This is the "examples are not enforcement; the gate is"
  principle.

Layer 1 stops the action; layer 2 catches what slips through structurally and
forces a fix. Both are written against a provider-neutral seam so a non-Claude
model can be swapped in without rewriting the gate logic.
