//! Auditable governance-event trail.
//!
//! A `governance_events` table records the lifecycle of a governed run: when it
//! started, every agent step, every Layer-1/Layer-2 gate verdict, escalations raised
//! and answered, sign-offs, commit/PR gates, stalls, and how the run finished. Unlike
//! the write-only [`crate::enforcement_catch`] ledger (external-analytics only), this
//! store is meant to be READ BACK by the app itself — e.g. a per-run activity timeline
//! in the cockpit — so it ships with a `by_run` / `recent` read path from day one.
//!
//! Phase H1 (this module) builds ONLY the plumbing: the table, the model, the
//! writer, and the reader. No lifecycle site calls `record` yet — that instrumentation
//! is a later phase. `AppState::record_governance` (in `camerata-server`) is the single
//! call-site helper future phases will use.
//!
//! Design principles honored (mirrors `enforcement_catch.rs`):
//! - RUST-DOMAIN-4: newtype-flavored, explicit fields (no stringly-typed catch-all)
//! - RUST-DOMAIN-5: async I/O throughout
//! - RUST-DOMAIN-6: thiserror error enum (reused from crate root)
//! - SQL-AUDIT-COLUMNS-1: `ts` on every row (RFC3339 UTC, human-readable — this store
//!   is read back by humans/UI, so a readable timestamp beats an epoch-ms int here)
//! - SQL-DB-INDEX-1/2: the `run_id` WHERE/JOIN column is indexed
//! - ORCH-NEW-PATH-TESTS-1: unit tests included in this file
//! - FAIL-SOFT: callers (AppState::record_governance) must log-and-swallow errors;
//!   this module itself returns `Result` faithfully — the fail-soft contract lives
//!   one layer up, same split as the enforcement ledger.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{Row, SqlitePool};
use std::path::Path;

use crate::error::PersistenceError;

// ---------------------------------------------------------------------------
// Migration SQL (idempotent)
// ---------------------------------------------------------------------------

const CREATE_GOVERNANCE_EVENTS: &str = "
CREATE TABLE IF NOT EXISTS governance_events (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id      TEXT    NOT NULL,
    story_id    TEXT,
    ts          TEXT    NOT NULL,  -- RFC3339 UTC
    kind        TEXT    NOT NULL,  -- 'run_started' | 'agent_step' | 'gate_allow' | ...
    severity    TEXT    NOT NULL,  -- 'info' | 'warn' | 'error'
    actor       TEXT    NOT NULL,  -- 'agent' | 'human' | 'system'
    rule_id     TEXT,
    reason      TEXT,
    detail      TEXT               -- JSON blob for structured extras
);
";

const CREATE_IDX_GOVERNANCE_EVENTS_RUN_ID: &str = "
CREATE INDEX IF NOT EXISTS idx_governance_events_run_id
    ON governance_events(run_id);
";

// ---------------------------------------------------------------------------
// Model
// ---------------------------------------------------------------------------

/// One governance-event row. Event `kind`s currently in use across the app:
/// `"run_started"`, `"agent_step"`, `"gate_allow"`, `"gate_deny"`, `"layer2_bounce"`,
/// `"check_failed"`, `"escalation_raised"`, `"escalation_answered"`, `"sign_off"`,
/// `"commit_gate"`, `"pr_gate"`, `"stall_cancel"`, `"run_finished"`. `kind` is a plain
/// `String` (not a closed enum) so a later phase can introduce new kinds without a
/// migration; readers should treat unknown kinds as forward-compatible data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GovernanceEvent {
    /// Row id, assigned by the database. `None` until the event has been recorded
    /// (or freshly constructed); `Some` once read back via [`GovernanceLog::by_run`]
    /// or [`GovernanceLog::recent`], or returned from [`GovernanceLog::record`].
    pub id: Option<i64>,
    /// The run this event belongs to. Indexed — this is the primary lookup key.
    pub run_id: String,
    /// The story this run is executing, if known.
    pub story_id: Option<String>,
    /// RFC3339 UTC timestamp of the event. Set at construction time (`Utc::now()`),
    /// not at insert time, so a batch of events built together share a coherent
    /// ordering even if the actual INSERT is briefly delayed.
    pub ts: String,
    /// The event kind (see the type-level doc for the current vocabulary).
    pub kind: String,
    /// `"info"` | `"warn"` | `"error"`.
    pub severity: String,
    /// Who/what produced the event: `"agent"` | `"human"` | `"system"`.
    pub actor: String,
    /// The rule id involved, if this event is rule-driven (a gate verdict, an
    /// escalation condition, etc.).
    pub rule_id: Option<String>,
    /// A human-readable one-line reason/summary.
    pub reason: Option<String>,
    /// A JSON blob of structured extras the specific `kind` wants to carry (e.g. the
    /// full gate-decision record, or the escalation's justification text). Stored as
    /// a raw string — callers serialize/deserialize their own shape; this module
    /// does not interpret it.
    pub detail: Option<String>,
}

