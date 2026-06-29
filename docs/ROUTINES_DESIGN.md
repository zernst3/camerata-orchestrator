# Routines page: design document

> **Status: DRAFT v0.2 (2026-06-29).** For review and iteration with Zach. This consolidates the
> three routine ADRs, audits what is built today, and proposes the visual + functional target so we
> can build the rest deliberately. The §10 decisions are now **RESOLVED** (Zach, 2026-06-29).

### Resolved decisions (v0.2)
- **Scope vocabulary (D):** adopt the emit-cascade language. Read-only base; a Write routine
  escalates through cascading toggles (Save on a branch, Push to GitHub, Open a PR), each requiring
  the previous, mirroring the Rules emit control.
- **Run records (F):** a bounded per-routine run history (last N runs, FIFO). Not a shared
  story/run substrate.
- **Placement (B/C/G/H):** INLINE panels (no drawers). Create/edit below the table; run history as a
  row-expand; escalation/review inline. Single sectioned create form (no wizard).
- **Templates (E):** ship bug-triage + security-scan (exist) plus dependency-and-license sweep,
  stale-PR nudge, docs-drift check, and **daily digest**.
- **Create flow (C):** single sectioned form. Templates **prefill** the whole form (editable). A
  custom routine is intent-first: free-text intent, the AI drafts the operational prompt, both
  fields editable; raw intent is never executed.
- **Status strip (A):** total, enabled, running, blocked, due-in-24h; segments filter the table.

Companion ADRs (the functional decisions, already made):
- [`2026-06-15_routine_dashboard.md`](decisions/2026-06-15_routine_dashboard.md): a routine is a scheduled governed run; the dashboard manages them.
- [`2026-06-15_routine_authoring_intent_not_prompt.md`](decisions/2026-06-15_routine_authoring_intent_not_prompt.md): you author from intent, the AI writes the operational prompt.
- [`2026-06-20_routine_templates.md`](decisions/2026-06-20_routine_templates.md): preset templates, instantiated into editable routines.

---

## 1. What a routine is (the model, settled)

A **routine** is a first-class **scheduled, governed run**: a name, a schedule, an intent + an
AI-authored operational prompt, a permission scope, an optional owning project, a model, an enabled
flag, and a lifecycle status + run record. It runs through the **same engine and the same gate** as
an interactive run, so its writes are gated, jailed to a worktree, and Camerata remains the sole
committer. The dashboard is a scheduler plus a view over runs, not a new execution path.

The model layer for this is **built and well-tested** (see §3). The work ahead is almost entirely
the **page**: making it legible, consistent with the rest of the app, and complete (run history,
next-fire, status-at-a-glance, templates with preview).

---

## 2. Design goals

1. **Status at a glance.** The dashboard ADR's headline promise: see which routines are enabled,
   running, blocked, and due soon, without opening anything. This barely exists today.
2. **Legible governance.** A scheduled agent running unattended is exactly where the scope (rule
   subset + permission level) must be visible, not implicit.
3. **Consistent with the app.** The routine table should ride the central `CamerataTable` primitive
   (like Rules and Governed Development), not a bespoke `routine-table` (currently bespoke; flagged
   in `UI_BACKLOG.md`).
4. **Intent-first authoring.** Keep the "describe what you want, the lead engineer writes the
   prompt" flow front and center; never run raw intent.
5. **A real run history.** Today only the single `last_run` summary is shown. A scheduled thing's
   whole value is the trail of what it did over time.

---

## 3. Current state (honest audit)

### Built and solid (keep)
- **Data model** (`crates/server/src/routine.rs`): `Routine` (id, name, schedule, intent, prompt,
  scope, enabled, last_run, provisioned, last_fired, project_id, model, status), `RoutineStore`
  (CRUD + persistence + counter), `RoutineStatus` (Idle/Running/BlockedNeedsReview/Done/Failed),
  `RoutineRunSummary` (outcome, denies, allows, denied_rules). 13 unit tests.
