//! The Product-Owner feedback-loop's defect-report store.
//!
//! A `feedback` table records every [`DefectReport`] Camerata ingests, whether it came
//! from a scaffolded app's built-in auto-capture reporter (a wasm panic hook /
//! `window.onerror` / `unhandledrejection` / failed-request interceptor) or a human's
//! click-to-report in the preview. Like [`crate::governance_event::GovernanceLog`] (and
//! unlike the write-only [`crate::enforcement_catch`] ledger), this store is meant to be
//! READ BACK — the orchestrator lists a project's open reports and turns them into work
//! items; a human triages via `by_project`/`recent`/`set_status`.
//!
//! This module mirrors `governance_event.rs`'s structure EXACTLY (idempotent migration,
//! `open`/`open_in_memory`, an index on the primary lookup column, `Row::try_get`
//! decoding), with ONE deliberate difference: the record type ([`DefectReport`]) is NOT
//! defined in this crate. It is the canonical shape from `camerata-api-types` (the
//! pure-serde wire-contract leaf), so `camerata-server`'s `POST /api/feedback` handler
//! and this store operate on the exact same type with no mirroring/translation layer.
//!
//! Design principles honored (mirrors `governance_event.rs`):
//! - RUST-DOMAIN-5: async I/O throughout
//! - RUST-DOMAIN-6: thiserror error enum (reused from crate root)
//! - SQL-AUDIT-COLUMNS-1: `ts` on every row (RFC3339 UTC, stamped at construction —
//!   see `DefectReport::new`)
//! - SQL-DB-INDEX-1/2: the `project_id` WHERE column is indexed
//! - ORCH-NEW-PATH-TESTS-1: unit tests included in this file

use camerata_api_types::feedback::{
    DefectContext, DefectKind, DefectReport, DefectSeverity, DefectSource, DefectStatus,
};
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{Row, SqlitePool};
use std::path::Path;

use crate::error::PersistenceError;

// ---------------------------------------------------------------------------
// Migration SQL (idempotent)
// ---------------------------------------------------------------------------

const CREATE_FEEDBACK: &str = "
CREATE TABLE IF NOT EXISTS feedback (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id  TEXT    NOT NULL,
    source      TEXT    NOT NULL,  -- 'auto' | 'user'
    kind        TEXT    NOT NULL,  -- 'runtime_error' | 'user_report' | 'build_error' | 'other'
    title       TEXT    NOT NULL,
    description TEXT    NOT NULL,
    context     TEXT    NOT NULL,  -- JSON blob (DefectContext)
    severity    TEXT    NOT NULL,  -- 'info' | 'warning' | 'error' | 'critical'
    status      TEXT    NOT NULL,  -- 'open' | 'acknowledged' | 'resolved'
    ts          TEXT    NOT NULL,  -- RFC3339 UTC
    fingerprint TEXT,              -- dedupe key (kind + top stack frame + route)
    count       INTEGER NOT NULL DEFAULT 1  -- occurrences folded into this row
);
";

const CREATE_IDX_FEEDBACK_PROJECT_ID: &str = "
CREATE INDEX IF NOT EXISTS idx_feedback_project_id
    ON feedback(project_id);
";

/// `fingerprint` is the dedupe lookup key `bump_fingerprint` filters on — index it
/// alongside `project_id` (SQL-DB-INDEX-1/2).
const CREATE_IDX_FEEDBACK_FINGERPRINT: &str = "
CREATE INDEX IF NOT EXISTS idx_feedback_fingerprint
    ON feedback(project_id, fingerprint);
";

// ---------------------------------------------------------------------------
// FeedbackStore: SQLite-backed store (write + read)
// ---------------------------------------------------------------------------

/// SQLite-backed defect-report store. Mirrors
/// [`crate::governance_event::GovernanceLog`]'s shape: a write path (`record`,
/// `set_status`) and a read path (`by_project`, `recent`), both exercised from day one.
#[derive(Debug, Clone)]
pub struct FeedbackStore {
    pool: SqlitePool,
}

