# Camerata Orchestrator: Technical North Star

> Where this architecture leads, stated as engineering direction. The companion
> docs are [`RATIONALE.md`](RATIONALE.md) (why it is built this way),
> [`ARCHITECTURE.md`](ARCHITECTURE.md) (the stack), and [`ENFORCEMENT.md`](ENFORCEMENT.md)
> (the gate in detail). Where this doc and `TECH_DESIGN.md` disagree, the verified
> findings in `TECH_DESIGN.md` win.

---

## 1. The one idea

A deterministic governance gate is the invisible engine that makes AI-agent output
trustworthy. Spawning parallel agents is commodity; the durable, hard part is
mechanically enforcing the rules those agents must follow, outside the model, so the
result is a binary pass/fail rather than a model's opinion. Everything else in this
repository is built around that idea to show where it leads.

One-liner: **CI/CD for a fleet of coding agents, led by an AI staff engineer, steered
by a human.**

---

## 2. The end-to-end workflow

1. **Intake.** A user story / feature request.
2. **Investigation (the engine as staff engineer).** Explore the codebase, then review
   a large rule corpus and recommend the relevant subset for THIS feature (everyone
   else dumps all rules into context; this selects), and produce two panels: clarifying
   questions and technical tradeoffs, each with a recommendation.
3. **Clarify.** The human answers the questions and picks the tradeoffs.
4. **Plan.** Decompose into role-scoped tasks (Frontend, Backend, DB), each carrying its
   own scoped rule subset and file/permission boundaries.
5. **Execute (governed fleet).** Spawn role agents, each in an isolated git worktree.
   Every output passes the governance gate before integration; a violation bounces back
   to the agent with the specific rule it broke. A coordinator manages handoffs and
   dependencies.
6. **Status.** Live, per task, per agent, per gate.
7. **QA (human).** Review the governed result and sign off, with a provenance trail:
   which agent produced each change, which rules it passed, the human decision.

The human never operates N chat windows; they steer one surface.

---

## 3. Two interaction surfaces on one engine

The same governance engine drives two interaction surfaces, distinguished only by where
the human stands:

1. **Architect surface.** The user is the principal architect: they steer the
   investigation, answer clarifying questions, approve the plan, and QA the governed
   diff. Collaboration with a non-technical requirements owner happens through the work
   tracker the team already uses (Jira / Azure DevOps / GitHub), used as an asynchronous
   bridge rather than a multi-tenant web app (see [`WORKTRACKER_INTEGRATION.md`](WORKTRACKER_INTEGRATION.md)).
2. **Requirements-owner surface.** The non-technical user fills a structured intake form
   and refines the app with an AI lead engineer, and the governed engine builds a
   CRUD-class app (frontend, backend, database) that deploys to the user's own cloud
   (see [`CONSUMER_UX.md`](CONSUMER_UX.md) and [`PO_MODE.md`](PO_MODE.md)).

Both run on the same deterministic gate. The thesis is the same at both altitudes:
execution is cheap, judgment is not, so the engine elevates the human to the judgment
and mechanizes the rest.

---

## 4. Architecture in one breath

- **Everything load-bearing is Rust.** One Tokio process is the server, the brain, and
  the gate.
- **Deterministic orchestrator core** that makes ZERO model calls (intake, rule
  selection, planning, worktrees, coordination, provenance), so ~80% of the build/test
  happens with no model spend (stub the agent layer with fixtures).
- **Governance gateway:** a Rust MCP server; every agent tool call routes through it and
  is allowed or denied before anything executes (deny-before-execute).
- **Agent layer:** short-lived `claude -p` subprocesses, one per role, scoped by prompt,
  allowed tools, path boundaries, and rule subset, behind a provider-agnostic seam.
- **Provider-agnostic by design.** Because every model call lives behind one seam, a
  non-Claude model is an additive swap, not a rewrite. The layer-2 gate inspects the
  produced CODE, not the model, so it works regardless of provider.

---

## 5. The build order (de-risking, engine first)

A build ORDER that proves the hard part before the polish, not a scope cap:

- **Engine first** (behind a minimal surface): one story, two role agents, the gate
  firing on a planted violation, a governed diff the human QAs.
- **Rule-selection intelligence:** corpus review plus per-task recommendation plus
  conflict/gap flags.
- **More roles plus coordination/handoffs plus parallel execution.**
- **The interface:** a single-pane cockpit and a live status surface. This is what makes
  it easier than five chat windows.
- **Provenance / audit trail**, surfaced in the UI.
- **The requirements-owner surface:** the same engine, fronted by a structured intake
  and an AI lead-engineer clarify loop, generating a bespoke app deployed to the user's
  own cloud.

---

## 6. Onboarding axis: new repo vs existing codebase

Two entry modes, both first-class:

- **Greenfield.** Scaffold a fresh project with the rules baked in from commit zero. The
  rule set is authoritative; nothing to reconcile.
- **Brownfield.** Ingest existing code, map its architecture, infer its conventions, and
  reconcile them with the corpus: which rules apply, which conflict, which to synthesize
  from the code itself. Harder, and the more valuable mode, because every real codebase
  already exists. Multi-repo matters (a feature can span repos), and adoption is
  incremental (a team adopts a subset and expands), not all-or-nothing.

Brownfield needs a dedicated onboarding phase: architecture map plus convention
extraction plus an initial rule-set proposal, with conflicts surfaced for the human.

---

## 7. Where the architecture leads

Where this leads is not a better developer tool; it is a software generator whose
output stays coherent because the same governance that tames professional agent fleets
is embedded in the generator's baseline harness. Consumer "vibe coding" fails today for
one structural reason: the absence of a deterministic safety net. A general user prompts
an ungoverned agent and gets fragile code that collapses on the next change, because
they have no architectural map to catch an N+1 query or a broken layering boundary, and
nothing else does either.

The engine flips this by mechanizing the missing judgment. The same rule corpus that
governs an enterprise fleet supplies, at genesis, the patterns the user does not know to
ask for:

- **Performance by default.** The user never learns what a database index is; the
  harness mandates performant patterns from the start.
- **Explicit, robust structure.** Non-technical users change their minds constantly; a
  strict robustness stance keeps the generated app flexible enough to survive endless
  iteration.
- **The layer-2 gate as a silent sandbox.** When an executing agent tries a shortcut,
  the deterministic check runner bounces it back until it complies. The user never sees
  the error; they just get a working result.

### The genesis harness

The system never starts from a blank slate. Before a single line of application code is
allowed to exist, it installs a mandatory baseline: the genesis harness. This is the
greenfield mirror of the brownfield rule that onboarding installs what the repo SHOULD
have, not merely what it has, and the same commitment as the project's own enforcement
stance: codified rules are enforced gates, not advisory documents. The harness comes
first; the application is built inside it.

The realization underneath all of it: the constraint on software creation is no longer
code generation, which is commodity. It is the enforcement of quality. Whoever
mechanizes that enforcement owns the floor under everyone else's generation.
