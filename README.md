# Camerata Orchestrator

I set out to build Camerata because managing multiple AI agents across multiple projects through conversational chat alone became unwieldy, and frankly cognitively tiring. The governance itself worked: my rules held and the agent's code quality stayed high. But routines would fail silently, and I often wouldn't know a run had failed until I went and asked the agent about it. The missing piece was never the governance, it is already baked into my process. What was missing was a structured place for it to live: status and state visible and managed in a real interface, instead of buried in chat. That is what Camerata is: a structured management layer for orchestrating AI agents across any number of projects, while enforcing rules and deterministic gates to maintain code quality.

Camerata is an all-Rust, governed multi-agent engineering platform. This README leads
with what runs and is defensible today, and is explicit about what is built but staged.
The intended reader is the person who will clone the repo and check, so the line between
"proven" and "built but not yet live" is drawn deliberately, not blurred.

The fastest way to see the real, working part: `cargo run -p camerata-ui` (the Enterprise
Cockpit, a Dioxus desktop app), then open the **Onboard repos** view.

```text
 CAMERATA — WHAT'S REAL vs STAGED
 ════════════════════════════════════════════════════════════════════════════

 ✅ REAL — runs today, you can reproduce it
    ▸ GOVERNANCE GATE — deny-before-execute blocks a real `claude -p` agent's
      forbidden write before it touches disk. Fail-closed, provider-neutral,
      binary pass/fail (not an LLM grading an LLM).      ← the defensible wedge
      Runs in the CLI, not yet in the UI. The video shows the onboarding
      pipeline; the gate is verified separately at the command line:
      cargo run -p camerata -- live-demo
    ▸ BROWNFIELD ONBOARDING, end-to-end on fixtures — detect → two-tier audit
      → calibrate/dedup → triage → baseline waivers + GitHub issues → apply.
    ▸ 14-crate Rust workspace · 670+ tests · governs its OWN source in CI.

 ⏳ STAGED — built & tested, NOT yet live (please don't grade these as proven)
    ▸ App-builder "AI lead engineer" is a deterministic stub by default
      (real governed fleet opt-in: CAMERATA_LIVE_BUILD=1); Azure deploy = plan.
    ▸ Architect / board adapters tested against fake transports — 0 live calls.
    ▸ The gate inside a full live DEVELOPMENT CYCLE — next milestone, not done.

 → Grade the wedge: the GATE is the proven core. Everything staged is labeled
   honestly in the sections below — nothing here is blurred.
```

## What works today: brownfield onboarding, end to end

Point Camerata at one or more existing GitHub repositories and it runs the full
onboarding flow, in-app, front to back:

- **Per-repo stack detection** (languages and frameworks) and a **per-repo proposed
  ruleset** drawn from a 107-rule corpus. Each repo is scanned against its own selected
  rules plus an always-on deterministic security floor.
- **A two-tier audit:** a deterministic mechanical scan (hardcoded secrets, raw-SQL
  concatenation, path escapes) plus an AI architectural audit (missing auth on a write
  path, a service bypassing the repository layer, N+1, cross-boundary imports, and the
  like), with severity/authority calibration, cross-rule dedup, and snippet-to-line
  resolution. Mechanical (CI/runtime-context) rules are deliberately excluded from the
  code scan and routed to CI, where they can actually be checked.
- **Three triage tables** (Unresolved / Ignored / Tech debt) with free re-bucketing,
  then a **Process** step that turns each disposition into a durable artifact: ignores
  become reasoned baseline waivers, and every tech-debt item is filed as a real
  **GitHub issue** (the story).
- **Apply:** the chosen governance files (`AGENTS.md`, `CONVENTIONS.md`, a CI workflow
  for the mechanical rules, and `.camerata/baseline.json`) are written to a governance
  branch and pushed, with the PR opened as a separate, deliberate step.

This is the part of the system that is exercised end to end. The honest boundary: it has
been run on **small fixture repositories, not yet on a large real-world codebase**, so the
AI audit's precision and recall at scale are not yet proven, and the actual *development*
that fixes a finding belongs to a later phase (see the gate section). What is solid today
is the onboarding pipeline itself: scan, calibrate, triage, and emit stories.

Onboarding's design principle is that it **emits stories and never does the development
work itself**: a "resolve now" finding and up to two CI-wiring tasks (one for mechanical
rules mapping to off-the-shelf linters, one for architectural rules requiring custom
checkers) each become a GitHub issue that the development layer would pick up. Walked screen by screen in
[`docs/USER_GUIDE.md`](docs/USER_GUIDE.md); the under-the-hood mechanics are in
[`docs/TECHNICAL.md`](docs/TECHNICAL.md).

### Report everything, enforce the delta

The audit shows **every** existing violation, but onboarding does **not** freeze your team
on day one: arming snapshots the current violations into a committed
`.camerata/baseline.json` as accepted pre-existing debt, and the gate then enforces only on
**new or changed** code (the eslint/ruff/sonar baseline model). The match is by rule id plus
a content fingerprint, so touching a baselined line un-baselines it: fix it or waive it.