impl FeedbackStore {
    /// Open (or create) the feedback database at `path`, creating the table and its
    /// `project_id` index if they don't already exist. Idempotent — safe to call on
    /// every startup.
    ///
    /// Uses [`SqliteConnectOptions`] (not a `sqlite://` URL) so paths containing
    /// characters that are awkward in a URL (spaces, as in macOS's
    /// `Application Support`) work without encoding — same reasoning as
    /// `SqliteStore::open_path` / `GovernanceLog::open`.
    pub async fn open(path: &Path) -> Result<Self, PersistenceError> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);
        let pool = SqlitePool::connect_with(options).await?;
        let store = Self { pool };
        store.migrate().await?;
        Ok(store)
    }

    /// Open an in-memory feedback database (tests / ephemeral runs).
    pub async fn open_in_memory() -> Result<Self, PersistenceError> {
        let pool = SqlitePool::connect("sqlite::memory:").await?;
        let store = Self { pool };
        store.migrate().await?;
        Ok(store)
    }

    /// Create / migrate the `feedback` table and its indexes. Idempotent.
    ///
    /// `CREATE TABLE IF NOT EXISTS` is a no-op on a table that already exists, so an
    /// on-disk database created before `fingerprint`/`count` existed would otherwise
    /// never gain those columns. The `ALTER TABLE` pass below adds them to an
    /// already-existing table; SQLite errors with "duplicate column name" if the
    /// column is already present (i.e. a brand-new table, whose `CREATE TABLE` above
    /// already included them), which is swallowed here as the expected no-op case —
    /// any OTHER error still propagates.
    async fn migrate(&self) -> Result<(), PersistenceError> {
        sqlx::query(CREATE_FEEDBACK).execute(&self.pool).await?;
        sqlx::query(CREATE_IDX_FEEDBACK_PROJECT_ID)
            .execute(&self.pool)
            .await?;
        for stmt in [
            "ALTER TABLE feedback ADD COLUMN fingerprint TEXT",
            "ALTER TABLE feedback ADD COLUMN count INTEGER NOT NULL DEFAULT 1",
        ] {
            if let Err(e) = sqlx::query(stmt).execute(&self.pool).await {
                if !e.to_string().contains("duplicate column name") {
                    return Err(e.into());
                }
            }
        }
        sqlx::query(CREATE_IDX_FEEDBACK_FINGERPRINT)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Record one defect report. Returns the auto-assigned row id (the caller does NOT
    /// need to have set `report.id` — it is ignored on write).
    pub async fn record(&self, report: DefectReport) -> Result<i64, PersistenceError> {
        let context_json = serde_json::to_string(&report.context)?;
        let row = sqlx::query(
            "INSERT INTO feedback
                 (project_id, source, kind, title, description, context, severity, status, ts,
                  fingerprint, count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             RETURNING id",
        )
        .bind(&report.project_id)
        .bind(report.source.as_str())
        .bind(report.kind.as_str())
        .bind(&report.title)
        .bind(&report.description)
        .bind(&context_json)
        .bind(report.severity.as_str())
        .bind(report.status.as_str())
        .bind(&report.ts)
        .bind(&report.fingerprint)
        .bind(report.count)
        .fetch_one(&self.pool)
        .await?;
        let id: i64 = row.try_get("id")?;
        Ok(id)
    }

    /// The dedupe fold: if a recent OPEN report exists for `project_id` with the same
    /// `fingerprint`, increment its `count` in place and return its id. Returns `None`
    /// when there is no match (the caller should then `record` a new row).
    ///
    /// "Recent" here means "the most recently recorded OPEN report with this
    /// fingerprint" (`ORDER BY id DESC LIMIT 1`) — there is no additional TTL/time
    /// window cutoff. Adding one would require deciding a window duration, a product
    /// choice this store does not make on its own; see the call site's doc for the
    /// explicit flag.
    pub async fn bump_fingerprint(
        &self,
        project_id: &str,
        fingerprint: &str,
    ) -> Result<Option<i64>, PersistenceError> {
        let row = sqlx::query(
            "SELECT id FROM feedback
             WHERE project_id = ?1 AND fingerprint = ?2 AND status = 'open'
             ORDER BY id DESC
             LIMIT 1",
        )
        .bind(project_id)
        .bind(fingerprint)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let id: i64 = row.try_get("id")?;
        sqlx::query("UPDATE feedback SET count = count + 1 WHERE id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(Some(id))
    }

    /// Read all reports for `project_id`, newest first (`id DESC`).
    pub async fn by_project(
        &self,
        project_id: &str,
    ) -> Result<Vec<DefectReport>, PersistenceError> {
        let rows = sqlx::query(
            "SELECT id, project_id, source, kind, title, description, context, severity, status, ts,
                    fingerprint, count
             FROM feedback
             WHERE project_id = ?1
             ORDER BY id DESC",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_report).collect()
    }

    /// Read the `limit` most recently recorded reports, newest first (`id DESC`),
    /// across ALL projects. Cross-project — used for a global recent-activity view.
    pub async fn recent(&self, limit: i64) -> Result<Vec<DefectReport>, PersistenceError> {
        let rows = sqlx::query(
            "SELECT id, project_id, source, kind, title, description, context, severity, status, ts,
                    fingerprint, count
             FROM feedback
             ORDER BY id DESC
             LIMIT ?1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_report).collect()
    }

    /// Update one report's status (`Open` -> `Acknowledged` -> `Resolved`, or any
    /// direct transition — no state-machine enforcement here, the caller decides).
    pub async fn set_status(&self, id: i64, status: DefectStatus) -> Result<(), PersistenceError> {
        sqlx::query("UPDATE feedback SET status = ?1 WHERE id = ?2")
            .bind(status.as_str())
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

/// Map one SQLite row to a [`DefectReport`]. Shared by `by_project` and `recent`.
fn row_to_report(row: sqlx::sqlite::SqliteRow) -> Result<DefectReport, PersistenceError> {
    let context_json: String = row.try_get("context")?;
    let context: DefectContext = serde_json::from_str(&context_json)?;
    Ok(DefectReport {
        id: Some(row.try_get("id")?),
        project_id: row.try_get("project_id")?,
        source: DefectSource::parse(row.try_get::<String, _>("source")?.as_str()),
        kind: DefectKind::parse(row.try_get::<String, _>("kind")?.as_str()),
        title: row.try_get("title")?,
        description: row.try_get("description")?,
        context,
        severity: DefectSeverity::parse(row.try_get::<String, _>("severity")?.as_str()),
        status: DefectStatus::parse(row.try_get::<String, _>("status")?.as_str()),
        ts: row.try_get("ts")?,
        fingerprint: row.try_get("fingerprint")?,
        count: row.try_get("count")?,
    })
}

// ---------------------------------------------------------------------------
// Tests (ORCH-NEW-PATH-TESTS-1)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── open / migrate is idempotent ────────────────────────────────────────

    #[tokio::test]
    async fn open_in_memory_and_reopen_migrate_is_idempotent() {
        let store = FeedbackStore::open_in_memory().await.expect("open");
        store.migrate().await.expect("second migrate is a no-op");
    }

    #[tokio::test]
    async fn open_path_persists_across_reopen() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("feedback.db");

        {
            let store = FeedbackStore::open(&path).await.expect("open");
            store
                .record(DefectReport::auto(
                    "proj-durable",
                    DefectKind::RuntimeError,
                    "boom",
                ))
                .await
                .expect("record");
        }
        assert!(path.exists(), "open should create the database file");

        let store = FeedbackStore::open(&path).await.expect("reopen");
        let reports = store.by_project("proj-durable").await.expect("by_project");
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].title, "boom");
    }

    // ── record + by_project: ordering + project_id filtering ────────────────

    #[tokio::test]
    async fn record_and_by_project_returns_newest_first_filtered_by_project() {
        let store = FeedbackStore::open_in_memory().await.expect("open");

        let id1 = store
            .record(DefectReport::auto(
                "proj-a",
                DefectKind::RuntimeError,
                "first error",
            ))
            .await
            .expect("record 1");
        let id2 = store
            .record(
                DefectReport::user("proj-a", DefectKind::UserReport, "second, a user report")
                    .with_description("the button does nothing")
                    .with_severity(DefectSeverity::Warning)
                    .with_route("/dashboard")
                    .with_element("button.save"),
            )
            .await
            .expect("record 2");
        // A different project — must NOT show up in proj-a's history.
        store
            .record(DefectReport::auto(
                "proj-b",
                DefectKind::BuildError,
                "unrelated build failure",
            ))
            .await
            .expect("record proj-b");

        assert!(id1 < id2, "ids should be monotonically increasing");

        let reports = store.by_project("proj-a").await.expect("by_project");
        assert_eq!(reports.len(), 2, "only proj-a's 2 reports, not proj-b's");
        // Newest first.
        assert_eq!(reports[0].title, "second, a user report");
        assert_eq!(reports[0].id, Some(id2));
        assert_eq!(reports[0].source, DefectSource::User);
        assert_eq!(reports[0].severity, DefectSeverity::Warning);
        assert_eq!(reports[0].context.route.as_deref(), Some("/dashboard"));
        assert_eq!(reports[0].context.element.as_deref(), Some("button.save"));
        assert_eq!(reports[0].description, "the button does nothing");

        assert_eq!(reports[1].title, "first error");
        assert_eq!(reports[1].id, Some(id1));
        assert_eq!(reports[1].kind, DefectKind::RuntimeError);
    }

    #[tokio::test]
    async fn by_project_empty_for_unknown_project() {
        let store = FeedbackStore::open_in_memory().await.expect("open");
        store
            .record(DefectReport::auto("proj-known", DefectKind::Other, "x"))
            .await
            .expect("record");
        let reports = store.by_project("proj-unknown").await.expect("by_project");
        assert!(reports.is_empty());
    }

    // ── recent: newest-first, cross-project, limited ────────────────────────

    #[tokio::test]
    async fn recent_returns_newest_n_across_projects_newest_first() {
        let store = FeedbackStore::open_in_memory().await.expect("open");

        for i in 0..5 {
            store
                .record(DefectReport::auto(
                    format!("proj-{i}"),
                    DefectKind::Other,
                    format!("report {i}"),
                ))
                .await
                .expect("record");
        }

        let recent = store.recent(3).await.expect("recent");
        assert_eq!(recent.len(), 3, "limited to 3");
        assert_eq!(recent[0].project_id, "proj-4");
        assert_eq!(recent[1].project_id, "proj-3");
        assert_eq!(recent[2].project_id, "proj-2");
    }

    #[tokio::test]
    async fn recent_with_limit_larger_than_row_count_returns_all() {
        let store = FeedbackStore::open_in_memory().await.expect("open");
        store
            .record(DefectReport::auto("proj-only", DefectKind::Other, "x"))
            .await
            .expect("record");
        let recent = store.recent(50).await.expect("recent");
        assert_eq!(recent.len(), 1);
    }

    // ── set_status ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn set_status_updates_the_row_status() {
        let store = FeedbackStore::open_in_memory().await.expect("open");
        let id = store
            .record(DefectReport::user("proj-x", DefectKind::UserReport, "y"))
            .await
            .expect("record");

        let reports = store.by_project("proj-x").await.expect("by_project");
        assert_eq!(
            reports[0].status,
            DefectStatus::Open,
            "default status is Open"
        );

        store
            .set_status(id, DefectStatus::Acknowledged)
            .await
            .expect("set_status");
        let reports = store.by_project("proj-x").await.expect("by_project");
        assert_eq!(reports[0].status, DefectStatus::Acknowledged);

        store
            .set_status(id, DefectStatus::Resolved)
            .await
            .expect("set_status");
        let reports = store.by_project("proj-x").await.expect("by_project");
        assert_eq!(reports[0].status, DefectStatus::Resolved);
    }

    #[tokio::test]
    async fn set_status_on_unknown_id_is_a_silent_no_op() {
        // No matching row: UPDATE affects zero rows, no error. Mirrors SQLite's default
        // UPDATE semantics — the caller can distinguish via a prior existence check if
        // it cares; this store doesn't manufacture a NotFound error for it.
        let store = FeedbackStore::open_in_memory().await.expect("open");
        store
            .set_status(999, DefectStatus::Resolved)
            .await
            .expect("set_status on unknown id must not error");
    }

    // ── context extras round-trip through the JSON column ───────────────────

    #[tokio::test]
    async fn context_with_stack_console_and_extras_round_trips() {
        let store = FeedbackStore::open_in_memory().await.expect("open");
        store
            .record(
                DefectReport::auto("proj-ctx", DefectKind::RuntimeError, "panic")
                    .with_stack("at foo (app.js:1:1)")
                    .with_console("warn: deprecated API")
                    .with_extra("viewport", "375x812")
                    .with_extra("browser", "safari"),
            )
            .await
            .expect("record");

        let reports = store.by_project("proj-ctx").await.expect("by_project");
        assert_eq!(reports.len(), 1);
        let ctx = &reports[0].context;
        assert_eq!(ctx.stack.as_deref(), Some("at foo (app.js:1:1)"));
        assert_eq!(ctx.console.as_deref(), Some("warn: deprecated API"));
        assert_eq!(
            ctx.extra.get("viewport").map(String::as_str),
            Some("375x812")
        );
        assert_eq!(ctx.extra.get("browser").map(String::as_str), Some("safari"));
    }

    // ── fingerprint default + round trip ─────────────────────────────────────

    #[tokio::test]
    async fn record_defaults_fingerprint_none_and_count_one() {
        let store = FeedbackStore::open_in_memory().await.expect("open");
        store
            .record(DefectReport::auto("proj-fp", DefectKind::RuntimeError, "boom"))
            .await
            .expect("record");
        let reports = store.by_project("proj-fp").await.expect("by_project");
        assert!(reports[0].fingerprint.is_none());
        assert_eq!(reports[0].count, 1);
    }

    #[tokio::test]
    async fn record_persists_a_client_provided_fingerprint() {
        let store = FeedbackStore::open_in_memory().await.expect("open");
        store
            .record(
                DefectReport::auto("proj-fp", DefectKind::RuntimeError, "boom")
                    .with_fingerprint("fp-123"),
            )
            .await
            .expect("record");
        let reports = store.by_project("proj-fp").await.expect("by_project");
        assert_eq!(reports[0].fingerprint.as_deref(), Some("fp-123"));
    }

    // ── bump_fingerprint: the dedupe fold ────────────────────────────────────

    #[tokio::test]
    async fn bump_fingerprint_increments_count_on_matching_open_report() {
        let store = FeedbackStore::open_in_memory().await.expect("open");
        let id = store
            .record(
                DefectReport::auto("proj-a", DefectKind::RuntimeError, "first occurrence")
                    .with_fingerprint("fp-match"),
            )
            .await
            .expect("record");

        let bumped = store
            .bump_fingerprint("proj-a", "fp-match")
            .await
            .expect("bump_fingerprint")
            .expect("a matching open report must be found");
        assert_eq!(bumped, id);

        let reports = store.by_project("proj-a").await.expect("by_project");
        assert_eq!(reports.len(), 1, "no new row was inserted");
        assert_eq!(reports[0].count, 2, "count incremented from 1 to 2");

        // Bump again: count keeps climbing on the SAME row.
        store
            .bump_fingerprint("proj-a", "fp-match")
            .await
            .expect("bump_fingerprint")
            .expect("still matches");
        let reports = store.by_project("proj-a").await.expect("by_project");
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].count, 3);
    }

    #[tokio::test]
    async fn bump_fingerprint_returns_none_when_no_match() {
        let store = FeedbackStore::open_in_memory().await.expect("open");
        store
            .record(
                DefectReport::auto("proj-a", DefectKind::RuntimeError, "x")
                    .with_fingerprint("fp-one"),
            )
            .await
            .expect("record");

        // Different fingerprint.
        assert!(store
            .bump_fingerprint("proj-a", "fp-two")
            .await
            .expect("bump_fingerprint")
            .is_none());

        // Different project, same fingerprint.
        assert!(store
            .bump_fingerprint("proj-b", "fp-one")
            .await
            .expect("bump_fingerprint")
            .is_none());
    }

    #[tokio::test]
    async fn bump_fingerprint_ignores_a_resolved_report() {
        // A report the user already resolved should NOT silently reopen/absorb a new
        // occurrence of the "same" fingerprint — a fresh row is the right call so the
        // regression is visible again, not folded into an already-closed report.
        let store = FeedbackStore::open_in_memory().await.expect("open");
        let id = store
            .record(
                DefectReport::auto("proj-a", DefectKind::RuntimeError, "x")
                    .with_fingerprint("fp-closed"),
            )
            .await
            .expect("record");
        store
            .set_status(id, DefectStatus::Resolved)
            .await
            .expect("set_status");

        assert!(store
            .bump_fingerprint("proj-a", "fp-closed")
            .await
            .expect("bump_fingerprint")
            .is_none());
    }
}