impl GovernanceEvent {
    /// The general constructor every ergonomic helper below delegates to.
    /// `ts` is stamped `Utc::now()` at construction.
    pub fn new(
        run_id: impl Into<String>,
        kind: impl Into<String>,
        severity: impl Into<String>,
        actor: impl Into<String>,
    ) -> Self {
        Self {
            id: None,
            run_id: run_id.into(),
            story_id: None,
            ts: Utc::now().to_rfc3339(),
            kind: kind.into(),
            severity: severity.into(),
            actor: actor.into(),
            rule_id: None,
            reason: None,
            detail: None,
        }
    }

    /// Construct an `"info"`-severity event.
    pub fn info(run_id: impl Into<String>, kind: impl Into<String>, actor: impl Into<String>) -> Self {
        Self::new(run_id, kind, "info", actor)
    }

    /// Construct a `"warn"`-severity event.
    pub fn warn(run_id: impl Into<String>, kind: impl Into<String>, actor: impl Into<String>) -> Self {
        Self::new(run_id, kind, "warn", actor)
    }

    /// Construct an `"error"`-severity event.
    pub fn error(run_id: impl Into<String>, kind: impl Into<String>, actor: impl Into<String>) -> Self {
        Self::new(run_id, kind, "error", actor)
    }

    /// Attach a story id (builder-style, chainable).
    pub fn with_story_id(mut self, story_id: impl Into<String>) -> Self {
        self.story_id = Some(story_id.into());
        self
    }

    /// Attach a rule id (builder-style, chainable).
    pub fn with_rule_id(mut self, rule_id: impl Into<String>) -> Self {
        self.rule_id = Some(rule_id.into());
        self
    }

    /// Attach a human-readable reason (builder-style, chainable).
    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }

    /// Attach a structured-extras JSON blob (builder-style, chainable). Callers are
    /// responsible for serializing their own shape; this module treats it opaquely.
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

// ---------------------------------------------------------------------------
// GovernanceLog: SQLite-backed store (write + read)
// ---------------------------------------------------------------------------

/// SQLite-backed governance-event log. Unlike the enforcement-catch ledger, this
/// store is meant to be read back (`by_run`, `recent`), so it exposes both halves.
#[derive(Debug, Clone)]
pub struct GovernanceLog {
    pool: SqlitePool,
}

impl GovernanceLog {
    /// Open (or create) the governance-event database at `path`, creating the table
    /// and its `run_id` index if they don't already exist. Idempotent — safe to call
    /// on every startup.
    ///
    /// Uses [`SqliteConnectOptions`] (not a `sqlite://` URL) so paths containing
    /// characters that are awkward in a URL (spaces, as in macOS's
    /// `Application Support`) work without encoding — the same reasoning as
    /// `SqliteStore::open_path`.
    pub async fn open(path: &Path) -> Result<Self, PersistenceError> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);
        let pool = SqlitePool::connect_with(options).await?;
        let log = Self { pool };
        log.migrate().await?;
        Ok(log)
    }

    /// Open an in-memory governance-event database (tests / ephemeral runs).
    pub async fn open_in_memory() -> Result<Self, PersistenceError> {
        let pool = SqlitePool::connect("sqlite::memory:").await?;
        let log = Self { pool };
        log.migrate().await?;
        Ok(log)
    }

    /// Create / migrate the `governance_events` table and its index. Idempotent.
    async fn migrate(&self) -> Result<(), PersistenceError> {
        sqlx::query(CREATE_GOVERNANCE_EVENTS)
            .execute(&self.pool)
            .await?;
        sqlx::query(CREATE_IDX_GOVERNANCE_EVENTS_RUN_ID)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Append one event. Returns the auto-assigned row id.
    pub async fn record(&self, event: GovernanceEvent) -> Result<i64, PersistenceError> {
        let row = sqlx::query(
            "INSERT INTO governance_events
                 (run_id, story_id, ts, kind, severity, actor, rule_id, reason, detail)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             RETURNING id",
        )
        .bind(&event.run_id)
        .bind(&event.story_id)
        .bind(&event.ts)
        .bind(&event.kind)
        .bind(&event.severity)
        .bind(&event.actor)
        .bind(&event.rule_id)
        .bind(&event.reason)
        .bind(&event.detail)
        .fetch_one(&self.pool)
        .await?;
        let id: i64 = row.try_get("id")?;
        Ok(id)
    }

    /// Read all events for `run_id`, ordered by `id ASC` (insertion order).
    pub async fn by_run(&self, run_id: &str) -> Result<Vec<GovernanceEvent>, PersistenceError> {
        let rows = sqlx::query(
            "SELECT id, run_id, story_id, ts, kind, severity, actor, rule_id, reason, detail
             FROM governance_events
             WHERE run_id = ?1
             ORDER BY id ASC",
        )
        .bind(run_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_event).collect()
    }

    /// Read the `limit` most recently recorded events, ordered by `id DESC` (newest
    /// first). Cross-run — used for a global recent-activity view.
    pub async fn recent(&self, limit: i64) -> Result<Vec<GovernanceEvent>, PersistenceError> {
        let rows = sqlx::query(
            "SELECT id, run_id, story_id, ts, kind, severity, actor, rule_id, reason, detail
             FROM governance_events
             ORDER BY id DESC
             LIMIT ?1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_event).collect()
    }
}

