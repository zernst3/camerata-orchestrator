# Camerata Orchestrator

> Working name. "Conductor" is a candidate, fitting the Camerata / Chorale musical
> theme: Camerata writes the rules, Chorale renders the tables, the Conductor leads
> the ensemble.

**Camerata is a governed multi-agent engineering platform: a deterministic governance
gate (mechanically enforced rules, not prompt suggestions) over a fleet of AI coding
agents.** Every agent action is allowed or denied BEFORE it executes, the result is
auditable, and the gate is provider-neutral by construction (it does not bet on one
model vendor). The gate is the core and the moat.

It is exposed at two tiers on one engine, and they play different roles:

- **Tier 2 is what is proven in code today.** The small-business app builder: a
  non-technical owner refines an app with an AI lead engineer, gets a working app they
  own on their own cloud, and a standing AI maintenance routine keeps it alive. It is
  built and tested end to end. It is the demonstration artifact and the larger-TAM
  bet. See [`docs/CONSUMER_UX.md`](docs/CONSUMER_UX.md).
- **Tier 1 is where the durable business is.** Enterprise governed orchestration: a
  human architect and a real Product Owner collaborate through the tracker they
  already use (Jira / Azure DevOps / GitHub), governed agents execute, and Camerata
  writes provenance, gate results, PR links, and sign-off back onto their work items.
  Governance here is infrastructure woven into an org's workflow, with switching cost
  and a provider-neutrality an incumbent's guardrail checkbox cannot match. This is the
  defensible wedge. The moat argument lives once, in
  [`docs/POSITIONING.md`](docs/POSITIONING.md); the integration design is in
  [`docs/WORKTRACKER_INTEGRATION.md`](docs/WORKTRACKER_INTEGRATION.md).

## Tier 2 in detail

A non-technical owner IS the Product Owner, with no developer or architect in the
loop. They fill a structured intake form (including a shipped style kit), then work a
**refinement session** with an AI lead engineer: an editable list of plain-language
user stories, a climbing confidence score, proactive product suggestions, and honest
limits. The same refinement loop runs before the build, during it (escalations), and
after it (QA + structured bug reports). Camerata is the all-in service, including the
standing AI maintenance routine. The whole flow is the artifact. See
[`docs/CONSUMER_UX.md`](docs/CONSUMER_UX.md).

The small-business target (NOT individuals making to-do apps, NOT enterprises) and
the "real competitor is the spreadsheet, not Buildium" thesis are detailed in
[`docs/POSITIONING.md`](docs/POSITIONING.md).

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

- A 14-crate workspace, 500+ passing tests, zero warnings, no
  `todo!`/`unimplemented!` stubs, governing its OWN source in CI (unsafe forbidden,
  clippy `-D warnings`, fmt, tests; see [`docs/ENFORCEMENT.md`](docs/ENFORCEMENT.md)).
- **Tier 2 (proven in code):** the full Product-Owner flow end to end (intake +
  style kit, refinement, the reviewer, versioned persistence, the shared corpus, the
  post-build bug loop), composed into a Dioxus UI, with the build screen wired to the
  real governed fleet and publish wired to a deploy seam.
- **Tier 1 (built out):** the `WorkItemProvider` port with a native provider plus
  Jira, Azure DevOps, and GitHub adapters; the async clarify-bridge; and SyncPolicy
  per-field source-of-truth + echo suppression (loop avoidance), with an end-to-end
  flow test.
- The governance gate is verified denying a real `claude -p` agent's tool call end to
  end ([`docs/LIVE_RUN_VERIFICATION.md`](docs/LIVE_RUN_VERIFICATION.md),
  [`docs/RUST_CORE_VERIFICATION.md`](docs/RUST_CORE_VERIFICATION.md)), and
  provider-neutrality is proven with a second non-Claude driver
  ([`docs/PROVIDER_NEUTRALITY.md`](docs/PROVIDER_NEUTRALITY.md)).

Still ahead: live execution wiring for the external worktracker adapters (OAuth /
webhooks), the Azure deploy adapter's live execution (BYO-infra credentials), and
closing the tracked unwrap-cleanup frontier into the blocking lint bar.

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
