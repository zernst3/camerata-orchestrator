# Camerata Orchestrator: UI_TASKS.md

Status: V1 UI + Dashboard build plan. Additive to PHASE0_TASKS.md. This file EXTENDS the
engine task graph (T0-T14, defined in PHASE0_TASKS.md, not restated here) with the tasks
that build the V1 cockpit and dashboard against UI_DESIGN.md. Task numbering CONTINUES the
PHASE0_TASKS sequence starting at T15, and engine tasks are referenced by their T0-T14 ids
in `depends_on`.

This document does NOT relitigate the verified engine. The TS/Node orchestrator core
(making zero LLM calls), the Claude Agent SDK agent layer, the two-layer gate, sequential
Phase 0 execution, the three-state `enforcement_kind` model, and the metered-API-key +
Max-credit auth all stand exactly as TECH_DESIGN.md specifies. Where this plan and VISION
disagree, TECH_DESIGN's verified findings win.

Estimates use the same AI-orchestrated-build calibration as PHASE0_TASKS: an hour is an
hour of orchestrated agent work plus the human's review-and-approve loop, not a human
typing the code. Do not pad them into human-week shapes. Net-new surfaces with no
precedent in either repo (the TS HTTP/event surface, the Rust BFF, the serde mirror, the
live reconcile loop) carry honest risk premiums, flagged per task.

---

## 0. Sequencing principle (read this first)

**Engine-first build ORDER, full-V1 SCOPE.** This is the authoritative scope correction.
The old PHASE0_TASKS "CLI-only / no dashboard" boundary was a de-risking build ORDER, not
a scope cap. The governance engine (T0-T14) is proven behind a MINIMAL front-end first;
the polished cockpit and the multi-feature dashboard expand only after the gate has
already fired. Effort is not poured into a polished cockpit over an unproven engine.

Concretely, three ordered tiers:

```
  TIER E (engine)        T0 ............ T14         [PHASE0_TASKS.md, unchanged]
                              |
                              |  the gate fires on the planted violation at T9;
                              |  the full engine ties together at T14.
                              v
  TIER M (minimal UI)    T15 -> T16 -> T17 -> T18    [the thinnest cockpit that drives
                              |                       the engine end to end; proves the
                              |                       cross-stack BFF boundary works]
                              v
  TIER F (full V1 UI)    T19 ... T27                 [complete cockpit surfaces, the
                                                      dashboard + chorale dogfood, the
                                                      live WS stream, the shared theme]
```

The hard ordering rule, inherited from PHASE0_TASKS and made explicit for the UI: **no UI
task starts before the engine task it binds to is real.** The minimal-UI milestone (T15-T18)
cannot begin until the engine exposes state to bind to, which is why T15 (the TS HTTP +
event surface) is the first UI task and gates everything after it. The full dashboard
(T24+) is deliberately LAST, because a multi-feature roll-up over a single-Story P0 engine
is the least thesis-critical surface and the most chorale-live-update-risky one.

**Priority amendment (2026-06-13, VISION §3.5 / WORKTRACKER §0.5):** the async
CLARIFY-BRIDGE is pulled FORWARD into V1, slotted AFTER the minimal local cockpit (TIER M)
and AHEAD of the full dashboard (TIER F tail). Rationale: the bridge (route product
clarifying questions to a remote Product Owner as tracker comments, ingest the answers) is
what turns a solo tool into a team tool WITHOUT any cloud infrastructure, it is cheap (it
reuses the WorkItemProvider outbound/inbound comment channels), and it is more
thesis-critical than dashboard polish. So the V1 order is: engine (T0-T14) -> minimal local
cockpit (TIER M) -> clarify-bridge for the connected provider -> full dashboard. The bridge
stays BEHIND the minimal cockpit because the Architect must drive the engine solo (and the
clarify-loop data model must exist locally) before the answer path can be wired to the
tracker. The bridge connects to whichever ONE tracker the Architect/PO primarily use (Jira,
Azure DevOps, GitHub Issues, or native), chosen per deployment, not a fixed provider; WHICH
concrete adapter ships first follows where the first real PO lives (GitHub Issues is the
lowest-friction to implement but is only the default when the team also tracks product work
there). Concrete bridge tasks are added to TIER F as T28-T30 (outbound question-comment,
inbound answer-ingest + provenance, the cockpit `Ask in tracker` affordance) and are
sequenced before the dashboard tasks despite the higher number; build order follows this
amendment, not the numeric id.

Why this order and not "build the pretty cockpit first": the single most important fact
the engine proves is the gate firing on the planted violation (PHASE0_TASKS T9). Until
that is real, a cockpit has nothing governed to render. The minimal UI (T15-T18) exists to
prove the OTHER load-bearing unknown, the cross-language seam (Rust/Dioxus front-end ->
Rust BFF -> TS core, including the live WS leg that UI_DESIGN section 1 identifies as the
single most framework-unsupported surface). Proving the seam on a thin client, before
building eight polished panels on top of it, is the same de-risking logic the engine tier
already follows.

---

## 1. The minimal-UI milestone (TIER M): thinnest end-to-end driver

