//! Append-only enforcement-catch ledger.
//!
//! An `enforcement_catches` table records every time the Layer-1 gate denied a write,
//! the Layer-2 check bounced a stage, or the floor audit caught a violation. It is a
//! WRITE-ONLY, DURABLE, APPEND-ONLY evidence log: nothing in the app reads it back;
//! it exists for external SQL analytics (prevented-merges count, per-rule breakdown,
//! revised-after rate, trend over time).
//!
//! Design principles honored:
//! - RUST-DOMAIN-4: newtype IDs
//! - RUST-DOMAIN-5: async I/O throughout
//! - RUST-DOMAIN-6: thiserror error enum (reused from crate root)
//! - SQL-AUDIT-COLUMNS-1: `ts_ms` on every row (epoch-ms for quick range queries)
//! - SQL-DB-INDEX-1/2: WHERE columns indexed
//! - ORCH-NEW-PATH-TESTS-1: unit tests included in this file
//! - FAIL-SOFT: callers must never propagate errors from this module; they log-and-swallow
//! - WRITE-ONLY: no read / query path exists in app code; external SQL only
//! - CONTENT-HASH-NOT-RAW: store a SHA-256 hex hash of offending content, never the raw
//!   string (public repo; the offending content may itself be a secret)

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{error::PersistenceError, store::SqliteStore};

// ---------------------------------------------------------------------------
// Migration SQL (idempotent)
// ---------------------------------------------------------------------------

const CREATE_ENFORCEMENT_CATCHES: &str = "
CREATE TABLE IF NOT EXISTS enforcement_catches (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    ts_ms           INTEGER NOT NULL,
    layer           TEXT    NOT NULL,  -- 'gate' | 'layer2' | 'floor'
    verdict         TEXT    NOT NULL,  -- 'deny' | 'bounce' | 'catch'
    rule_id         TEXT,
    repo            TEXT,
    path            TEXT,
    line            INTEGER,
    content_hash    TEXT,              -- SHA-256 hex of offending content; NEVER the raw string
    run_id          TEXT,
    story_id        TEXT,
    revised_after   INTEGER            -- nullable bool: 0=no, 1=yes (agent revised after deny)
);
";

const CREATE_IDX_CATCHES_LAYER_TS: &str = "
CREATE INDEX IF NOT EXISTS idx_enforcement_catches_layer_ts
    ON enforcement_catches(layer, ts_ms);
";

const CREATE_IDX_CATCHES_RULE_ID: &str = "
CREATE INDEX IF NOT EXISTS idx_enforcement_catches_rule_id
    ON enforcement_catches(rule_id);
";

// ---------------------------------------------------------------------------
// Model
// ---------------------------------------------------------------------------

/// A single enforcement catch record to be inserted into the ledger.
///
/// This is the INSERT-only shape: `id` is assigned by the database;
/// every other field comes from the capture site.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnforcementCatch {
    /// Unix-epoch milliseconds when the catch was recorded.
    pub ts_ms: i64,
    /// Which layer produced the catch: `"gate"` (Layer-1 deny), `"layer2"` (Layer-2
    /// bounce), or `"floor"` (deterministic floor audit finding).
    pub layer: String,
    /// The outcome at this layer: `"deny"` (gate), `"bounce"` (layer-2), `"catch"` (floor).
    pub verdict: String,
    /// The rule id that fired, if applicable.
    pub rule_id: Option<String>,
    /// The repo (`owner/repo`) where the catch occurred, if applicable.
    pub repo: Option<String>,
    /// The file path involved, if applicable.
    pub path: Option<String>,
    /// 1-based line number, if applicable.
    pub line: Option<i64>,
    /// SHA-256 hex digest of the offending content (NEVER the raw string). None when
    /// no content is available (e.g. a path-based rule or delegation deny).
    pub content_hash: Option<String>,
    /// The run id this catch is associated with, if applicable.
    pub run_id: Option<String>,
    /// The story id this catch is associated with, if applicable.
    pub story_id: Option<String>,
    /// Whether a LATER event in the same run was an `allow` on the SAME target —
    /// i.e. the agent revised and the write later succeeded. `None` = not computed
    /// / not applicable.
    pub revised_after: Option<bool>,
}

