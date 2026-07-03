# ADR: Every AI action holds a LoadingGuard (the Bombe invariant)

**Date:** 2026-07-02
**Status:** Accepted (invariant established; first full sweep shipped)
**Related:** `2026-07-02` L3 completeness dimension

## Context

The Bombe background animation is Camerata's "the machine is doing real thinking" signal.
Its UI value depends entirely on it being an accurate signal: the animation must run
whenever AI/long-running work is in progress, and only then. If the animation is idle
while work is running, users get no feedback; if it fires on trivial list fetches, it loses
its gravitas.

The mechanism is `crate::loading::LoadingGuard`: an RAII type that increments a global
ref-counted `Signal<usize>` on construction and decrements (saturating) on `Drop`. The
`BombeBg` component watches this count; when it is positive the `.bombe-running` class
is active. Two or more concurrent guards compose correctly: the count tracks them
independently and the animation stays on until all have dropped.

The prior state: the guard was added to some AI call sites but not all. During a single
session a sweep found 13 sites missing the guard, and then a second pass caught a 14th.
This is the textbook cross-cutting invariant that demands a durable rule, not a one-off
patch.

## Decision

**Every spawn that calls an AI endpoint or starts a long-running operation (a run, an
audit, a scan, a streamed reply) MUST hold a `crate::loading::LoadingGuard` for the
duration of that operation.**

The guard is created at the top of the `async move` closure or the `spawn` block and
held until the operation completes (including streamed/polled completions). It is NOT
used for ordinary list fetches or quick HTTP calls that return immediately.

Formally: guard sites include chat turns, story authoring, investigation/development run
starts, clarification-replay author turns, prompt drafting, deep-report exports, audit job
starts, routine run-now, escalation Ask chats, and any future AI endpoint spawn.
Explicitly NOT guarded: `use_resource` list/detail fetches, credential loads, settings
reads, or any GET that resolves in a single round-trip with no AI model invocation.

### Sites guarded in the 2026-07-02 sweep (commits 0aff90a + d043dc6)

1. `design.rs` — Design Send (Cmd+Enter) `api_design_author`
2. `design.rs` — Design Send (button) `api_design_author`
3. `design.rs` — Mockup Generate `api_generate_mockup`
4. `cockpit/live_run.rs` — escalation Ask chat `chat_uow_escalation`
5. `routines.rs` — Draft operational prompt `draft_prompt`
6. `cockpit/uow.rs` — clarification-replay author turn `post_author_message`
7. `cockpit/scan.rs` — Export deep report `fetch_deep_report`
8. `cockpit/uow.rs` — governed DEV run start `start_dev_run` + `poll_run_to_done`
9. `cockpit/uow.rs` — DEV run start (bug-fix reuse) `start_dev_run`
10. `cockpit/uow.rs` — UPDATE-BRANCH run start `start_update_branch_run`
11. `cockpit/uow.rs` — PR-RESOLVE run start `start_pr_resolve_run`
12. `cockpit/scan.rs` — async AUDIT job start `audit_job_start` + `poll_job`
13. `routines.rs` — routine Run now `run_now`
14. `routines.rs` — routine-escalation Ask chat (d043dc6, the one the sweep missed)

Sites 8 and 12 intentionally double-guard with poll helpers' internal guards; the
ref-counted `LoadingCount` handles concurrent guards correctly, so double-guarding is
safe.

## Connection to L3 completeness

The 14th missing guard (d043dc6) is an exact instance of the failure mode the L3
completeness dimension (2026-07-02 ADR) was introduced to catch: a cross-cutting story
("every AI action must animate the Bombe") where a spot-fix leaves siblings. The
narrative runs the other direction here (the human sweep caught it, the L3 ADR was
written the same day), but the connection is direct and intentional. Future stories of
the same shape ("guard every X") will get an automatic completeness bounce from the L3
reviewer instead of relying on a second human pass.

## Conformance

This rule is codified as `UI-BOMBE-GUARD-1` in CONVENTIONS.md. A new spawn that invokes
an AI endpoint without a `LoadingGuard` violates it. The L3 reviewer's completeness
dimension guards it at review time for cross-cutting stories; mechanical enforcement
(grep for `spawn` or known AI client calls without an adjacent `LoadingGuard`) can be
added to CI if the pattern recurs.

## Files touched

- `crates/ui/src/loading.rs` — `LoadingGuard` RAII definition (pre-existing)
- `crates/ui/src/design.rs` — guards on 3 design call sites
- `crates/ui/src/cockpit/live_run.rs` — guard on escalation Ask
- `crates/ui/src/routines.rs` — guards on draft-prompt, run-now, routine-escalation Ask
- `crates/ui/src/cockpit/scan.rs` — guards on deep-report + audit job
- `crates/ui/src/cockpit/uow.rs` — guards on 4 run-start sites