- **Schedule grammar** (`crates/server/src/schedule.rs`): `daily HH:MM`, `weekly Mon,Wed HH:MM`,
  `monthly day N HH:MM`, `once YYYY-MM-DD HH:MM`, plus `Manual` (never fires). Pure `is_due(sched,
  now, last_fired)` with catch-up + dedup. 6 unit tests.
- **Auto-fire scheduler** (`crates/server/src/auto_fire.rs`): a tokio tick (default 60s) that fires
  due + provisioned + enabled routines through the real gate (`run_now`), stamps `last_fired`, and
  raises an escalation when a run is blocked. 3 unit tests.
- **Templates** (`builtin_templates()`): `bug-triage` (read-only) and `security-scan` (write).
  Instantiate into a fully editable routine. Tested.
- **Endpoints**: `GET/POST /api/routines`, `GET /api/routines/templates`,
  `POST /api/routines/templates/:id/instantiate`, `POST /api/routines/draft-prompt`,
  `PUT/DELETE /api/routines/:id`, `POST /api/routines/:id/{enable,provision,run}`, plus the
  escalation endpoints for blocked routines.
- **UI flows that work** (`crates/ui/src/routines.rs`, ~1380 lines): list grouped by project,
  structured schedule picker, scope/project/model selects, intent + draft-prompt, full CRUD,
  run-now, enable/disable, provision ("Set up"), and a rich inline escalation/review panel.

### Gaps (the design targets)
- **Table is bespoke**, not on `CamerataTable` (inconsistent look, no shared collapse/sort/virtual).
- **No run history.** Only `last_run` shows; `last_fired` persists but is explicitly "not yet
  rendered"; `denied_rules` is captured but never surfaced.
- **No next-fire preview.** The user cannot see *when* a routine will next run.
- **No status-at-a-glance** summary (counts of enabled / running / blocked / due).
- **Template UI is minimal** (a card list, no preview of the full prompt before instantiating).
- **Zero UI unit tests** for the pure helpers (`build_schedule`, `parse_schedule`, `status_badge`,
  grouping). The server side is well covered; the UI helpers are not.
- **Lifecycle status** (Idle to Running to Blocked/Done) is only partly wired through the UI.

---

## 4. Information architecture (proposed)

A single scrollable page, top to bottom:

```
┌─ Routines ───────────────────────────────────────────────────────────────┐
│ AUTOMATION / Routines                                                      │
│ "Scheduled governed runs. Each runs through the same gate as an            │
│  interactive run; run one now to see its real verdicts."                   │
│                                                                            │
│ ┌─ Status strip ───────────────────────────────────────────────────────┐  │
│ │  6 routines · 4 enabled · 1 running · 1 blocked · 2 due in <24h        │  │
│ └──────────────────────────────────────────────────────────────────────┘  │
│                                                                            │
│ ┌─ Routine table (CamerataTable) ──────────────────────────────────────┐  │
│ │  Name+status │ Schedule (next fire) │ Scope │ Project │ Last run │ ⋯   │  │
│ │  ▸ collapsible group per project; expand a row -> run history drawer   │  │
│ └──────────────────────────────────────────────────────────────────────┘  │
│                                                                            │
│ [ + New routine ]   [ Start from a template ▾ ]                            │
│                                                                            │
│ ┌─ Create / edit panel (opens below or as a right drawer) ─────────────┐  │
│ │  Intent -> Draft operational prompt -> review; Schedule; Scope;       │  │
│ │  Project; Model; Save                                                  │  │
│ └──────────────────────────────────────────────────────────────────────┘  │
│                                                                            │
│ (Blocked routine -> escalation/review panel, inline or drawer)             │
└────────────────────────────────────────────────────────────────────────────┘
```

The blocked-routine escalation/review panel already exists and is good; it stays, possibly promoted
to a drawer so it does not push the table around.

