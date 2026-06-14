//! [`Store`] trait + [`SqliteStore`] impl.
//!
//! Schema migrations are embedded and run at startup via `MIGR_SQL` constants
//! (no separate migration files needed for this crate; keeps the crate
//! self-contained). Migrations are idempotent (`CREATE TABLE IF NOT EXISTS`,
//! `CREATE INDEX IF NOT EXISTS`).

use async_trait::async_trait;
use camerata_core::{Role, SessionId};
use chrono::{DateTime, Utc};
use sqlx::{Pool, Row, Sqlite, SqlitePool};

use crate::{
    error::PersistenceError,
    model::{ProvenanceEntry, ProvenanceId, SessionRecord},
};

// ---------------------------------------------------------------------------
// Migration SQL (idempotent)
// ---------------------------------------------------------------------------

/// DDL for `agent_sessions`.
///
/// - `created_at` — SQL-AUDIT-COLUMNS-1
const CREATE_SESSIONS: &str = "
CREATE TABLE IF NOT EXISTS agent_sessions (
    session_id  TEXT    NOT NULL PRIMARY KEY,
    role        TEXT    NOT NULL,
    started_at  TEXT    NOT NULL,
    created_at  TEXT    NOT NULL
);
";

/// DDL for `provenance_log`.
///
/// - FK `session_id` indexed — SQL-DB-INDEX-1
/// - `created_at` — SQL-AUDIT-COLUMNS-1
const CREATE_PROVENANCE: &str = "
CREATE TABLE IF NOT EXISTS provenance_log (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id          TEXT    NOT NULL REFERENCES agent_sessions(session_id),
    change_description  TEXT    NOT NULL,
    rule_ids            TEXT    NOT NULL,   -- JSON array of rule-id strings
    outcome             TEXT    NOT NULL,
    created_at          TEXT    NOT NULL
);
";

/// Index on FK column `provenance_log.session_id` (SQL-DB-INDEX-1).
const CREATE_PROVENANCE_IDX: &str = "
CREATE INDEX IF NOT EXISTS idx_provenance_log_session_id
    ON provenance_log(session_id);
";

// ---------------------------------------------------------------------------
// Store trait (the seam)
// ---------------------------------------------------------------------------

/// Async persistence seam. Every method is async (RUST-DOMAIN-5).
#[async_trait]
pub trait Store: Send + Sync {
    /// Create / migrate the schema. Idempotent — safe to call on every startup.
    async fn migrate(&self) -> Result<(), PersistenceError>;

    /// Persist a new agent session. `started_at` is caller-supplied so tests
    /// can inject deterministic timestamps (RUST-PURE-STATE-TRANSITIONS-1).
    async fn record_session(
        &self,
        session_id: &SessionId,
        role: &Role,
        started_at: DateTime<Utc>,
    ) -> Result<(), PersistenceError>;

    /// Append one provenance entry. Returns the auto-assigned row id.
    async fn append_provenance(
        &self,
        entry: &ProvenanceEntry,
    ) -> Result<ProvenanceId, PersistenceError>;

