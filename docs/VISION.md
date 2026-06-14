# Camerata Orchestrator (working name; "Conductor" is a candidate, fits the Camerata/Chorale musical theme)

A governed multi-agent development environment. The human stays at Product-Owner / Principal-Architect
altitude. Camerata acts as the staff engineer that investigates, clarifies, plans, leads a team of
role-scoped agents, and enforces the rules. Thesis: execution is cheap, judgment is not, so the tool
elevates the human to the judgment and mechanizes the rest.

Status: design / Phase 0. This is a background build (orchestrated under a governance bar) that rides
BEHIND the job search, not in front of it.

**V1 scope = the FULL single-user tool, UI and Dashboard INCLUDED, not a CLI.** This is a personal,
agent-built project with no external deadline and no stakeholder to answer to, so it is NOT scoped like a
corporate quarterly release. The "thin slice" below is a de-risking build ORDER (prove the engine before
the polish), not a scope cap. This is a general-purpose tool for any developer, team, or company that
orchestrates a fleet of coding agents, and the whole point is to replace the multi-window
chat-orchestration setup they live in today with something genuinely easier, which a CLI cannot deliver.
So the interface is core, not optional.

**Source of truth.** This folder is the authoritative project record:
- `VISION.md` (this) - the north star: what it is, the workflow, scope, principles.
- `TECH_DESIGN.md` - verified architecture decisions (answers the section 16 questions; flags what was
  refuted in verification).
- `PHASE0_TASKS.md` - the engine build plan (14 tasks, de-risking order).
- `WORKTRACKER_INTEGRATION.md` - the Jira / Azure DevOps / GitHub tracker-integration design (post-V1-core).
- `UI_DESIGN.md` - (forthcoming) the cockpit + dashboard design.

Where VISION and TECH_DESIGN disagree, **TECH_DESIGN's verified findings win** (it fact-checked the
VISION's assumptions against current docs and refuted several; see TECH_DESIGN section 9).

---

## 1. The wedge (what makes this different)

Orchestration (spawning parallel agents) is already commodity (emdash, Devin, Factory, and others).
The differentiator is three things, none of which is "run many agents":

1. **Front-loaded judgment.** Investigation first. A PM-clarification panel and a tech-tradeoff panel
   are produced BEFORE any code. The human answers and decides; the agents then execute.
2. **Governance baked in.** Mechanical rule enforcement across every agent. Not rules in a prompt
   (examples are not enforcement); an actual gate that output must pass.
3. **Intelligent rule selection.** A large rule corpus (Camerata has 100+ rules) is curated PER TASK:
   the system reviews the corpus and recommends the relevant subset for this feature. Everyone else
   dumps all rules into context; this selects.

One-liner: **CI/CD for a fleet of coding agents, led by an AI staff engineer, steered by you.**

---

## 2. The end-to-end workflow

1. **Intake.** The human (Product Owner) writes a user story / feature request. One input box.
2. **Investigation (Camerata as Staff Engineer).** This mirrors the human's own pre-work routine:
   - Explore the codebase, identify affected areas and unknowns.
   - **Rule review + selection:** scan the 100+ Camerata rule corpus, recommend the relevant subset
     for THIS feature, flag conflicts and gaps. The human approves or edits the active rule set.
     (This replaces painstakingly reading every rule by hand.)
   - Output two panels:
     - **Product panel:** clarifying questions from a PM point of view.
     - **Tech panel:** technical tradeoffs (e.g. alternatives for a third-party API integration),
       each with a recommendation and reasoning.
3. **Clarify loop.** The human answers the clarifications and picks the tradeoffs. Architect altitude.
4. **Plan.** Camerata decomposes the work into role-scoped tasks (Frontend, Backend, DB, etc.), each
   carrying its own scoped rule subset and file/permission boundaries.