**Goal.** The thinnest front-end that lets one human drive the engine end to end without a
CLI: intake a Story, approve a RuleSet, answer clarifications, watch live status (including
the gate bounce), and QA the governed diff. This milestone proves the cross-stack boundary
(UI_DESIGN section 1) is real BEFORE any dashboard or cockpit-polish effort. It is the UI
analogue of T9/T14: prove the seam works, then dress the rest.

The minimal UI is allowed to be ugly. It renders raw-ish panels, no chorale tables, no
theme polish, no multi-Story spine. It binds to exactly one Story at a time (matching the
P0 engine's single-Story scope) and proves every leg of the data flow in UI_DESIGN section
4. What it MUST prove: a command leaves the Dioxus client over a server function, reaches
the TS core via the BFF, mutates state, and the resulting state transition arrives back at
the client over ONE live WebSocket and re-renders, including the layer-1 `gate_deny` and
layer-2 `gate_bounce` events.

### T15: TS core local HTTP API + event stream (the orchestrator transport surface)

- **id:** T15
- **title:** Expose the engine's deterministic state over a local HTTP API + a versioned event stream
- **description:** Add the net-new local transport surface to the TS/Node core
  (UI_DESIGN section 1 "Concrete API surface"): a small `node:http` / Fastify server
  exposing the REST routes (`POST /stories`, `GET /stories/:id`,
  `POST /stories/:id/investigate`, `GET /stories/:id/investigation`,
  `POST /stories/:id/clarifications`, `GET/PUT /stories/:id/ruleset`,
  `POST /stories/:id/plan`, `GET /stories/:id/tasks`, `POST /stories/:id/run`,
  `GET /tasks/:id`, `GET /tasks/:id/diff`, `POST /stories/:id/qa`,
  `GET /stories/:id/provenance`, `GET /onboarding/proposal`) plus `GET /events` as an SSE
  or WS stream. Bridge an internal event emitter to the stream, emitting the versioned
  envelope from UI_DESIGN section 1 (`feature_status_changed`, `task_status_changed`,
  `gate_result`, `gate_deny` (layer-1), `gate_bounce` (layer-2), `provenance_appended`,
  `clarification_required`, `cost_tick`, `snapshot`). **Critical invariant:** this surface
  is PURE TRANSPORT over the deterministic state the engine already computes. It adds ZERO
  LLM calls; the "zero model calls in orchestrator code" boundary (TECH_DESIGN section 8,
  the `agents/session.ts`-only rule) is preserved. `snapshot` is emitted on every
  (re)connect so a reconnecting client re-syncs FeatureStatus without missing transitions.
  Use the `v:1` envelope so the Rust client and TS server can evolve independently. Pin a
  fixed localhost port + a startup readiness signal for T16's handshake.
- **REQUIRED deliverable — the event contract (do this BEFORE T16 binds, not as a TODO):**
  produce a written, versioned event-contract spec that pins, for every event type above:
  (1) **which engine state transition emits it** (e.g. `task_status_changed` fires on every
  `Task.status` change; `cost_tick` fires per agent turn, NOT per token), (2) its **emission
  frequency / coalescing rule** (raw vs debounced, and the debounce interval per type, so
  the high-churn role-agent feed cannot thrash the reconcile loop, UI_DESIGN risk 8), and
  (3) the **schema version + back-compat rule** (`v:1` now; additive-only within a major).
  This is cheap to author now and expensive to retrofit after the BFF and every panel bind
  to an unspecified stream (UI_DESIGN risk 11). The Live Status and role-agent views cannot
  be finalized until this is locked. Treat it as T15's acceptance gate.
- **depends_on:** T14 (the full engine flow must exist to be exposed; the events mirror the
  T8/T9 gate, T10 investigation, T11 provenance, T12 QA transitions)
- **estimate:** 4h (net-new surface, but pure transport over already-computed state; the
  event granularity per section-8 delta is the genuinely undesigned part, UI_DESIGN risk 11)
- **advances:** UI_DESIGN section 1 (the cross-stack API surface) + section 4 (the data
  flow spine). Unblocks every other UI task. Hypothesis: makes the engine drivable by a
  client at all.

### T16: Rust BFF + two-process local supervisor

- **id:** T16
- **title:** Dioxus-fullstack Axum BFF proxying REST + re-broadcasting the event stream, with a local supervisor
- **description:** Stand up the Rust/Axum BFF (UI_DESIGN section 1, Option 4) as a Dioxus
  0.7 fullstack app. The BFF is the ONLY component that crosses the language boundary: its
  server functions proxy the REST routes 1:1 to the TS core via reqwest, and a server-side
  `tokio-tungstenite` client subscribes to the TS core `GET /events`, normalizing and
  re-broadcasting frames to the browser/webview over the BFF's OWN first-class Axum
  WebSocket (the move that converts the unsupported raw-external-WS problem into the
  framework-supported server-side-Axum-WS problem, UI_DESIGN section 1 "Why" point 2). The
  BFF owns heartbeat, exponential-backoff reconnect to the TS core, and the
  snapshot-on-(re)connect push. Add the two-process local supervisor: spawn the TS core as
  a child of (or alongside) the BFF with a fixed localhost port and a startup handshake
  (UI_DESIGN "Two-process desktop note"). Target `dioxus::desktop` (OS webview, matching
  `camerata-ai/src/bin/gui.rs`), keeping the component tree renderer-agnostic so a later
  hosted/WASM build is a config flip. **Caveat baked in:** do NOT rely on Dioxus streaming
  server functions for the live leg; they are broken on desktop (issue #3694). The live leg
  is the plain BFF-owned WebSocket consumed via `use_coroutine`, not the built-in
  `use_websocket` hook.
- **depends_on:** T15
- **estimate:** 5h (net-new BFF + supervisor + reconnect/heartbeat; UI_DESIGN risks 9, 10,
  12 all live here; the reconnect-with-backoff is unbuilt and untested per the design)
- **advances:** UI_DESIGN section 1 (the entire cross-stack recommendation) + the
  connection-state spine. Hypothesis: proves the verified BFF boundary actually carries
  commands one way and events the other.

### T17: serde-struct mirror + generated FeatureStatus enum at the BFF boundary

- **id:** T17
- **title:** Mirror the TS entity shapes as Rust serde structs, generated so the enum cannot drift
- **description:** Define the Rust serde structs at the BFF boundary mirroring the VISION
  section 8 entities (Story, Investigation, Rule, RuleSet, Role, Task, Gate/Check,
  Provenance, FeatureStatus) and the section-1 event envelope. **Generate** the Rust types
  from a shared JSON Schema or a TS type export (typeshare-style step), NOT by hand, so the
  canonical 11-state FeatureStatus enum (`INTAKE, INVESTIGATING, AWAITING_CLARIFICATION,
  PLANNED, EXECUTING, GATING, AWAITING_QA, SIGNED_OFF, DONE, BLOCKED, REJECTED`) cannot
  drift between the two languages (UI_DESIGN section 1 "Two-process desktop note",
  section 6, risk 13). Keep actor-shaped fields actor-shaped (`Story.created_by`,
  `Provenance.human_decision`); never hardcode one user. Honor the three-state
  `Rule.enforcement_kind` (`deterministic-active`, `deterministic-declared`,
  `review-heuristic`) so the UI can never claim enforcement that does not exist.
- **depends_on:** T15 (the TS shapes are the source of generation), T16 (the BFF is where
  the mirror lives)
- **estimate:** 2h (the generation-tool choice is undecided, risk 13; once chosen, the
  mirror is mechanical)
- **advances:** UI_DESIGN section 6 (data model fidelity) + the non-blocking multi-user
  shape constraint (actor-shaped fields survive). Hypothesis: the seam carries faithful,
  drift-proof entity shapes.

### T18: minimal cockpit shell + the `useOrchestrator` coroutine (end-to-end thin driver)

- **id:** T18
- **title:** Thinnest Dioxus client that drives one Story intake -> ruleset -> clarify -> live status -> QA
- **description:** Build the minimal single-window Dioxus client and the
  `useOrchestrator` `use_coroutine` that owns the BFF WebSocket: it pushes incoming events
  into Story/Task/Gate signals and exposes the command senders (submitStory, submitAnswers,
  approveRuleSet, approvePlan, signOff, reject) over server-function-backed reqwest calls
  (UI_DESIGN "Cockpit component list", section 4 "Direction discipline"). Render the five
  stage panels in their THINNEST form (raw text, no chorale, no theme polish, no left
  rail): an intake box -> a flat RuleSet approve list -> a clarifications answer form -> a
  live-status text log that visibly shows the layer-1 `gate_deny` and layer-2 `gate_bounce`
  events and the `(rev)` revise-to-pass loop -> a QA diff + provenance line + sign-off /
  reject. Stage panels swap by the active Story's FeatureStatus (engine owns progression;
  tabs are read-only indicators). Surface the BFF connection state (Connected /
  Reconnecting / Offline) so a dropped socket is visible, not a silent stall. **This is the
  minimal-UI milestone's proof point:** one human drives the full T14 engine flow through
  the UI, the gate bounce is visible, and the cross-stack seam carries it. No dashboard, no
  multi-Story spine, no chorale, no theme yet.
- **depends_on:** T16, T17, T14 (binds to the full engine flow)
- **estimate:** 4h
- **advances:** UI_DESIGN section 2 (cockpit, thin form) + section 4 (full data flow) +
  the CORE HYPOTHESIS in its minimal form: a human steers the engine from one screen
  instead of a CLI. This milestone proves the hypothesis is testable before polish.

**Minimal-UI milestone exit criterion:** a human, with no CLI, intakes a Story, approves
its RuleSet, answers clarifications, watches the planted-violation gate bounce-and-revise
in the live log, and signs off the governed diff, all in one Dioxus window, with every
command crossing the BFF and every state transition arriving over the one live WebSocket.
When this passes, the seam is proven and TIER F (full V1) is unblocked.

---

## 2. Full V1 UI tasks (TIER F): complete cockpit, dashboard, live stream, theme

These expand the thin T18 shell into the full UI_DESIGN cockpit and add the multi-feature
dashboard, the chorale dogfood, and the shared theme. They are intentionally AFTER the
minimal milestone: the seam is already proven, so this tier is composition and polish, not
risk discovery (except the chorale live-update reconcile loop, T25, which carries its own
flagged risk).

### T19: cockpit shell decomposition + contexts + camerata-family chrome

- **id:** T19
- **title:** CockpitShell three-region layout, context providers, TopBar, StorySpineRail, Inspector
- **description:** Expand the thin T18 client into the full three-region cockpit shell
  (UI_DESIGN section 2): persistent LEFT RAIL (`StorySpineRail`: Story list with
  FeatureStatus badges, the "NEEDS YOU" judgment queue, new-story button, the dual-axis
  search filter reused from the curation GUI), CENTER STAGE (status-driven panel swap),
  persistent RIGHT INSPECTOR (`Inspector`: context-binding detail pane reusing the curation
  GUI's rationale + selectable-alternatives + custom-rule sub-components verbatim), and the
  `TopBar` (story title, FeatureStatus badge, cost-vs-Max-credit meter, live agent count,
  connection-state pill, urgent-gate indicator). Decompose into `#[component]` functions
  (portfolio model, NOT one monolithic `app()` fn); global state (selected story, theme,
  BFF connection status) lives in `contexts/` providers (`use_context_provider`), not flat
  signal soup. State idiom matches the curation GUI (`use_signal` / `use_effect` /
  `with_mut` / `.peek()`). Stage tabs are READ-ONLY status indicators; the engine owns
  progression.
- **depends_on:** T18
- **estimate:** 3h
- **advances:** UI_DESIGN section 2 (cockpit shell, camerata family). Hypothesis: the ONE
  durable steerable screen with a persistent spine + inspector.

### T20: Intake + Investigation panels (the front-loaded-judgment wedge)

- **id:** T20
- **title:** IntakePanel + InvestigationPanel (findings, product questions, tech tradeoffs, RuleSet review)
- **description:** Build `IntakePanel` (one input box, brownfield-default repo selector,
  greyed tracker-issue affordance per VISION 18; Investigate POSTs the Story and flips to
  INVESTIGATING) and the full `InvestigationPanel` (UI_DESIGN section 2.2): `CodebaseFindings`,
  the two side-by-side `ProductQuestionsPanel` (free-text + choice answers over
  `Investigation.product_questions[]`) and `TechTradeoffsPanel` (option/pros/cons/recommend
  with accept-rec or override over `Investigation.tech_tradeoffs[]`), and the
  `RuleSetReviewSurface` using the camerata domain-group + checkbox + detail-pane idiom over
  the REAL rule index (T1). Each rule row carries the `EnforcementBadge` with the THREE
  verified states (`(o)active` / `(-)declared` / `(o)review`); `declared` and `review` are
  grouped on the human-surfaced side, NEVER shown as auto-enforced (TECH_DESIGN Q5, the
  honesty invariant). The deterministic-active set is MULTI-RULE across API + UI layers
  (Q5 downgrade), so the selected column renders multiple active rules. Render conflicts /
  gaps with the three resolution options (adopt+migrate / keep+exception / synthesize) and a
  "+ add rule" gap action (TECH_DESIGN Q6). The human edits and OWNS the final active
  RuleSet (VISION 11); the picked tradeoff option persists on the Investigation.
- **depends_on:** T19, T10 (investigation driver), T1 (real rule index), T2 (three-state
  bucket classifier feeds the EnforcementBadge), T13 (brownfield conflicts/gaps feed the
  panel)
- **estimate:** 4h
- **advances:** UI_DESIGN sections 2.2, 2.3 (clarify loop) + VISION 1.1 front-loaded
  judgment. Hypothesis: the human works only at architect altitude; no code until the
  RuleSet gate passes.

### T21: Plan panel (chorale master/detail over the Task DAG)

- **id:** T21
- **title:** PlanPanel: chorale master/detail grid over Task[] with rule subsets + boundaries, approvable pre-spawn
- **description:** Build `PlanPanel` (UI_DESIGN section 2.4) as a chorale master/detail
  table over the Task DAG. Each Task master row shows role, write `path_boundaries`,
  `depends_on`, and rule count; the `detail_renderer` expands to the scoped `rule_subset`
  (with enforcement badges), `allowed_tools`, and the path/hook boundaries. Show the
  contract handoff line (Backend emits `api-contract.ts` -> Frontend consumes via prompt
  context, NOT a premature merge; VISION 12 / T7) and label execution SEQUENTIAL
  (TECH_DESIGN Q4). The DAG is the two-node Backend -> Frontend chain (PHASE0_TASKS
  out-of-scope: no DAG beyond this). The DB boundary renders as a RULE on the Frontend role
  (layer-1 hook deny `psql|pg_dump` -> `ROLE-PATH-BOUNDARY-FE-1`), NOT a third agent
  (exactly two roles, T6). The plan is shown and approvable BEFORE any agent spawns
  (`Approve plan & execute`).
- **depends_on:** T19, T6 (roles + scoping), T7 (worktree coordinator + handoff +
  sequential schedule), T25 (chorale adapter; the grid renders through it)
- **estimate:** 2.5h
- **advances:** UI_DESIGN section 2.4 (chorale dogfood begins in the cockpit). Hypothesis:
  the human approves the decomposition + boundaries at architect altitude before execution.

### T22: Live Status panel (the bounce-and-revise loop made visible)

- **id:** T22
- **title:** LiveStatusPanel: sequential timeline rendering the two-layer gate as defense-in-depth
- **description:** Build `LiveStatusPanel` (UI_DESIGN section 2.5) as the
  `SequentialTimeline` -> `TaskCard` -> `GateLayerRow` -> `BounceLoopIndicator` tree. Render
  the SEQUENTIAL per-task / per-agent / per-gate progress (NOT parallel swimlanes,
  TECH_DESIGN Q4): per task, the `agent_session` id + model, the layer-1 PreToolUse row
  (real-time DENY that blocks a tool call and produces NO diff, showing the model-visible
  `systemMessage`, the verified Q2 correction over `permissionDecisionReason`), the layer-2
  post-task gate row (structural FAIL bounced with `rule_id + file:line + fix suggestion`,
  with the `(rev)` marker + attempt counter converging to PASS), and the produced-diff
  summary + gate_results. Render the two layers as DEFENSE-IN-DEPTH, not strict either/or: a
  layer-1 pass is NOT treated as proof the diff is clean; a violation can still surface at
  layer-2. Transport: events arrive over the BFF WebSocket via the `useOrchestrator`
  `use_coroutine` pushing into a `Signal<Vec<TaskStatus>>`; reactive re-render, no polling
  loop, no Dioxus streaming server functions (broken on desktop, #3694).
- **depends_on:** T19, T8 (post-task gate + bounce loop), T9 (planted-violation events to
  render), T4 (layer-1 hook deny events), T18 (`useOrchestrator` coroutine)
- **estimate:** 3h
- **advances:** UI_DESIGN section 2.5 + the CORE HYPOTHESIS most directly: this panel is
  what makes "easier than five chat windows" visible. The gate bounce is a first-class
  visible event, not buried in N scrolling chat logs.

### T23: QA / review panel (governed diff + provenance line + sign-off)

- **id:** T23
- **title:** QaPanel: chorale RowCellRenderer over diff hunks, per-change provenance, honest surfaced-rules block
- **description:** Build `QaPanel` (UI_DESIGN section 2.6): the governed diff rendered via
  a chorale grid with row-aware `RowCellRenderer` for per-hunk provenance and an action
  column. Each hunk carries exactly the VISION section 8 Provenance line `{task_id, role,
  agent_session_id, rules_passed[], human_decision}` (one line per change, no audit store
  in V1; T11). Render the "SURFACED FOR YOUR JUDGMENT" block: `deterministic-declared` +
  `review-heuristic` rules SHOWN to the human, never claimed as auto-passed (the honesty
  invariant, Q5 / T12). Per-hunk approve/reject + a Story-level sign-off / reject, both
  through a `ConfirmBanner` (the curation GUI banner-confirm idiom, for the irreversible-ish
  action). Sign-off writes `human_decision` (actor-shaped) and flips the Story to
  SIGNED_OFF -> DONE; reject flips to REJECTED.
- **depends_on:** T19, T11 (provenance line), T12 (QA presentation semantics), T25 (chorale
  adapter for the diff grid)
- **estimate:** 2.5h
- **advances:** UI_DESIGN section 2.6 + VISION criterion 4 (governed diff presented with
  provenance). Hypothesis: the human owns the final accept, with honest enforcement
  surfacing.

### T24: Dashboard shell + four chorale table surfaces + needs-attention queue

- **id:** T24
- **title:** Dashboard: Feature / Role-Agent / Gate tables + always-visible Needs-Attention inbox, with cockpit drill-down
- **description:** Build the multi-feature dashboard (UI_DESIGN section 3): a left nav rail,
  a top status strip, and a main pane swapping between four chorale-dioxus surfaces, plus
  the always-visible NEEDS-ATTENTION queue. (1) FEATURE table (one row per Story:
  expander/`detail_renderer` -> task sub-table, Story title, FeatureStatus badge via
  built-in `RenderKind::Badge` + `BadgeVariantMap`, derived phase, active-roles chip list,
  gate pass/fail split via `RowCellRenderer`, needs-attention flag, updated timestamp);
  `on_row_click: Option<Callback<RowId>>` drills into the single-pane cockpit. Honor the
  verified `on_row_click` firing exclusions (no fire on selection checkbox, chevron,
  edit-mode cells, group-header rows, modified clicks) and do NOT make the same cell both an
  `on_row_click` target AND inline-editable; route in-table edits through a dedicated action
  cell. (2) ROLE-AGENT view (one row per live session; the most update-intensive view, where
  the reconcile debounce matters most; a live agent LOG tail is a SEPARATE non-table
  component, not a table cell). (3) GATE view (one row per Gate/Check firing; group by
  `rule_id` or task with pass/fail aggregate in the group header, the v0.2.1 fix that made
  group-header aggregates render, for the governance roll-up). (4) NEEDS-ATTENTION queue
  (filter status in {AWAITING_CLARIFICATION, AWAITING_QA, BLOCKED}; action cells via
  `RowCellRenderer` routing the click with the row id + siblings). The P0 engine drives one
  Story, but the dashboard is multi-feature-shaped (roll-up over `Story[]`).
- **depends_on:** T19 (cockpit to drill into), T25 (chorale adapter; all four are chorale
  tables), T15 (multi-Story `GET /stories` + tasks/gates feeds)
- **estimate:** 4h
- **advances:** UI_DESIGN section 3 (the entire dashboard + dogfood). Hypothesis: the
  at-a-glance multi-feature roll-up + the human action inbox, with one-click drill into any
  feature's cockpit.

### T25: chorale-dioxus adapter + the live reconcile loop (the dogfood's load-bearing risk)

- **id:** T25
- **title:** chorale v0.2.2 adapter: badge/RowCellRenderer/master-detail wiring + the poll-and-reconcile live-update loop
- **description:** **GATED ON chorale 0.2.2 shipping first (architect decision, 2026-06-13).**
  chorale 0.2.2 adds the native row-set primitives (`insert_row` / `remove_row` /
  `set_rows` + a reconcile that carries forward sort/filter/selection/scroll/expansion) via
  Zach's separate chorale investigative routine; chorale owns the API, Camerata builds
  around it (NOT the reverse). Do not start T25 until 0.2.2 ships those. Build the
  chorale-dioxus adapter shared by the Plan grid (T21), QA grid (T23), and all four
  dashboard tables (T24), pinned to chorale `=0.2.2` (resolve from `Cargo.toml` /
  crates.io). Wire the verified surface: `RenderKind::Badge` +
  `BadgeVariantMap` for status badges (zero custom code), `RowCellRenderer<TRow>` for the
  row-aware cells (gate pass/fail chips aggregating a `Vec<Check>`, action columns reading
  `task_id` + worktree from siblings), `detail_renderer: Option<Callback<TRow, Element>>`
  for master/detail, grouping + `AggregatorKind` for the gate roll-up, sort/filter/selection
  /frozen columns, virtualization, CSV/XLSX export. **The load-bearing piece: the
  live-update reconcile loop, now built ON chorale 0.2.2's native primitives.** The adapter
  converts a WS snapshot into chorale transitions using 0.2.2's `insert_row` / `remove_row`
  / `set_rows` / reconcile (no more rebuild-`TableState`-by-hand workaround; that was the
  0.2.1 gap, closed by shipping the API in chorale first). **The carry-forward of
  sort/filter/selection/scroll/expansion across a row-set change is now a chorale-core
  capability, not bespoke adapter code** (the API was designed in chorale precisely so this
  is not re-solved per consumer). What REMAINS in the Camerata adapter is only app-specific
  POLICY: the debounce/coalesce interval (1 to 3s; the role-agent view emits many
  ticks/second) and which transient state to carry. Run a perf spike before the role-agent
  view is finalized (live high-tick render cost is still UNTESTED for Camerata's workload,
  risk 8). **Front-load this task in TIER-F** so the perf reality surfaces before the
  role-agent view is built on top of it. **Risk-flagged task: this is the one TIER-F task
  that is risk discovery, not just composition.**
- **depends_on:** chorale 0.2.2 (the row-set primitives must SHIP before T25 starts),
  T16 (the BFF WS that feeds the reconcile loop), T17 (the serde row types the tables
  render)
- **estimate:** 5h (the reconcile helper + carry-forward + the perf spike are the real
  cost; UI_DESIGN risks 7, 8 live here)
- **advances:** UI_DESIGN section 5 (the chorale dogfood) + section 3 (every dashboard
  table) + sections 2.4 / 2.6 (the cockpit grids). Hypothesis: the build is its own
  showcase; the orchestrator is chorale's first genuinely live consumer.

### T26: shared theme sheet + StatusBadge / EnforcementBadge / ConfirmBanner

- **id:** T26
- **title:** App-scope `--chorale-*` token sheet, the 11-state -> 5-family StatusBadge, EnforcementBadge, ConfirmBanner
- **description:** Author the shared theme (UI_DESIGN section 7). Adopt chorale's
  `--chorale-*` CSS-variable token contract (~38 tokens; use "~38" or chorale's "~39"
  hedge, never hardcode 39, risk 1) as the single source of truth. Author ONE app-level
  token sheet that RESTATES chorale's palette at app scope (`:root` / `.orchestrator-root`)
  so panes OUTSIDE chorale tables (the cockpit shell, the camerata-style detail pane) theme
  consistently, because chorale's tokens are scoped under `.chorale-root` only (net-new
  sheet, risk 14; confirm at authoring time whether chorale exports its palette for
  non-table use, else restate the hex values). Migrate camerata's 121 inline hexes onto
  tokens (stylesheet-class-driven, PORT-CSS-1 discipline, not inline strings). Build the
  shared components: `StatusBadge` for the 11 FeatureStatus states (the consumer-owned
  many-to-five mapping: green = SIGNED_OFF/DONE; yellow = INVESTIGATING/EXECUTING/GATING/
  AWAITING_QA; red = BLOCKED/REJECTED; gray = INTAKE/PLANNED/AWAITING_CLARIFICATION; MUST
  carry label + icon since 11 states exceed 5 colors); `EnforcementBadge` for the three
  states (`active` -> green/accent, `declared` -> gray with an explicit "review" label so it
  never reads as enforced, `review` -> gray/default); `ConfirmBanner` (the curation-GUI
  banner-confirm idiom). Wire light/dark as a single `data-chorale-theme` attribute swap
  shared across the curation GUI, cockpit chrome, and chorale grids.
- **depends_on:** T19 (the shell that consumes the theme), T25 (chorale token contract +
  badge render path), T2 (the three enforcement_kind states the EnforcementBadge renders)
- **estimate:** 3h
- **advances:** UI_DESIGN section 7 (shared style/theme) + the "same family" requirement.
  Hypothesis: the curation GUI, cockpit, and dashboard read as one coherent product.

### T28: Async clarify-bridge, outbound (post product questions to the tracker)

Depends on: T15 (TS event/command surface), T20 (Investigation/clarify panel), and the
connected provider's `WorkItemProvider` outbound comment channel (WORKTRACKER §0.5 / §3).
When a Story enters `AWAITING_CLARIFICATION`, an `Ask in tracker` action on a product
question calls the orchestrator to post a formatted, @-mentioning comment with that question
(or the whole product set) onto the linked tracker item (the Jira/ADO/GitHub/native issue
the Story is connected to). Technical tradeoffs and the RuleSet are never posted. The
question row enters an `awaiting-PO` sub-state. Estimate: 2h per board adapter (net-new
outbound use; the provider's comment auth is the precondition). BOARD-AXIS PRIORITY (VISION
§3.5 / WORKTRACKER §3): the clarify-bridge targets the PRODUCT tracker where the PO lives, so
the first shipping board adapters are **Jira and Azure DevOps Boards** (the two most-used
enterprise story trackers), NOT GitHub Issues (deprioritized: underused as a formal board).
GitHub Issues may serve as a cheap mechanical test-harness during development only. Note the
board adapters are heavier than GitHub (Jira: OAuth 3LO + ~25-day webhook-refresh cron; ADO:
Service Hooks, no HMAC), so budget accordingly. Advances: VISION §3.5 async collaboration;
the team-tool-without-cloud hypothesis. BUILD ORDER: before T24 (dashboard).

### T29: Async clarify-bridge, inbound (ingest the PO answer + provenance)

Depends on: T28, plus the inbound webhook + reconciliation-poll path (WORKTRACKER §4.1) and
echo-suppression/idempotency (§4.2). A new PO comment of kind `answer` is normalized,
matched to the open question by issue ref + thread, and folded into the Investigation
answer; the Story leaves `AWAITING_CLARIFICATION` only once answered. The PO comment
(id/url/author/timestamp) is recorded as the `human_decision` provenance source. Estimate:
2.5h (webhook verification + match logic + the resume trigger). Advances: VISION §3.5;
auditable external sign-off. BUILD ORDER: before T24 (dashboard).

### T30: Cockpit affordance + status for the bridge round-trip

Depends on: T28, T29. The `[ Ask in tracker ]` affordance on each product question, the
`awaiting-PO` row sub-state, and a clear surfaced timeline of the round-trip (posted ->
awaiting -> answered) in the live-status panel, so the Architect sees the async loop without
leaving the cockpit. No PO-facing Camerata UI (the PO's surface is their tracker). Estimate:
1.5h. Advances: VISION §3.5 single-cockpit-for-the-Architect. BUILD ORDER: before T24.

### T27: V1 cockpit + dashboard end-to-end acceptance run

- **id:** T27
- **title:** Full-V1 acceptance: drive a Story through the themed cockpit AND see it on the dashboard, live
- **description:** Tie the full UI together and run the V1 acceptance pass: with the BFF +
  supervisor up (T16), drive one Story end to end through the FULL themed cockpit (intake
  T20 -> investigation + RuleSet approval T20 -> plan approval T21 -> live status with the
  visible gate bounce-and-revise T22 -> QA sign-off T23), while the SAME Story renders live
  on the dashboard (T24) across the feature / role-agent / gate tables, updating in place
  via the reconcile loop (T25), themed by the shared sheet (T26), with the cockpit
  drill-down (`on_row_click`) and the needs-attention queue routing the human into each
  judgment moment. Confirm: every command crosses the BFF; every transition arrives over the
  one WebSocket; the connection-state pill reflects a forced socket drop + reconnect; the
  cost meter renders authoritative `spent` (token-derived) with the $100 Max 5x credit as
  an estimate only (Q1; credit-remaining math is NOT trusted as exact until verified live,
  risk 16; the meter is not blocked on that confirmation); the layer-1 deny and layer-2 bounce are both visibly
  distinct (defense-in-depth). This is the UI analogue of T14: it proves the full-V1 UI
  scope fires together against the verified engine.
- **depends_on:** T20, T21, T22, T23, T24, T25, T26 (and transitively the engine via those)
- **estimate:** 3h
- **advances:** the WHOLE UI_DESIGN + both top-level hypotheses (single cockpit replaces N
  windows; dashboard answers at a glance). The V1 UI integration proof.

---

## 3. Estimate roll-up

| Task | Tier | Estimate |
|------|------|----------|
| T15  | M (minimal) | 4h   |
| T16  | M           | 5h   |
| T17  | M           | 2h   |
| T18  | M           | 4h   |
| **Minimal-UI milestone subtotal** | | **15h** |
| T19  | F (full V1) | 3h   |
| T20  | F           | 4h   |
| T21  | F           | 2.5h |
| T22  | F           | 3h   |
| T23  | F           | 2.5h |
| T24  | F           | 4h   |
| T25  | F           | 5h   |
| T26  | F           | 3h   |
| T27  | F           | 3h   |
| **Full-V1 UI subtotal** | | **30h** |
| **UI total (T15-T27)** | | **~45h orchestrated-build** |

Combined with the engine's ~31h (PHASE0_TASKS T0-T14), the full V1 tool (engine + UI) is
~76h of orchestrated-build. The critical path to the FIRST drivable UI (the minimal-UI
milestone) is the engine through T14, then T15 -> T16 -> T17 -> T18, reaching an
end-to-end UI-driven run at ~15h of UI build on top of the proven engine, before any
dashboard or theme effort. That ordering is deliberate: prove the seam on a thin client,
then build the eight polished surfaces on top of it.

The single highest-risk UI task is T25 (the chorale live-update reconcile loop): it is the
one full-V1 task that is risk discovery rather than composition, and it carries the only
UNTESTED-at-scale claim in UI_DESIGN (live high-tick render cost, risk 8). Its perf spike
should land before the role-agent view is treated as final.

---

## 4. Definition of done for the V1 UI

The V1 UI is done when the two top-level hypotheses are demonstrably satisfied by a single
run through the themed cockpit + dashboard against the engine, on metered-key / Max-credit
auth.

### Hypothesis 1: the single cockpit replaces N chat windows

A human steers the entire governed-development flow (intake -> investigation + RuleSet
curation -> plan approval -> live execution -> QA sign-off) from ONE Dioxus window, never
operating a second OS window, and the gate bounce-and-revise loop is a first-class VISIBLE
event rather than something buried across N scrolling chat logs.

| Satisfied by |
|---|
| T15 (engine state exposed to bind to), T16 + T17 + T18 (the seam + the thin end-to-end driver: the minimal-UI milestone proves the hypothesis is testable), T19 (the durable single-screen shell), T20 (intake + investigation at architect altitude), T21 (plan approval pre-spawn), **T22 (the bounce-and-revise loop made visible: the most direct proof)**, T23 (QA sign-off with provenance), T26 (one coherent themed family), T27 (the full themed end-to-end run). |

### Hypothesis 2: the dashboard answers the multi-feature questions at a glance

When more than one Story is in flight, the human sees the multi-feature roll-up (which
features are running, which agents are live, which gates fired, what needs a human), and
drills into any single feature's cockpit in one click, with all collection views dogfooding
chorale.

| Satisfied by |
|---|
| T24 (the four chorale tables: feature roll-up, role-agent view, gate roll-up, needs-attention inbox + the `on_row_click` cockpit drill-down), T25 (the chorale adapter + the live reconcile loop that keeps the at-a-glance views current in place), T26 (the 11-state StatusBadge + shared theme so the roll-up reads coherently), T27 (the live multi-surface acceptance run). |

### The non-blocking shape constraints (verified preserved, not V1 features)

| Constraint | Held by |
|---|
| Actor-shaped fields (`Story.created_by`, `Provenance.human_decision`) never hardcoded to one user | T17 (serde mirror keeps them actor-shaped), T20 / T23 (capture + write them actor-shaped) |
| Renderer-agnostic component tree (later hosted/WASM is a build-target flip, not a rewrite) | T16 (desktop target, tree kept renderer-agnostic), T19 (decomposed `#[component]` + contexts) |
| Plain serialized JSON seam (front-end stays swappable) | T15 (`v:1` JSON envelope), T16 (BFF proxies JSON 1:1), T17 (generated mirror) |

### What "done" explicitly does NOT include (engine-first order, honesty invariants)

- No UI task ships before its engine dependency is real; the gate must fire (T9) and the
  engine must integrate (T14) before the minimal UI binds.
- The UI never makes a model call. That invariant lives entirely in the orchestrator's
  `agents/session.ts`; the cockpit is a pure client (commands over HTTP, events over one
  WebSocket).
- The UI never claims enforcement that does not exist: `deterministic-declared` and
  `review-heuristic` rules are always surfaced to the human, never rendered as auto-passed
  (T20, T23, T26 EnforcementBadge).
- The two gate layers are always rendered as defense-in-depth (T22), never collapsed into a
  single pass/fail that hides the layer-1 deny behind a layer-2 pass.

This document is additive to PHASE0_TASKS.md. The engine tasks T0-T14 are defined there and
referenced here by id; they are not restated or modified.
