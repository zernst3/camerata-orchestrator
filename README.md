# Camerata Orchestrator

> Working name. "Conductor" is a candidate, fitting the Camerata / Chorale musical
> theme: Camerata writes the rules, Chorale renders the tables, the Conductor leads
> the ensemble.

**The proven core, in one sentence:** Camerata is a deterministic, deny-before-execute
MCP gate written in Rust; a real `claude -p` agent, locked to a single gated tool, is
blocked from a forbidden write before it touches disk, in microseconds, in-process and
fail-closed. That is the claim this repo backs end to end, and it is reproducible by
running `cargo run -p camerata -- live-demo`.

Everything else here is the product **vision** built out around that core to show where
it leads. The vision is a governed multi-agent engineering platform exposed at two tiers
on one engine. What is genuinely proven versus what is staged or opt-in is stated
plainly in [Status](#status-what-runs-today-and-what-is-staged) below; this intro does
not blur the two.

- **Tier 2 — the consumer app builder.** A non-technical owner refines an app with an
  AI lead engineer, gets a working app they own on their own cloud, and a standing AI
  maintenance routine keeps it alive. The full data-and-flow spine is built and tested
  end to end; in the default experience the "lead engineer" is a deterministic stub and
  the build screen is a timed narrative, with the real governed fleet available opt-in
  (see Status). It is the demonstration artifact and the larger-TAM bet. See
  [`docs/CONSUMER_UX.md`](docs/CONSUMER_UX.md).
- **Tier 1 — enterprise governed orchestration.** A human architect and a real Product
  Owner collaborate through the tracker they already use (Jira / Azure DevOps / GitHub),
  governed agents execute, and Camerata writes provenance, gate results, PR links, and
  sign-off back onto their work items. This is the strongest **strategic** story (the
  switching cost and provider-neutrality argument an incumbent's guardrail toggle cannot
  match) and, honestly, the weakest **runtime** proof: it is the most code and the
  most-tested crate, but every adapter test runs against a scripted fake and no live
  Jira/ADO/GitHub call has been made yet (the real HTTP transport exists but is wired in
  nowhere). The moat argument lives once, in [`docs/POSITIONING.md`](docs/POSITIONING.md);
  the integration design is in [`docs/WORKTRACKER_INTEGRATION.md`](docs/WORKTRACKER_INTEGRATION.md).

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
   thinks this looks right." The architecture is the differentiator, not the rule
   count: today the gate enforces four rules (one a path-substring guard, three regex
   heuristics; no AST analysis yet), and the broader corpus is catalogued and selected
   but not yet given executable enforcement arms. The point this repo proves is the
   *seam* (deny-before-execute, provider-neutral, fail-closed); deepening the rule set
   behind it is incremental, not architectural.
3. **A standing maintenance/ops agent.** A published app is alive: upgrades, security
   patches, key rotation, all run through the same governed loop, with calm
   plain-language recommendations. The owner gets the maintenance a real team would
   give, without hiring one.
4. **A shared, consented design corpus.** With opt-in (and opt-out-is-deletion),
   abstracted designs and bug fixes from prior apps make future builds faster, more
   consistent, and easier to maintain. A network effect a lone owner could never have.

## Status: what runs today, and what is staged

This is a compiling, tested, all-Rust workspace, not a design folder. The line between
what is verified at runtime and what is built-but-not-yet-live is drawn deliberately,
because the intended reader is exactly the person who will run the code and check.

**Verified at runtime (you can reproduce it):**

- A 14-crate workspace, 500+ passing tests, zero warnings, no
  `todo!`/`unimplemented!` stubs, governing its OWN source in CI (unsafe forbidden,
  clippy `-D warnings`, fmt, tests; see [`docs/ENFORCEMENT.md`](docs/ENFORCEMENT.md)).
- **The gate denies a real agent.** `camerata -- live-demo` runs a real `claude -p`
  subprocess locked to a single gated tool and shows the forbidden write blocked before
  it hits disk. Provider-neutrality is shown with a second, non-Claude driver
  ([`docs/PROVIDER_NEUTRALITY.md`](docs/PROVIDER_NEUTRALITY.md)). Caveat, stated so you
  are not surprised: the committed proof in
  [`docs/LIVE_RUN_VERIFICATION.md`](docs/LIVE_RUN_VERIFICATION.md) /
  [`docs/RUST_CORE_VERIFICATION.md`](docs/RUST_CORE_VERIFICATION.md) is a captured
  transcript, and `cargo test` exercises the gate against a fake in-process
  `EchoDriver`, not a live model. The live denial is reproducible by running the
  `live-demo` binary; it is not re-run by the test suite (a live agent in CI spends
  tokens on every push).
- **The Tier-2 data-and-flow spine, end to end:** typed intake + style kit, the
  refinement session, versioned persistence with full revision history (durable
  on-disk across launches), the shared corpus, and the post-build bug loop, composed
  into a runnable Dioxus desktop app.

**Built and tested, but not yet wired to anything live:**

- **The default Tier-2 experience is deterministic, not model-driven.** The "AI lead
  engineer" in the default flow is a deterministic `StubRefinementReviewer` (it asks
  smart, form-derived questions, but calls no model), and the build screen is a timed
  narrative. The REAL governed fleet (gateway + `claude -p` agents, the same path the
  `po-demo` exercises) is opt-in behind `CAMERATA_LIVE_BUILD=1`, because a live build
  spends tokens. Publish runs through a deploy seam whose Azure path is a plan, not a
  live `az` execution.
- **Tier 1 is the most code and the most-tested crate, and has made zero live API
  calls.** The `WorkItemProvider` port, the Jira / Azure DevOps / GitHub adapters, the
  async clarify-bridge, and SyncPolicy per-field source-of-truth + echo suppression all
  exist with an end-to-end flow test, but every adapter test runs against a scripted
  fake transport. The real `ReqwestTransport` compiles but is instantiated nowhere; no
  real board has been touched yet.

**Still ahead:** live execution wiring for the worktracker adapters (OAuth / webhooks),
the Azure deploy adapter's live execution (BYO-infra credentials), deepening the gate's
rule set (more enforcement arms, AST-level checks), and closing the tracked
unwrap-cleanup frontier into the blocking lint bar.

## Try it (runnable demos)

These run end to end on the in-process providers and stubs, no network or credentials
needed, and narrate what they exercise:

```
cargo run -p camerata -- live-demo          # the gate denies a real claude -p agent's forbidden write
cargo run -p camerata -- po-demo            # a PO form -> lead engineer -> governed fleet -> cargo build/test
cargo run -p camerata -- worktracker-demo   # Tier 1: ingest a story, the PO answers from their board, status written back
cargo run -p camerata -- maintenance-demo   # Tier 2: the standing ops agent (security recommendation, approval gate, rotation)
cargo run -p camerata -- deploy-demo        # Tier 2: the draft->publish gate, a local deploy, and the Azure az-CLI plan
cargo run -p camerata-ui                    # the Dioxus consumer app (desktop)
```

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