---

## 5. Functional design (surface by surface)

### 5.1 Status strip (NEW)
A one-line summary above the table: total, enabled, running, blocked-needs-review, and "due in next
24h" (computed from each schedule's next slot). This is the dashboard ADR's "the thing you cannot
see today." Clicking a segment (e.g. "blocked") filters the table to that state. **OPEN-A:** exact
metrics + whether segments filter.

### 5.2 Routine table (on CamerataTable)
Columns:
1. **Name + status** badge (Idle/Running/Blocked/Done/Failed) + the intent as a subtitle.
2. **Schedule** (human string) **+ next fire** ("next: Tue 09:00", computed). NEW: next-fire.
3. **Scope** (the permission level, as a labeled pill, not raw text).
4. **Project** (or "Global").
5. **Last run** (outcome + denies/allows; "not run yet").
6. **Actions**: Run now, Enable/Disable, Edit, Delete (two-click confirm). For imported/unprovisioned
   routines, "Set up" replaces Start.

Grouped by project (collapsible groups, the central primitive's collapse). Expanding a row opens the
**run-history** for that routine (§5.6). **OPEN-B:** row-expand for history vs a separate drawer.

### 5.3 Create / edit panel
Keep the intent-first flow, in this order:
1. **Name.**
2. **Intent** (plain language) -> **Draft operational prompt** button -> the AI authors it
   (`authored_by: claude`) or falls back to a deterministic scaffold (`authored_by: scaffold`),
   shown for review and edit. Never run raw intent.
3. **Schedule** (the structured picker: One-off / Daily / Weekly / Monthly + time, with a live
   serialized preview AND the computed next-fire).
4. **Scope** (permission level, with the inline explanation).
5. **Project** (Global or a specific project) and **Model**.
6. **Save** (Add / Save changes) + Cancel (edit mode).

**Create flow (resolved):** a single sectioned form (no wizard). Picking a template **prefills every
field** (name, intent, prompt, schedule, scope, model), fully editable. A custom routine is
intent-first: free-text intent, the AI drafts the operational prompt, both editable; raw intent is
never executed. (A "paste exact prompt, skip the draft" escape hatch is a possible later addition.)

**Scope (resolved): emit-cascade language.** Mirror the Rules emit control: a Read-only base
(inspect + report, no writes), or a Write routine that escalates through cascading toggles, each
requiring the previous: **Save on a branch** then **Push to GitHub** then **Open a PR**. Same
vocabulary and component feel as the emit cascade, so "what a routine can do" reads the same as
"what an emit does". Nothing auto-merges. The serialized `scope` string maps to these levels.

### 5.3a Permissions model (PROPOSED, decide before Phase 3)
A routine's "what it can do" is **two axes**, both shown for legibility (the dashboard ADR's
explicit-governance promise):

1. **Write reach** (ordered cascade, §5.3): read-only -> branch -> push -> open PR. **Mechanically
   enforced** today (git tool gating).
2. **Capabilities** (a discrete multi-select): create issues, comment on PRs, modify CI/workflow
   files, touch dependency manifests, call external services, etc. Not ordered.

**Where capabilities come from:** the AI draft step reads the intent to author the operational
prompt, so it **proposes the capability set at the same time**. For a custom routine the user
reviews and adjusts the multi-select; for a **template the set is FIXED and locked** (shown, not
editable), because the template's job has specific requirements.

**Enforcement (the honest split):** capabilities that map to a gate constraint are enforced now
(write-reach via the cascade; CI/workflow + dependency-manifest writes via path rules;
external-service calls via the existing gate). Capabilities without a mechanical mapping are carried
as operational-prompt directives and shown for legibility, until enforcement is wired. So the
multi-select is legible + enforced-where-it-maps now, with mechanical coverage growing over time;
"every capability hard-gated" is a larger gate project, not part of this page.