5. **Execute (governed team).** Spawn role agents. Each works in isolation (git worktree). Every output
   passes the governance gate (the active rules) before integration; a violation is bounced back to the
   agent with the specific rule it broke. A coordinator manages handoffs (the Backend agent defines the
   API contract, the Frontend agent consumes it) and dependencies.
6. **Status.** The feature status updates live, per task, per agent, per gate.
7. **QA (human).** The human reviews the governed result, QAs, approves or rejects. A provenance trail is
   attached: which agent produced each change, which rules it passed, the human sign-off.

The human never operates N windows. They steer one cockpit: the story, the investigation output, the
plan, the live status, the review surface.

---

## 3. Architecture (deterministic core, single agent seam, model-agnostic by design)

- **Orchestration + governance layer = your own deterministic code.** Routing, rule-selection, gates,
  status, coordination, provenance. It makes ZERO LLM calls itself, so it has no per-call cost, and ~80%
  of the build/test happens here with no API spend at all (stub the agent layer with fixtures).
- **Agents = the Claude Agent SDK, with every LLM call isolated to ONE module (`agents/session.ts`).**
  Each role agent is an SDK session with a scoped system prompt, scoped tools/permissions, and its active
  rule subset.
- **Auth (CORRECTED, verified in TECH_DESIGN Q1).** The original "Max-subscription OAuth, no metered API"
  thesis did NOT survive: OAuth tokens may not be used with the Agent SDK in a third-party tool (Consumer
  Terms; lockout risk). Use a metered `ANTHROPIC_API_KEY`; the plan's separate monthly Agent SDK credit
  auto-applies (**~$100/mo on Max 5x**, $200 on Max 20x; effective June 15 2026). A full Opus run is
  ~$7.50, so ~13 real runs/mo fit the Max-5x credit; light dogfood use stays inside it.
- **Dev-cost discipline (avoid testing fees).** Build/test on a cheap model (Haiku/Sonnet), reserve Opus
  for real validation runs; stub the LLM for the deterministic ~80%; record real runs as replayable
  fixtures; set a console spend cap. (Zach self-funds API as an investment, but disciplined dev stays cheap.)
- **Model-agnostic by design (a real differentiator, not Claude-locked).** Because every model call lives
  behind the single `agents/session.ts` seam, a Gemini / OpenAI / other adapter is an ADDITIVE swap, not a
  rewrite (same port pattern as the work-tracker providers). Build Phase 0 on the Claude Agent SDK (best
  tooling); a future routine should design the **provider port** so other models drop in later when there
  is a reason. Crucially, the MOAT is already model-agnostic: the layer-2 governance gate inspects the
  produced CODE, not the model, so it works regardless of provider. Only the layer-1 real-time hook is
  Anthropic-specific, and layer 2 backstops it.