    /// Read all provenance entries for a session, ordered by `created_at ASC`.
    async fn provenance_by_session(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<ProvenanceEntry>, PersistenceError>;

    /// Read a session record. Returns `None` if not found.
    async fn session_by_id(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<SessionRecord>, PersistenceError>;
}

// ---------------------------------------------------------------------------
// SqliteStore impl
// ---------------------------------------------------------------------------

/// SQLite-backed [`Store`] implementation using `sqlx`.
#[derive(Clone, Debug)]
pub struct SqliteStore {
    pool: Pool<Sqlite>,
}

impl SqliteStore {
    /// Open (or create) a SQLite database at the given path.
    ///
    /// Pass `":memory:"` for an in-memory database (tests / ephemeral runs).
    pub async fn open(database_url: &str) -> Result<Self, PersistenceError> {
        let pool = SqlitePool::connect(database_url).await?;
        let store = Self { pool };
        store.migrate().await?;
        Ok(store)
    }

    /// Expose the underlying pool for callers that need raw access (e.g. tests).
    pub fn pool(&self) -> &Pool<Sqlite> {
        &self.pool
    }
}

#[async_trait]
impl Store for SqliteStore {
    async fn migrate(&self) -> Result<(), PersistenceError> {
        sqlx::query(CREATE_SESSIONS).execute(&self.pool).await?;
        sqlx::query(CREATE_PROVENANCE).execute(&self.pool).await?;
        sqlx::query(CREATE_PROVENANCE_IDX).execute(&self.pool).await?;
        Ok(())
    }

    async fn record_session(
        &self,
        session_id: &SessionId,
        role: &Role,
        started_at: DateTime<Utc>,
    ) -> Result<(), PersistenceError> {
        let now = Utc::now();
        sqlx::query(
            "INSERT INTO agent_sessions (session_id, role, started_at, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(session_id) DO NOTHING",
        )
        .bind(&session_id.0)
        .bind(&role.name)
        .bind(started_at.to_rfc3339())
        .bind(now.to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn append_provenance(
        &self,
        entry: &ProvenanceEntry,
    ) -> Result<ProvenanceId, PersistenceError> {
        let rule_ids_json = serde_json::to_string(&entry.rule_ids)?;
        let row = sqlx::query(
            "INSERT INTO provenance_log
                 (session_id, change_description, rule_ids, outcome, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             RETURNING id",
        )
        .bind(&entry.session_id)
        .bind(&entry.change_description)
        .bind(&rule_ids_json)
        .bind(&entry.outcome)
        .bind(entry.created_at.to_rfc3339())
        .fetch_one(&self.pool)
        .await?;

        let id: i64 = row.try_get("id")?;
        Ok(ProvenanceId(id))
    }

    async fn provenance_by_session(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<ProvenanceEntry>, PersistenceError> {
        let rows = sqlx::query(
            "SELECT id, session_id, change_description, rule_ids, outcome, created_at
             FROM provenance_log
             WHERE session_id = ?1
             ORDER BY created_at ASC",
        )
        .bind(&session_id.0)
        .fetch_all(&self.pool)
        .await?;

        let mut entries = Vec::with_capacity(rows.len());
        for row in rows {
            let id: i64 = row.try_get("id")?;
            let rule_ids_json: String = row.try_get("rule_ids")?;
            let rule_ids: Vec<String> = serde_json::from_str(&rule_ids_json)?;
            let created_at_str: String = row.try_get("created_at")?;
            let created_at: DateTime<Utc> = created_at_str
                .parse()
                .unwrap_or_else(|_| Utc::now());

            entries.push(ProvenanceEntry {
                id: Some(ProvenanceId(id)),
                session_id: row.try_get("session_id")?,
                change_description: row.try_get("change_description")?,
                rule_ids,
                outcome: row.try_get("outcome")?,
                created_at,
            });
        }
        Ok(entries)
    }

    async fn session_by_id(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<SessionRecord>, PersistenceError> {
        let maybe_row = sqlx::query(
            "SELECT session_id, role, started_at, created_at
             FROM agent_sessions
             WHERE session_id = ?1",
        )
        .bind(&session_id.0)
        .fetch_optional(&self.pool)
        .await?;

        match maybe_row {
            None => Ok(None),
            Some(row) => {
                let started_at_str: String = row.try_get("started_at")?;
                let created_at_str: String = row.try_get("created_at")?;
                Ok(Some(SessionRecord {
                    session_id: row.try_get("session_id")?,
                    role: row.try_get("role")?,
                    started_at: started_at_str.parse().unwrap_or_else(|_| Utc::now()),
                    created_at: created_at_str.parse().unwrap_or_else(|_| Utc::now()),
                }))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests (ORCH-NEW-PATH-TESTS-1)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_core::{Role, RuleId, SessionId};
    use chrono::Utc;

    fn make_role(name: &str, rules: &[&str]) -> Role {
        Role {
            name: name.to_string(),
            rule_subset: rules.iter().map(|r| RuleId(r.to_string())).collect(),
            allowed_paths: vec![],
        }
    }

    /// Opens a fresh in-memory store (schema created automatically in `open`).
    async fn in_memory_store() -> SqliteStore {
        SqliteStore::open("sqlite::memory:").await.expect("in-memory store")
    }

    #[tokio::test]
    async fn test_record_and_read_session() {
        let store = in_memory_store().await;
        let sid = SessionId("sess-001".to_string());
        let role = make_role("Backend", &["RUST-DOMAIN-5", "SERVICE-PARALLEL-1"]);
        let started = Utc::now();

        store.record_session(&sid, &role, started).await.expect("record_session");

        let rec = store.session_by_id(&sid).await.expect("session_by_id");
        assert!(rec.is_some(), "session should exist");
        let rec = rec.unwrap();
        assert_eq!(rec.session_id, "sess-001");
        assert_eq!(rec.role, "Backend");
    }

    #[tokio::test]
    async fn test_append_and_read_provenance() {
        let store = in_memory_store().await;
        let sid = SessionId("sess-002".to_string());
        let role = make_role("Frontend", &["RUST-DIOXUS-3"]);
        store
            .record_session(&sid, &role, Utc::now())
            .await
            .expect("record_session");

        let entry1 = ProvenanceEntry::new(
            "sess-002",
            "Added async boundary for DB call",
            vec!["RUST-DOMAIN-5".to_string(), "SERVICE-PARALLEL-1".to_string()],
            "allowed",
        );
        let entry2 = ProvenanceEntry::new(
            "sess-002",
            "Denied direct db.select outside repository",
            vec!["REPO-1".to_string()],
            "denied",
        );

        let id1 = store.append_provenance(&entry1).await.expect("append 1");
        let id2 = store.append_provenance(&entry2).await.expect("append 2");
        assert_ne!(id1.0, id2.0, "ids should differ");

        let entries = store
            .provenance_by_session(&sid)
            .await
            .expect("read provenance");
        assert_eq!(entries.len(), 2);

        let first = &entries[0];
        assert_eq!(first.session_id, "sess-002");
        assert_eq!(first.change_description, "Added async boundary for DB call");
        assert_eq!(first.rule_ids, vec!["RUST-DOMAIN-5", "SERVICE-PARALLEL-1"]);
        assert_eq!(first.outcome, "allowed");

        let second = &entries[1];
        assert_eq!(second.outcome, "denied");
        assert_eq!(second.rule_ids, vec!["REPO-1"]);
    }

    #[tokio::test]
    async fn test_session_not_found_returns_none() {
        let store = in_memory_store().await;
        let sid = SessionId("nonexistent".to_string());
        let result = store.session_by_id(&sid).await.expect("query ok");
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_provenance_empty_for_unknown_session() {
        let store = in_memory_store().await;
        // must have the session row because of FK, so we skip FK insert and test
        // the non-FK path with a session that has no provenance entries.
        let sid = SessionId("sess-no-prov".to_string());
        let role = make_role("Checks", &[]);
        store.record_session(&sid, &role, Utc::now()).await.expect("record");

        let entries = store.provenance_by_session(&sid).await.expect("read");
        assert!(entries.is_empty());
    }

    #[tokio::test]
    async fn test_idempotent_session_insert() {
        // ON CONFLICT DO NOTHING: second insert for same session_id is a no-op
        let store = in_memory_store().await;
        let sid = SessionId("sess-idem".to_string());
        let role = make_role("Backend", &[]);
        store.record_session(&sid, &role, Utc::now()).await.expect("first insert");
        // second call must not error
        store.record_session(&sid, &role, Utc::now()).await.expect("second insert (idempotent)");
    }

    #[tokio::test]
    async fn test_provenance_entry_new_is_pure() {
        // RUST-PURE-STATE-TRANSITIONS-1: constructor produces expected shape
        let e = ProvenanceEntry::new(
            "s1",
            "desc",
            vec!["RULE-A".to_string()],
            "allowed",
        );
        assert!(e.id.is_none());
        assert_eq!(e.session_id, "s1");
        assert_eq!(e.rule_ids, vec!["RULE-A"]);
    }
}