Suppressing a rule has two homes, by intent:

- **Per-line, surgical waiver, an inline comment** co-located with the code:

  ```rust
  let key = SANDBOX_PUBLIC_KEY; // camerata:allow SEC-NO-HARDCODED-SECRETS-1 -- public sandbox value, JIRA-123
  ```

  It shows up in the PR diff, so silencing a rule is reviewable, and `git blame` records
  who and when for free.
- **Bulk / legacy / policy, the central `.camerata/baseline.json`** (written for you at
  onboarding).

Three rules hold regardless of where a suppression lives: a **reason is mandatory** (a
reason-less waiver suppresses nothing and is itself flagged), **everything is indexed
centrally** (one auditable registry, not a grep), and **stale waivers are surfaced** (a
waiver whose violation no longer exists is flagged for removal). A waiver can carry its
tracked ticket id, so "ignore" and "open a debt story" are one act.

## The governance gate: a proven seam, narrow by design

The technical wedge is **deterministic governance, not an AI verifier**: a real-time,
deny-before-execute MCP tool-gateway that blocks a forbidden agent write before a byte hits
disk, fail-closed and provider-neutral. Binary pass/fail, not "the model thinks this looks
right." That is a more defensible claim than letting an LLM grade another LLM.

**What is proven today:** `cargo run -p camerata -- live-demo` runs a real `claude -p`
agent locked to a single gated tool and shows the forbidden write denied before it reaches
disk, in-process and fail-closed; a second, non-Claude driver shows the seam is
provider-neutral ([`docs/PROVIDER_NEUTRALITY.md`](docs/PROVIDER_NEUTRALITY.md)).

**The honest boundary:** this is proven as a **standalone denial via that CLI demo**. The
gate has **not** yet been exercised inside a full governed *development cycle*, the phase
where an agent does real multi-step work, layer-2 structural checks bounce failures back,
and the loop iterates to a result. That development engine is built but not yet validated
live. So today's claim is narrow and specific: the seam works, deny-before-execute is real,
and deepening the enforced rule set behind it is incremental rather than architectural. The
gate currently enforces a small high-strictness security tier (a `..`/`.git`/`.ssh` path
guard, a secret-file guard, and content heuristics for secrets / raw-SQL-concat /
secrets-in-URLs; no AST yet). Proving it inside a live dev cycle is the next milestone, not
a finished claim.

## Where this leads (built around the core, not yet the showcase)

These surfaces are built to varying depth and are staged honestly in
[Status](#status-what-runs-today-and-what-is-staged). They show where the architecture
points; none is claimed as proven end to end.

- **App-builder surface.** A non-technical owner refines an app with an AI lead engineer
  through a clarification-first intake (a Product Owner being interviewed before any code
  is written). The data-and-flow spine is built and tested; in the default flow the lead
  engineer is a deterministic stub and the build screen is a timed narrative, with the real
  governed fleet opt-in behind `CAMERATA_LIVE_BUILD=1`. See
  [`docs/CONSUMER_UX.md`](docs/CONSUMER_UX.md).
- **Architect / board surface.** Governed agents collaborate with a requirements owner
  through the tracker they already use (Jira / Azure DevOps / GitHub) and write provenance,
  gate results, PR links, and sign-off back to the work item. It is the most code and the
  most-tested crate, but every adapter test runs against a scripted fake transport: no live
  board call has been made yet. See
  [`docs/WORKTRACKER_INTEGRATION.md`](docs/WORKTRACKER_INTEGRATION.md).
- **Standing maintenance agent and a consented design corpus.** Where a published app stays
  alive (upgrades, security patches, key rotation, through the same governed loop), and
  prior builds make future ones faster. Designed and partially built.

## Status: what runs today, and what is staged

A compiling, tested, all-Rust workspace, not a design folder.

**Verified at runtime (you can reproduce it):**

- A 14-crate workspace, 670+ passing tests, zero warnings, no `todo!`/`unimplemented!`
  stubs, governing its OWN source in CI (`unsafe` forbidden, clippy `-D warnings`, fmt,
  tests; see [`docs/ENFORCEMENT.md`](docs/ENFORCEMENT.md)).
- **Brownfield onboarding, end to end** on fixture repos: per-repo detection and rule
  proposal, the two-tier audit with calibration and dedup, the three triage tables with
  re-bucketing, Process emitting baseline waivers and GitHub issues, and Apply writing the
  governance branch. Validated on small fixtures, not yet a large real codebase.
- **The gate denies a real agent (standalone).** `camerata -- live-demo` runs a real
  `claude -p` subprocess locked to a single gated tool and shows the forbidden write
  blocked before it hits disk. The committed proof in
  [`docs/LIVE_RUN_VERIFICATION.md`](docs/LIVE_RUN_VERIFICATION.md) is a captured transcript,
  and `cargo test` exercises the gate's verdict function against synthetic calls and an
  in-process fake driver, not a live model (a live agent in CI spends tokens on every push).
  The seam is proven; it is not yet proven inside a live development cycle.
- **The Tier-2 data-and-flow spine:** typed intake plus style kit, the refinement session,
  versioned persistence with full revision history (durable across launches), the shared
  corpus, and the post-build bug loop, composed into a runnable Dioxus desktop app.

**Built and tested, but not yet wired to anything live:**

- **The default app-builder experience is deterministic, not model-driven.** The "AI lead
  engineer" in the default flow is a deterministic stub (smart, form-derived questions, no
  model call) and the build screen is a timed narrative. The real governed fleet is opt-in
  behind `CAMERATA_LIVE_BUILD=1`. Publish runs through a deploy seam whose Azure path is a
  plan, not a live `az` execution.