- **Governance gate = code:** lint / AST / architectural checks the agent output must pass (e.g. "a DB
  call must go through the repository layer", the real rule that caught the drift). On failure, return
  the violated rule to the agent.
- **Rule corpus = the existing Camerata rules** (CONVENTIONS.md / AGENTS.md / ORCH rules), plus the
  per-task selection pass in step 2.
- **Cost-safe for Agora (corrected mechanism).** Single-user dogfood spend stays inside the Max credit;
  the mechanism is API-key + credit, not OAuth. Team / multi-user use needs separate Anthropic clearance
  (TECH_DESIGN risk 12) and product-stage metered economics.

---

## 3.5. Collaboration architecture (V1): the Architect is the node, the tracker is the async bridge

Decision (2026-06-13). For V1, Camerata does NOT run on shared cloud infrastructure. The Principal
Architect is the single central node: they run Camerata locally, hold the git worktrees, own the RuleSet,
and control execution. Collaboration with non-technical stakeholders (the Product Owner) happens THROUGH
the external work tracker (section 18 / WORKTRACKER_INTEGRATION.md) used as an ASYNCHRONOUS BRIDGE, not
through a multi-tenant web app.

The tracker is whichever ONE the Architect/PO primarily live in: Jira, Azure DevOps Boards, GitHub Issues,
or the built-in native tracker. This is about giving OPTIONS, not presuming a link between trackers and not
defaulting to any one of them. The Architect connects a Story to the single tracker their team already uses;
if the PO lives in Jira, the direct link is to Jira. Note that the code host and the product tracker can
DIFFER (code on GitHub, product management in Jira is common), and the clarify-bridge targets where the PO
lives, the product tracker, which is not necessarily the code host. So the integration runs on TWO
independent axes with their own priorities (decided 2026-06-13, detailed in WORKTRACKER §3): the CODE-HOST
axis builds GitHub first then Azure DevOps Repos (most teams host code on GitHub); the BOARD axis builds
Jira and Azure DevOps Boards first (the two most-used enterprise story trackers), with the native tracker
always available and GitHub Issues deprioritized (underused as a formal enterprise board). A Story records
a code-host ref and a board ref independently; which concrete adapters a deployment uses is its choice, not
a product constraint.

Two architectures were weighed:

- **Rejected: shared remote database + local compute (the "thick client / split-brain" trap).** A remote
  DB that local desktop apps sync against forces every collaborator into the developer tool: does the PO
  clone the repo, or install a specialized developer desktop app just to answer a business-logic question?
  The app then has to handle split states where some users have the code and some do not. Pure friction,
  and it drags multi-tenant cloud infrastructure into V1.
- **Adopted: the tracker as the asynchronous bridge (the "Trojan Horse").** The PO never leaves their
  natural habitat. The clarify loop flows out and back through the board they already use:
  1. Investigation: the agent generates the product clarifying questions.
  2. Outbound: Camerata posts them as a formatted comment on the Jira / GitHub / Azure DevOps issue,
     @-mentioning the PO ("the investigation agent needs clarification on these edge cases before execution").
  3. Human loop: the PO gets their normal tracker notification, opens the ticket on phone or browser, and
     replies in the comments.
  4. Inbound: Camerata's webhook (poll fallback) pulls the comment back into local context.
  5. Execution: the Architect reviews the answer locally, approves the technical tradeoffs (architect
     altitude, stays local), and runs the agents locally.

Role split that falls out of this: PRODUCT clarifying questions route to the PO via the tracker; TECHNICAL
tradeoffs and the RuleSet stay with the Architect locally (architect altitude). The PO can only ANSWER and
sign off, never execute; the Architect is the gatekeeper. Clean privilege boundary, no central OAuth, no
multi-tenant DB, no containerized compute sandboxes.

Why this is the right V1 cut: it delivers enterprise-grade async collaboration without enterprise-grade
infrastructure. It plugs into the board a team already runs on instead of asking them to adopt a new
platform, which keeps V1 scope to "a local desktop app plus tracker API adapters." Multi-user cloud,
central auth, and hosted compute remain deliberate beyond-V1 product-stage concerns (consistent with
TECH_DESIGN: single-user local is the V1 shape).

Consistency: this is the SAME `WorkItemProvider` port and the same "our spine is canonical, the tracker is
a MIRROR" default already chosen in WORKTRACKER_INTEGRATION.md. The async bridge is a NEW USE of that
port's outbound (post a clarifying-question comment) and inbound (ingest the answer comment) methods, not
a new architecture. The PO's answer becomes a Provenance source (comment id / url / author = the auditable
`human_decision`).

---

## 4. Phase 0 thin slice (build THIS first, not the platform)

ONE vertical thread, end to end:

> One story -> Camerata produces an investigation writeup + a recommended rule subset + 2-3 clarifying
> questions -> the human answers -> spawn TWO role agents (e.g. Backend + Frontend) under ONE enforced
> rule -> a governed diff is produced -> the human QAs it.

Deferred to a LATER phase (NOT cut from V1, just not in this first engine-proving slice): the polished
cockpit UI and the live status dashboard (both V1-essential, built in P3 once the engine is proven),
multiple concurrent features, more than two roles, the full provenance audit. Phase 0 uses a CLI or
minimal panel ONLY because its goal is to prove the engine and the gate, not to ship the interface.
Dogfood it on Agora (a real brownfield codebase with a genuine first user).

