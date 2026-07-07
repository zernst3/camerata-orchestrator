# Routine dashboard: manage scheduled, governed agent routines

Date: 2026-06-15
Status: Accepted; dashboard + CRUD BUILT (2026-06-16). Auto-fire scheduler still pending.
Deciders: Zach (architect), Claude (architect)

Companion docs: [`ENFORCEMENT.md`](../ENFORCEMENT.md), [`VISION.md`](../VISION.md),
ADR [`cross_agent_integration_gate`](2026-06-15_cross_agent_integration_gate.md).

## Context

Some agent work is not a one-off story; it is a recurring routine that fires on a
schedule (a nightly audit, a dependency-and-security sweep, a digest). Today those are
managed conversationally, by telling the agent what to run and when, with no surface
showing what exists, what each one does, or its current state. A routine dashboard
makes that legible and manageable.

## Decision: a routine is a scheduled governed run, and the dashboard manages them

A **routine** is a first-class scheduled run: a name, a schedule (fire time / cron),
a prompt (what it does), a permission/rule scope (the rule subset and path/tool
boundaries it runs under), an enabled flag, and a status + run history. Because it
runs through the same engine, a routine is a *governed* run, the gate applies to it
exactly as it does to an interactive run, and its provenance is recorded the same way.

The dashboard is a management surface (a table, the kind of surface the cockpit's
right home is) over the set of routines:

- **List/table:** name, schedule, next fire, last-run status, the prompt summary, the
  permission scope. Dogfoods a Chorale table.
- **Manage:** create / edit / enable-disable / delete, run-now, and view a routine's
  run history (each run's status, gate activity, provenance).
- **Status at a glance:** which are running now, which passed/failed last, which are
  due. The thing you cannot see today.
- **Permissions are explicit and visible.** A routine's rule subset and tool/path
  boundaries are shown and editable, because a scheduled agent running unattended is
  exactly where you want its governance scope to be legible, not implicit.

## Where it sits

This is a management surface distinct from the per-story cockpit, plausibly its own
surface (a third tab alongside the app-builder and the cockpit) or a cockpit panel. It
presupposes the run model (Phase 3 execution), since a routine is a scheduled run and
the dashboard shows run status/history; build it after execution exists.

## Relationship to the engine

Routines reuse the run model and the gate, they are not a separate execution path.
The dashboard is a scheduler + a view over runs, not a new orchestrator. The scheduler
itself (cron/launchd-equivalent that fires routines) is the new infrastructure; the
execution and governance are the existing engine.

## Honest current state

The dashboard and its full CRUD are BUILT (`crates/ui/src/routines.rs`,
`crates/server/src/routine.rs`): list with loading/empty states, create, **edit**,
**delete** (two-click confirm), enable/disable, run-now (real-gate verdict summary).
The remaining gap is the **auto-fire scheduler** — the dashboard manages routines and
run-now executes them, but nothing fires them on their schedule yet (see Open
questions). Endpoints: `GET/POST /api/routines`, `PUT/DELETE /api/routines/:id`,
`POST /api/routines/:id/{enable,run}`, `POST /api/routines/draft-prompt`.

### Schedule + scope are STRUCTURED inputs (2026-06-16)

Two UX decisions made when building the create/edit form, both because free text is the
wrong input for a value with a small known shape:

- **Schedule** is a frequency picker (One-off / Daily / Weekly / Monthly) with the
  controls each frequency needs — weekday toggles, day-of-month, a one-off calendar
  date — plus a native time input, with a live preview of the serialized string. The
  BFF still STORES a human-readable schedule string (`daily 09:00`,
  `weekly Mon,Wed 09:00`, `monthly day 15 09:00`, `once 2026-06-20 14:00`); the UI owns
  the shape and parses it back for Edit prefill. (When the auto-fire scheduler lands it
  will parse this same string — or the picker can emit cron directly then.)
- **Scope** is a select of meaningful permission levels — Read-only (inspect, no
  writes), Write (gated edits on a branch, no push), Write + open PR (pushes a branch,
  opens a PR; nothing auto-merges) — with an inline explanation, not an opaque free-text
  field.

### Scope is a STRUCTURED, ENFORCED boundary, not prose (2026-07-05, GAP-8)

`Routine.scope` was a decorative `String`: it was only interpolated into the scaffolded
prompt, never an enforced boundary. That is the advisory-guardrail anti-pattern Camerata
exists to reject, so the audit flagged it (GAP-8). It is now a structured `RoutineScope`
(`crates/app-core/src/routine.rs`): a **rule subset** + a **write policy** (which drives
the **tool allowlist**) + a **write jail**. These are the SAME enforcement primitives a
live DEV run registers with the gateway, so a routine's scope maps directly onto
`governed_role` + `allowed_tools_for_role` + the `prepare_session` worktree arg via
`resolve_scope_registration` (`crates/server/src/scope_registration.rs`), wired at the
routine-run seam (`RoutineStore::resolve_run_registration`). Serde accepts BOTH a legacy
string scope and the structured object, so routines persisted (or exported) with a string
`scope` load with no data loss. The honest limit: live routine execution itself is still
latent (the auto-fire scheduler runs the token-free scripted gate today), so the
resolution is a real, tested seam that WILL enforce the scope the moment execution lands.
Full rationale: ADR [`routine-structured-scope`](2026-07-05_routine-structured-scope.md).

## Open questions

- The scheduler: in-process timer vs an OS scheduler (launchd/cron) vs a cloud
  scheduler when hosted. The cloud-hostable goal argues for an engine-owned scheduler
  rather than depending on the host OS.
- Whether a "routine" and a "story" share a run record or are distinct kinds over a
  common run substrate.
- Secrets/permissions for routines that touch third-party providers (ties to the
  Phase 4/5 auth work).
