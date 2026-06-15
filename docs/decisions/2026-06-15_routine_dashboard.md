# Routine dashboard: manage scheduled, governed agent routines

Date: 2026-06-15
Status: Accepted (design); NOT built.
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

Design only; not built. Prerequisites: the run model (Phase 3, in progress), a routine
definition + store, and a scheduler that fires them. Today there is no routine concept
in the codebase.

## Open questions

- The scheduler: in-process timer vs an OS scheduler (launchd/cron) vs a cloud
  scheduler when hosted. The cloud-hostable goal argues for an engine-owned scheduler
  rather than depending on the host OS.
- Whether a "routine" and a "story" share a run record or are distinct kinds over a
  common run substrate.
- Secrets/permissions for routines that touch third-party providers (ties to the
  Phase 4/5 auth work).
