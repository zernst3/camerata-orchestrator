# PO_MODE.md — Product-Owner mode, the second abstraction level

Camerata's governance engine has ONE job: run real coding agents against a real
worktree under a real-time gate plus post-task checks. PO mode does not change
that engine at all. It changes only WHERE the human stands and HOW the work
enters the engine. This document states the design, the pipeline, the deploy
target, and the captured live output of the `po-demo` subcommand.

The headline claim this document backs:

> A filled-out Product-Owner intake form was evaluated by a LIVE AI lead engineer
> into a build plan, and the GOVERNED fleet built that plan into a compiling,
> test-passing Rust crate through the Rust gate, end to end. No app code was
> hand-written. Captured proof is at the bottom.

---

## 1. Two abstraction levels, one engine

V1 ships two altitudes for the human on the same governance engine (VISION
section 5, the "two abstraction levels" subsection). They differ only in where
the human stands and who owns the infrastructure.

1. **Architect mode (the user is the principal architect).** The cockpit: the
   user steers the investigation, answers clarifying questions, approves the
   plan, and QAs the governed diff. This is the enterprise tool. The human is
   inside the engineering loop.

2. **Product-Owner mode (the user fills out a form).** The user is a Product
   Owner, not an architect. A structured intake form captures story-level
   requirements (what the app is, what it tracks, what screens it has). On
   submit, an AI evaluates the project AS the lead engineer and produces a plan
   (and, in the fuller version, converses back to clarify). The governed engine
   then builds the bespoke CRUD app. The principal-architect role is abstracted
   away for simple-enough apps.

The engine underneath is identical. PO mode is a different FRONT DOOR onto the
same governed fleet, not a different engine.

```
architect mode:  human ── investigation ── approve plan ── governed fleet ── QA diff
PO mode:         human ── intake form ──── lead engineer ── governed fleet ── (managed)
                                            (AI plays architect)
```

---

## 2. The intake → lead-engineer → governed-fleet pipeline

PO mode is implemented as three bounded contexts in `crates/intake` plus a thin
wiring subcommand (`po-demo`) in `crates/cli`.

### Stage 1 — the intake form (`crates/intake/src/form.rs`)

`IntakeForm` is the structured-requirements payload a Product Owner submits. It
is deliberately story-level and CRUD-shaped: an app name, a description, a list
of `Entity` (each with typed `Field`s), and a list of `ViewSpec` (list / detail
/ form views over those entities). It carries NO engineering decisions. The
sample form `po-demo` uses, `IntakeForm::sample_budgeting_app()`, is a tiny
budgeting app: one `Expense` entity (amount, category, spent_on, optional note)
with a single list view.

### Stage 2 — the lead engineer (`crates/intake/src/engine.rs`)

`LeadEngineer` is the seam. It takes an `IntakeForm` and returns an `Intake`:
either `Intake::Ready(Plan)` (it understood the form well enough to plan) or
`Intake::NeedsClarification(Vec<String>)` (it has questions for the PO first).
The `Intake` enum IS the clarify-loop state machine in one type.

Two implementations ship:

- **`ClaudeLeadEngineer`** — the REAL evaluation. It spawns a headless
  `claude -p --output-format json` call with the form's brief inlined, asks for a
  strict-JSON plan, and parses it (tolerating prose or a ```json fence around the
  object). Crucially, this call is UNGOVERNED and tool-less: the lead engineer
  REASONS and PLANS, it does not write to the worktree. The gate is for the
  BUILDERS, which run later. Provider and model live behind this seam (only the
  concrete type names `claude-sonnet-4-5`), the same stance as
  `camerata_core::AgentDriver`.

- **`StubLeadEngineer`** — a deterministic, no-network fallback that derives a
  plan straight from the form's shape (a backend task then a test task). It is
  what tests use, and what `po-demo` falls back to if the live call fails. The
  fallback is HONEST: `po-demo` prints exactly which lead engineer produced the
  plan, so a stub fallback is never mistaken for a live evaluation.

The output of this stage is a `Plan` (`crates/intake/src/plan.rs`): a summary
plus an ORDERED list of `PlanTask`s (role, `TaskKind`, precise description). This
is intentionally the SAME shape the governed fleet already consumes. PO mode's
only real work is turning a story-level form into this engineering plan.

### Stage 3 — the governed fleet (`crates/core/src/fleet.rs`, wired in `crates/cli/src/po_demo.rs`)

The plan's tasks are mapped one-to-one onto governed `FleetStage`s over a fresh
temp cargo library crate (the shared worktree). Each task becomes one governed
`claude -p` agent locked to the Rust gateway's `gated_write` tool, exactly as in
`build-demo`. The shared worktree is the inter-agent channel: the first agent
overwrites `src/lib.rs`; each later agent is told to READ what exists and EXTEND
it while preserving prior code. Every write passes through the in-process Rust
gate (GOV-1 path rule + SEC-NO-HARDCODED-SECRETS-1 content rule ride along in
each session's rule-subset). Finally `cargo build` + `cargo test` judge the
result. The fleet machinery is shared with `build-demo` via
`crates/cli/src/fleet_support.rs`, so the two demos run ONE governed path.

```
IntakeForm ──▶ LeadEngineer::evaluate ──▶ Intake::Ready(Plan)
                  (live claude -p,              │
                   ungoverned planning)         │  plan.tasks (ordered)
                                                ▼
                              FleetCoordinator over a temp worktree
                              stage 1: governed agent (Implementer) ─┐
                              stage 2: governed agent (Tester) ──────┤ shared src/lib.rs
                                                                     ▼
                                              cargo build + cargo test (the judge)
```

### What is honest about this

- The lead engineer plans; governed agents build; cargo judges. No app code is
  hand-written.
- The plan source (live vs stub fallback) is always reported.
- If the live call fails, or the governed build does not compile / pass, the
  summary says PARTIAL with the exact reason. A PARTIAL is an honest
  engine-quality signal, not a harness failure and never a faked PASS.

---

## 3. The deploy target: BYO-infra (V1) vs the Camerata PaaS (endgame)

The pipeline above stops at "a built, tested crate in a worktree." Turning that
into a running app for the Product Owner is the deploy step, and the deploy
target is where the two product tiers diverge (VISION section 20).

- **V1 — bring-your-own-infra (BYO-infra).** The generated app deploys to the
  Product Owner's OWN cloud account. Camerata generates and governs the app; the
  user owns the infrastructure it runs on. This is the solo-achievable prototype
  of the consumer vision: it proves the abstraction (a form becomes a deployed
  bespoke app) without Camerata having to own a cloud platform. The deploy
  ADAPTER that takes the governed worktree and pushes it to the user's cloud is
  the next major increment after the pipeline (see remaining work). `po-demo`
  today stops at the cargo gate and does not yet deploy.

- **Endgame — the Camerata-owned PaaS (VISION section 20).** A platform-as-a-
  service for non-technical people. The consumer logs into the Camerata cloud,
  fills out the intake form, and the platform provisions and manages EVERYTHING
  underneath the generated app: containers, databases, blob storage, custom
  domains, the lot. The consumer never sees a container or a connection string.
  This is the funded, company-scale magnum opus, not V1. V1 (BYO-infra) proves
  the abstraction; the PaaS productizes it by owning the full resource lifecycle.

The intake form and the AI lead engineer are the SAME in both tiers. Only the
deploy/ownership surface changes: the user's cloud (V1) vs. Camerata's cloud
(endgame).

---

## 4. Captured `po-demo` output (live, end to end)

Run with `camerata po-demo` (requires a built `camerata-gateway` binary and the
`claude` CLI on PATH). This is a REAL run: one live `claude -p` lead-engineer
call plus two live governed `claude -p` build agents through the Rust gate.

```text
== Camerata PO-MODE pipeline (intake → lead engineer → governed fleet → cargo) ==

── 1. INTAKE FORM (the Product Owner's submission) ──
App: budget-tracker
Description: A tiny personal budgeting app to record expenses and see them in a list.
Entities:
  - Expense
      amount : decimal (required)
      category : text (required)
      spent_on : date (required)
      note : text
Views:
  - Expense list

── 2. LEAD ENGINEER evaluation ──
  attempting LIVE ClaudeLeadEngineer (claude-sonnet-4-5) ...
  plan source: LIVE ClaudeLeadEngineer
  lead-engineer wall: 21.35s

Plan for: budget-tracker
Summary:  A personal budgeting CRUD application with a single Expense entity tracking amount, category, date, and optional notes. Backend implements the core domain model as Rust structs with validation for required fields. Initial scope covers data structures and basic CRUD operations, with an expense list view as the primary interface.
Tasks (2):
  1. [backend/Implementer] Define the Expense struct in a new expenses module with fields: amount (rust_decimal::Decimal), category (String), spent_on (chrono::NaiveDate), and note (Option<String>). Implement basic CRUD operations: create_expense, get_expense, list_expenses, update_expense, delete_expense. Add validation to ensure amount is positive, category is non-empty, and spent_on is not in the future.
  2. [test/Tester] Write unit tests for the Expense module covering: struct instantiation with valid data, validation of required fields (amount > 0, non-empty category, spent_on <= today), CRUD operation correctness, and edge cases (missing optional note field, boundary dates). Ensure all tests pass and cover happy path and validation failures.

── 3. GOVERNED FLEET BUILD ──
  governed tool (agents locked to this): mcp__camerata__gated_write
  shared worktree (cargo lib crate):     /var/folders/.../T/camerata-po-47257/crate
  corpus domains:                        ["rust", "agentic"]
  fleet stages (one governed agent each): 2
    stage 1: role=Implementer-1 kind=backend (domain types / API)
    stage 2: role=Tester-2 kind=test (tests over the produced code)

  Running governed fleet: 2 live `claude -p` agent(s) through the gate ...
  fleet wall: 155.60s

  ── stage 1: Implementer-1 ──
    session_id: 5355b954-61aa-4594-82cd-fb1203cb84be
    cost_usd:   0.331227
    agent said: Tool result: `ALLOWED: wrote 9076 bytes to .../crate/src/lib.rs`  Wrote a self-contained `expenses` module with `Expense`, `ExpenseError`, and `ExpenseStore` (CRUD + validation), plus a small dependency-free date parser/today-getter built on `std::time` for the future-date check. Includes unit tests covering each validation path.
  ── stage 2: Tester-2 ──
    session_id: fab9794c-8c98-41a0-9e70-d070f1a9c003
    cost_usd:   0.453291
    agent said: Tool result: `ALLOWED: wrote 19721 bytes to .../crate/src/lib.rs`.  I preserved the existing `expenses` module and its in-module tests exactly, then appended a top-level `#[cfg(test)] mod extra_tests` covering: struct instantiation/derives, default store, optional-note handling, ID auto-increment and non-reuse after delete, full CRUD happy paths and `NotFound` failures, no-op updates, validation rejection without mutation on update, NaN/±∞ amount rejection, smallest positive amount, whitespace/empty category, exhaustive bad date formats, leap day acceptance, boundary dates, and `ExpenseError` equality.

  ── produced src/lib.rs ──
    path:  /var/folders/.../T/camerata-po-47257/crate/src/lib.rs
    bytes: 19721
    wrote through gate (non-placeholder): true

── 4. VERIFY (cargo build + test on the governed-built crate) ──
  cargo build success: true
  cargo test success:  true
  --- cargo test stdout (tail) ---
  test extra_tests::update_no_op_preserves_record ... ok
  test extra_tests::update_rejects_bad_inputs_without_mutating ... ok

  test result: ok. 31 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

── PO-DEMO SUMMARY ──
  intake form:                              budget-tracker (1 entity, 1 view)
  plan source:                              LIVE ClaudeLeadEngineer
  plan tasks (governed fleet stages):       2
  all governed agents ran live:             YES
  produced code through the gate:           YES
  governed build compiled:                  YES
  governed build tests passed:              YES

PO-DEMO: PASS (a Product-Owner form was evaluated by the lead engineer into a plan, and the governed fleet built it into a compiling, passing crate through the Rust gate)
```

### Honest reading of this run

- The pipeline ran fully live: a 21s lead-engineer planning call, then two
  governed build agents (~156s wall, ~$0.78 total), then cargo. End to end PASS.
- **A real plan-vs-build tension showed up and was handled honestly.** The live
  lead engineer's plan named external crates (`rust_decimal::Decimal`,
  `chrono::NaiveDate`). The governed stage-task wrapper, however, constrains build
  agents to dependency-free Rust (the scaffolded crate has no dependencies). The
  implementer agent reconciled this itself: it built a dependency-free date
  parser on `std::time` and used `f64` for the amount, rather than pulling in the
  crates the plan suggested. The result compiled and passed 31 tests. This is the
  honest seam to tighten next: the lead engineer's plan and the builder's
  constraints are not yet contract-aligned (the plan can propose dependencies the
  builder is told not to use). Richer entity→schema codegen and a dependency
  contract between the plan and the fleet is on the remaining-work list below.
- The lead engineer's `kind` routing (backend, then test) drove the fleet stage
  order; the shared worktree carried the implementer's module into the tester's
  view, and the tester preserved it exactly while appending tests, as instructed.

---

## 5. Remaining work for PO mode (prioritized)

1. **Multi-turn clarify loop.** Consume `Intake::NeedsClarification`: surface the
   lead engineer's questions to the PO, collect answers, fold them back into the
   form, and re-evaluate until `Ready`. Today `po-demo` falls back to the stub if
   the live engineer asks for clarification rather than driving the loop. This is
   the single biggest PO-mode gap and the most PO-shaped one.
2. **BYO-infra deploy adapter.** Take the governed, tested worktree and deploy it
   to the user's own cloud (the V1 deploy target). Today the pipeline stops at
   the cargo gate. This is what makes PO mode produce a RUNNING app, not just a
   crate.
3. **Plan↔builder dependency contract + richer entity→schema codegen.** Close the
   tension surfaced in the live run: let the plan declare a dependency set the
   governed builder is allowed to use, and grow the codegen from "structs in one
   `lib.rs`" toward real schema/migrations, a repository layer, and the
   frontend/database task kinds the plan already models but the demo does not yet
   build.
4. **Real frontend + database stages.** The `Plan` already carries
   `TaskKind::Frontend` and `TaskKind::Database`; the demo exercises only
   backend + test. Wire governed stages (and per-kind path scoping + rule-subsets)
   for the other two layers so a full CRUD app (frontend + backend + database)
   is generated, not just a domain module.
5. **Layer-2 checks during the fleet, not just final cargo.** `po-demo` uses a
   no-op `CheckRunner` and lets the final cargo gate judge. Per-stage structural
   checks (the bounce-and-revise path the coordinator already supports) would
   catch violations earlier and exercise the revise loop in PO mode.
6. **Form validation + a real intake UI.** The schema is typed but unvalidated
   (e.g. a `ViewSpec.entity` that names no `Entity`). Validate the form before
   evaluation, and put a real form UI in front of it (the cockpit's PO-mode
   front door).
