# Tiered model fleet: open-model routing, per-tier pools, and domain fan-out

> **Status: DESIGN v1 — PRE-BUILD LOCKED (2026-06-25). Ready to build on "Go".** Grounded in
> `crates/gateway/src/delegate.rs`, `crates/fleet/src/lib.rs`, `crates/server/src/project.rs`.
> Companion to the UoW 3-phase redesign (#104) — this is the *fleet* that powers governed
> Development; the two build **together, in one pass** (tightly coupled — shared worktree/branch
> substrate, repo/branch scoping, contract gate, per-repo Ship).
> **Locked at the pre-build review:** build everything in one pass · fan-out via a dedicated
> `fan_out([...])` tool (R3.b) · contract = free-form prose in the story, enforced by a
> cross-repo *agent* integration gate, not a per-repo rule (R3.e/R3.g). Defaults in §5.
> **External dependency:** open-model tiers (R1/R2/R4) need an OpenRouter key to live-verify.

## 1. Motivation

Two goals, one architecture:

- **Cost.** Offload the implementation tiers (Sonnet/Haiku-equivalent work) to free/cheap
  **open-weight models** while keeping **Opus as the orchestrator**. Camerata's gate makes
  cheap models *safe* to use: their only write path is `gated_write`, bad output bounces.
- **Capability.** Let the orchestrator **fan a story's work out to domain-dedicated workers**
  — one lead-engineer orchestrator, and FE/BE "senior engineers" working in parallel.

The unifying premise: **the orchestrator is the lead engineer; the delegated workers are
senior engineers, each owning a domain.** Camerata already owns delegation in Rust (native
CLI subagents are disabled), so the workers behind each tier are swappable.

## 2. Current state (grounded in code — what exists today)

- **`TierMap`** (`crates/server/src/project.rs`): three capability bands —
  `fast` / `balanced` / `strongest` — each mapped to **one** concrete model id, per project.
  Default `fast=haiku`, `balanced=sonnet`, `strongest=opus`. A separate axis from the
  per-`StepKind` non-fleet model config.
- **`delegate`** (`crates/gateway/src/delegate.rs`): the MCP `delegate(subtask, tier)` tool.
  The **strongest tier is the orchestrator and the ONLY delegator**; `fast`/`balanced` are the
  delegation targets. Calling it spawns a child agent **in the SAME shared worktree**, wired to
  its **own** gateway exposing **`gated_write` ONLY (delegate disabled)**, runs the subtask
  **synchronously**, and returns the child's output. The raw CLI **`Task` tool is disallowed
  for every agent**; the child can't re-delegate, so **depth is inherently 1**; the child
  inherits the **same rule subset + worktree jail** as the orchestrator.
- **Gates:** Layer-1 (deny-before-execute) per worker; Layer-2 post-task bounce; **escalation
  = a child returns `INCOMPLETE:` and the orchestrator re-handles** it.
- **Provider neutrality:** the `AgentDriver` seam + a non-Claude driver already exist
  (`docs/PROVIDER_NEUTRALITY.md`).

**Key implication:** delegation today is **sequential, single-worktree, depth-1**. One child
at a time, all jailed to the same worktree (which is *why* it's synchronous — concurrent
writers to one tree would collide). True concurrent fan-out with per-worker isolation is **not
built**; it is the core new work in R3.

## 3. Requirements (everything discussed — "I want all of it to work")

### R1 — Open-model routing via a single OpenRouter key (additive, never replace)

Any tier's model id can be an open model, routed through **one** OpenRouter key
(Anthropic-Messages-compatible base URL + model slug). Opus / Sonnet / Haiku stay selectable.

- **Bare-LLM steps** (`model_for_step`: audit, calibration, authoring, decompose, escalation,
  clarification, chat): base-URL + key override + slug. Small — verify `llm.rs` exposes a
  base-URL override.
- **Agentic tiers** (the orchestrator + the delegate per-tier model ids): reuse the `claude`
  CLI via `ANTHROPIC_BASE_URL`→OpenRouter + the per-tier slug — the CLI keeps the whole
  MCP/tool/jail harness; no new driver. (Alternative: a dedicated MCP-agent driver via the
  seam.)
- **Make-or-break:** the model must **reliably emit the MCP tool-calls** (`gated_write` +
  reads). Vet per model (R4); the gate is the safety net for failures.

### R2 — Per-tier model POOLS (multi-select + load-balance)

`fast` and `balanced` bands become **sets** of interchangeable models (TierMap: a list per
band), surfaced as **multi-selects** in the tier-map UI. At delegate-spawn time the fleet picks
**one** pool member (round-robin / least-recently-used) → spreads usage across providers and
**dodges free-tier rate limits** (the real constraint on free models).

### R3 — Multi-domain, multi-repo fan-out: the orchestrator as lead engineer (THE new capability)

The orchestrator (strongest tier / Opus = **lead engineer**) decomposes a story into
**domain- and repo-scoped slices**, delegates each to a dedicated worker, runs them
**concurrently**, then **assembles per repo** and validates the whole via an **integration
gate**. The senior-engineer analogy holds, generalized to the real world: one story might
touch a **.NET backend API (repo A)**, a **React frontend (repo B)**, and **Python
microservices (repo C)** — three repos, three branches, all of which must end coherent.

This **extends** today's synchronous, single-worktree `delegate`. The pieces, with the
open design choices now **resolved**:

- **R3.a — What fans out is the orchestrator's judgment (the lead-dev intelligence).** There
  is *no fixed FE/BE taxonomy.* A slice can be backend, frontend, a microservice, a
  database-query script for a user to run, a migration, docs — anything. Deciding **what the
  units of work are, which repos they touch, and how to partition them** is exactly what a lead
  developer does, and it lives with the orchestrator: it reasons about cross-repo contracts,
  shared types, API shapes, and sequencing before delegating. *Whatever a lead dev would think
  of, the orchestrator must.*
- **R3.b — Concurrency via a dedicated `fan_out` tool (DECIDED 2026-06-25).** A new MCP tool
  **`fan_out([{repo, domain, subtask}])`** is the orchestrator's fan-out primitive: the fleet
  spawns each entry as an isolated gated worker on a `tokio` task and joins them. Chosen over
  reusing N parallel `delegate` calls for clearest provenance + the easiest surface to gate and
  test. It is a **structural gateway tool-surface addition** (sits alongside `delegate`; both
  are orchestrator-only; children get neither).
- **R3.c — Isolation by repo/path partition (RESOLVED — was open Q §5.1).** Each worker writes
  into a **disjoint scope** the orchestrator declares — naturally a **whole repo** in the
  multi-repo case, or **disjoint paths** within a repo for same-repo FE/BE. Because scopes
  don't overlap, assembly is a **conflict-free union**, not an N-way merge. (Rare same-repo
  overlap falls back to a git merge + the **gated-resolver** already built for "Update
  branch".) Chosen over per-worker sub-worktrees-with-merge because real work partitions
  cleanly by repo/domain.
- **R3.d — Assembly: Camerata is the sole committer (RESOLVED).** Agents have **no `git`** —
  Camerata commits/merges deterministically in Rust. Workers produce content via `gated_write`
  in their isolated scope; the fleet **unions each scope into the correct per-repo story
  branch**. The orchestrator's assembly role is **partition up front + reconcile semantic
  mismatches** the integration gate surfaces — never to run merges itself.
- **R3.e — Integration gate: a NEW cross-repo *semantic* check, distinct from per-repo rules
  (RESOLVED — answers open Q §5.2/§5.3).** This is **not** a Camerata mechanical rule and
  **cannot be one**: mechanical rules (Layer-1/Layer-2) bind **per repo** and structurally
  cannot see both sides of a cross-repo boundary. The integration gate is therefore a
  **separate, agent-driven, cross-repo check** that, after assembly, reads **the prose contract
  from the story (R3.g) + the assembled code across all touched repos** and verifies they
  agree — each repo builds, and the contract holds (client↔API shapes, shared types, service
  interfaces — whatever the prose specifies). Because the contract is prose and the check is an
  agent, it is **protocol-agnostic** (REST, GraphQL, gRPC, shared schema, CLI — all just prose).
  On a mismatch it **bounces to the orchestrator** to reconcile (possibly re-delegating a fix to
  one repo's worker). **This is the orchestrator's integration duty — always on, non-negotiable,
  and independent of the opt-in Layer-3 reviewer (R7).** It is *not* a per-repo mechanical rule
  (those can't span repos) and it does *not* depend on L3 being enabled; if L3 is on it adds an
  **independent** review on top. See `docs/ENFORCEMENT_MODEL.md` for the full layer model.
- **R3.f — End-state invariant: one clean branch per repo.** No matter how many repos and no
  matter the story's scope (one repo or all of them), the result is **N coherent branches —
  one per touched repo — that build and agree with each other**, each shippable via a per-repo
  Ship panel (the 3-phase doc §5.7, generalized per repo).
- **R3.g — Contract precondition: no contract, no development — *when contracts are in scope*
  (RESOLVED — the §5.2 answer).** A contract is required **only when the story's own tasks
  cross a contract boundary** — i.e. the work changes **both sides of a shared interface** (an
  API and its caller, a service and its consumer, a shared schema/type and its users). The
  trigger is the **story's scope of work, not the project's repo count.** A change confined to
  one side — a **frontend-only bug fix**, a single-repo refactor, a copy tweak — needs **no
  contract**, even in a multi-repo project. When the orchestrator *does* determine the work
  crosses a contract boundary, the **contract is a gating artifact produced during Investigation
  & Refinement** (3-phase doc §4): the orchestrator **refuses to start development and pushes
  back** if none exists in the UoW — as a lead engineer refuses to parallelize a team across an
  interface no one has agreed. The **refinement agent may author the contract itself** (so the
  human needn't enumerate every field), but it **MUST exist before development starts, or the
  work fails/blocks.**
  **Form (DECIDED 2026-06-25): the contract is free-form prose written within the story —
  "whatever the story calls for."** No formal schema, no protocol-specific document (no
  OpenAPI/GraphQL SDL requirement). The orchestrator's job is only to **determine whether a
  contract is needed and whether one exists, and push back if not**; the prose is what the
  cross-repo integration gate (R3.e) reads and checks the assembled code against. Prose +
  agent-check is what makes the whole mechanism stack-agnostic.

The gate stays **universal**: every fanned-out worker is spawned gated (`gated_write` only, no
`delegate`, no `Task`); **depth stays 1** (workers never re-delegate).

### R4 — Model-selection criteria (the data)

Pick tier-2/3 models (and pool members) by **BFCL (tool-use) ∩ SWE-bench Verified (coding)**,
then validate with your **own** gate + Layer-2 eval (`camerata gate-probe` / `eval`).
**Tool-use reliability is the gating metric** — a model that fumbles function-calls is useless
here regardless of coding score. Leaderboards: BFCL (Berkeley Function Calling), SWE-bench
Verified, τ-bench; OpenRouter's own rankings for what's actually free + hosted.

### R5 — Reliability / escalation / cost

- The **Layer-2 bounce** + the **`INCOMPLETE:` → orchestrator re-handle** path *is* the
  "cheap model failed the checks → elevate to a stronger tier" mechanism — already built.
- Free-tier **rate-limit/latency** mitigated by the R2 pools.
- The **gate is the safety net**: bad model output bounces and never corrupts the tree.
- **Data/ToS:** third-party models see your code — fine for your own dogfooding; flag before
  pointing it at a private/client repo.

### R6 — Per-story repo + branch scoping (intake-time; cross-refs the 3-phase Intake doc §3)

A project can hold **multiple repos** (FE, BE, services, …); a given story rarely touches all
of them. Scoping is both a correctness and a **token-cost** control:

- **The user selects which repos — and which branch per repo — are in scope** for the story at
  Intake. If the project has FE + BE + Services repos but the story doesn't touch Services, the
  user picks only FE + BE.
- **Per in-scope repo, the story branch is either an existing branch to work off of, or a new
  UoW-specific branch created from a chosen base** — both are first-class options at Intake.
  (New branch is the common case; working off an existing branch is fully supported.)
- **Out-of-scope repos are not mounted** into the workers' read grounding — only the selected
  repos' clones are `--add-dir`'d — so agents aren't bloated with irrelevant repo context
  (wasted tokens). This is the fleet-side honoring of the Intake selection.
- **"Pull `<branch>` into the story branch" is per selected repo.** The Intake "Update branch"
  control operates **per repo in scope**: each selected repo's story branch can pull from a
  chosen source branch (clean merge; conflicts → the gated resolver).
- The orchestrator's fan-out (R3) is **bounded to the selected repos** — it can only partition
  work across repos the user put in scope, and it produces exactly one story branch per such
  repo.

(The Intake UI for selection lives in the 3-phase redesign doc §3; this requirement is the
fleet-side contract: fan-out + grounding both honor the per-story repo/branch scope.)

### R7 — Layer-3: the opt-in agentic code-review gate

An **AI code reviewer** at the development gate that runs **with / parallel to Layer-2's
mechanical checks**, expanding gated enforcement from purely mechanical to **architectural** —
the onboarding AI architectural scan, brought down to the dev gate. Per-repo. (Full layer model:
`docs/ENFORCEMENT_MODEL.md`.)

- **Fully opt-in, model-selectable.** L3 is a **project setting: on / off**, and **which model**
  runs it is configurable in project settings. **When off, the human is the reviewer** — L3
  exists to save *your* time, at a token cost, so opting out is first-class.
- **Hard principle — sees the spec, blind to the other agents.** The reviewer sees **the story
  (requirements, contract, integrations, acceptance) + the selected rules + the diff** — so it
  verifies the code meets **both the rules and the story's intent**, exactly as a human reviewer
  reads the ticket before the diff. What it must **NOT** see is **any other agent's context** —
  the investigation agent's notes, the developing agent's reasoning, the orchestrator's chat.
  *That* isolation — from the other agents, **not** from the story — is what stops it
  rubber-stamping the implementer's own rationalizations. (Spec-grounded, implementer-blind.)
- **Scope = per-repo review against rules + story:** `story + diff(this repo) + that repo's
  selected rules` → architectural **and intent** enforcement; bounces to the orchestrator like
  Layer-2.
- **Contract is NOT L3's responsibility.** Contract existence + cross-repo coherence are the
  **orchestrator's non-negotiable integration duty (R3.e)** — always on, independent of L3. If
  L3 is enabled it adds an **independent** review on top (its rule-list may include
  contract-related expectations), but **contracts never depend on L3 being on.** This keeps
  "optional reviewer" and "mandatory contract" cleanly separate.
- **The four-layer model:** **L1** Security (deny-before-write, mechanical, pre) · **L2**
  Mechanical (per-repo lint/build, fail-closed) · **L3** Code review (this — agentic, opt-in) ·
  **L4** Origin (GitHub PR/CI). The **rules are the SSOT**: they generate L2 *and* L4 and feed
  L3, so local and remote can't drift; L4 may extend beyond L2 and may run its own reviewer
  overlapping L3 (L3 is the cheap local preview of L4). **L2 and L3 both bounce locally; L4 is
  the user's existing remote pipeline.**

## 4. Architecture delta (new vs. exists)

| Piece | Status |
|---|---|
| `TierMap` (fast/balanced/strongest), per-step models | **exists** |
| `delegate` (synchronous, depth-1, single shared worktree, `gated_write`-only children) | **exists** |
| Layer-1 / Layer-2 gates, `INCOMPLETE:` escalation, `AgentDriver` seam + non-Claude driver | **exists** |
| Open-model routing config (single OpenRouter key; base-URL+slug per tier/step) — **R1** | **new (small)** |
| Per-tier model **pools** + round-robin selection + multi-select UI — **R2** | **new (small)** |
| **Concurrent, repo/path-partitioned fan-out** + per-repo assembly (Camerata-committed) — **R3.a–d** | **new (the big one)** |
| **Cross-repo integration gate** (each repo builds + contracts agree) — **R3.e** | **new (the big one)** |
| Per-model tool-use vetting harness — **R4** | **new (small, reuses eval)** |
| **Per-story repo + branch scoping** (intake selection → grounding + fan-out bounds) — **R6** | **new (small; UI in 3-phase doc §3)** |
| **Layer-3 agentic code-review gate** (zero-context diff+rules review; per-repo arch + cross-repo contract) — **R7** | **new (medium; reuses onboarding scan concept)** |

## 5. Decisions resolved + still-open questions

**Resolved (this round, 2026-06-25):**

- **Fan-out isolation** → **repo/path partition** (disjoint scopes → conflict-free union),
  *not* per-worker sub-worktrees with N-way merges. (R3.c)
- **Assembly** → **Camerata is the sole committer**; agents have no `git`; the fleet unions
  each scope into the correct per-repo story branch; the orchestrator partitions + reconciles,
  never merges. (R3.d)
- **Integration-gate scope** → **each repo builds + cross-repo contracts agree** (client↔API
  shapes, shared types/DTOs, service interfaces); mismatch bounces to the orchestrator. (R3.e)
- **End-state invariant** → **one clean branch per touched repo**, all coherent. (R3.f)
- **Cross-repo contract representation** → an **explicit UoW contract artifact settled in
  Investigation & Refinement** (human- or refinement-agent-authored), **mandatory before
  development** for cross-boundary work (orchestrator refuses + pushes back if absent); the
  integration gate validates against it; *not* inferred from diffs. (R3.e, R3.g)
- **Repo/branch scoping** → **per-story user selection at Intake**; out-of-scope repos aren't
  grounded; "pull branch" is per-repo; each repo's branch is **existing-or-new-from-base**;
  fan-out is bounded to selected repos. (R6)

**Resolved at the 2026-06-25 pre-build lock (build everything in one pass):**

- **Fan-out trigger** → a dedicated **`fan_out([{repo,domain,subtask}])`** MCP tool (R3.b).
- **Contract form** → **free-form prose in the story**; the cross-repo integration gate is an
  **agent-driven semantic check** (NOT a per-repo mechanical rule — those can't cross repos),
  so the mechanism is protocol-agnostic (R3.e, R3.g).

**Defaulted (will build this way; flag if you want otherwise):**

- **Inter-repo sequencing** → serialize **dependent** slices, fan out **independent** ones.
- **Pool selection policy** → round-robin.
- **Per-repo model assignment** → deferred to v2 (one tier-map for all workers in v1).
- **Open-model routing mechanism** → `claude`-CLI-via-proxy (reuse the harness), not a new driver.

**Still genuinely open (does not block the build):**

- **Open-model live verification** depends on an **OpenRouter account + key** (external). The
  routing/pool plumbing builds without it; *proving* a live open model drives the gate does not.

## 6. Acceptance criteria

- Tier-map cells accept **open-model slugs via one OpenRouter key**; Opus/Sonnet/Haiku remain
  selectable; bare-LLM steps + agentic tiers both route correctly.
- `fast`/`balanced` bands are **multi-select pools**; delegations **round-robin** across members
  to spread rate-limit load.
- The orchestrator can **fan a story out to concurrent, repo/path-isolated workers** (FE/BE/
  service/db/… — its own judgment), with each repo's changes **assembled by Camerata** and the
  whole passing the **integration gate** (each repo builds + cross-repo contracts agree) **+
  per-repo Layer-2**.
- **Contract gate:** for cross-boundary work, **development cannot start without a contract
  artifact** in the UoW — the orchestrator refuses and pushes back; the refinement agent may
  author the contract; the integration gate validates the assembly against it.
- **Layer-3 agentic review (R7):** a **fully opt-in, model-selectable** per-repo reviewer runs
  with/parallel to Layer-2, seeing **the story (requirements/contract/integrations) + the
  selected rules + the diff** — and **blind to every other agent's context** — checking the code
  against **both the rules and the story's intent**, and bouncing on violations; **when off, the
  human reviews.** Contract
  enforcement is **separate and always on** (the orchestrator's duty, R3.e) — it does **not**
  depend on L3. The **four-layer model** (L1 Security · L2 Mechanical · L3 Code review · L4
  Origin) with **rules as the SSOT binding L2↔L4** is documented in `docs/ENFORCEMENT_MODEL.md`.
- **End state: one clean, coherent branch per touched repo** — for N repos, N branches that
  build and agree; each shippable via a per-repo Ship panel.
- **Per-story repo/branch scoping** works: the user selects in-scope repos+branches at Intake;
  out-of-scope repos are not grounded; "pull branch" runs per selected repo; fan-out is bounded
  to the selection.
- The **Layer-1 gate is provably intact** for every fanned-out worker (gateway jail tests pass);
  **depth stays 1**; `Task` stays disallowed.
- Model selection is documented (BFCL ∩ SWE-bench + own gate-probe eval), and a per-model
  tool-use vetting step gates adoption.
