# Enforcement-catch ledger

**Date:** 2026-06-23
**Status:** Implemented (feat/enforcement-catch-ledger)

## Context

We need durable, local evidence that the Layer-1 gate (and Layer-2 / floor) is
working as a "prevented merges" dataset for external analytics. The capture must
not slow or break runs, scans, or requests, and must never store raw secret
content (public repo).

## Decisions

### 1. Storage: reuse `camerata-persistence` SQLite pool

A new `enforcement_catches` table in `camerata-persistence` follows the same
append-only, idempotent-migration pattern as `artifact_revisions` and
`provenance_log`. The `SqliteStore` opens and migrates it on startup alongside
the other tables. In the server, a SEPARATE database file
(`enforcement_catches.db` under the data dir) is used so the catch ledger is
independently inspectable without touching the artifacts store.

### 2. Schema

```sql
CREATE TABLE IF NOT EXISTS enforcement_catches (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    ts_ms           INTEGER NOT NULL,
    layer           TEXT    NOT NULL,  -- 'gate' | 'layer2' | 'floor'
    verdict         TEXT    NOT NULL,  -- 'deny' | 'bounce' | 'catch'
    rule_id         TEXT,
    repo            TEXT,
    path            TEXT,
    line            INTEGER,
    content_hash    TEXT,   -- FNV-1a hex; NEVER raw content
    run_id          TEXT,
    story_id        TEXT,
    revised_after   INTEGER -- nullable bool: 0=no 1=yes
);
CREATE INDEX idx_enforcement_catches_layer_ts ON enforcement_catches(layer, ts_ms);
CREATE INDEX idx_enforcement_catches_rule_id  ON enforcement_catches(rule_id);
```

### 3. Content hash, not raw content

`content_hash` stores the FNV-1a 64-bit hex of the offending content (snippet or
denied write content). The raw string is never inserted. This matches the
existing `suppression::fnv1a` / `scan_cache::content_fingerprint` conventions and
is safe for a public repo where the offending content may itself be a secret.

### 4. Capture at terminal points, not inline hooks

Capture is extracted from ALREADY-RECORDED in-memory data at three terminal
points, not injected inline into gate evaluation:

**Point 1 — Run finalization** (`stamp_provenance_when_done` in `server/lib.rs`):
When a run reaches `done=true` (AwaitingQa, Failed, Cancelled), the watcher
already fires. It now also calls `capture_run_finalization`, which iterates the
run's `GateEvents`, emitting one `gate`/`deny` catch per `verdict == "deny"` and
`layer == "layer-1"` event (or `layer2`/`bounce` for layer-2 failures).
`revised_after` is derived from the event slice: a later `allow` on the same
target means the agent revised and succeeded.

**Point 2 — Scan completion** (`onboard_scan` handler in `server/lib.rs`):
After `audit_repos` returns, a background task calls `capture_scan_findings`,
which writes one `floor`/`catch` record for each `active` floor finding
(`AUDIT_RULES` rule ids). Suppressed and non-floor findings are skipped.

**Point 3 — Gateway DENY observability** (`crates/gateway/src/main.rs`):
The existing `GateDecisionRecord` JSONL sink gains `content_hash: Option<String>`,
set to the FNV-1a hex of the denied write's content on DENY records only. This
field flows through the server-side mirror `GateDecisionRecord` into `GateEvent`,
so Point 1 captures it automatically at run finalization without any separate
read of the denied content.

### 5. Fail-soft everywhere

- `EnforcementLedger` is `Option<Arc<SqliteStore>>`. If `open_path` fails (no
  runtime handle, disk error, permissions), a `None` ledger is used and all
  capture methods are no-ops. The run/scan paths are unaffected.
- Every `record_catch` error is logged via `eprintln!` and swallowed.
  `capture_run_finalization` and `capture_scan_findings` never return errors to
  callers.
- The scan capture runs in a background `tokio::spawn` so the HTTP response is
  never delayed by ledger writes.

### 6. Write-only in app code

There is no read / query path in app code. The ledger is purely `INSERT`-only.
External operators query it directly with SQLite tooling.

## Example SQL queries

**Prevented merges count (all time):**
```sql
SELECT COUNT(*) AS prevented_merges
FROM enforcement_catches
WHERE layer = 'gate' AND verdict = 'deny';
```

**Per-rule breakdown:**
```sql
SELECT rule_id,
       COUNT(*) AS catches,
       SUM(CASE WHEN revised_after = 1 THEN 1 ELSE 0 END) AS revised
FROM enforcement_catches
WHERE layer = 'gate' AND verdict = 'deny'
GROUP BY rule_id
ORDER BY catches DESC;
```

**Revised-after rate (agent learned and fixed it):**
```sql
SELECT
    ROUND(100.0 * SUM(CASE WHEN revised_after = 1 THEN 1 ELSE 0 END) / COUNT(*), 1)
        AS revised_after_pct
FROM enforcement_catches
WHERE layer = 'gate' AND verdict = 'deny';
```

**Trend over time (weekly gate denies):**
```sql
SELECT
    strftime('%Y-W%W', datetime(ts_ms / 1000, 'unixepoch')) AS week,
    COUNT(*) AS gate_denies
FROM enforcement_catches
WHERE layer = 'gate' AND verdict = 'deny'
GROUP BY week
ORDER BY week DESC;
```

**Floor catches by repo:**
```sql
SELECT repo,
       rule_id,
       COUNT(*) AS catches
FROM enforcement_catches
WHERE layer = 'floor' AND verdict = 'catch'
GROUP BY repo, rule_id
ORDER BY catches DESC;
```

## Files changed

- `crates/persistence/src/enforcement_catch.rs` (new) — model, trait, SqliteStore impl, tests
- `crates/persistence/src/lib.rs` — re-exports
- `crates/persistence/src/store.rs` — call `migrate_enforcement()` on open
- `crates/gateway/src/main.rs` — `content_hash` on `GateDecisionRecord`, `fnv1a_hex`, updated `build_gate_record`
- `crates/server/src/run.rs` — `content_hash` field on `GateEvent`
- `crates/server/src/live_fleet.rs` — mirror `content_hash` on server `GateDecisionRecord`, carry through `gate_record_to_event`
- `crates/server/src/enforcement_ledger.rs` (new) — `EnforcementLedger`, capture fns, pure extraction helpers, tests
- `crates/server/src/lib.rs` — `enforcement_ledger` field on `AppState`, wired in `from_env` and `stamp_provenance_when_done`, scan capture in `onboard_scan`
