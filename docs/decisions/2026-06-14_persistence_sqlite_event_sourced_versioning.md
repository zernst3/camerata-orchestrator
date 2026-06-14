# Persistence: SQLite now, event-sourced versioning, Postgres later

Date: 2026-06-14
Status: Accepted
Deciders: Zach (PO), Claude (architect)

## Context

The consumer-mode artifacts (onboarding document, user stories, clarifications,
product suggestions, refinement sessions) are the product's source of truth, and
Zach required that they persist to a database, update in real time as the user AND
the AI edit, and carry full version history. The open questions were: SQLite or
Postgres, and whether to use database-native temporal (system-versioned) tables.

## Decision

1. **Engine: embedded SQLite for the prototype and the desktop cockpit.** It is
   zero-ops, ships in one binary, and matches the single-process Tokio monolith the
   whole orchestrator already is (MONOLITH-1). The persistence is behind the
   `Store` / `ArtifactStore` trait seam, so a later managed-cloud direction
   (VISION) swaps in managed Postgres without touching callers.

2. **Versioning: an application-level event-sourced revision log, NOT
   database-native temporal/system-versioned tables.** One append-only
   `artifact_revisions` table: every edit appends a new row with a per-artifact
   monotonic `version`, the `actor` (user vs AI), the `op` (create/update/delete),
   a JSON `payload` snapshot, and `created_at`. The "current" state is the latest
   non-deleted revision per artifact; the history is every revision in version
   order; `revision_at` gives time-travel.

## Why not Postgres now

The prototype is local / BYO-infra and single-process. Postgres adds an external
service, connection management, and ops for zero benefit at prototype scale. The
seam makes the later switch cheap, so paying that cost now is premature.

## Why not temporal tables

- SQLite has no native temporal tables at all.
- Postgres has no native SQL:2011 system-versioned tables in core; it needs a
  trigger-based extension (`temporal_tables`) or hand-rolled history tables.
- Our requirement is richer than row-state-over-time: we need to know WHO made each
  change (user or AI), the intent (operation), and a plain-language note, and to
  reconstruct a session's whole back-and-forth transcript. That is an event log,
  which is a superset of what temporal tables provide.
- The event-sourced log is portable across SQLite and Postgres unchanged, so it
  survives the engine switch the seam is designed for.

## Consequences

- `crates/persistence` gains `ArtifactStore` (append-only, versioned). Higher
  layers serialize their typed artifacts to JSON; persistence stays generic and
  does not depend on `camerata-intake`.
- "Real-time updates" is a wiring obligation on the UI: each user/AI edit calls
  `record_revision` immediately. The store already supports per-edit append.
- A later managed-cloud direction must port the schema + queries to Postgres behind
  the same trait; the event-sourced shape means no data-model rethink.