---

## 5. Build phases (a de-risking ORDER; V1 = the full single-user tool, UI + Dashboard included)

The phases are a build order that proves the hard part (the governance engine) before the polish. They
are NOT a scope limit: V1 is the complete tool the user runs, interface and all.

- **P0 (engine, behind a minimal UI).** The thin vertical thread (section 4): one story, two role agents,
  the governance gate firing on a planted violation, a governed diff the human QAs. A CLI or minimal panel
  is fine HERE because the goal is to prove the engine, not ship the interface.
- **P1** Rule-selection intelligence: corpus review + per-task recommendation + conflict/gap flags.
  (Directly kills the "read 100+ rules by hand" pain.)
- **P2** More roles + coordination/handoffs + parallel execution.
- **P3 (the interface, V1-ESSENTIAL).** The single-pane cockpit (intake, the two investigation panels,
  the plan, the QA/review surface) AND the live status Dashboard (features, agents, gates, what needs the
  human). This is what makes it "easier than five chat windows", the core value prop, so it is required
  for V1, not optional. Reuse Camerata's existing Dioxus UI for consistency; consider dogfooding chorale
  for the dashboard tables.
- **P4** Provenance / audit trail, surfaced in the UI.
- **V1 = P0 through P4, shipped as one coherent tool.**
- **Later (product stage, beyond V1)** Multi-user, metered API economics at scale, teams, hosted.

---

## 6. Discipline guardrails (the human's own known risks)

- THIN means build ORDER, not feature exclusion: prove the engine before the polish. V1 IS full scope
  (UI + Dashboard included); the discipline is sequencing (engine first), not cutting features. The real
  constraint is your review bandwidth and the job-search priority, not an artificial feature cap.
- Cheap via orchestration is NOT free. Budget the upfront design and the review/QA. The human-in-the-loop
  IS the product; skipping review is becoming the slop.
- It rides BEHIND the job search. Background build + credibility artifact, not a substitute for the search.
- Build it as a PERSONALLY-OWNED repo that Agora uses, not Agora work product. Keep the IP yours.
- Build in public as it progresses. It is the strongest job-search artifact for emdash / Rivet, and the
  orchestrated, governed build process is itself the demo.

---

## 7. Why this could be the differentiating artifact

- Most on-thesis for the target companies (governed multi-agent orchestration is their exact world).
- Dogfooded on a real codebase with a real first user (Agora) = credible, not a toy.
- Proves the governance thesis ("examples are not enforcement") end to end.
- The way it gets built (orchestrated, under a governance bar, by one architect) is the proof of the
  method, not just the artifact. The build is the demo.

---

## 8. Core entities (the data model)

- **Story** the user request. { id, title, description, status, created_by }
- **Investigation** the staff-engineer output for a Story. { codebase_findings, recommended_rule_set,
  product_questions[], tech_tradeoffs[] (each: option, pros, cons, recommendation) }
- **Rule** one Camerata rule. { id, category, scope (FE/BE/DB/global), statement, enforcement_kind
  (deterministic-check | review-heuristic), check_ref (if mechanically enforceable) }
- **RuleSet** the active subset selected for a Story/Task (a list of Rule ids + the selection rationale).
- **Role** a scoped worker archetype. { name (Backend/Frontend/DB/...), system_prompt, allowed_tools,
  path_boundaries (globs it may write), rule_subset }
- **Task** a unit of work assigned to a Role. { id, role, description, depends_on[], worktree, status,
  produced_diff, gate_results[] }
- **Gate / Check** an enforcement step. { rule_id, kind (hook | post-task), result (pass|fail), message }
- **Provenance** per change: { task_id, role, agent_session_id, rules_passed[], human_decision }
- **FeatureStatus** the live roll-up across Tasks/Gates for a Story.

