# Camerata Orchestrator: UI_DESIGN.md

Status: Phase 0 design (P3 target). This document designs the single-user cockpit and the
status dashboard for Camerata Orchestrator (working name; "Conductor" is the candidate).
It is decision-first: every choice is stated as Question, Recommendation, Why,
Alternatives. ASCII wireframes are included for the load-bearing layouts.

Verification anchor date: 2026-06-13. Forward-looking Dioxus / Agent-SDK / chorale
capability claims in this design were checked against current (2026) primary sources and
the live chorale v0.2.1 working copy. Where a load-bearing claim was refuted or could not
be confirmed, the fallback is adopted in place and called out plainly. Every such item is
also listed in the closing "Unverified assumptions and open risks" register.

This document EXTENDS the verified engine in TECH_DESIGN.md. It does NOT relitigate it.
The TS/Node orchestrator core (making zero LLM calls), the Claude Agent SDK agent layer,
the two-layer gate, sequential Phase 0 execution, the three-state `enforcement_kind`
model, and the metered-API-key + Max-credit auth all stand exactly as TECH_DESIGN
specifies. Where this document and VISION disagree, TECH_DESIGN's verified findings win.

---

## 0. Scope

**V1 = the FULL single-user, local tool, with the cockpit UI and the status Dashboard
included.** Not a CLI. The whole point (VISION section 2 close, section 5 P3) is to
replace the multi-window chat-orchestration setup the user lives in today with ONE
steerable cockpit, and a CLI cannot test that hypothesis. The UI is core, not polish.

**Engine-first build ORDER, full-V1 SCOPE.** The old PHASE0_TASKS "CLI-only / no
dashboard" boundary is a de-risking build ORDER (prove the governance engine T0-T14
behind a minimal harness first), not a scope cap. This is authoritative per the scope
correction: build order is engine-first, scope is full V1. The CLI path from PHASE0_TASKS
remains useful as the harness that proves the engine; the cockpit and dashboard wrap the
SAME local HTTP + event surface afterward. Building the cockpit before the gate fires
(T9) would violate the build order; it would not violate the scope.

**Single-user LOCAL is the V1 shape.** The design must not bake in assumptions that block
a later multi-user / team product. Concretely: actor-shaped fields (`Story.created_by`,
`Provenance.human_decision`) stay actor-shaped, never hardcoded to one user; the cockpit
component tree stays renderer-agnostic so a later hosted/WASM target is a build-target
flip, not a rewrite; the data contract on the front-end/core seam stays plain serialized
JSON so the front-end stays swappable. None of this is a V1 feature. It is a
non-blocking-shape constraint only.

**Family consistency.** The cockpit reuses the existing Camerata curation GUI idiom
(`/Users/zacharyernst/Documents/Repos/camerata-ai/src/bin/gui.rs`): collapsible groups,
per-item checkboxes, a right-hand detail pane with rationale plus selectable
alternatives, and banner-style confirm flows for destructive actions. The dashboard
tables dogfood chorale v0.2.1 so the build is its own showcase. Routing, context
providers, animation, and component decomposition come from rust-portfolio's Dioxus
patterns. The "same family" cockpit is the UNION of camerata's state/layout idioms and
portfolio's structure/routing discipline, themed by chorale's CSS-variable tokens.

---

## 1. The cross-stack decision (how the front-end talks to the TS/Node core)

### Question

The orchestrator core is verified TypeScript/Node (TECH_DESIGN section 1, section 8) and
makes zero LLM calls. The cockpit and dashboard want to be Dioxus (camerata family +
chorale dogfood). How does a Rust/Dioxus front-end talk to a TS/Node core, especially for
the live status stream (the `EXECUTING -> GATING -> AWAITING_QA` push and the gate-bounce
events that make "easier than five chat windows" visible)?

### Recommendation

Build the cockpit as a **Dioxus 0.7 fullstack app whose own Rust/Axum server is a thin
Backend-For-Frontend (BFF) in front of the TS/Node orchestrator** (the hybrid, "Option 4"
below). The Dioxus UI never talks to the TS core directly. It calls Dioxus server
functions (for commands and queries) and consumes ONE live event stream. The Rust BFF is
the only component that crosses the language boundary: it proxies REST to the TS core's
local HTTP API and subscribes to the core's event stream (SSE or WS), re-broadcasting
normalized events to the browser/webview over the BFF's own Axum WebSocket.

