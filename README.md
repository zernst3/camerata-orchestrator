# Camerata Orchestrator

> Working name. "Conductor" is a candidate (it fits the Camerata / Chorale musical theme: Camerata writes the rules, Chorale renders the tables, the Conductor leads the ensemble).

**A governed multi-agent development environment.** The human stays at Product-Owner / Principal-Architect altitude. The tool acts as the staff engineer that investigates, clarifies, plans, leads a team of role-scoped agents, and mechanically enforces the rules. Thesis: execution is cheap, judgment is not, so the tool elevates the human to the judgment and mechanizes the rest.

One-liner: **CI/CD for a fleet of coding agents, led by an AI staff engineer, steered by you.**

## What makes it different

Spawning parallel agents is already commodity. The differentiator is three things, none of which is "run many agents":

1. **Front-loaded judgment.** Investigation first. A product-clarification panel and a technical-tradeoff panel are produced before any code. The human answers and decides; the agents then execute.
2. **Governance baked in.** Mechanical rule enforcement across every agent. Not rules in a prompt (examples are not enforcement), but an actual gate that output must pass.
3. **Intelligent rule selection.** A large rule corpus (the [Camerata](../camerata-ai) rules, 100+) is curated per task: the system recommends the relevant subset for this feature instead of dumping all rules into context.

## Status

Design complete, implementation starting. This repo currently holds the design set in [`docs/`](docs/); code lands next, engine-first.

## Design documents

Read in this order:

1. [docs/VISION.md](docs/VISION.md) — the product, the workflow, the collaboration architecture, the data model.
2. [docs/TECH_DESIGN.md](docs/TECH_DESIGN.md) — the verified engine architecture, six resolved design questions, the module layout, and an honest risk register.
3. [docs/PHASE0_TASKS.md](docs/PHASE0_TASKS.md) — the engine build plan (T0 through T14) and the acceptance criteria.
4. [docs/UI_DESIGN.md](docs/UI_DESIGN.md) — the cockpit and dashboard, the cross-stack decision, the chorale dogfood.
5. [docs/UI_TASKS.md](docs/UI_TASKS.md) — the V1 UI build plan (T15+), additive to PHASE0_TASKS.
6. [docs/WORKTRACKER_INTEGRATION.md](docs/WORKTRACKER_INTEGRATION.md) — the work-tracker integration design (the async collaboration bridge).

## Architecture in one breath

- **Orchestration + governance layer:** deterministic TypeScript / Node that makes ZERO model calls itself (routing, rule selection, gates, status, coordination, provenance).
- **Agent layer:** Claude Agent SDK sessions, one per role, scoped by system prompt, allowed tools, path boundaries, and rule subset. Provider / tier / model agnostic behind a single auth-and-model seam.
- **Two-layer governance gate:** a real-time tool-call gate (deny before execution) plus a post-task structural check that bounces the specific violated rule back to the agent for revision.
- **UI:** a single cockpit the human steers from (intake, investigation panels, the clarify loop, the plan, live status, QA with provenance) plus a status dashboard. Built in [Dioxus](https://dioxuslabs.com); dashboard tables dogfood [Chorale](../rust-chorale).

## Build order

Engine first, full-V1 scope. The governance engine is proven behind a minimal UI before effort goes into a polished cockpit, so nothing is built on an unproven engine. Collaboration runs with no shared cloud: the Architect is the local node, and the external work tracker is the asynchronous bridge to a remote Product Owner.

## Family

- [camerata-ai](../camerata-ai) — the rule corpus and conventions engine.
- [rust-chorale](../rust-chorale) — the headless, virtualized Dioxus / Leptos table library used for the dashboard.
- this repo — the conductor that leads the ensemble.