## 9. Roles and scoping (how an agent is constrained)

Each Role is an Agent SDK session configured with:
- A **scoped system prompt** (its job + its rule subset injected).
- **allowed_tools** limited to what the role needs (the DB role gets migration tools, not arbitrary FS).
- **path_boundaries** the globs it may write (the Frontend role cannot write under `db/` or `migrations/`).
- Its **rule_subset** from the RuleSet (only the rules relevant to its role, not all 100+).

The scoping is BOTH prompt-level (it is told its boundaries) AND enforced (section 10), because prompt
boundaries alone are the "examples are not enforcement" failure.

## 10. The governance gate (enforcement, two layers)

1. **Real-time (Agent SDK hooks).** PreToolUse hooks reject tool calls that violate a hard boundary
   before they happen (e.g. a write outside the role's path_boundaries, a raw DB call from the FE role).
   This is the cheap, immediate guardrail.
2. **Post-task validation.** After a Task produces a diff, run the deterministic checks for its active
   rules: lint, AST/structural checks (e.g. "DB access only through the repository layer"), build, tests.
   On any FAIL, the diff is bounced back to the agent with the specific violated rule + message; it
   revises and resubmits. A diff only integrates after all gates pass.

Rules whose enforcement_kind is `review-heuristic` (not mechanically checkable) are surfaced to the human
at QA rather than auto-enforced. Be honest in the model about which rules are enforced vs advisory.

## 11. The rule-selection engine (the 100+ rules problem)

Goal: given a Story, recommend the relevant rule subset instead of dumping all rules into context.
- **Phase 0 approach (simple):** the investigation agent is given a compact **rule index** (id +
  category + one-line statement, which fits in context even at 100+) plus the Story and codebase
  findings, and returns the recommended subset with a one-line rationale per rule, plus flagged conflicts
  and gaps ("no rule covers X, consider adding one").
- **Later (scale):** embed rules, retrieve top-k by relevance to the Story to pre-filter, then the agent
  refines. Only needed if the corpus outgrows the context window.
- Output is human-approvable/editable. The human owns the final active RuleSet.

## 12. Coordinator and handoff protocol

- Build a **task DAG** from the plan (Frontend depends_on the Backend API contract, etc.).
- Each Task runs in its own **git worktree/branch** (isolation; no clobbering).
- **Contract handoffs:** an upstream Task emits an artifact (e.g. an API contract / type definitions) that
  downstream Tasks consume; the coordinator passes it forward rather than letting agents guess.
- **Integration order:** merge completed-and-gated Tasks in dependency order; re-run gates at integration
  in case of cross-task interactions.
- Concurrency is bounded by Max rate limits; the coordinator schedules within that ceiling.

## 13. Agent SDK integration specifics

- Use the **Claude Agent SDK** (TypeScript). Each Role = one SDK session with its scoped system prompt,
  allowed_tools, permission mode, and working directory (its worktree).
- Auth via a **metered `ANTHROPIC_API_KEY`** (CORRECTED, see section 3 and TECH_DESIGN Q1; the original
  Max-subscription-OAuth "no metered API" path was refuted as a Consumer Terms violation). The separate
  Agent SDK monthly credit pool on a Max plan auto-applies to API-key usage, so Phase 0 stays cost-safe.
  Concurrency behavior was resolved by running Phase 0 agents sequentially (TECH_DESIGN Q1/Q4).
- Hooks (PreToolUse/PostToolUse) implement the real-time gate (section 10.1).
- The orchestration/governance code makes no model calls itself; all generation is inside the sessions.

## 14. Recommended stack

- **Orchestrator + governance layer: TypeScript/Node** (matches the Agent SDK and Zach's strongest stack).
- **Deterministic checks:** TS (ts-morph for AST, ESLint) for TS targets; shell out to language-native
  linters for others (e.g. clippy for Rust targets). Keep the check interface pluggable per language.
- **Persistence (Phase 0):** flat files / SQLite for Stories, RuleSets, Provenance. No service yet.
- **UI (later):** a single local web panel; Phase 0 is CLI.

## 15. Phase 0 acceptance criteria (definition of done for the thin slice)

Given a seeded Story against a real (Agora) repo:
1. The system outputs an Investigation: codebase findings, a recommended RuleSet (from the real rule
   corpus), and 2-3 product clarifying questions. [human answers]
2. It spawns exactly TWO Role agents (Backend + Frontend) in separate worktrees with scoped boundaries.
3. A **planted violation** (e.g. the FE agent attempts a direct DB call) is **caught by the gate** and
   bounced back with the specific rule, then resolved. (This proves enforcement, not just orchestration.)
4. A governed diff is produced and presented to the human for QA, with a provenance line per change.
5. Runs on a metered `ANTHROPIC_API_KEY` whose spend is absorbed by the Max plan's Agent SDK credit pool
   at Phase 0 scale (CORRECTED from the original "no metered API" assumption; see section 3 / TECH_DESIGN Q1).

If those five hold, the thesis is proven end to end. Everything else is expansion.

## 16. INVESTIGATION CHARTER (for the overnight routine)

The routine's job is the FIRST ANALYSIS, not building. Produce a `TECH_DESIGN.md` + a Phase 0 task
breakdown. Do NOT write production code overnight.

**Resolve these open questions (research + decide, with rationale):**
1. Can the Claude Agent SDK run **headless on a Claude Code Max subscription** (no API key), spawn
   **multiple concurrent sessions**, and what are the real **rate-limit ceilings**? If subscription
   headless is not viable, what is the cheapest fallback and its cost?
2. Do Agent SDK **hooks** support rejecting a tool call (PreToolUse) reliably enough to be the real-time
   gate? Confirm with a concrete hook example.
3. Best mechanism for the **post-task structural checks** (the "DB access only via repository layer"
   class of rule): ts-morph AST queries? custom lint rules? Sketch one real check end to end.
4. Worktree-per-agent: confirm the **git worktree** flow the coordinator will drive, and how diffs are
   produced and integrated.
5. The **rule corpus**: read the actual Camerata rules, propose the **Rule index** schema (section 11),
   and identify which existing rules are mechanically enforceable vs review-only.
6. **Onboarding UX (greenfield vs brownfield, section 17):** sketch both entry flows. For brownfield,
   how does the system build the architecture map, extract existing conventions, and propose a baseline
   RuleSet (some rules selected from the corpus, some synthesized from observed patterns, conflicts
   flagged)? How does multi-repo work? Treat this as a first-class product axis even if Phase 0 only
   implements minimal single-repo brownfield (Agora is the dogfood target, so brownfield IS the default).

**Constraints to honor:** TypeScript/Node; Max subscription (no metered API); Phase 0 = the thin slice
(section 4) and its acceptance criteria (section 15); personally-owned repo; keep it thin (do not design
the full platform).

**Deliverables:** `TECH_DESIGN.md` (answers to the six questions + the chosen architecture + module
layout) and `PHASE0_TASKS.md` (an ordered, estimated task breakdown to satisfy section 15). Flag any
assumption it could not verify rather than guessing.

---

## 17. Onboarding axis: new repo vs existing codebase (a KEY product dimension)

Two fundamentally different entry modes. Supporting both well is what separates a real product from a
demo, and brownfield is the one that wins real users.

- **Greenfield (new repo from scratch).** The orchestrator scaffolds a fresh project with the rules baked
  in from commit zero. Easier: clean slate, the RuleSet is authoritative, nothing to reconcile.
- **Brownfield (onboard an existing repo, or several).** The orchestrator must ingest existing code, map
  its architecture, infer its established conventions, and reconcile them with the Camerata corpus: which
  rules apply, which conflict with established patterns, which to synthesize from the code itself. Harder,
  and far more valuable, because every real team already has code. Multi-repo matters (Agora is a
  mono/multi-service codebase; a feature can span repos).

Implications for the design:
- Brownfield needs a dedicated **codebase-onboarding phase**: architecture map + convention extraction +
  an initial RuleSet proposal (some rules selected from the corpus, some synthesized from observed
  patterns, conflicts surfaced for the human to resolve).
- This extends the rule-selection engine (sections 2, 11): not only "pick the rules for this story" but
  "establish the baseline RuleSet for this codebase" on first onboard.
- **Incremental adoption:** a brownfield team will not accept all rules at once. Support adopting a subset
  and expanding over time, rather than an all-or-nothing gate.
- Phase 0 dogfoods on Agora, which is brownfield, so even the thin slice needs minimal brownfield
  onboarding (point at the existing repo, run the investigation against real code). The full
  convention-extraction / rule-synthesis is a later phase; the minimal "work against an existing repo"
  is Phase 0.

Design both modes as first-class. Greenfield is the easy demo; brownfield is the product.

---

## 18. Follow-up idea: work-tracker integration and the story spine (NOT Phase 0; flag for a follow-up investigation)

Status: INVESTIGATED (see WORKTRACKER_INTEGRATION.md for the full design memo). UPDATE 2026-06-13: the
async clarify-bridge SLICE of this is now promoted to V1 collaboration architecture (see section 3.5) and
is no longer a pure follow-up. The full storyification (status sync, multi-provider, native tracker)
remains post-V1. The original framing below is kept for context.

The whole expansion is a STORYIFICATION of the dev process: a Story is the unit of work that flows
intake -> investigation -> clarify -> plan -> governed execution -> QA, carrying its provenance the whole
way (Story / Investigation / Task / Gate / Provenance / FeatureStatus already exist in section 8). That
spine is exactly what issue trackers model, so two complementary directions are worth evaluating:

- **External work-tracker integration (meet teams where they live).** Ingest a Story from, and write status
  back to, the tracker the team already uses: **Jira, Azure DevOps Boards, GitHub Issues/Projects**
  (Linear, Shortcut as fast-followers). The tracker issue becomes the intake (section 2.1) instead of, or
  alongside, the one input box; the orchestrator syncs the live FeatureStatus, the governed diff/PR link,
  the gate results, and the human sign-off back onto the issue. This is the on-thesis enterprise wedge:
  governed multi-agent execution attached to the board a team already runs on, with the provenance trail
  posted where their process of record already lives.
- **In-built story tracker (own the spine when there is no tracker, or when ours is better).** A minimal
  native tracker so the product is self-sufficient: greenfield teams and solo users get a first-class Story
  board without needing Jira. It also keeps the canonical state ours (the Story spine, the provenance,
  the RuleSet history) rather than renting it from an external system whose schema we do not control.

Design tension to resolve in the follow-up: is the external tracker the **source of truth** (we sync to it)
or a **mirror** (our Story spine is canonical, the tracker is a projection)? Brownfield enterprise teams
will want their tracker to stay authoritative; the native tracker wants ours to be. A clean abstraction is
a **WorkItemProvider** port (native | jira | azure-devops | github) behind the same Story interface, so the
core orchestration never knows which backend it is talking to. That port also localizes the auth, webhook,
and field-mapping mess per provider.

Open questions for the follow-up investigation: webhook vs poll for inbound issue events; bidirectional
status mapping (our Task/Gate states <-> their workflow columns); how a multi-repo feature (section 17)
maps to a single tracker issue; whether the PR/diff link or the full provenance trail is what gets posted
back; and the auth model per provider under the no-metered-API constraint. None of this is Phase 0; the
thin slice stays the one input box (section 4).

## 19. The endgame: from enterprise cockpit to consumer software generator

The logical endgame of this architecture is not a better developer tool. It is a consumer software
generator: a non-technical person describes the small, bespoke app they want (a budgeting tracker, a
recipe organizer, a league scheduler), and the same governed engine that tames professional agent fleets
produces a stable, production-grade application. This is the same product scaled down, not a different
product.

The reason consumer "vibe coding" fails today is the absence of a structural safety net. A general user
prompts an un-governed agent and gets a fragile, disorganized pile of code that collapses the moment they
ask for the next change. They have no architectural map in their head to catch an N+1 query or a broken
layering boundary, so nothing catches it. A 13-year developer succeeds at vibe coding precisely because
that map is intuition; the consumer has none.

Camerata flips this by mechanizing the missing intuition. The same rule corpus that governs an enterprise
fleet, embedded in the generator's baseline harness, supplies the judgment the user does not have:

- **SPIRIT-OPTIMIZE-1 (performance by default).** The consumer never learns what a database index or a
  parallel async call is. Because the harness mandates performant patterns at genesis, the generated app is
  fast and cheap to run from day one, with the user never hearing the word "indexing."
- **SPIRIT-ROBUSTNESS-1 (explicit, robust structure).** Consumers change their minds constantly. An
  un-governed agent writes terse, hacked code to satisfy the immediate prompt, which buckles under the next
  feature request. The strict robustness stance keeps the app flexible enough to survive a non-technical
  user's endless iteration.
- **The layer-2 gate as a silent sandbox (section 10).** When an executing agent tries a shortcut, the
  deterministic check runner and AST gate bounce the change back automatically until it complies with
  standard engineering hygiene. The consumer never sees the error; they just get a working app. The
  bounce-and-revise loop, proven in the Phase-0 acceptance run, is the self-healing cycle that makes this
  possible.

### Over-engineering the side project is the moat

Setting up a full test suite, a CI pipeline, and infrastructure-as-code for a simple budgeting app is
normally absurd. But once the setup is mechanized, as the New Agora work proved end to end, it becomes
trivial, and the absurdity inverts into the differentiator. For a consumer generator, that
"over-engineering" is exactly what produces stability:

- **Automated quality bars.** Every bespoke app Camerata generates spins up its own localized test suite,
  lint rules, and type-checking. That is a self-healing software cycle the user never has to think about.
- **Safe deployment baked into the corpus.** The user does not want to know how hosting works. A
  standardized, lightweight deploy template (a pre-configured container or a miniature serverless stack)
  lives in the rule corpus itself, so shipping is a one-click background task. The result is not a "vibe
  coded app." It is production-grade software wrapped in a consumer-friendly skin.

### The strategic path bridges both audiences

The two target audiences are the same engine at two altitudes:

1. **Near-term (enterprise / the high-leverage artifact).** Build Camerata as the principal architect's
   cockpit, hooking into the team's tracker (section 18) and GitHub to govern professional agent fleets and
   prove the thesis on real corporate codebases. This is the demonstrable artifact.
2. **Long-term (consumer / the product).** Once the deterministic governance engine is flawlessly taming
   agents on complex codebases, strip away the technical dashboard, replace it with a simple
   requirement-intake form, and open it to the public. The exact same rules protect the consumer that
   protect the enterprise. Nothing about the core changes; only the skin does.

The realization underneath all of it: the ultimate constraint on software creation is not code generation,
which is now commodity. It is the enforcement of quality. Whoever mechanizes that enforcement owns the
floor under everyone else's generation.

### The genesis harness answers the onboarding question

This is why the onboarding axis (section 17) is the load-bearing product dimension, not a detail. The
system never starts an empty repository from a blank slate. Before a single line of application code is
allowed to exist, it injects a mandatory, non-negotiable set of universal laws: the genesis harness. This
is the greenfield mirror of the brownfield rule that onboarding installs what the repo SHOULD have, not
merely what it has (section 17), and it is the same commitment as ORCH-CONFORMANCE-1: codified rules are
enforced gates, not advisory documents. For the enterprise user the harness is a starting convention set
they curated. For the consumer it is invisible and absolute, the rigid foundation that makes the app
literally unable to be built poorly regardless of who is driving. The harness comes first; the application
is built inside it.