The TS orchestrator stays exactly as TECH_DESIGN specifies. The only net-new orchestrator
work is a thin local HTTP + event surface (pure transport over the deterministic state the
engine already computes; it adds NO LLM calls, preserving the "zero model calls in
orchestrator code" invariant).

**Target Dioxus desktop for V1** (`dioxus::desktop`, OS webview, matching
`camerata-ai/src/bin/gui.rs`), keeping the component tree renderer-agnostic so a later
hosted/WASM build is a config flip, not a rewrite.

### Why

Two verified facts force this shape:

1. **The Claude Agent SDK ships only in TypeScript and Python in 2026; Rust has no
   official Agent SDK.** CONFIRMED against the official Agent SDK overview
   (code.claude.com/docs/en/agent-sdk/overview): the SDK is "programmable in Python and
   TypeScript," with only `npm install @anthropic-ai/claude-agent-sdk` and `pip install
   claude-agent-sdk` install paths. Rust is absent from every official SDK list (the
   broader Messages-API client SDK list covers 7 languages, none Rust; for unsupported
   languages including Rust the docs offer only cURL / raw HTTP). PreToolUse hooks and
   ts-morph AST checks are equally TS-native. So a Rust rewrite of the core (the
   "stack-unify toward Rust" option) is off the table: it would discard the entire
   verified TECH_DESIGN engine and reimplement session/hook/MCP plumbing over raw HTTP.
   The cross-language boundary is therefore UNAVOIDABLE. The only question is where it
   lives.

   Honest framing correction (from verification): the docs do not explicitly say "Rust is
   unsupported"; Rust is simply absent from every list, and unofficial community Rust
   crates do exist. Betting the governance plumbing on an unofficial wrapper of a moving
   CLI target is a worse risk posture than the verified TS core, so this strengthens, not
   weakens, the rejection of a Rust core.

2. **Dioxus 0.7's first-class WebSocket support is the INTEGRATED Axum / server-functions
   path; a raw Dioxus client connecting to an arbitrary non-Dioxus, non-Rust WebSocket
   server is NOT framework-supported.** CONFIRMED: the 0.7 fullstack WebSockets docs
   describe WebSockets exclusively as server functions returning a `Websocket` response,
   "built on top of the underlying Axum websocket API," with shared server/client types.
   For an arbitrary external server, maintainer guidance (GitHub discussion #3242) is
   verbatim "Dioxus does not do anything to help you," and the reference example "doesn't
   yet support reconnecting if socket goes down." You would hand-roll a per-target client
   (gloo-net on WASM, tokio-tungstenite on native) behind `#[cfg]`, including reconnect.
   (Note for precision: the gloo-net / tokio-tungstenite crate names are the standard
   Rust cross-target pattern, not a verbatim maintainer quote; the "no help" and
   "no reconnect" statements ARE verbatim from #3242.)

   Putting the language boundary inside a Rust BFF converts the HARD part (an unsupported
   raw external WS client in the browser) into the EASY part (a Dioxus-native server-side
   Axum WS toward the client, which IS first-class in 0.7, plus a plain server-side
   tokio-tungstenite client toward the TS core). The BFF is also the natural home for the
   reconnect-with-backoff, heartbeat, and snapshot-on-connect logic the live dashboard
   needs regardless.

This keeps full Camerata/portfolio family consistency and lets the dashboard dogfood
chorale 0.2.1 (chorale ships Dioxus + Leptos adapters only; there is no React adapter, so
a TS/React front-end would forfeit the dogfood and force a second table library).

### Two-process desktop note (an honest cost)

Option 4 means two local processes: the Rust Dioxus-fullstack app (UI + BFF) and the TS
orchestrator. For a single-user LOCAL tool this is acceptable but must be DESIGNED, not
assumed: a small local supervisor spawns the TS core as a child of the BFF (or a tiny
process manager owns both), with a fixed localhost port and a startup handshake. Entity
shapes (Story, Task, Gate, Provenance, FeatureStatus) are defined in TS and mirrored as
Rust serde structs at the BFF boundary; generate the Rust types from a shared JSON Schema
(or a TS type export via a typeshare-style step) so the canonical FeatureStatus enum
(INTAKE..REJECTED) cannot drift between the two languages.

**Supervisor failure contract (REQUIRED, architect 2026-06-13 — a silent stale-state hang
is the worst outcome for a tool whose entire value is trustworthy enforcement):**

- **Health check.** The supervisor polls the TS core readiness endpoint on a fixed
  interval (default 2s); a missed health check transitions the connection state to
  `Reconnecting`, then `Offline` after N consecutive misses.
- **Configurable retries.** Restart-on-crash is **configurable in the UI** (max retry
  count + backoff, with a "do not auto-restart" option). The default is a small bounded
  retry, not infinite, so a hard-failing core does not silently loop.
- **Loud, explicit failure.** When the core dies or retries are exhausted, the UI raises a
  **noisy, unmissable error surface** (modal/banner, not a quiet status pill): which
  process died, the captured stderr tail, the retry count consumed, and the recovery
  action. Never a silent stall.
- **Fail closed on the work.** If the core dies mid-execution or mid-gate, the in-flight
  Story is marked `BLOCKED` (not silently advanced), and its worktree is left **intact and
  recoverable** — **never auto-merged or cleaned up** on a crash path. Merge/cleanup happen
  ONLY on a clean gated completion. A crash must never be able to ship ungated code.

### A desktop-specific transport caveat (folded into the recommendation)

If a future build collapses the BFF and renders Dioxus DESKTOP talking straight to a
non-Rust server (the "Option 1" shortcut), note the verified hazard: **Dioxus fullstack
STREAMING server functions do not work on desktop** (GitHub issue #3694, closed
not-planned: "Streaming server functions are not called at all on desktop"). A desktop
cockpit that relied on Dioxus streaming server functions for the live stream would
silently receive nothing. The BFF design avoids this because the live leg the desktop UI
consumes is a plain WebSocket the BFF owns, driven by a `use_coroutine` that pushes frames
into signals (NOT the built-in `use_websocket` hook, which is bound to Dioxus server
functions). If V1 ever ships the BFF and UI as one desktop binary, the live status must
ride that `use_coroutine` + plain-WS path, with explicit reconnect (Dioxus discussions
#1721 / #3004 confirm `use_coroutine` WebSockets do not auto-reopen after a drop) and a
visible connection-state signal (Connected / Reconnecting / Offline) so a dropped socket
is visible, not a silent stall.

### Alternatives considered

- **Option 1: raw Dioxus client (desktop or WASM) straight to the TS core's HTTP + WS.**
  Simplest topology on paper. REST via `use_resource` + reqwest (native) / gloo-net (WASM)
  is clean. Disqualifier: the live WS leg is the unsupported, hand-rolled, per-target,
  reconnect-it-yourself path (verified above), AND the desktop streaming-server-function
  defect (#3694) bites if anyone reaches for the framework path. Viable but you maintain
  bespoke per-platform transport with zero framework help for the single most
  latency-sensitive surface. Rejected as primary.
- **Option 2: stack-unify toward TS (React/Next front-end colocated with the core).** One
  language, trivial WS push. But it abandons the Dioxus family (VISION P3), CANNOT dogfood
  chorale (no React adapter, forces AG Grid / TanStack), and forfeits the "build is the
  demo" lever that the BFF already neutralizes. Rejected.
- **Option 3: stack-unify toward Rust (rewrite the core in Rust).** REJECTED with a hard
  reason: no official Rust Agent SDK; ts-morph and PreToolUse hooks are TS-native. A Rust
  core reimplements the governance plumbing from scratch and discards the verified engine.
  Off the table.
- **Option 4 (RECOMMENDED): hybrid BFF.** Detailed above. One extra process and a
  serialization mirror, both cheap and bounded for a local single-user tool, in exchange
  for family consistency, the chorale dogfood, and a clean home for reconnect/snapshot.

### Concrete API surface

The TS core exposes a thin local HTTP API (net-new, pure transport over existing
deterministic state; adds NO LLM calls). The BFF proxies these 1:1 via Dioxus server
functions.

```
POST   /stories                      # intake: { title, description, created_by, repo } -> Story
GET    /stories                      # Story[] for the left-rail spine
GET    /stories/:id                  # Story + current FeatureStatus
POST   /stories/:id/investigate      # kick off investigation pass (async); 202 + job id
GET    /stories/:id/investigation    # Investigation { codebase_findings, recommended_rule_set,
                                      #   product_questions[], tech_tradeoffs[] }
POST   /stories/:id/clarifications   # human answers + tradeoff picks -> updates plan/RuleSet
GET    /stories/:id/ruleset          # active RuleSet (+ per-rule rationale, enforcement_kind)
PUT    /stories/:id/ruleset          # human edits the active RuleSet (final ownership, VISION 11)
POST   /stories/:id/plan             # decompose into role-scoped Tasks (the 2-node DAG)
GET    /stories/:id/tasks            # Task[] { role, depends_on[], worktree, status, gate_results[] }
POST   /stories/:id/run              # execute (SEQUENTIAL in P0): spawn role agents
GET    /tasks/:id                    # Task detail incl. produced_diff, gate_results[]
GET    /tasks/:id/diff               # the governed diff for QA
POST   /stories/:id/qa               # human decision { decision: signed_off | rejected, notes }
GET    /stories/:id/provenance       # Provenance[] { task_id, role, session_id, rules_passed[] }
GET    /onboarding/proposal?repo=... # brownfield baseline RuleSet proposal + conflicts/gaps
GET    /events                       # SSE or WS: the live event stream (shape below)
```

Live event stream (TS core emits one event per state transition / gate result; the BFF
consumes and re-broadcasts to the browser over its own Axum WS). Use a versioned envelope
so the Rust client and TS server evolve independently:

```jsonc
{ "v": 1, "type": "feature_status_changed", "story_id": "...", "from": "EXECUTING", "to": "GATING", "at": "..." }
{ "v": 1, "type": "task_status_changed",    "story_id": "...", "task_id": "...", "role": "Frontend",
  "from": "EXECUTING", "to": "GATING", "at": "..." }
{ "v": 1, "type": "gate_result",            "task_id": "...", "rule_id": "ARCH-STRICT-LAYERING-1",
  "ruleset_hash": "...", "enforcement_kind": "deterministic-active",
  "kind": "post-task", "result": "fail", "message": "...", "file": "x.ts", "line": 42 }
{ "v": 1, "type": "gate_deny",              "task_id": "...", "rule_id": "ROLE-PATH-BOUNDARY-FE-1",
  "ruleset_hash": "...", "enforcement_kind": "deterministic-active",
  "kind": "hook", "blocked_tool": "Bash", "system_message": "..." }   // layer-1, no diff produced
{ "v": 1, "type": "gate_bounce",            "task_id": "...", "rule_id": "UI-IMAGE-COMPONENT-1",
  "ruleset_hash": "...", "enforcement_kind": "deterministic-active",
  "attempt": 1, "fix_suggestion": "use the shared <Image/> wrapper" } // layer-2 diff bounce
{ "v": 1, "type": "provenance_appended",    "task_id": "...", "role": "Backend",
  "ruleset_hash": "...", "session_id": "...", "rules_passed": ["ARCH-STRICT-LAYERING-1", "..."] }
{ "v": 1, "type": "clarification_required", "story_id": "...", "count": 3 }   // -> "needs you" badge
{ "v": 1, "type": "cost_tick",              "spent": 4.10, "credit_est": 100.0, "agents_live": 1 }
// `spent` is authoritative (computed from token counts). `credit_est` is the $100 Max 5x
// pool as an ESTIMATE only, until the June 15 2026 billing behavior is confirmed live (risk 16).
{ "v": 1, "type": "snapshot", "stories": [...], "tasks": [...], "feature_status": "EXECUTING" } // on (re)connect
```

The `snapshot` event is what the BFF pushes on every (re)connect so a reconnecting client
re-syncs FeatureStatus without missing transitions. The BFF owns heartbeat and
exponential-backoff reconnect to the TS core. The two gate layers are distinct event types
(`gate_deny` = layer-1 real-time deny, no diff; `gate_bounce` = layer-2 post-task diff
bounce), matching the verified two-layer gate.

The canonical FeatureStatus enum the UI renders: `INTAKE, INVESTIGATING,
AWAITING_CLARIFICATION, PLANNED, EXECUTING, GATING, AWAITING_QA, SIGNED_OFF, DONE, BLOCKED,
REJECTED`.

### Net plan delta to the engine

- No change to the engine. TS/Node core, Agent SDK, two-layer gate, sequential P0
  execution, metered-key auth all stand.
- Add ONE orchestrator task: expose the local HTTP API + event stream above (pure
  transport over deterministic state; zero LLM calls).
- Add ONE UI workstream: the Dioxus fullstack BFF + cockpit + dashboard, reusing chorale
  0.2.1 and the camerata-ai component patterns.

---

## 2. The Cockpit (the single steerable screen)

### Question

What is the one screen the human steers from, such that they never operate N windows?

### Recommendation

ONE Dioxus window with three durable regions: a persistent LEFT RAIL (the Story spine +
global FeatureStatus + the "NEEDS YOU" queue), a CENTER STAGE whose panel swaps by the
active Story's FeatureStatus (Intake -> Investigation -> Plan -> Live Status -> QA), and a
persistent RIGHT INSPECTOR that context-binds to whatever is selected in the stage.
Nothing opens a separate OS window; stage transitions are in-place panel swaps driven by
status, exactly as the curation GUI swaps its right pane by mode rather than opening
dialogs. Stage tabs are READ-ONLY status indicators, not free navigation, because the
engine owns state progression (you cannot QA before GATING completes).

### Full cockpit wireframe

```
+===========================================================================================================+
|  Camerata Orchestrator  ·  "Conductor"            [Story: Add CSV export to org members]   (o) EXECUTING  |
|  spent: $4.10  (~$100 Max 5x est)   ·   agents: 1 live   ·   conn: (o) Connected   ·   (!) 1 gate bounce   |
+===============+===========================================================+===============================+
| STORY SPINE   |  CENTER STAGE  (swaps by FeatureStatus)                   |  INSPECTOR / DETAIL PANE      |
| (left rail)   |                                                           |  (binds to stage selection)   |
|               |  [ INTAKE ][ INVESTIGATION ][ PLAN ][ STATUS ][ QA ]      |  Rule: ARCH-STRICT-LAYERING-1 |
| (o) Add CSV   |   stage tabs are READ-ONLY indicators, not free nav;      |  layer: api-layer             |
|   EXECUTING   |   active stage is driven by the engine's status.          |  enforcement:                 |
|               |                                                           |   (o) deterministic-active    |
| ( ) Fix tz    |  +-----------------------------------------------------+  |       (ESLint check exists)   |
|   SIGNED_OFF  |  |  << whichever stage panel is active renders here >> |  |                               |
|               |  |  (see panels in sections 2.1 - 2.6)                 |  |  Statement (directive):       |
| ( ) Invite    |  |                                                     |  |  "DB access only through the  |
|   BLOCKED (x) |  |                                                     |  |   repository layer."          |
|               |  |                                                     |  |  Rationale: ...               |
| + New story   |  |                                                     |  |                               |
|               |  +-----------------------------------------------------+  |  Alternatives (selectable):   |
| --- filter -- |                                                           |   ( ) repository-only         |
| [search ____] |  STATUS STRIP (always visible under the stage):           |   ( ) repository + raw-read   |
|               |  Backend (v)gated -> Frontend (o)exec -> Integrate (.)    |   (o) [current]               |
| NEEDS YOU (2) |  layer-1 PreToolUse: 1 deny   layer-2 post-task: 1 bounce  |  [ write your own... ]        |
|  - answer Q   |                                                           |                               |
|  - QA diff    |                                                           |                               |
+===============+===========================================================+===============================+
```

Top bar carries: Story title, live FeatureStatus, the cost meter, live
agent count, the BFF connection state (Connected / Reconnecting / Offline, so a dropped
live socket is visible), and the most urgent gate state.

**Cost meter behavior (TECH_DESIGN Q1):** Opus 4.8 at $5/$25 per 1M against the
operator's **$100 Max 5x** Agent SDK credit. The meter displays **`spent`** as the
authoritative figure (computed from token counts, always exact) and shows the **$100
credit as an estimate only** ("~$100 est") until the June 15 2026 billing behavior
(rollover, fail-closed-on-exhaustion) is confirmed live (risk 16). Do not block the meter
on that confirmation; ship `spent` now, label credit-remaining as an estimate.

### 2.1 Intake panel (FeatureStatus = INTAKE)

```
+-- CENTER STAGE: INTAKE -------------------------------------------------+
|  New story                                                             |
|  +------------------------------------------------------------------+  |
|  |  As an org admin I want to export the member directory to CSV    |  |
|  |  so I can reconcile it against payroll.                          |  |
|  +------------------------------------------------------------------+  |
|  target repo: [ /Users/.../agora-mono (v) ]   (brownfield, default)    |
|  [ later: or reference a tracker issue  AZ-1423 (v) ]  (greyed in V1)  |
|                                                                        |
|                                   [ Investigate > ]  -> INVESTIGATING  |
+------------------------------------------------------------------------+
```

One input box (VISION 2.1 / 4). The tracker-issue reference is a greyed affordance in V1
(VISION 18: native one-box stays Phase 0; WorkItemProvider is a later product axis).
Brownfield is the default (VISION 17). Pressing Investigate POSTs the Story and flips
FeatureStatus to INVESTIGATING; the panel auto-swaps. `created_by` is captured
actor-shaped (single user today, multi-user-ready).

### 2.2 Investigation panel (INVESTIGATING -> AWAITING_CLARIFICATION)

Two panels side by side over the codebase findings, with the RuleSet review surface below.
This is the front-loaded-judgment wedge (VISION 1.1).

```
+-- CENTER STAGE: INVESTIGATION ----------------------------------------------------+
|  Codebase findings  (Investigation.codebase_findings)                             |
|  - Member directory served by apps/api/.../members controller; repo layer present.|
|  - No existing CSV path; closest pattern is the XLSX export in reports/.           |
|  - Affected: api (export endpoint) + ui (download button). 2 roles.               |
|---------------------------------+-------------------------------------------------|
|  PRODUCT PANEL                  |  TECH PANEL                                     |
|  (product_questions[])          |  (tech_tradeoffs[]: option/pros/cons/recommend) |
|                                 |                                                 |
|  Q1 Include soft-deleted        |  Tradeoff: CSV generation site                  |
|     members?                    |   o Option A: server streams CSV                |
|     ( ) yes  ( ) no  [ ? ]      |       + handles 100k rows  - new endpoint        |
|     > [ your answer ______ ]    |   o Option B: client builds from existing JSON  |
|                                 |       + no API work    - breaks at scale        |
|  Q2 Which columns are PII-      |   * RECOMMENDATION: Option A (matches XLSX path)|
|     restricted?                 |     [ accept rec ]  [ pick A ]  [ pick B ]      |
|     > [ ______ ]                |                                                 |
|  [ Submit answers > ]           |  Tradeoff: column-set source ...                |
+-----------------------------------------------------------------------------------+
|  RECOMMENDED RULE SET   (Investigation.recommended_rule_set -> RuleSet)            |
|  camerata domain-group + checkbox + detail-pane pattern.   18 of 106 selected     |
|                                                                                    |
|  (v) api-layer (3)                        selection rationale shows in inspector   |
|     [x] ARCH-STRICT-LAYERING-1   (o)active    "feature adds a DB read path"        |
|     [x] ARCH-STRUCTURED-ERRORS-1 (-)declared  "export endpoint uses the wrapper"   |
|     [ ] ARCH-SERVER-AUTHZ-1      (o)active                                         |
|  (v) ui (2)                                                                        |
|     [x] UI-IMAGE-COMPONENT-1     (o)active                                         |
|     [x] UI-UTC-DATES-1           (o)active    "CSV timestamps must be SSR-safe UTC"|
|  (>) review-heuristic (12)                surfaced to you, not auto-gated          |
|                                                                                    |
|  (!) CONFLICTS (1): audit-column strategy -> [adopt+migrate][keep+except][synth]   |
|  (!) GAPS (1):      no rule covers CSV-injection escaping -> [ + add rule ]        |
|                                          [ Approve rule set > ]                    |
+-----------------------------------------------------------------------------------+
```

Engine-consistent specifics:

- Each rule row carries an enforcement badge with the THREE verified states (TECH_DESIGN
  Q5): `(o)active` = deterministic-active (a runnable shipping check exists), `(-)declared`
  = deterministic-declared (mechanical by declaration, no shipping check, degrades to
  human review, never auto-passed), `(o)review` = review-heuristic. The UI NEVER shows a
  `declared` rule as auto-enforced. `declared` and `review` are grouped on the
  human-surfaced side of the line, never with `active`.
- **RuleSet version + drift detection (REQUIRED, architect 2026-06-13 — the honest-
  enforcement promise is only as honest as the engine's `enforcement_kind` tagging).** The
  approved RuleSet carries a **version/hash** stamped at approval time. Every `gate_result`
  / `gate_deny` / `gate_bounce` / `provenance_appended` event carries that ruleset hash and
  the rule's `enforcement_kind` AS APPLIED. The cockpit compares the hash on incoming gate
  events against the RuleSet the human approved for that Story; if they diverge (a rule was
  promoted/demoted, added, or its `enforcement_kind` changed between investigation/approval
  and execution), the UI raises a **loud drift banner** ("the rules that ran are not the
  rules you approved") and routes the affected diff to human review rather than trusting the
  pass. A rule must never silently change enforcement class between approval and execution
  and have the UI report it as cleanly gated. This is enforcement applied to our own
  enforcement metadata; it is the gap a skeptic auditing Camerata would probe first.
- The deterministic-active set is MULTI-RULE, not a single anchor (TECH_DESIGN Q5
  downgrade: 3 to 6 shipping checks across API and UI layers). The "selected" column must
  comfortably render multiple active rules across layers.
- Checkbox + detail-pane is the camerata GUI verbatim; the human edits and OWNS the final
  active RuleSet (VISION 11). The detail pane (right inspector) renders each rule's
  rationale + selectable alternatives + a custom-rule textarea.
- Conflicts/gaps render with the three resolution options from TECH_DESIGN Q6 (adopt +
  migrate / keep + exception / synthesize variant) and a "+ add rule" gap action.

### 2.3 Clarify loop (architect-altitude feedback to the engine)

```
INVESTIGATING --(engine emits questions + tradeoffs + ruleset)--> AWAITING_CLARIFICATION
   human answers product_questions[]  +  picks one option per tech_tradeoff
   human approves/edits RuleSet (checkboxes + alternative selection)
   [ Submit answers ] + [ Approve rule set ]
        --> POST /clarifications + PUT /ruleset  (NOT an LLM call from the cockpit;
            the cockpit is a pure client of the deterministic core)
        --> orchestrator folds answers into Investigation, finalizes RuleSet,
            slices role rule_subsets, builds the Plan
   --> PLANNED
```

The human works only at architect altitude: answer free-text/choice questions, pick a
recommended-or-alternative tradeoff option, curate which rules are active. No code is
written until this gate passes (VISION 2.3). The picked tradeoff option is persisted on
the Investigation (`tech_tradeoffs[i].recommendation` accept vs an explicit override) so
the Plan and provenance can reference the decision.

**Dual routing of clarifications (VISION 3.5, the async bridge).** The clarify loop has two
faces, and the cockpit reflects the split:
- TECHNICAL tradeoffs and the RuleSet review are ALWAYS local to the Architect (architect
  altitude). They never leave the cockpit.
- PRODUCT clarifying questions (`product_questions[]`) can be answered locally by the
  Architect OR dispatched to a remote Product Owner through the work tracker. Each product
  question carries an optional `[ Ask in tracker ]` action that calls the orchestrator to
  post the question as a comment on the linked issue (@-mentioning the PO); the PO replies
  in their tracker, and the answer is ingested via the WorkItemProvider inbound path
  (WORKTRACKER_INTEGRATION §0.5) and lands back in this panel. While a question is
  out-for-answer, its row shows an `awaiting-PO` sub-state; the Story stays in
  `AWAITING_CLARIFICATION` until answered. The PO's comment (id/url/author) is recorded as
  the `human_decision` provenance source. This needs no PO-facing UI in Camerata: the PO's
  entire surface is their existing tracker. The bridge connects to whichever ONE tracker the
  Architect/PO primarily use (Jira, Azure DevOps, GitHub Issues, or the native tracker),
  chosen per project, not a fixed provider. Which concrete adapter ships first is a
  build-order pick (WORKTRACKER §0.5), not a product constraint.

### 2.4 Plan panel (FeatureStatus = PLANNED)

Dogfoods a chorale master/detail table over the Task DAG. Each Task row expands to its
scoped rule subset + path boundaries + depends_on. Shown and approvable BEFORE any agent
spawns.

```
+-- CENTER STAGE: PLAN -------------------------------------------------------------+
|  Task decomposition (Task[])     chorale grid: master/detail (detail_renderer)    |
|  +-+-----------+----------+------------------------+---------------+------------+  |
|  |v| Task      | Role     | path_boundaries (write)| depends_on    | rules      |  |
|  +-+-----------+----------+------------------------+---------------+------------+  |
|  |v| API export| Backend  | apps/api/**            | -             | 3 (api)    |  |
|  | |   detail: rule_subset = ARCH-STRICT-LAYERING-1 (o), ARCH-STRUCTURED-ERRORS-1 (-) |
|  | |           allowed_tools = [Read, Write, Edit, Bash(migrations)]               |
|  |v| UI button | Frontend | apps/ui/**             | API export    | 2 (ui)     |  |
|  | |   detail: rule_subset = UI-IMAGE-COMPONENT-1 (o), UI-UTC-DATES-1 (o)          |
|  | |           path-boundary: may NOT write apps/api/** or migrations/**          |
|  | |           layer-1 hook: deny Bash psql|pg_dump (ROLE-PATH-BOUNDARY-FE-1)      |
|  +-+-----------+----------+------------------------+---------------+------------+  |
|  contract handoff:  Backend emits api-contract.ts  ->  Frontend consumes (prompt) |
|  execution: SEQUENTIAL (Backend to completion + gate, then Frontend)  [engine fact]|
|                                                  [ Approve plan & execute > ]      |
+-----------------------------------------------------------------------------------+
```

Engine-consistent: the DAG is the two-node Backend -> Frontend chain (PHASE0_TASKS
out-of-scope: no DAG beyond this); execution is labeled SEQUENTIAL (TECH_DESIGN Q4); the
contract handoff (`api-contract.ts` copied into `.claude/coordination/`, passed as prompt
context, NOT a premature merge) is shown (VISION 12 / PHASE0_TASKS T7). The DB boundary is
a RULE on the Frontend role, not a third agent (PHASE0_TASKS T6: exactly two roles).

### 2.5 Live Status panel (EXECUTING -> GATING): the bounce-and-revise loop made visible

This panel proves "easier than five chat windows." It renders the SEQUENTIAL per-task /
per-agent / per-gate progress and makes the gate-fail bounce a first-class visible event
(PHASE0_TASKS T8/T9 thesis moment). Rendered as a sequential timeline, NOT parallel
swimlanes (TECH_DESIGN Q4).

```
+-- CENTER STAGE: LIVE STATUS -------------------------------------------------------+
|  SEQUENTIAL TIMELINE                                                               |
|                                                                                   |
|  (1) Backend  (task-backend worktree)                              (v) SIGNED off  |
|      |- agent_session: sess_8f2a   model: opus-4.8                                 |
|      |- layer-1 PreToolUse: (no denies)                                            |
|      |- layer-2 post-task gate: ESLint (v)  ts-morph (v)  build (v)                |
|      '- produced_diff: +124 / -3   gate_results: [ARCH-STRICT-LAYERING-1 PASS]     |
|                                                                                   |
|  (2) Frontend (task-frontend worktree)                             (-) GATING      |
|      |- agent_session: sess_b71c   model: opus-4.8                                 |
|      |- layer-1 PreToolUse:  (x) DENY  Bash `psql ...`                             |
|      |     rule ROLE-PATH-BOUNDARY-FE-1  ·  systemMessage delivered  ·  call blocked|
|      |     (real-time: NO diff was ever produced for this call)                    |
|      |- layer-2 post-task gate:                                                    |
|      |     (x) FAIL  UI-IMAGE-COMPONENT-1  at MemberTable.tsx:42  (next/image ban)  |
|      |       (rev) BOUNCED to sess_b71c with rule id + file:line + fix suggestion   |
|      |       ... agent revising (attempt 2) ...                                     |
|      |     (v) PASS on re-run                                                       |
|      '- produced_diff: +57 / -1   gate_results: [UI-IMAGE-COMPONENT-1 PASS(after rev)]|
|                                                                                   |
|  (3) Integrate  (merge in dependency order, re-gate)               (.) pending     |
+-----------------------------------------------------------------------------------+
```

The two gate layers are visually distinct (TECH_DESIGN Q2 / section 1) and rendered as
DEFENSE-IN-DEPTH, not strict either/or per violation:

- Layer-1 is a real-time DENY that blocks a tool call and produces NO diff. The
  model-visible `systemMessage` is shown (the verified Q2 correction: `permissionDecision
  Reason` is audit-only, `systemMessage` is the model-visible channel).
- Layer-2 is a post-task structural FAIL that bounces the produced diff with `rule_id +
  file:line + fix suggestion`, with revise attempts converging to PASS.
- Because the gate is defense-in-depth (TECH_DESIGN section 1, "even if a real-time deny
  is ever bypassed, the post-task structural check still catches the violation"), a
  violation can surface at layer-2 even after passing layer-1. The panel does NOT treat a
  layer-1 pass as proof the diff is clean.

The `(rev)` bounce marker and the attempt counter make the "violated rule returned to the
agent" loop visible (VISION 2.5). Transport: these events arrive over the BFF WebSocket
via a `use_coroutine` pushing into a `Signal<Vec<TaskStatus>>`; the cockpit re-renders
reactively. No polling loop, no Dioxus streaming server functions (verified broken on
desktop).

### 2.6 QA / review panel (AWAITING_QA -> SIGNED_OFF / REJECTED)

The governed diff with a PROVENANCE LINE per change and approve/reject controls. Dogfoods
a chorale grid with row-aware renderers (RowCellRenderer) for the per-hunk provenance and
an action column.

```
+-- CENTER STAGE: QA -----------------------------------------------------------------+
|  Governed diff for review        approve per-hunk, then sign off the Story          |
|                                                                                    |
|  apps/api/.../membersExport.ts                                                     |
|  +  export async function exportMembersCsv(orgId) {                                |
|  +     const rows = await memberRepository.listForExport(orgId)   // repo layer (v)|
|  +     return toCsv(rows)                                                           |
|  PROVENANCE  task_id=task-backend · role=Backend · session=sess_8f2a               |
|             rules_passed=[ARCH-STRICT-LAYERING-1, ARCH-STRUCTURED-ERRORS-1]         |
|             human_decision = ( )approve ( )reject   [ approve hunk ]               |
|  ----------------------------------------------------------------------------------|
|  apps/ui/.../MemberTable.tsx                                                        |
|  +  <DownloadCsvButton onClick={...} />                                            |
|  PROVENANCE  task_id=task-frontend · role=Frontend · session=sess_b71c             |
|             rules_passed=[UI-IMAGE-COMPONENT-1, UI-UTC-DATES-1]  (UI-IMAGE after rev)|
|             human_decision = ( )approve ( )reject   [ approve hunk ]               |
|  ----------------------------------------------------------------------------------|
|  SURFACED FOR YOUR JUDGMENT (not auto-gated):                                       |
|   (-) ARCH-STRUCTURED-ERRORS-1 (deterministic-declared: no shipping check, review)  |
|   (o) <review-heuristic rules from the active set>                                  |
|                                                                                    |
|                         [ Reject story ]            [ Sign off story > SIGNED_OFF ] |
+------------------------------------------------------------------------------------+
```

The provenance line is exactly the VISION section 8 Provenance entity `{task_id, role,
agent_session_id, rules_passed[], human_decision}` (PHASE0_TASKS T11: one line per change,
no audit store in V1). The "surfaced for your judgment" block is the honest-enforcement
requirement: `deterministic-declared` + `review-heuristic` rules are SHOWN to the human,
never claimed as auto-passed (TECH_DESIGN Q5, PHASE0_TASKS T12). Sign-off writes
`human_decision` (actor-shaped) and flips the Story to SIGNED_OFF -> DONE; reject flips to
REJECTED. Both go through a `ConfirmBanner` (irreversible-ish action).

### Cockpit component list (Dioxus, camerata family)

Persistent shell:
- `CockpitShell` - three-region layout (left rail / center stage / right inspector). Same
  state idiom as the curation GUI (`use_signal` / `use_effect` / `with_mut` / `.peek()`),
  but decomposed into `#[component]` functions (portfolio model), NOT one monolithic
  `app()` fn. Global state (selected story, theme, BFF connection status) lives in
  `contexts/` providers (`use_context_provider`), not flat signal soup.
- `TopBar` - story title, FeatureStatus badge, cost-vs-Max-credit meter, live agent count,
  connection-state pill, urgent-gate indicator.
- `StorySpineRail` - Story list with FeatureStatus badges; the "NEEDS YOU" judgment queue;
  new-story button; the dual-axis search filter (reuse the curation GUI's filter input).
- `Inspector` - context-binding detail pane; reuses the curation GUI's rationale +
  selectable-alternatives + custom-rule sub-components verbatim.

Stage panels (swap by FeatureStatus):
- `IntakePanel`, `InvestigationPanel` (which contains `CodebaseFindings`,
  `ProductQuestionsPanel`, `TechTradeoffsPanel`, `RuleSetReviewSurface`), `PlanPanel`
  (chorale master/detail), `LiveStatusPanel` (`SequentialTimeline` -> `TaskCard` ->
  `GateLayerRow` -> `BounceLoopIndicator`), `QaPanel` (chorale RowCellRenderer over diff
  hunks + `ProvenanceLine` + sign-off/reject).

Shared:
- `EnforcementBadge` - the three states `(o)active / (-)declared / (o)review`.
- `StatusBadge` - the 11 FeatureStatus states (label + icon + color; see section 7 on why
  color alone cannot distinguish all 11).
- `ConfirmBanner` - the curation GUI's banner-confirm pattern, reused for sign-off /
  reject / plan-approve.
- `useOrchestrator` (a `use_coroutine`) - owns the BFF WebSocket; pushes events into the
  Story/Task/Gate signals; exposes command senders (submitStory, submitAnswers,
  approveRuleSet, approvePlan, signOff, reject) via server-function-backed reqwest calls.

### Per-surface data-binding table

| Cockpit surface | VISION section 8 entity.field(s) | Direction | Notes |
|---|---|---|---|
| TopBar status badge | `FeatureStatus` (roll-up) | read (WS) | 11 canonical states |
| TopBar cost meter | engine cost telemetry (`cost_tick`) | read (WS) | Opus 4.8 $5/$25, Max credit pool (Q1) |
| TopBar connection pill | BFF socket state | local | Connected / Reconnecting / Offline |
| StorySpineRail rows | `Story.{id,title,status}` | read (WS) | multi-feature roll-up; one in P0 |
| StorySpineRail "NEEDS YOU" | derived: `status in {AWAITING_CLARIFICATION, AWAITING_QA, BLOCKED}` | read | pulls human in only at judgment |
| IntakePanel input | `Story.{title,description,created_by}` | write (HTTP) | one box; `created_by` actor-shaped |
| IntakePanel repo selector | `Story` target repo (brownfield default) | write | VISION 17 |
| InvestigationPanel findings | `Investigation.codebase_findings` | read (WS) | |
| ProductQuestionsPanel | `Investigation.product_questions[]` | read + write (HTTP) | architect-altitude answers |
| TechTradeoffsPanel | `Investigation.tech_tradeoffs[]{option,pros,cons,recommendation}` | read + write (HTTP) | accept-rec or override |
| RuleSetReviewSurface | `RuleSet` (ids + rationale) + `Rule.{id,category,scope,statement,enforcement_kind}` | read + edit (HTTP) | camerata checkbox+detail |
| EnforcementBadge | `Rule.enforcement_kind` (3 states) | read | active / declared / review (Q5) |
| Conflicts/Gaps | RuleSet conflicts + gaps (Investigation output) | read + resolve (HTTP) | three options (Q6) |
| PlanPanel grid | `Task.{id,role,description,depends_on,worktree,status}` + `Role.{path_boundaries,allowed_tools,rule_subset}` | read + approve (HTTP) | chorale master/detail; sequential |
| Contract handoff line | coordinator artifact (api-contract.ts) | read | VISION 12 (coordination fact, not a section 8 entity) |
| LiveStatus task card | `Task.{status,produced_diff,gate_results,worktree}` + `Provenance.agent_session_id` | read (WS) | per-task/per-agent |
| LiveStatus layer-1 row | `Gate/Check.{rule_id,kind:hook,result,message}` | read (WS) | real-time deny; no diff |
| LiveStatus layer-2 bounce | `Gate/Check.{rule_id,kind:post-task,result:fail,message}` | read (WS) | bounce-and-revise |
| QaPanel diff hunks | `Task.produced_diff` | read (WS) | chorale RowCellRenderer |
| ProvenanceLine | `Provenance.{task_id,role,agent_session_id,rules_passed[],human_decision}` | read + write (HTTP) | one line per change (T11) |
| QaPanel surfaced rules | `Rule.enforcement_kind in {deterministic-declared, review-heuristic}` | read | honest non-auto-gated surface |
| Sign-off / reject | `Provenance.human_decision` + `Story.status` -> SIGNED_OFF/REJECTED | write (HTTP) | ConfirmBanner |

---

## 3. The Dashboard (the multi-feature roll-up and the human's action inbox)

### Question

When more than one Story is in flight, what is the multi-feature surface, and how does the
human drill into a single feature's cockpit?

### Recommendation

A dashboard with a left nav rail, a top status strip, and a main pane that swaps between
four surfaces: the FEATURE table, the ROLE-AGENT view, the GATE view, and the always
visible NEEDS-ATTENTION queue. All four are chorale-dioxus tables (the dogfood). A feature
row drills into its single-pane cockpit (section 2) via `on_row_click`; an inline chevron
expands the feature-to-tasks sub-table without leaving the dashboard.

### Dashboard wireframe

```
+------------------------------------------------------------------------------+
| Camerata Orchestrator    [Features][Agents][Gates]   (o) running 3  (!) attn 2|
+--------------------+---------------------------------------------------------+
|  NAV               |  FEATURE TABLE  (chorale)                               |
|  > Features        |  +---------------------------------------------------+  |
|    Agents          |  | v | Story        | Status      | Phase | Agents |..|  |
|    Gates           |  | > | Add SSO      | EXECUTING   | Exec  |  BE FE |..|  |
|    Needs Attention |  |   |  (detail: task sub-table via detail_renderer) |  |
|                    |  | > | Export CSV   | AWAITING_QA | QA    |   -    |(!)|  |
|  ATTENTION (2)     |  +---------------------------------------------------+  |
|  (!) Export CSV: QA|                                                         |
|  (!) Add SSO: clar |  Drill-down: on_row_click(RowId) -> Feature Cockpit     |
+--------------------+---------------------------------------------------------+
```

`on_row_click: Option<Callback<RowId>>` (the verified exact prop type; default None) drives
drill-down from a feature row into its single-pane cockpit. Note the verified firing
exclusions: it does NOT fire on selection-checkbox clicks, the detail-expander chevron,
cells in edit mode, group-header rows, or Ctrl/Cmd/Shift-modified clicks. The chevron
column (`detail_renderer`) gives an inline expand to the task sub-table without leaving the
dashboard; the full cockpit is the click-through. Do not make the SAME feature-row cell
both an `on_row_click` drill-down target AND inline-editable (verified double-click
interaction caveat); route any in-table edit through a dedicated action cell.

### 3.1 Feature table (one row per Story)

| Column | Source field | Chorale render |
|---|---|---|
| expander | (detail) | chevron column (`detail_renderer`) -> task sub-table |
| Story | `Story.title` | text; `on_row_click` -> cockpit |
| Status | `FeatureStatus` | status badge (built-in `RenderKind::Badge` + `BadgeVariantMap`; color family per section 7 mapping) |
| Phase | derived (intake/investigate/plan/execute/gate/qa) | badge |
| Active roles | `Task.role` where status active | `RowCellRenderer` chip list (BE / FE) |
| Gates pass/fail | count over `Task.gate_results[]` | `RowCellRenderer` "12 / 1" (green/red split) |
| Needs attention | derived flag | `RowCellRenderer` "!" when status in {AWAITING_CLARIFICATION, AWAITING_QA, BLOCKED} |
| Updated | derived timestamp | Date render kind |

Detail sub-table (`detail_renderer`, master/detail) = the TASK table for that Story:
`Task.id`, `Task.role`, `Task.status`, `depends_on[]`, `Task.worktree`, gate pass/fail.
This is the verified feature-to-tasks drill.

### 3.2 Role-Agent view (one row per live agent session)

| Column | Source | Chorale render |
|---|---|---|
| Role | `Role.name` / `Task.role` | badge |
| Task | `Task.id` / description | text |
| Worktree | `Task.worktree` | mono text |
| Status | `Task.status` | status badge |
| Last tool activity | live (agent session) | `RowCellRenderer` (text + spinner when active) |
| Last gate result | latest `Gate.result` + message | `RowCellRenderer` pass/fail pill |
| Session | `Provenance.agent_session_id` | mono, truncated |

This is the most update-intensive view (tool-activity ticks), so it is where the
reconcile-loop debounce matters most (section 4 / section 5). Note: a running agent LOG
that tails live output is a SEPARATE non-table component, not a table cell. A table is the
wrong primitive for streaming log tail; chorale renders the grid state, not the log.

### 3.3 Gate view (one row per Gate/Check firing)

| Column | Source (Gate/Check) | Chorale render |
|---|---|---|
| Task | `Gate.task` (via Task) | text |
| Rule | `Gate.rule_id` | mono badge |
| Kind | `Gate.kind` (hook / post-task) | badge (layer-1 vs layer-2) |
| Result | `Gate.result` (pass / fail) | `RowCellRenderer` pass/fail pill |
| Message | `Gate.message` | text (truncate + title) |
| Bounce | derived (fail -> revise events) | `RowCellRenderer` counter |

Grouping + aggregation shines here: group by `rule_id` or by Task, aggregate count of pass
vs fail in the group header (the v0.2.1 fix made group-header aggregates actually render).
That is the governance roll-up ("which rule fails most, which task bounced most").

### 3.4 Needs-attention queue (the human action inbox)

| Column | Source | Chorale render |
|---|---|---|
| Kind | derived (Clarification / QA sign-off / Block) | badge |
| Story | `Story.title` | text |
| Detail | product_question / blocked reason / QA summary | text |
| Age | derived | Date/duration |
| Action | derived | `RowCellRenderer` action cell (Answer / Sign off / Resolve button) |

Filter to status in {AWAITING_CLARIFICATION, AWAITING_QA, BLOCKED}. The action cells are
exactly the `RowCellRenderer` action-column use case the chorale CHANGELOG calls out (they
need the row id + sibling fields to route the click).

---

## 4. Data flow (end to end)

```
Cockpit/Dashboard (Dioxus, in the BFF process)        Orchestrator (TS/Node, zero LLM calls)
---------------------------------------------         --------------------------------------
IntakePanel: submitStory(text,repo) --server-fn-->    POST /stories: create Story{status:INTAKE}
   (BFF proxies via reqwest to TS core)               run brownfield onboard + investigation
   FeatureStatus <--BFF WS (event)--------------       Story.status = INVESTIGATING
InvestigationPanel renders findings /                  emit Investigation{questions, tradeoffs,
   questions / tradeoffs / ruleset <--BFF WS----         recommended_rule_set}; status=AWAITING_CLARIFICATION
human answers + picks + edits ruleset
   submitAnswers + approveRuleSet --server-fn-->       POST /clarifications + PUT /ruleset:
                                                       fold answers; finalize RuleSet;
                                                       slice role rule_subsets; build Plan
PlanPanel renders Task[] <--BFF WS------------         status = PLANNED
human approvePlan --server-fn-->                       POST /run: spawn Backend session
                                                       (cwd=worktree, allowed_tools, hook attached,
                                                        ANTHROPIC_API_KEY auth)
LiveStatusPanel streams <--BFF WS (per event)-         status=EXECUTING; layer-1 gate_deny +
   task/agent/gate deltas, bounce loop                  layer-2 gate_bounce events; status=GATING
QaPanel renders governed diff + provenance <--WS       status = AWAITING_QA
human approve/reject hunks; signOff --server-fn-->     POST /qa: write Provenance.human_decision;
                                                       status = SIGNED_OFF -> DONE (or REJECTED)
```

Direction discipline: the cockpit issues COMMANDS over HTTP (Dioxus server functions ->
BFF reqwest -> TS core REST) and consumes EVENTS over ONE WebSocket (TS core /events -> BFF
subscription -> BFF Axum WS -> `use_coroutine` -> signals -> chorale re-render). The cockpit
NEVER makes a model call; that invariant lives entirely in the orchestrator's
`agents/session.ts`. This keeps the verified responsibility boundary intact: the front end
is a pure client of the deterministic core.

FeatureStatus is the spine that ties the flow to VISION section 8: every transition in the
left column is an event whose `to` field is one of the 11 canonical states, and the cockpit
stage panel + dashboard status badge both render off that single roll-up.

---

## 5. Dogfooding chorale

### Question

Should the four dashboard surfaces (and the Plan / QA grids in the cockpit) be built on
chorale-dioxus v0.2.1, dogfooding it, or on a hand-rolled / third-party table?

### Recommendation

YES, dogfood chorale for every collection view. Every column-schema need maps cleanly onto
VERIFIED, shipping v0.2.x capabilities. The build becomes its own showcase. The one
load-bearing caveat is the LIVE-UPDATE path, which is met today via a poll-and-reconcile
loop (no chorale change needed); the missing native bulk-row transition and a cell-flash
primitive are dogfood-generated chorale roadmap items, NOT adoption blockers.

Pin the dependency to `=0.2.1` exactly: the group-header aggregate render fix and the
corrected `detail_renderer` Callback signature both landed in 0.2.1. Resolve the version
from `Cargo.toml` or crates.io, NOT from a git tag (verified: there is no `v0.2.1` git tag;
the release published from commit `20a30f1`; the crates.io upload date is 2026-06-13 while
the CHANGELOG/commit are dated 2026-06-12).

### Chorale capability-to-need mapping (all verified against the live v0.2.1 source)

| Dashboard need | Chorale v0.2.x capability | Fit |
|---|---|---|
| Status badges by FeatureStatus | built-in `RenderKind::Badge` + `BadgeVariantMap` (zero custom code) | FIT |
| Gate pass/fail composite cell | `RowCellRenderer<TRow> = Arc<dyn Fn(&TRow,&CellValue)->Element>` | FIT |
| Action buttons (Answer/Sign off/Resolve) | `RowCellRenderer` action column (reads row id + siblings) | FIT |
| Feature-row drill-down to cockpit | `on_row_click: Option<Callback<RowId>>` | FIT (verified exclusions, section 3) |
| Feature -> tasks sub-table | `detail_renderer: Option<Callback<TRow, Element>>` (24px chevron, full-width row) | FIT |
| Gate roll-ups by rule/task | grouping (`set_grouping`) + `AggregatorKind`; group-header aggregates render (0.2.1) | FIT |
| Sort/filter the queues | `sort_enabled` / `filter_enabled` | FIT |
| Bulk QA sign-off | `selection_enabled` + selection toolbar | FIT |
| Pin Status / Story columns | `FrozenSide` (frozen columns) | FIT |
| Light/dark to match cockpit | `Theme` enum {Light,Dark,Custom} + ~38 `--chorale-*` tokens | FIT |
| Large agent/gate history scroll | row virtualization (fixed + variable height) | FIT |
| i18n labels | `Labels` struct | FIT |
| CSV / XLSX export | `to_csv` / `to_xlsx` (XLSX behind the `xlsx` Cargo feature) | FIT |
| **Live row updates (status/gate change in place)** | `update_row(RowId, new_row)` bumps `data_generation`; `view_key` tracks it -> re-render | FIT via reconcile (see gap) |
| **Rows appearing/disappearing** | NO bulk transition; rebuild `TableState` + `.set()` the signal; `view_key` tracks `rows.len()` | GAP, workaroundable |
| Cell flash on change | none | GAP, adapter workaround or feature request |

Status badges use the built-in `RenderKind::Badge` path (zero custom code), reserving
`RowCellRenderer` for the genuinely row-aware cells: gate-result chips that aggregate a
`Vec<Check>`, and the approve/reject/open-worktree action column that needs `task_id` +
worktree path from sibling fields.

### The live-update gap, precisely (verified in source)

`chorale-core` ships exactly ONE row-data mutation: `update_row(state, row_id, new_row)`
(`transitions.rs:424`), which `Arc::make_mut`s the rows vec, replaces the slot by RowId,
and bumps `data_generation`. There is NO `set_rows` / `insert_row` / `remove_row` /
`append_rows`. The dioxus `view_key` memo (`components.rs:465`) keys on both
`s.data_generation` (so a same-length content edit re-renders) and `s.rows.len()` (so an
add/remove re-renders). `use_table(init)` runs `init` once on mount; adding/removing rows
means building a fresh `TableState` and `.set()`-ing it onto the handle's signal.

Therefore the supported live-dashboard pattern (no chorale change):

```
// Dioxus side: a use_coroutine (the BFF WS consumer) writes the latest snapshot into a
// signal; a reconcile step converts a snapshot into chorale transitions:
//   for each row in new_snapshot:
//     if row_id exists and content changed -> handle.update_row(id, new)
//     if row_id is new                     -> rebuild needed (count changed)
//   if any add OR remove:
//     rebuild TableState from new rows, CARRY FORWARD sort/filter/selection/scroll,
//     then handle.signal().set(new_state)
```

Two real costs to bake into the dashboard adapter:

1. The reconcile helper must carry sort / filter / selection / scroll / expansion forward
   across a rebuild, or the human's view resets on every feature add. This is the single
   most material adapter cost. It is consumer code, not a chorale blocker.
2. `view_key` does NOT track row content beyond `data_generation`, so a content-only
   `update_row` that should ALSO re-sort/re-filter the visible window (e.g. a status flip
   that should re-order the row) will not re-run the filter+sort+paginate pipeline until
   the next sort/filter/page transition (a documented chorale known limitation). When a
   content change crosses a sort/filter boundary, the reconcile loop should follow it with
   a sort/filter/page re-apply (or fold the change into a rebuild).

Debounce/coalesce ticks (a 1 to 3s reconcile) rather than firing per-event `update_row`,
because the role-agent view can emit many tool-activity ticks per second; optionally batch
a tick's changes into a single rebuilt `TableState` `.set()` (one signal write per tick) if
per-row writes prove too chatty at fleet scale. The live event source is the BFF WS
(push); a plain poll over the BFF is the simpler fallback and matches the reconcile model
exactly.

### Gaps surfaced as chorale roadmap items (dogfood-driven, not blockers)

- `reconcile_rows(new_rows)` / `set_rows(new_rows)`: a transition that diffs by RowId,
  preserves selection/sort/scroll/expansion, and bumps `data_generation` once. Removes the
  most material adapter cost above; the single highest-value addition for any live consumer.
  Build the reconcile helper in the dashboard adapter first; promote it into chorale-core
  once its shape settles.
- A transient row/cell FLASH primitive (highlight on `data_generation` change or a per-row
  `updated_at` marker) for "this row just changed" feedback. Today it is a CSS-transition
  workaround via a `data-` attribute or a `RowCellRenderer` keyed on `updated_at`.

These are exactly the gaps the dogfood is meant to surface: the orchestrator dashboard is
chorale's first genuinely LIVE consumer, so it stress-tests the one axis chorale has not
yet built for. The build is its own showcase AND its own roadmap driver.

### Verification honesty note

The `update_row` mechanism and the absence of a bulk transition are confirmed from source.
What was NOT done: running a live Dioxus harness streaming updates into a chorale table at
a high update rate. Flashing / cell-level render cost under many ticks per second is
untested; the 1 to 3s reconcile debounce is the mitigation, and a perf spike should
confirm it before the role-agent view is finalized.

---

## 6. Data model and FeatureStatus mapping

The cockpit and dashboard render VISION section 8 entities verbatim: Story, Investigation
(codebase_findings, recommended_rule_set, product_questions[], tech_tradeoffs[] each
{option, pros, cons, recommendation}), Rule, RuleSet, Role, Task (role, depends_on[],
worktree, status, produced_diff, gate_results[]), Gate/Check (rule_id, kind, result,
message), Provenance (task_id, role, agent_session_id, rules_passed[], human_decision),
FeatureStatus.

The one schema refinement from TECH_DESIGN Q5 that the UI must honor: `Rule.enforcement_kind`
has THREE runtime states, not two: `deterministic-active`, `deterministic-declared`,
`review-heuristic`. The UI renders all three (`EnforcementBadge`), and `deterministic
-declared` is ALWAYS grouped with `review-heuristic` on the human-surfaced side, never with
`deterministic-active` on the auto-gated side. This is the honesty invariant: the UI must
never claim enforcement that does not exist.

The 11 canonical FeatureStatus states drive the cockpit stage selector and every status
badge: INTAKE, INVESTIGATING, AWAITING_CLARIFICATION, PLANNED, EXECUTING, GATING,
AWAITING_QA, SIGNED_OFF, DONE, BLOCKED, REJECTED.

---

## 7. Shared style / theme

### Question

How do the curation GUI, the cockpit chrome, and the chorale dashboard tables share one
coherent look (the "same family" requirement) with light/dark support?

### Recommendation

Adopt chorale's `--chorale-*` CSS-variable token contract as the single source of truth for
the whole cockpit, NOT camerata's inline hex strings. Author one app-level token sheet that
RESTATES chorale's palette at app scope so panes OUTSIDE chorale tables theme consistently.

### Why and the mechanics (verified against chorale v0.2.1 `chorale-core/src/theme.rs`)

- Chorale ships a complete token contract (~38 distinct `--chorale-*` tokens; chorale's own
  docs and CHANGELOG say "~39", and the exact source count is 38, so use "~38" or
  chorale's "~39" hedge, never a hardcoded 39) defined TWICE (light + dark) under
  `.chorale-root[data-chorale-theme="light"|"dark"]`. A parity test
  (`every_token_is_defined_in_both_theme_blocks`) enforces that both blocks declare the
  identical token set. Runtime light/dark is a single attribute swap, no stylesheet
  re-injection. Token groups: surface/text (`surface`, `text`, `text-muted`, `text-subtle`,
  `text-disabled`), structure (`border`, `divider`, `header-bg`, `toolbar-bg`), rows
  (`row-bg`, `row-hover-bg`, `row-selected-bg`), accent (`accent`, `accent-contrast`,
  `accent-strong`, `active-cell-outline`), inputs/popovers (`input-bg`, `input-border`,
  `popover-bg`, `popover-shadow`), buttons, groups, `error`, and the FIVE badge pairs
  `badge-{green,yellow,red,gray,default}-{bg,text}`.

- The catch (verified from the scoping selector): chorale's tokens are scoped under
  `.chorale-root`, so they are NOT automatically visible to panes outside chorale tables.
  The shared app-level sheet must RESTATE chorale's palette as app-scope `:root` (or
  `.orchestrator-root`) variables so the cockpit shell, the camerata-style detail pane, and
  the chorale grids all read the same palette. Authoring this shared sheet is net-new (it
  did not exist in either repo). Confirm at authoring time whether chorale exports its
  palette for non-table use; if not, restate the same hex values at app scope.

- Migrate camerata's inline hexes onto tokens: `#1452a3` / `#5b8def` -> `--chorale-accent`
  / `--chorale-accent-strong`; `#dce9ff` selected-card -> `--chorale-row-selected-bg`;
  `#ddd` borders -> `--chorale-border`; `#f3f3f3` / `#fafafa` group/toolbar ->
  `--chorale-header-bg` / `--chorale-toolbar-bg`; banner colors -> the badge pairs. The
  cockpit is stylesheet-class-driven (PORT-CSS-1 discipline from the portfolio UI), NOT
  inline-string-driven; inline styles are reserved only for incidental one-off positioning.
  This also fixes the one real camerata liability (121 inline-hex callsites, zero token
  layer, which does not theme and does not scale to a multi-pane cockpit).

### FeatureStatus -> badge-color-family mapping (consumer-owned)

Chorale provides FIVE badge color families, but there are ELEVEN FeatureStatus states, so
the mapping is many-to-five and the states are NOT 1:1 distinguishable by color alone. The
`StatusBadge` component MUST carry a label and an icon in addition to color. Chorale
provides the themed color pairs; the orchestrator owns this status-to-family table:

| FeatureStatus | Badge family |
|---|---|
| SIGNED_OFF, DONE | green |
| INVESTIGATING, EXECUTING, GATING, AWAITING_QA | yellow |
| BLOCKED, REJECTED | red |
| INTAKE, PLANNED, AWAITING_CLARIFICATION | gray / default |

Gate pass/fail maps to green/red badges in a chorale `RowCellRenderer` column.
Enforcement badges map: `deterministic-active` -> green/accent, `deterministic-declared` ->
gray (with an explicit "review" label so it never reads as enforced), `review-heuristic` ->
gray/default. Light/dark comes free for the whole cockpit from the one shared sheet + the
`data-chorale-theme` attribute swap; the cockpit shell exposes the same light/dark toggle so
the curation GUI, the cockpit chrome, and the chorale grids share one palette.

---

## 8. Why this design does not weaken the verified engine

- The core stays TS/Node making zero LLM calls. The cockpit is a separate Dioxus process
  (inside the BFF) and a pure client: commands over HTTP, events over one WebSocket. No
  model call ever originates in the UI.
- The two-layer gate is rendered AS two layers: layer-1 real-time deny (no diff, with the
  model-visible `systemMessage`) vs layer-2 post-task bounce (diff returned), as
  defense-in-depth, not strict either/or.
- Sequential Phase 0 execution is rendered as a sequential timeline, not parallel
  swimlanes (Q4).
- Three-state enforcement is rendered honestly: `deterministic-declared` rules are surfaced
  to the human, never shown as auto-passed (Q5).
- Metered-key + Max-credit cost is surfaced in the TopBar meter (Q1), replacing the
  withdrawn OAuth thesis.
- The engine-first build ORDER is respected: this is the P3 cockpit/dashboard target,
  built after the P0 CLI harness proves the gate (PHASE0_TASKS T9). Building it earlier
  would violate the order, even though it is full-V1 scope.
- No Rust rewrite of the core (no official Rust Agent SDK), no React rewrite of the front
  end (no chorale React adapter). The BFF absorbs the unavoidable cross-language seam.

---

## 9. Unverified assumptions and open risks

Honesty over confidence. Items the adversarial verdicts did NOT confirm, plus anything that
could not be checked.

### A. Claims adopted with a stated FALLBACK or CORRECTION (not the original wording)

1. **Chorale token count is 38, not 39.** Verified exact count in
   `chorale-core/src/theme.rs` is 38 distinct `--chorale-*` tokens; chorale's own docs say
   "~39". Use "~38" or chorale's "~39" hedge; never hardcode 39.
2. **`on_row_click` is `Option<Callback<RowId>>`, not a bare `Callback<RowId>`.** Optional,
   default None, with documented firing exclusions (checkbox, chevron, edit-mode cells,
   group-header rows, modified clicks).
3. **`detail_renderer` is `Callback<TRow, Element>`** (corrected from an earlier
   `EventHandler` mislabel in the 0.2.0 CHANGELOG; the live source agrees it is Callback).
4. **No `v0.2.1` git tag exists.** Resolve chorale's version from `Cargo.toml` or
   crates.io, not a git tag. crates.io upload date is 2026-06-13; CHANGELOG/commit say
   2026-06-12.
5. **"Rust is explicitly unsupported by the Agent SDK" is overstated.** Rust is ABSENT from
   every official SDK list (Agent SDK = Python + TS only); unofficial community Rust crates
   exist. The rejection of a Rust core stands and is strengthened (betting governance on an
   unofficial wrapper is worse than the verified TS core), but the framing is "absent / not
   officially supported," not "explicitly denied."
6. **`permissionDecisionReason` does NOT reliably reach the model** (engine-side, already in
   TECH_DESIGN). The UI renders the layer-1 deny's `systemMessage` (the model-visible
   channel) in the Live Status panel, not `permissionDecisionReason`.

### B. Confirmed mechanisms with caveats that constrain the build

7. **Chorale has NO bulk row-set transition.** Live updates use single-row `update_row`
   plus full `TableState` rebuild on add/remove. The reconcile helper that carries
   sort/filter/selection/scroll/expansion across a rebuild is real, unbuilt adapter code
   and the single most material dashboard cost. `view_key` does not track row content beyond
   `data_generation`, so a status-flip that should re-sort needs a follow-up
   sort/filter/page re-apply.
8. **Live-update render cost at high tick rate is UNTESTED.** No live Dioxus harness
   streaming many updates per second into a chorale table was run. Mitigation: 1 to 3s
   reconcile debounce. A perf spike should confirm before the role-agent view is finalized.
9. **Dioxus desktop streaming server functions are broken** (issue #3694, closed
   not-planned). The BFF design avoids this by using a plain WebSocket the BFF owns, driven
   by `use_coroutine` (NOT the built-in `use_websocket` hook). If a future build collapses
   the BFF into a single desktop binary, the live leg must ride that path.
10. **`use_coroutine` WebSockets do not auto-reopen after a drop** (Dioxus discussions
    #1721 / #3004). The BFF (and any desktop fallback) must own explicit reconnect-with
    -backoff + heartbeat + snapshot-on-connect, and surface a visible connection-state
    signal (Connected / Reconnecting / Offline). This is unbuilt and untested here.

### C. Net-new surfaces with no existing precedent in either repo (highest build risk)

11. **The TS core local HTTP API + event stream does not exist yet.** PHASE0_TASKS T0-T14
    are CLI-only. This is net-new orchestrator work (a small `node:http` / Express / Fastify
    server + an event emitter bridged to SSE/WS). It must preserve the "zero LLM calls in
    orchestrator code" invariant (pure transport). Exact event granularity (which section 8
    deltas get pushed, how often) is not yet specified beyond the envelope in section 1 and
    must be pinned before the Live Status panel binds.
12. **The Rust BFF + two-process local supervisor is net-new.** Spawn/handshake/port config
    for running the TS core as a child of (or alongside) the BFF is designed here but not
    built or validated.
13. **The serde-struct mirror at the BFF boundary is net-new.** Entity shapes are TS-defined
    and must be mirrored as Rust types; generate from a shared JSON Schema / TS export so the
    FeatureStatus enum cannot drift. The generation step is not yet chosen.
14. **The app-scope theme sheet that restates `--chorale-*` outside `.chorale-root` is
    net-new.** Whether chorale exports its palette for non-table use is UNVERIFIED;
    `theme_stylesheet()` scopes vars under `.chorale-root` only. Confirm at authoring time;
    otherwise restate the hex values at app scope.
15. **The FeatureStatus board / 11-state lifecycle visualization is net-new.** Neither repo
    has it.

### D. Could not be checked (carried from TECH_DESIGN, relevant to the UI's cost meter)

16. **The June 15 2026 billing change and the $100 Max 5x credit mechanism** (the
    operator's plan; NOT $200 Max 20x) are near-primary-sourced (TECH_DESIGN risk 11). The
    TopBar cost meter therefore shows authoritative **`spent`** (token-derived, always
    exact) and renders the $100 credit pool as an **estimate only**; exact rollover /
    fail-closed-on-exhaustion behavior should be confirmed live before any "credit
    remaining" figure is trusted as exact. The meter ships now on `spent`; it is NOT
    blocked on the billing confirmation.

End of UI_DESIGN.md.