- **The architect surface has made zero live API calls.** The `WorkItemProvider` port, the
  Jira / Azure DevOps / GitHub adapters, the async clarify-bridge, and the per-field
  source-of-truth plus echo-suppression sync policy all exist with an end-to-end flow test,
  but every adapter test runs against a scripted fake; only a startup rate-limit probe has
  touched the real HTTP transport.

**Still ahead:** proving the gate inside a full live development cycle; live execution wiring
for the worktracker adapters (OAuth sign-in, webhooks); the Azure deploy adapter's live
execution; the dev-engine ingest of "resolve now" stories; and deepening the gate's rule set
(more enforcement arms, AST-level checks).

## Try it (runnable demos)

These run end to end on in-process providers and stubs, no network or credentials needed,
and narrate what they exercise:

```
cargo run -p camerata-ui                    # ► START HERE: the Enterprise Cockpit; open "Onboard repos"
cargo run -p camerata -- live-demo          # the gate denies a real claude -p agent's forbidden write
cargo run -p camerata -- po-demo            # a PO form -> lead engineer -> governed fleet -> cargo build/test
cargo run -p camerata -- worktracker-demo   # architect surface (against a fake board transport)
cargo run -p camerata -- maintenance-demo   # the standing ops agent (recommendation, approval gate, rotation)
cargo run -p camerata -- deploy-demo        # the draft->publish gate, a local deploy, and the Azure az-CLI plan
```

## Read in this order

1. [`docs/USER_GUIDE.md`](docs/USER_GUIDE.md): the onboarding flow and the features, how to use them.
2. [`docs/TECHNICAL.md`](docs/TECHNICAL.md): how it works under the hood (the gate, the scan pipeline, persistence).
3. [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md): the all-Rust stack, top to bottom.
4. [`docs/decisions/`](docs/decisions/): the design-decision records.
5. [`docs/VISION.md`](docs/VISION.md): the north star and where the architecture leads.

## Architecture in one breath

- **Everything load-bearing is Rust** (no TypeScript core; that early design was abandoned
  on evidence). One Tokio process is the server, the brain, and the gate.
- **Orchestrator core:** deterministic Rust that makes ZERO model calls (intake, rule
  selection, planning, worktrees, coordination, provenance).
- **Governance gateway:** a Rust MCP server. Every agent tool call routes through it; it
  allows or denies before anything executes (deny-before-execute).
- **Agent layer:** short-lived `claude -p` subprocesses, one per role, scoped by prompt,
  allowed tools, path boundaries, and rule subset. Provider/model agnostic behind a seam.
- **Persistence:** a versioned store (SQLite now, Postgres later behind the same trait seam)
  so every user/AI edit is saved with full history.
- **UI:** a Dioxus app; tabular surfaces dogfood [Chorale](../rust-chorale).

## How an AI agent fits behind the gate

The orchestrator makes zero model calls; it prepares a session and spawns a `claude -p`
agent behind the `AgentDriver` seam. The agent's built-in write tools are disallowed, so its
only way to write is the gateway's MCP tool, which denies or allows each write before it
executes (layer 1). Allowed writes land in an isolated worktree; layer-2 checks bounce
failures back. The agent uses your own local Claude login (Camerata holds no model
credentials), and the gate is model-agnostic.

![Camerata architecture: how an AI agent fits behind the governance gate](docs/architecture-agent-gate.svg)

## Family

- [camerata-ai](../camerata-ai): the conventions engine the corpus format originates from.
  The corpus is vendored into this repo at `crates/rules/principles/` (107 TOML rules), so
  the workspace is self-contained; override with `CAMERATA_CORPUS_PATH`.
- [rust-chorale](../rust-chorale): the headless, virtualized Dioxus / Leptos table library
  used for tabular surfaces.
- this repo: the conductor that leads the ensemble.

## License

Source-available under the [PolyForm Noncommercial License 1.0.0](LICENSE). The code is
fully readable, and noncommercial use (study, evaluation, personal and research projects) is
permitted. Commercial and competing use is reserved by the copyright holder, Zachary Ernst.
This is a deliberate choice over a permissive license: a license can be loosened later but
never tightened, so it starts reserved.