/// Map one SQLite row to a [`GovernanceEvent`]. Shared by `by_run` and `recent`.
fn row_to_event(row: sqlx::sqlite::SqliteRow) -> Result<GovernanceEvent, PersistenceError> {
    Ok(GovernanceEvent {
        id: Some(row.try_get("id")?),
        run_id: row.try_get("run_id")?,
        story_id: row.try_get("story_id")?,
        ts: row.try_get("ts")?,
        kind: row.try_get("kind")?,
        severity: row.try_get("severity")?,
        actor: row.try_get("actor")?,
        rule_id: row.try_get("rule_id")?,
        reason: row.try_get("reason")?,
        detail: row.try_get("detail")?,
    })
}

// ---------------------------------------------------------------------------
// Tests (ORCH-NEW-PATH-TESTS-1)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── Constructors set severity/actor correctly ───────────────────────────

    #[test]
    fn info_constructor_sets_severity_and_actor() {
        let e = GovernanceEvent::info("run-1", "run_started", "system");
        assert_eq!(e.severity, "info");
        assert_eq!(e.actor, "system");
        assert_eq!(e.run_id, "run-1");
        assert_eq!(e.kind, "run_started");
        assert!(e.id.is_none(), "unrecorded event has no id yet");
    }

    #[test]
    fn warn_constructor_sets_severity_and_actor() {
        let e = GovernanceEvent::warn("run-2", "check_failed", "system");
        assert_eq!(e.severity, "warn");
        assert_eq!(e.actor, "system");
    }

    #[test]
    fn error_constructor_sets_severity_and_actor() {
        let e = GovernanceEvent::error("run-3", "gate_deny", "agent");
        assert_eq!(e.severity, "error");
        assert_eq!(e.actor, "agent");
    }

    #[test]
    fn builder_methods_attach_optional_fields() {
        let e = GovernanceEvent::info("run-4", "escalation_raised", "agent")
            .with_story_id("CAM-42")
            .with_rule_id("ORCH-ONE-WAY-DOOR-1")
            .with_reason("agent proposed a schema change")
            .with_detail("{\"depth\":1}");
        assert_eq!(e.story_id.as_deref(), Some("CAM-42"));
        assert_eq!(e.rule_id.as_deref(), Some("ORCH-ONE-WAY-DOOR-1"));
        assert_eq!(e.reason.as_deref(), Some("agent proposed a schema change"));
        assert_eq!(e.detail.as_deref(), Some("{\"depth\":1}"));
    }

    // ── open / migrate is idempotent ────────────────────────────────────────

    #[tokio::test]
    async fn open_in_memory_and_reopen_migrate_is_idempotent() {
        let log = GovernanceLog::open_in_memory().await.expect("open");
        // Calling migrate again (indirectly, via a second open on the SAME pool
        // path is not possible for :memory:, so just re-run migrate directly).
        log.migrate().await.expect("second migrate is a no-op");
    }

    #[tokio::test]
    async fn open_path_persists_across_reopen() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("governance_events.db");

        {
            let log = GovernanceLog::open(&path).await.expect("open");
            log.record(GovernanceEvent::info("run-durable", "run_started", "system"))
                .await
                .expect("record");
        }
        assert!(path.exists(), "open should create the database file");

        let log = GovernanceLog::open(&path).await.expect("reopen");
        let events = log.by_run("run-durable").await.expect("by_run");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "run_started");
    }

    // ── record + by_run: ordering + run_id filtering ────────────────────────

    #[tokio::test]
    async fn record_and_by_run_returns_events_in_order_filtered_by_run() {
        let log = GovernanceLog::open_in_memory().await.expect("open");

        let id1 = log
            .record(GovernanceEvent::info("run-a", "run_started", "system"))
            .await
            .expect("record 1");
        let id2 = log
            .record(
                GovernanceEvent::info("run-a", "agent_step", "agent")
                    .with_reason("wrote src/foo.rs"),
            )
            .await
            .expect("record 2");
        let id3 = log
            .record(GovernanceEvent::error("run-a", "gate_deny", "system").with_rule_id("GOV-1"))
            .await
            .expect("record 3");
        // A different run — must NOT show up in run-a's history.
        log.record(GovernanceEvent::info("run-b", "run_started", "system"))
            .await
            .expect("record run-b");

        assert!(id1 < id2 && id2 < id3, "ids should be monotonically increasing");

        let events = log.by_run("run-a").await.expect("by_run");
        assert_eq!(events.len(), 3, "only run-a's 3 events, not run-b's");
        assert_eq!(events[0].kind, "run_started");
        assert_eq!(events[1].kind, "agent_step");
        assert_eq!(events[1].reason.as_deref(), Some("wrote src/foo.rs"));
        assert_eq!(events[2].kind, "gate_deny");
        assert_eq!(events[2].rule_id.as_deref(), Some("GOV-1"));
        assert_eq!(events[2].severity, "error");

        // ids returned from record() must match what by_run reads back.
        assert_eq!(events[0].id, Some(id1));
        assert_eq!(events[1].id, Some(id2));
        assert_eq!(events[2].id, Some(id3));
    }

    #[tokio::test]
    async fn by_run_empty_for_unknown_run() {
        let log = GovernanceLog::open_in_memory().await.expect("open");
        log.record(GovernanceEvent::info("run-known", "run_started", "system"))
            .await
            .expect("record");
        let events = log.by_run("run-unknown").await.expect("by_run");
        assert!(events.is_empty());
    }

    // ── recent: newest-first, cross-run, limited ────────────────────────────

    #[tokio::test]
    async fn recent_returns_newest_n_across_runs_newest_first() {
        let log = GovernanceLog::open_in_memory().await.expect("open");

        for i in 0..5 {
            log.record(GovernanceEvent::info(
                format!("run-{i}"),
                "run_started",
                "system",
            ))
            .await
            .expect("record");
        }

        let recent = log.recent(3).await.expect("recent");
        assert_eq!(recent.len(), 3, "limited to 3");
        // Newest first: the last-inserted run-4 comes first.
        assert_eq!(recent[0].run_id, "run-4");
        assert_eq!(recent[1].run_id, "run-3");
        assert_eq!(recent[2].run_id, "run-2");
    }

    #[tokio::test]
    async fn recent_with_limit_larger_than_row_count_returns_all() {
        let log = GovernanceLog::open_in_memory().await.expect("open");
        log.record(GovernanceEvent::info("run-only", "run_started", "system"))
            .await
            .expect("record");
        let recent = log.recent(50).await.expect("recent");
        assert_eq!(recent.len(), 1);
    }

    // ── kind vocabulary spot-check: a few real kinds round-trip cleanly ─────

    #[tokio::test]
    async fn assorted_kinds_across_severities_round_trip() {
        let log = GovernanceLog::open_in_memory().await.expect("open");
        let kinds_and_severities = [
            ("run_started", "info", "system"),
            ("agent_step", "info", "agent"),
            ("gate_allow", "info", "system"),
            ("gate_deny", "warn", "system"),
            ("layer2_bounce", "warn", "system"),
            ("check_failed", "warn", "system"),
            ("escalation_raised", "info", "agent"),
            ("escalation_answered", "info", "human"),
            ("sign_off", "info", "human"),
            ("commit_gate", "info", "system"),
            ("pr_gate", "info", "system"),
            ("stall_cancel", "error", "system"),
            ("run_finished", "info", "system"),
        ];
        for (kind, severity, actor) in kinds_and_severities {
            let event = GovernanceEvent::new("run-vocab", kind, severity, actor);
            log.record(event).await.expect("record");
        }
        let events = log.by_run("run-vocab").await.expect("by_run");
        assert_eq!(events.len(), kinds_and_severities.len());
        for (event, (kind, severity, actor)) in
            events.iter().zip(kinds_and_severities.iter())
        {
            assert_eq!(&event.kind, kind);
            assert_eq!(&event.severity, severity);
            assert_eq!(&event.actor, actor);
        }
    }
}