impl EnforcementCatch {
    /// Construct a gate-layer catch (Layer-1 deny).
    pub fn gate(
        ts_ms: i64,
        rule_id: Option<String>,
        path: Option<String>,
        content_hash: Option<String>,
        run_id: Option<String>,
        story_id: Option<String>,
        revised_after: Option<bool>,
    ) -> Self {
        Self {
            ts_ms,
            layer: "gate".to_string(),
            verdict: "deny".to_string(),
            rule_id,
            repo: None,
            path,
            line: None,
            content_hash,
            run_id,
            story_id,
            revised_after,
        }
    }

    /// Construct a floor-layer catch (deterministic audit finding).
    pub fn floor(
        ts_ms: i64,
        rule_id: String,
        repo: String,
        path: String,
        line: i64,
        content_hash: Option<String>,
    ) -> Self {
        Self {
            ts_ms,
            layer: "floor".to_string(),
            verdict: "catch".to_string(),
            rule_id: Some(rule_id),
            repo: Some(repo),
            path: Some(path),
            line: Some(line),
            content_hash,
            run_id: None,
            story_id: None,
            revised_after: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Trait seam
// ---------------------------------------------------------------------------

/// Write-only enforcement-catch ledger trait.
///
/// Append-only: `record_catch` inserts one row; there is NO read/query path.
/// All callers MUST fail-soft: a `Err(...)` return must be logged and swallowed,
/// never propagated.
#[async_trait]
pub trait EnforcementCatchLedger: Send + Sync {
    /// Create / migrate the `enforcement_catches` table. Idempotent.
    async fn migrate_enforcement(&self) -> Result<(), PersistenceError>;

    /// Append one catch record. Single INSERT. Append-only.
    /// Callers MUST log-and-swallow any returned error (fail-soft invariant).
    async fn record_catch(&self, catch: EnforcementCatch) -> Result<(), PersistenceError>;
}

// ---------------------------------------------------------------------------
// SqliteStore impl
// ---------------------------------------------------------------------------

#[async_trait]
impl EnforcementCatchLedger for SqliteStore {
    async fn migrate_enforcement(&self) -> Result<(), PersistenceError> {
        sqlx::query(CREATE_ENFORCEMENT_CATCHES)
            .execute(self.pool())
            .await?;
        sqlx::query(CREATE_IDX_CATCHES_LAYER_TS)
            .execute(self.pool())
            .await?;
        sqlx::query(CREATE_IDX_CATCHES_RULE_ID)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    async fn record_catch(&self, catch: EnforcementCatch) -> Result<(), PersistenceError> {
        let revised_after_int: Option<i64> = catch.revised_after.map(|b| if b { 1 } else { 0 });
        sqlx::query(
            "INSERT INTO enforcement_catches
                 (ts_ms, layer, verdict, rule_id, repo, path, line,
                  content_hash, run_id, story_id, revised_after)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        )
        .bind(catch.ts_ms)
        .bind(&catch.layer)
        .bind(&catch.verdict)
        .bind(&catch.rule_id)
        .bind(&catch.repo)
        .bind(&catch.path)
        .bind(catch.line)
        .bind(&catch.content_hash)
        .bind(&catch.run_id)
        .bind(&catch.story_id)
        .bind(revised_after_int)
        .execute(self.pool())
        .await?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Content hashing (SHA-256 hex — preimage-resistant; ledger is portable proof)
// ---------------------------------------------------------------------------

/// Compute the SHA-256 hash of `content`, returned as a 64-char lowercase hex string.
///
/// SHA-256 (not a fast non-crypto fingerprint) is deliberate: the ledger is meant to be
/// PORTABLE — shared as proof the gate works — and the hashed slice is offending code that
/// MIGHT contain a secret. A preimage-resistant hash means the shared ledger never lets a
/// hashed secret be brute-forced back. Stable across machines and Rust versions.
///
/// Callers MUST pass the offending content to this; they MUST NOT store the raw content.
pub fn content_hash(content: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(content.as_bytes());
    format!("{digest:x}")
}

// ---------------------------------------------------------------------------
// Tests (ORCH-NEW-PATH-TESTS-1)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::SqliteStore;
    use sqlx::Row as _;

    async fn in_memory() -> SqliteStore {
        SqliteStore::open("sqlite::memory:")
            .await
            .expect("in-memory store")
    }

    // ── Migration is idempotent ─────────────────────────────────────────────

    #[tokio::test]
    async fn migrate_enforcement_is_idempotent() {
        let store = in_memory().await;
        // Already migrated in `open`; calling again must not error.
        store
            .migrate_enforcement()
            .await
            .expect("second migrate is a no-op");
    }

    // ── Gate catch roundtrip ────────────────────────────────────────────────

    #[tokio::test]
    async fn gate_catch_insert_and_read_back() {
        let store = in_memory().await;
        let hash = content_hash("let token = \"secret\";");
        let catch = EnforcementCatch::gate(
            1_700_000_000_000,
            Some("SEC-NO-HARDCODED-SECRETS-1".to_string()),
            Some("crates/api/src/config.rs".to_string()),
            Some(hash.clone()),
            Some("run-42".to_string()),
            Some("CAM-7".to_string()),
            Some(false),
        );
        store.record_catch(catch).await.expect("record_catch gate");

        // Read back via raw SQL to confirm the row exists (write-only in app; tests may read).
        let row = sqlx::query(
            "SELECT layer, verdict, rule_id, path, content_hash, run_id, story_id, revised_after
             FROM enforcement_catches WHERE id = 1",
        )
        .fetch_one(store.pool())
        .await
        .expect("row exists");

        let layer: String = row.try_get("layer").unwrap();
        let verdict: String = row.try_get("verdict").unwrap();
        let rule_id: Option<String> = row.try_get("rule_id").unwrap();
        let path: Option<String> = row.try_get("path").unwrap();
        let ch: Option<String> = row.try_get("content_hash").unwrap();
        let run: Option<String> = row.try_get("run_id").unwrap();
        let story: Option<String> = row.try_get("story_id").unwrap();
        let revised: Option<i64> = row.try_get("revised_after").unwrap();

        assert_eq!(layer, "gate");
        assert_eq!(verdict, "deny");
        assert_eq!(rule_id.as_deref(), Some("SEC-NO-HARDCODED-SECRETS-1"));
        assert_eq!(path.as_deref(), Some("crates/api/src/config.rs"));
        assert_eq!(ch.as_deref(), Some(hash.as_str()));
        assert_eq!(run.as_deref(), Some("run-42"));
        assert_eq!(story.as_deref(), Some("CAM-7"));
        assert_eq!(revised, Some(0)); // false → 0
    }

    // ── Floor catch roundtrip ───────────────────────────────────────────────

    #[tokio::test]
    async fn floor_catch_insert_and_read_back() {
        let store = in_memory().await;
        let snippet = "password = \"hunter2\"";
        let hash = content_hash(snippet);
        let catch = EnforcementCatch::floor(
            1_700_000_001_000,
            "SEC-NO-HARDCODED-SECRETS-1".to_string(),
            "owner/myrepo".to_string(),
            "src/db.py".to_string(),
            42,
            Some(hash.clone()),
        );
        store.record_catch(catch).await.expect("record_catch floor");

        let row = sqlx::query(
            "SELECT layer, verdict, repo, path, line, content_hash, revised_after
             FROM enforcement_catches WHERE id = 1",
        )
        .fetch_one(store.pool())
        .await
        .expect("row exists");

        let layer: String = row.try_get("layer").unwrap();
        assert_eq!(layer, "floor");
        let verdict: String = row.try_get("verdict").unwrap();
        assert_eq!(verdict, "catch");
        let repo: Option<String> = row.try_get("repo").unwrap();
        assert_eq!(repo.as_deref(), Some("owner/myrepo"));
        let line: Option<i64> = row.try_get("line").unwrap();
        assert_eq!(line, Some(42));
        let ch: Option<String> = row.try_get("content_hash").unwrap();
        assert_eq!(ch.as_deref(), Some(hash.as_str()));
        let revised: Option<i64> = row.try_get("revised_after").unwrap();
        assert!(revised.is_none(), "floor catches have no revised_after");
    }

    // ── Multiple rows are append-only (never update) ────────────────────────

    #[tokio::test]
    async fn append_only_multiple_rows() {
        let store = in_memory().await;

        for i in 0..5i64 {
            let catch = EnforcementCatch {
                ts_ms: 1_000_000 + i,
                layer: "gate".to_string(),
                verdict: "deny".to_string(),
                rule_id: Some("GOV-1".to_string()),
                repo: None,
                path: Some(format!("file{i}.rs")),
                line: None,
                content_hash: Some(content_hash(&format!("content{i}"))),
                run_id: Some("run-1".to_string()),
                story_id: None,
                revised_after: None,
            };
            store.record_catch(catch).await.expect("insert");
        }

        let count: i64 = sqlx::query("SELECT COUNT(*) as c FROM enforcement_catches")
            .fetch_one(store.pool())
            .await
            .expect("count")
            .try_get("c")
            .unwrap();
        assert_eq!(count, 5, "all rows inserted; none updated");

        // Confirm ids are monotonically increasing (AUTOINCREMENT).
        let ids: Vec<i64> = sqlx::query("SELECT id FROM enforcement_catches ORDER BY id")
            .fetch_all(store.pool())
            .await
            .expect("ids")
            .iter()
            .map(|r| r.try_get::<i64, _>("id").unwrap())
            .collect();
        assert_eq!(ids, vec![1, 2, 3, 4, 5]);
    }

    // ── content_hash is stable and collision-resistant ──────────────────────

    #[test]
    fn content_hash_stable() {
        // Same input → same output, every time.
        assert_eq!(content_hash("hello"), content_hash("hello"));
        // Different inputs → different outputs.
        assert_ne!(content_hash("hello"), content_hash("world"));
        // 64 hex chars (SHA-256).
        assert_eq!(content_hash("test").len(), 64);
        // Known SHA-256 vector for "test" — proves it's real SHA-256, not a fingerprint.
        assert_eq!(
            content_hash("test"),
            "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
        );
    }

    // ── raw content is NEVER stored (write-only design verification) ─────────

    #[test]
    fn content_hash_does_not_expose_raw() {
        // The hash function is one-way: verifying the raw string is not recoverable
        // from the hash (structural: there's no decode path in this module).
        let raw = "ghp_ABCDEFGHIJ1234567890abcdefghij12";
        let h = content_hash(raw);
        // The hash must not contain the raw token.
        assert!(!h.contains("ghp_"), "hash must not contain the raw secret");
    }

    // ── revised_after bool encoding ─────────────────────────────────────────

    #[tokio::test]
    async fn revised_after_true_stores_as_1() {
        let store = in_memory().await;
        let catch = EnforcementCatch {
            ts_ms: 0,
            layer: "gate".to_string(),
            verdict: "deny".to_string(),
            rule_id: None,
            repo: None,
            path: None,
            line: None,
            content_hash: None,
            run_id: None,
            story_id: None,
            revised_after: Some(true),
        };
        store.record_catch(catch).await.unwrap();
        let revised: Option<i64> =
            sqlx::query("SELECT revised_after FROM enforcement_catches WHERE id = 1")
                .fetch_one(store.pool())
                .await
                .unwrap()
                .try_get("revised_after")
                .unwrap();
        assert_eq!(revised, Some(1));
    }
}