**Permissions (resolved 2026-06-29):** two-axis model adopted (write cascade + capability
multi-select), AI proposes the capability set from intent for custom routines, templates lock
theirs, enforcement is gate-mapped where it maps and advisory otherwise. Build with the Phase 3
create form.

### 5.4 Templates (gallery + preview)
The collapsible "Start from a template" gallery stays, but each card gains a **preview** (expand to
see the full operational prompt + the preset schedule/scope) before "Use this template", which
prefills the create form fully editably. **Templates to ship (resolved):** bug-triage and
security-scan (exist), plus dependency-and-license sweep, stale-PR nudge, docs-drift check, and
**daily digest**. Templates are data, so adding them is cheap.

### 5.5 Run now + verdict
"Run now" executes through the real gate immediately and records the run. The result shows outcome +
denies/allows, and (NEW) the **denied rules** inline so the user sees *why* it blocked, not just that
it did. A blocked run raises an escalation (existing path).

### 5.6 Run history (NEW, the biggest addition)
Each routine keeps a **bounded history of runs** (proposed: last N, e.g. 20). A run record:
`{ timestamp, trigger (scheduled|manual), outcome, denies, allows, denied_rules, escalation_id? }`.
The dashboard shows the history when a row is expanded (or in a drawer): a compact list, newest
first, each row linking to its escalation if it blocked. This needs a **data-model addition** (§7).
**OPEN-F:** bounded in-memory history per routine (simple, proposed) vs a shared run-record substrate
with stories (the dashboard ADR's open question). I recommend the bounded per-routine list now; a
shared substrate is a larger change we do not need yet.

### 5.7 Escalation / review (exists, keep)
When a routine blocks, the inline panel shows the reason, what it stopped for, a review conversation
(ask-for-clarification that does NOT unblock), suggestions, and an explicit "Authorize and unblock".
Keep as is; **OPEN-G:** promote to a right drawer so reviewing does not reflow the table.

### 5.8 Empty / loading / not-provisioned states
- Empty: "No routines yet. Add one below, or start from a template."
- Imported (unprovisioned): a clear "Set up" affordance + a note that imported routines start
  disabled by design (safety), and must be set up + started before the scheduler will fire them.

---

## 6. Visual design

- **Vocabulary:** Bletchley industrial amber, consistent with Rules and Governed Development. Reuse
  the existing tokens (page tint, surfaces, `--accent`).
- **Table:** the central `CamerataTable` (sortable, collapsible groups), replacing the bespoke
  `routine-table`. Status badges keep their color language: running (amber/active), blocked (red),
  done (green), failed (red-muted), idle (neutral).
- **Status strip:** a single slim row of count-pills above the table.
- **Create panel:** below the table by default (today's placement), with clear section labels; or a
  right-side drawer (**OPEN-B/C/G** all touch placement; decide together).
- **Scope pills:** labeled, color-keyed by how much the routine can do (read-only neutral, write
  amber, write+PR stronger).
- **Translucency:** the page rides the single page tint over the Bombe like every other page (one
  layer, never stacked).

---

## 7. Data-model changes needed

Almost everything exists. The one real addition is **run history**:

- Add `runs: Vec<RoutineRun>` (bounded) to `Routine`, or a parallel `RoutineRunStore`. A `RoutineRun`
  = `{ id, routine_id, ts (RFC3339), trigger: Scheduled|Manual, summary: RoutineRunSummary,
  escalation_id: Option<String> }`. `run_now` and the auto-fire tick push a record (capped at N,
  FIFO). Serde-default so existing `routines.json` rehydrates with an empty history.
- A new endpoint `GET /api/routines/:id/runs` (or fold the recent runs into the list payload).
- Optionally a small `next_fire(schedule, now) -> Option<DateTime>` helper in `schedule.rs` (we have
  `most_recent_slot`; add the symmetric `next_slot`) to drive the next-fire column + the "due in 24h"
  status metric. Pure + unit-testable.

No change to the gate, the scheduler loop, or the engine: this is additive observability.

---

## 8. Testing plan (the user asked for thorough coverage)

### Unit
- **UI (currently zero):** `build_schedule`/`parse_schedule` round-trip for all four frequencies +
  edge cases (empty fields, day clamping); `status_badge` mapping; the project-grouping sort
  (global last); scope-label mapping; next-fire formatting.
- **Server (mostly exists, extend):** `next_slot`/`next_fire` (new), run-history push + FIFO cap +
  serde-default rehydration, the "due in 24h" metric.

### Integration (server `tests/`)
- A `routines_api_e2e.rs` walking every endpoint through `router(state)`:
  create -> list -> draft-prompt -> templates -> instantiate -> enable -> provision -> run -> update
  -> delete, asserting the state transitions and payloads. Include the blocked path: run a routine
  whose scripted gate denies, assert an escalation is raised, then answer it and assert it unblocks.

### End to end (at least one, hermetic)
- A `routine_lifecycle_e2e.rs`: the full scheduled-routine loop with the in-memory stores + the
  scripted gate (no real AI/network). Create from a template -> provision + enable -> drive the
  `auto_fire::tick` at a time the schedule is due -> assert it fired once (and not twice), stamped
  `last_fired`, recorded a run in history, and (for a blocking routine) raised an escalation;
  answer the escalation -> assert unblock + status returns to Idle. This is the "routine E2E" we
  flagged when finishing the governed-dev and onboarding suites.

All hermetic, matching the existing suites' style (scripted gate, deterministic clock passed in).

---

## 9. Build phases (proposed order)

1. **Run history (data + endpoint + tests)** -- the load-bearing addition; everything visual builds
   on it. **DONE** (`2207b8f`): RoutineRun + bounded history, run_now/run_now_scheduled recording,
   escalation linking, `GET /api/routines/:id/runs`, unit + integration tests.
2. **Status strip + next-fire column.** **DONE** (server `d9db513`: `schedule::next_fire` + the list
   payload's `next_fire_label`/`due_soon`; UI `ce4166f`: filterable count-pill strip + next-fire
   subline). **CamerataTable conversion: DEFERRED** -- the routine rows are rich (multi-action + a
   full-width inline escalation panel rendered as a grid sibling), a poor fit for the tabular
   CamerataTable/chorale primitive, which is also `pub(super)` to cockpit while routines is a
   top-level module. High effort + risk, low benefit; the bespoke table stays.
3. **Create/edit polish** (scope vocabulary decision, sections) + **template preview**.
4. **Run-history UI** (row expand or drawer) + denied-rules surfacing.
5. **Escalation drawer** (optional polish).
6. **Tests throughout** (UI unit as we touch helpers; the integration + E2E suites in phase 1-4).

---

## 10. Decisions (RESOLVED 2026-06-29)

All settled; see the "Resolved decisions (v0.2)" block at the top for the canonical list.
- **A** status strip: total / enabled / running / blocked / due-in-24h; segments filter the table.
- **B/C/G/H** placement: INLINE panels throughout (create/edit below the table, run history as a
  row-expand, escalation inline); single sectioned create form (no wizard).
- **D** scope: emit-cascade language (Read-only base; Write -> branch -> push -> open PR, cascading).
- **E** templates: bug-triage, security-scan, dependency-and-license sweep, stale-PR nudge,
  docs-drift check, daily digest.
- **F** run records: bounded per-routine history (FIFO), not a shared substrate.

Authoring (resolved): intent-first is the default AND a true "paste an exact prompt, skip the AI
draft" escape hatch is available for power users. Free-text editing of the prompt is always
available either way. The form offers both: draft-from-intent, or paste-your-own.

Next: build **phase 1 (run history: data + endpoint + tests)**, the load-bearing addition.
