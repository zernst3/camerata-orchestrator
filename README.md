# Camerata Orchestrator

> Working name. "Conductor" is a candidate, fitting the Camerata / Chorale musical
> theme: Camerata writes the rules, Chorale renders the tables, the Conductor leads
> the ensemble.

**Camerata builds and maintains a bespoke software application for a small business
that has no engineering team, from a form and a conversation, and the business owns
the result.** A non-technical owner describes what they need, refines it with an AI
lead engineer until both are confident, watches a governed agent team build it, tries
it, and publishes it live, never seeing a line of code or an error message. Then a
standing AI maintenance routine keeps it alive: dependency upgrades, security
patches, key rotation, the operational care a real team would provide.

Underneath that simple surface is the hard part, and the moat: every agent action
passes through a **deterministic governance gate** (mechanically enforced rules, not
prompt suggestions). The governance is the reason a non-technical person can trust a
generated, evolving app at all. It is the invisible engine, not the product.

## Who it is for

The small-business middle, NOT individuals making to-do apps, and NOT enterprises.
The customer is a business whose process fits no off-the-shelf vertical SaaS (a
studio that rents kilns by the hour AND sells clay by weight AND runs a membership)
and is currently held together by spreadsheets and email. A few hundred dollars a
month for an app shaped to exactly how they work, kept alive by governance instead of
staff, is the cheap option against an agency build or a developer hire. See
[`docs/POSITIONING.md`](docs/POSITIONING.md) for the full thesis, including the answer
to "why not just use an established app" and "why doesn't the platform eat this."

## Two tiers, one governed engine

Camerata is explicitly a two-tier product:

- **Tier 1, Enterprise orchestration.** A human architect AND a real Product Owner
  stay in the loop, collaborating through the work tracker they already use (Jira /
  Azure DevOps): the PO's tickets and replies flow in, the architect steers, and a
  fleet of governed role-scoped agents executes. This is the general-purpose
  governed-orchestration tool for a developer or team, and the foundation the
  consumer tier is built on. See
  [`docs/WORKTRACKER_INTEGRATION.md`](docs/WORKTRACKER_INTEGRATION.md).
- **Tier 2, Small-business app builder (the consumer headline).** A non-technical
  owner IS the Product Owner, with no developer or architect in the loop. They fill a
  structured intake form (including a shipped style kit), then work a **refinement
  session** with an AI lead engineer: an editable list of plain-language user
  stories, a climbing confidence score, proactive product suggestions, and honest
  limits. The same refinement loop runs before the build, during it (escalations),
  and after it (QA + structured bug reports). Camerata is the all-in service,
  including the standing AI maintenance routine. The whole flow is the artifact. See
  [`docs/CONSUMER_UX.md`](docs/CONSUMER_UX.md).

## What makes it different

Spawning parallel agents is commodity. The differentiators, in order of weight:

1. **A clarification-first, consumer-facing intake.** The user is a Product Owner
   being interviewed before any code is written, not an engineer editing YAML. This
   is the hero of the experience and the thing the prompt-to-app tools do not have.
2. **Deterministic governance, not an AI verifier.** A real-time MCP tool-gateway
   denies bad agent actions before they execute, and an out-of-process structural
   check bounces violations back for revision. Binary pass/fail, not "the model
   thinks this looks right." This is what makes the generated apps durable instead of
   collapsing into debt at the three-month wall.
3. **A standing maintenance/ops agent.** A published app is alive: upgrades, security
   patches, key rotation, all run through the same governed loop, with calm
   plain-language recommendations. The owner gets the maintenance a real team would
   give, without hiring one.
4. **A shared, consented design corpus.** With opt-in (and opt-out-is-deletion),
   abstracted designs and bug fixes from prior apps make future builds faster, more
   consistent, and easier to maintain. A network effect a lone owner could never have.

## Status: a working system, not a plan

This is not a design folder. It is a compiling, tested, all-Rust workspace:

- Ten crates, the full Product-Owner flow built end to end (intake, refinement, the
  reviewer, versioned persistence, the shared corpus, the post-build bug loop),
  composed into a Dioxus consumer UI.
- 260+ passing tests, zero warnings, no `todo!`/`unimplemented!` stubs.
- The governance gate is verified denying a real `claude -p` agent's tool call end to
  end (see [`docs/LIVE_RUN_VERIFICATION.md`](docs/LIVE_RUN_VERIFICATION.md) and
  [`docs/RUST_CORE_VERIFICATION.md`](docs/RUST_CORE_VERIFICATION.md)).

Still ahead: wiring the build screen to live agent generation in-app, a
bring-your-own-infra deploy adapter, and the standing maintenance agent (spec'd).

## Read in this order

1. [`docs/CONSUMER_UX.md`](docs/CONSUMER_UX.md) — the product: the consumer flow,
   screen by screen, and the lead engineer's behavior.
2. [`docs/POSITIONING.md`](docs/POSITIONING.md) — who it is for, the moat, and the
   honest caveats.
3. [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — the all-Rust stack, top to bottom.
4. [`docs/decisions/`](docs/decisions/) — the design-decision records (start with the
   [index](docs/decisions/README.md)).
5. [`docs/VISION.md`](docs/VISION.md) — the long-form north star and the two-tier
   product model (the enterprise/architect tool and the consumer PaaS endgame).

## Architecture in one breath

- **Everything load-bearing is Rust** (no TypeScript core; that early design was
  abandoned on evidence). One Tokio process is the server, the brain, and the gate.
- **Orchestrator core:** deterministic Rust that makes ZERO model calls (intake,
  rule selection, planning, worktrees, coordination, provenance).
- **Governance gateway:** a Rust MCP server. Every agent tool call routes through it;
  it allows or denies before anything executes (deny-before-execute).
- **Agent layer:** short-lived `claude -p` subprocesses, one per role, scoped by
  prompt, allowed tools, path boundaries, and rule subset. Provider/model agnostic
  behind a seam.
- **Persistence:** a versioned, event-sourced store (SQLite now, Postgres at the
  managed-cloud endgame) so every user/AI edit is saved with full history.
- **UI:** a Dioxus app; tabular surfaces dogfood [Chorale](../rust-chorale).

## Family

- [camerata-ai](../camerata-ai) — the rule corpus and conventions engine.
- [rust-chorale](../rust-chorale) — the headless, virtualized Dioxus / Leptos table
  library used for tabular surfaces.
- this repo — the conductor that leads the ensemble.
