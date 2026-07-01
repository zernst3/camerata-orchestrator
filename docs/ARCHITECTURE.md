# ARCHITECTURE.md

The Camerata Orchestrator stack, top to bottom and sideways. This reflects the
**verified all-Rust core** (see `RUST_CORE_VERIFICATION.md`). It supersedes the
TypeScript-core / Rust-BFF shape in the earlier `TECH_DESIGN.md` and `UI_DESIGN.md`
diagrams; the design *reasoning* in those docs still holds, only the language
boundary moved.

> **Update (post-#116/#117):** the two headless-core extractions refined the "Orchestrator Core"
> below into three *framework-agnostic core crates* plus thin adapters. The layered chart in the next
> section is the current, canonical view; the ASCII vertical stack further down remains accurate as
> the *runtime / agent-execution* view.

---

## The layer separation (post-#116/#117)

**Thesis:** Camerata is a **framework-agnostic governance + orchestration core** wrapped by **thin
adapters**. The core is the single source of truth for *how everything works*; every interface
(visual, CLI, and — planned — chat and voice) is just an adapter over it. **State lives on the
adapter, not in the core.**

- **#116** — the UI headless core (`camerata-ui-core`): pure UI logic/state, no rendering framework.
- **#117** — the backend headless core (`camerata-app-core`): pure app-orchestration domain types +
  state transitions, no transport framework.

Both are enforced by `RUST-HEADLESS-CORE-1` + `RUST-PURE-STATE-TRANSITIONS-1` (see `../CONVENTIONS.md`).

```mermaid
flowchart TB
    subgraph SURFACES["① Surfaces — how humans and agents drive Camerata"]
        direction LR
        UI["Cockpit UI<br/>(Dioxus desktop)"]
        CLI["CLI"]
        CHAT["Chat adapter<br/><i>(planned)</i>"]
        VOICE["Voice adapter<br/><i>(planned)</i>"]
    end

    subgraph CONTRACT["② Capability surface — the governed verbs (one contract, many bindings)"]
        direction LR
        HTTP["HTTP / WS<br/><b>today</b>"]
        MCPOUT["MCP tools<br/><i>(planned — lets any LLM agent become an adapter)</i>"]
    end

    subgraph ADAPTERS["③ Adapters — own STATE + transport/render (thin shells)"]
        direction LR
        SERVER["<b>camerata-server</b><br/>Axum HTTP/WS · owns the stores · drives core transitions"]
        UIADAPT["<b>camerata-ui</b><br/>Dioxus render adapter"]
        STATE[("State / stores<br/>Project · Uow · Run · Escalation<br/>Routine · Checkpoint · …<br/>in-memory + camerata-persistence")]
    end

    subgraph CORES["④ Framework-agnostic CORES — no transport, no renderer · SOURCE OF TRUTH"]
        direction LR
        APPCORE["<b>camerata-app-core</b> (#117)<br/>domain types + PURE state transitions:<br/>uow lifecycle · escalation · run<br/>routine · schedule · checkpoint · project"]
        UICORE["<b>camerata-ui-core</b> (#116)<br/>UI logic + state:<br/>triage model · rules view-models<br/>run/scan derivations"]
        CORE["<b>camerata-core</b><br/>orchestrator brain<br/>(roles, task DAG, coordination —<br/>zero model calls)"]
        SUPPORT["supporting pure crates:<br/>rules · checks · fleet · intake<br/>worktracker · persistence · liveness<br/>deploy · maintenance · linter-registry"]
    end

    subgraph EXEC["⑤ Governed execution — the 'doing', on a leash the core holds"]
        direction LR
        GATEWAY["<b>camerata-gateway</b><br/>layer-1 MCP gate (allow / deny tool calls)"]
        AGENT["<b>camerata-agent</b><br/>runs claude -p · parses stream-json"]
    end

    UI --> UIADAPT
    UI --> HTTP
    CLI --> HTTP
    CHAT -.-> MCPOUT
    VOICE -.-> CHAT
    HTTP --> SERVER
    MCPOUT -.-> SERVER

    UIADAPT --> UICORE
    SERVER --> STATE
    SERVER --> APPCORE
    SERVER --> CORE
    SERVER --> SUPPORT
    SERVER --> AGENT
    AGENT --> GATEWAY
    GATEWAY --> CORE

    classDef planned stroke-dasharray:5 5,fill:#f5f5f5,color:#555;
    class CHAT,VOICE,MCPOUT planned;
    classDef core fill:#e8f0fe,stroke:#4285f4;
    class APPCORE,UICORE,CORE,SUPPORT core;
```

Solid boxes exist today; dashed boxes are the planned multi-adapter future.

### Four rules that hold the whole thing together

1. **The cores are the source of truth.** `camerata-core`, `camerata-app-core`, and
   `camerata-ui-core` own *how everything works*: the rules, the lifecycle state machine (`UowStage`),
   and the decisions (is this run cancellable, is this escalation blocked, is this tool call allowed).
   They are **stateless** — state goes in, the next state comes out.
2. **The cores never import a transport or a renderer.** `camerata-app-core` has no `axum`;
   `camerata-ui-core` has no `dioxus`. The compiler enforces this by crate boundary, which is why the
   logic is unit-testable with no HTTP server and no VirtualDom. This is `RUST-HEADLESS-CORE-1`.
3. **State lives on the adapter, not in the core.** The stores (`ProjectStore`, `UowStore`,
   `RunStore`, …) live in `camerata-server`. The adapter *owns* state and asks the stateless core how
   it should change, then persists the result. This is `RUST-PURE-STATE-TRANSITIONS-1`.
4. **The core governs execution; it is not the executor.** `camerata-agent` does the actual work
   (drives the LLM, executes tool calls); `camerata-gateway` is the layer-1 MCP gate that allows/denies
   each call against the core's rules. The core decides and governs; the agent acts, on a leash.

### Why this shape (and what it unlocks)

Adding an interface should mean **writing an adapter, not re-architecting**. Because the cores are
framework-agnostic and state lives on the adapter, a new surface is a thin shell that owns/holds state
and drives the **capability surface** (Camerata's governed verbs: create project, start run, answer
escalation, materialize a design):

- **Cockpit UI** — a visual adapter that renders `camerata-ui-core` state.
- **CLI** — a text adapter over the same capability surface.
- **Chat adapter** *(planned)* — an LLM agent whose tools *are* Camerata's capability surface.
- **Voice adapter** *(planned)* — the chat adapter + speech-to-text / text-to-speech.
- **Voice + cockpit together** *(planned)* — the voice agent and the UI driving **one shared state
  model**, which is exactly what the #116 UI state-lift makes possible.

Today the capability surface is HTTP endpoints on the server adapter. Exposing that same surface as
**MCP tools** turns "add a chat/voice interface" into "point an LLM agent at the existing governed
verbs" — no bespoke integration. That is the natural next architectural unit after #116/#117.

### Crate map

| Layer | Crate | Role |
|---|---|---|
| Core | `camerata-core` | Orchestrator brain: roles, task DAG, coordination (zero model calls) |
| Core | `camerata-app-core` | **(#117)** Backend domain types + pure state transitions |
| Core | `camerata-ui-core` | **(#116)** Framework-agnostic UI logic + state |
| Core (support) | `camerata-rules` | Rule corpus loader, enforcement kinds, rule-subset selection |
| Core (support) | `camerata-checks` | Layer-2 post-task gate logic (CheckRunner) |
| Core (support) | `camerata-fleet` | Reusable governed-fleet build logic (CLI + UI) |
| Core (support) | `camerata-intake` | PO-mode intake schema + LeadEngineer |
| Core (support) | `camerata-worktracker` | WorkItemProvider port + canonical shapes |
| Core (support) | `camerata-persistence` | SQLite state + JSON provenance |
| Core (support) | `camerata-liveness` | LivenessTracker + heartbeat/idle probe |
| Core (support) | `camerata-deploy` | Tier-2 BYO-infra publish (DeployTarget seam) |
| Core (support) | `camerata-maintenance` | Tier-2 standing post-publish ops agent |
| Core (support) | `camerata-linter-registry` | Citation validator (canonical linter rule-id lists) |
| Execution | `camerata-gateway` | Layer-1 real-time MCP governance gate (allow/deny) |
| Execution | `camerata-agent` | Agent runtime: drives `claude -p`, parses stream-json |
| Adapter | `camerata-server` | Axum HTTP/WS adapter; **owns the stores** |
| Adapter | `camerata-ui` | Dioxus cockpit (thin render adapter) |
| Adapter | `camerata-cli` (`camerata`) | Binary entrypoint wiring it together |

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
