# ADR: Server route correctness (status codes, read/write store access, deep-report scoping, no per-request env)

**Date:** 2026-07-05
**Status:** Accepted (Batch 4 shipped on `fix/routes-correctness`, stacked on `fix/checkrunner-diagnostics`)

## Context

The Fable 5 audit (`docs/ARCH_AUDIT_2026-07-04_fable5-complete.md`) found four HTTP-layer
correctness defects in the server crate. They are independent but share a theme: a request
handler doing the wrong thing at the boundary (wrong status, a write on a read, a process-global
mutation, or an unscoped lookup).

- **ROUTES-9: process-global `set_var` in request handlers.** `set_llm_backend` and
  `set_credential` called `std::env::set_var("CAMERATA_LLM_BACKEND" / "ANTHROPIC_API_KEY", ...)`
  at request time so a runtime backend/key change took effect without a restart. But the server is
  multithreaded: a request-handler thread writing an env var while worker threads read the same var
  via `getenv` is undefined behaviour on POSIX. It also made
  `credentials::tests::resolve_falls_back_to_env_when_store_empty` flaky under concurrent test runs
  (a concurrent writer racing its `getenv`).
- **ROUTES-5: `latest_deep_report` was not project-scoped.** It iterated all jobs in `HashMap`
  order and returned the FIRST job carrying a deep report, an arbitrary job that could belong to a
  DIFFERENT project. `GET /api/projects/:id/deep-report` could hand back another project's audit.
- **ROUTES-7: every failure mapped to HTTP 500.** `AppError` already carried a `status` field, but
  the default `AppError(e)` constructor and the `?` conversion set 500, and handlers rarely used
  `with_status`. A missing run returned 500; a malformed repo returned 500, even where the
  handler's own docs promised 4xx.
- **ROUTES-8: read GETs created records.** ~Five read handlers (`GET /api/uow/:id`, attachments
  list/get, diagram get, the mockup parent lookup, PR get) plus `decisions_for` called
  `get_or_create` / `or_insert_with`. A GET with a typo'd id materialized AND persisted a junk empty
  UoW that then leaked into every list view. A read has no business writing.

## Decision

### 1. No process-global `set_var` in request handlers (ROUTES-9)

- Both handlers drop the per-request `std::env::set_var`. The settings store is already the source
  of truth for the backend, and the credential store for the key, the handlers already persist to
  them.
- `anthropic_api_backend_key` now reads the Anthropic key from the CREDENTIAL STORE first (env
  fallback for back-compat), so a freshly-saved key still takes effect without a restart AND without
  touching process env. It threads the store through `build_claude_driver` / `build_agent_driver`
  (both already receive `creds`) and the orchestrator factory.
- **Startup env hydration is unchanged.** The single-threaded boot path still mirrors the persisted
  backend + keychain key into env once (before any worker thread exists), which is safe. Only the
  per-REQUEST mutation is removed. The backend signal is still read from `CAMERATA_LLM_BACKEND`
  (startup-hydrated); only the KEY moved to a store read.

### 2. Deep-report export is project-scoped + latest (ROUTES-5)

- `JobState` gains `project_id` (captured at creation from the active project) and
  `completed_at_ms` (stamped in `finish`).
- `latest_deep_report(project_id)` filters to that project's completed deep jobs and returns the one
  with the greatest `completed_at_ms`, newest wins WITHIN the project, deterministically, not by
  `HashMap` order. The export handler passes the URL's project id.

### 3. Correct HTTP status codes (ROUTES-7)

- Add `AppError::not_found` (404) and `AppError::bad_request` (400) alongside the existing
  `with_status`. The default `AppError(e)` / `?` still means 500 (a genuine internal fault).
- ~25 not-found sites (get_run, get_run_provenance, sign_off_run, routine / escalation / project /
  story / clarification / template lookups) return 404. ~10 input-validation sites (adopt_issue repo,
  uow_publish repo/title/gate, `parse_github_work_item_id`, comment/mockup/attachment empty-body,
  suppression reason + repo) return 400. `answer_escalation`'s already-resolved guard returns 409
  Conflict (the resource exists but is not open).
- **The response BODY is unchanged** (`{ "error": "..." }`). Only the status code is corrected, so UI
  code that parses `{error}` keeps working.

### 4. Read GETs don't create (ROUTES-8)

- Add non-creating store getters: `UowStore::get(id) -> Option<UnitOfWork>` and
  `get_or_default(id)` (a transient, `story_id`-stamped UoW that is NOT persisted).
- Read handlers switch to them; `decisions_for` reads without `or_insert`. An unknown id returns a
  UoW-shaped body (empty attachments/diagram/decisions) with nothing written.
- WRITE handlers (author, attach, diagram-set, set-status/branch, publish, run start) keep
  `get_or_create`, they legitimately upsert.

## Consequences

- No undefined behaviour from concurrent env mutation; the previously-flaky credential test is
  stable. A runtime backend/key change still takes effect without a restart (store reads).
- A project's deep-report export always reflects ITS OWN latest deep audit.
- Clients (and the UI) can distinguish gone (404) from bad input (400) from already-answered (409)
  from a real fault (500) without a body-shape change.
- A mistyped id no longer pollutes the UoW list with phantom drafts.

## Honest limits

- The backend CHOICE (`CAMERATA_LLM_BACKEND`) is still read from env by the driver-selection path
  (startup-hydrated), not from the settings store per-run. Removing the per-request `set_var` is the
  UB fix; fully threading the settings store through every driver factory was deliberately out of
  scope for this batch (it touches ~10 factory call sites with no correctness win beyond what
  startup hydration already provides).
- ROUTES-7 was applied to the clear-cut 4xx sites the audit enumerated. A few precondition errors
  (e.g. "connect GitHub") were classified as 400 where client-actionable; genuinely upstream/parse
  faults from GitHub responses stay 500.
