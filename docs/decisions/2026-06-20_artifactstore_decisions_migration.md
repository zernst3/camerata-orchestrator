# ArtifactStore-backed decision records + investigation notes (ROUTE-A)

Date: 2026-06-20
Status: Accepted
Scope: `crates/persistence`, `crates/server/src/uow.rs`, `crates/server/src/lib.rs` (AppState wiring)

## What

Per-story decision records and investigation notes are migrated from living
inline on the Unit of Work (UoW) to the central, version-tracked SQLite
`ArtifactStore`. The store gives queryable, versioned, provenance-tracked
history for these governance artifacts, the same treatment every other living
document (onboarding doc, user story, clarification) already gets.

Concretely:

1. Two new `ArtifactKind` variants — `DecisionRecord` and `InvestigationNote` —
   with their `as_str` / `parse_str` arms (stored strings `"decision_record"`
   and `"investigation_note"`).
2. `AppState` now holds an optional `Arc<dyn ArtifactStore>`, opened on the
   per-user data dir at `<data_dir>/camerata/artifacts.db` in
   `AppState::from_env` (the same data dir as `uow.json`, `projects.json`, etc.).
3. `UowStore::set_decisions` records each write as a NEW revision in the store
   (one revision per call) with actor + op provenance, so the decision history
   IS the revision history. The governed-dev gate
   (`decisions_approved_for_development`) reads decisions THROUGH the store via
   the new `UowStore::decisions_for`, falling back to the inline cache when no
   store is attached.
4. Investigation notes are persisted via `UowStore::set_investigation_note` /
   read via `UowStore::investigation_note_for`, keyed by the existing
   `"{story_id}/investigation"` convention. The investigation phase does not yet
   author notes through this path; the store path + typed accessors are provided
   so it can.

## Why

The decision records were parked inline on the UoW as an explicitly-temporary
home (see the old field doc and the `2026-06-19_investigation_and_decision_phases`
doc). Inline storage loses everything the architect needs at QA: there is no
diff between the AI's first proposal and the architect's approved version, no
provenance on who changed what when, and no way to query decision history across
stories. The `ArtifactStore` already solves exactly this for other living
documents (append-only revisions, per-artifact monotonic versions, actor + op
columns, time-travel reads). Routing the decisions there is the natural home;
it was held as ROUTE-A only because it is a cross-crate public-API addition
(new `ArtifactKind` variants), which was approved for this task.

## How

### Persistence additions (additive, back-compatible)

`ArtifactKind` gains two variants. All `match` arms (`as_str`, `parse_str`) are
exhaustively updated; the round-trip test covers both new variants and asserts
their exact wire strings so an accidental rename is caught.

### UoW ↔ store bridge

The UoW mutator API is synchronous (`Arc<Mutex<HashMap>>` + best-effort JSON
flush), while `ArtifactStore` is async. `UowStore` now optionally carries an
`Arc<dyn ArtifactStore>` plus a captured `tokio::runtime::Handle`. The sync API
drives the async store through a small `block_on_artifacts` helper that uses
`tokio::task::block_in_place` + `Handle::block_on` (the server runs on the
multi-thread runtime, which `block_in_place` requires). The helper is
panic-guarded so a misuse degrades to in-memory-only rather than crashing.

Decisions are filed under a single stable `project_id` namespace
(`UOW_ARTIFACT_PROJECT = "camerata-uow"`) with `artifact_id = "{story_id}/decisions"`,
carrying the FULL decision set as the payload (the gate reasons over the whole
set, so the set is the unit of revision). Investigation notes use
`artifact_id = "{story_id}/investigation"`.

### Read-through + back-compat

`decisions_for(story_id)`:

- No store attached → returns the inline `decisions` field (unchanged legacy
  behaviour; in-memory tests and a no-data-dir launch keep working).
- Store attached → performs a one-time, idempotent hydrate of any inline
  decisions loaded from an older `uow.json` that have no store revision yet
  (lazy, lossless migration), then reads the latest revision from the store and
  keeps the inline cache coherent with it.

The gate methods (`approve_decisions`, `start_development`) now read through
`decisions_for`, so the gate always sees the store's source of truth when one
exists. No existing data is lost: a legacy UoW's inline decisions are migrated
into the store the first time they are read.

### AppState wiring

`from_env` opens `SqliteStore::open_path(<data_dir>/camerata/artifacts.db)`
(best-effort: a missing runtime handle or sqlx error leaves the UoW on its inline
behaviour and the app still runs), stores the handle on `AppState.artifacts`, and
attaches it to the UoW via `UowStore::with_artifacts`. `AppState::new` /
`seeded()` (tests, demos) leave `artifacts` as `None`.

## Usage

```rust
// Production wiring (already done in AppState::from_env):
let store: Arc<dyn ArtifactStore> = Arc::new(SqliteStore::open_path(&db_path).await?);
let uow = UowStore::at(uow_json_path).with_artifacts(store);

// Writing decisions records a new revision automatically:
uow.set_decisions(story_id, decisions);

// The gate reads through the store:
let decisions = uow.decisions_for(story_id);
let permitted = decisions_approved_for_development(&decisions);

// Investigation notes:
let version = uow.set_investigation_note(&note);   // Some(version) when store-backed
let current = uow.investigation_note_for(story_id);
```

## Alternatives considered

- **Make the whole `UowStore` async.** Rejected for this task's blast radius:
  the UoW mutator API has many sync callers across the server; converting it
  would ripple far beyond the confined scope. The `block_in_place` bridge keeps
  the change additive and local.
- **Drop the inline `decisions` field entirely.** Rejected: it is the
  back-compat carrier for existing `uow.json` files and the read cache for the
  no-store path. Keeping it (now documented as a cache) preserves every existing
  behaviour while the store becomes the source of truth.
- **One revision per decision (not per set).** Rejected: the gate reasons over
  the whole set, and "the architect changed the set" is the meaningful audit
  event. Per-set revisions keep the history aligned with how the gate reads it.
</content>
</invoke>
