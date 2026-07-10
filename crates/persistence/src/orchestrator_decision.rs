//! The confidence engine's decision + calibration store.
//!
//! An `orchestrator_decisions` table records every autonomous decision the Architect
//! orchestrator makes (see `docs/plans/2026-07-09_product-owner-head-vibe-mode.md`,
//! "Architect-orchestrator design (decided 2026-07-10)"): its effect-signature CLASS
//! and CONFIDENCE (from `camerata-orchestrator-core::classify` — never an LLM
//! self-report), what it chose, what the alternatives were, and any assumption it
//! declared instead of asking. A companion `decision_outcomes` table records what
//! actually happened to that decision (a human redirected it, a later defect linked
//! back to it, or it simply survived) — the pairing is the calibration loop: "measured
//! override rate at max dial: X%" is the moat metric this store exists to produce.
//!
//! This module mirrors `governance_event.rs`'s structure EXACTLY (idempotent
//! migration, `open`/`open_in_memory`, an index on the primary lookup column,
//! `Row::try_get` decoding), with the SAME deliberate choice `governance_event.rs`
//! makes: `class` and `confidence` are stored as plain `String`s (not an imported
//! enum), forward-compatible the same way `GovernanceEvent::kind` is. This keeps
//! `camerata-persistence` from taking on a new dependency edge onto
//! `camerata-orchestrator-core` — callers convert with that crate's
//! `DecisionClass::as_str()` / `Confidence::as_str()` at the record-decision call
//! site. `decision_outcomes.outcome`, by contrast, IS a small closed enum
//! ([`DecisionOutcomeKind`]) here, because it directly drives the [`calibration`]
//! aggregate query and a typo there would silently corrupt the moat metric.
//!
//! Design principles honored (mirrors `governance_event.rs` / `feedback.rs`):
//! - RUST-DOMAIN-4: newtype-flavored, explicit fields (no stringly-typed catch-all,
//!   except `class`/`confidence` which are DELIBERATELY plain strings — see above)
//! - RUST-DOMAIN-5: async I/O throughout
//! - RUST-DOMAIN-6: thiserror error enum (reused from crate root)
//! - SQL-AUDIT-COLUMNS-1: `ts` on every row (RFC3339 UTC)
//! - SQL-DB-INDEX-1/2: the `run_id` / `decision_id` WHERE/JOIN columns are indexed
//! - ORCH-NEW-PATH-TESTS-1: unit tests included in this file

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{Row, SqlitePool};
use std::path::Path;

use crate::error::PersistenceError;

// ---------------------------------------------------------------------------
// Migration SQL (idempotent)
// ---------------------------------------------------------------------------

const CREATE_ORCHESTRATOR_DECISIONS: &str = "
CREATE TABLE IF NOT EXISTS orchestrator_decisions (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id        TEXT    NOT NULL,
    ts            TEXT    NOT NULL,  -- RFC3339 UTC
    class         TEXT    NOT NULL,  -- 'mechanically_verified' | 'preview_reversible' | 'irreversible'
    confidence    TEXT    NOT NULL,  -- 'high' | 'medium' | 'low'
    chosen        TEXT    NOT NULL,
    alternatives  TEXT    NOT NULL,  -- JSON array of strings
    assumption    TEXT
);
";

const CREATE_IDX_ORCHESTRATOR_DECISIONS_RUN_ID: &str = "
CREATE INDEX IF NOT EXISTS idx_orchestrator_decisions_run_id
    ON orchestrator_decisions(run_id);
";

const CREATE_DECISION_OUTCOMES: &str = "
CREATE TABLE IF NOT EXISTS decision_outcomes (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    decision_id   INTEGER NOT NULL,
    outcome       TEXT    NOT NULL,  -- 'redirected' | 'defect_linked' | 'survived'
    ts            TEXT    NOT NULL   -- RFC3339 UTC
);
";

const CREATE_IDX_DECISION_OUTCOMES_DECISION_ID: &str = "
CREATE INDEX IF NOT EXISTS idx_decision_outcomes_decision_id
    ON decision_outcomes(decision_id);
";

// ---------------------------------------------------------------------------
// Model
// ---------------------------------------------------------------------------

/// One recorded autonomous decision. `class`/`confidence` are the plain-string wire
/// vocabulary `camerata-orchestrator-core`'s `DecisionClass::as_str()` /
/// `Confidence::as_str()` produce (see module doc for why this crate does not import
/// those enums).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrchestratorDecision {
    /// Row id, assigned by the database. `None` until recorded.
    pub id: Option<i64>,
    /// The governed run this decision belongs to.
    pub run_id: String,
    /// RFC3339 UTC timestamp, stamped at construction.
    pub ts: String,
    /// The effect-signature decision class: `"mechanically_verified"` |
    /// `"preview_reversible"` | `"irreversible"`.
    pub class: String,
    /// The confidence ordinal: `"high"` | `"medium"` | `"low"`.
    pub confidence: String,
    /// What the orchestrator chose (human-readable — the option/action it took).
    pub chosen: String,
    /// The alternatives it considered but did not choose.
    pub alternatives: Vec<String>,
    /// An assumption the orchestrator declared instead of asking a clarifying
    /// question (the "assume-and-declare into an assumptions ledger" pattern from the
    /// design doc), if any.
    pub assumption: Option<String>,
}

impl OrchestratorDecision {
    /// The general constructor. `ts` is stamped `Utc::now()` at construction; `id` is
    /// `None`; `alternatives` starts empty; `assumption` starts `None`.
    pub fn new(
        run_id: impl Into<String>,
        class: impl Into<String>,
        confidence: impl Into<String>,
        chosen: impl Into<String>,
    ) -> Self {
        Self {
            id: None,
            run_id: run_id.into(),
            ts: Utc::now().to_rfc3339(),
            class: class.into(),
            confidence: confidence.into(),
            chosen: chosen.into(),
            alternatives: Vec::new(),
            assumption: None,
        }
    }

    /// Attach the alternatives considered (builder-style, chainable).
    pub fn with_alternatives(mut self, alternatives: Vec<String>) -> Self {
        self.alternatives = alternatives;
        self
    }

    /// Attach a declared assumption (builder-style, chainable).
    pub fn with_assumption(mut self, assumption: impl Into<String>) -> Self {
        self.assumption = Some(assumption.into());
        self
    }
}

/// The outcome kind for a recorded [`OrchestratorDecision`] — a SMALL CLOSED enum
/// (unlike `class`/`confidence` above) because it drives [`OrchestratorDecisionLog::calibration`]'s
/// aggregate grouping directly; an unrecognized string here would silently corrupt the
/// moat metric rather than just being forward-compatible data, so [`DecisionOutcomeKind::parse`]
/// fails loudly instead of falling back to a default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DecisionOutcomeKind {
    /// A human overrode/redirected the decision.
    Redirected,
    /// A later defect report was linked back to this decision.
    DefectLinked,
    /// The decision was never redirected or defect-linked — it survived.
    Survived,
}

impl DecisionOutcomeKind {
    /// The stable lowercase wire/column string for this variant.
    pub fn as_str(&self) -> &'static str {
        match self {
            DecisionOutcomeKind::Redirected => "redirected",
            DecisionOutcomeKind::DefectLinked => "defect_linked",
            DecisionOutcomeKind::Survived => "survived",
        }
    }

    /// Parse the stable lowercase string back to a variant. Unlike the
    /// `DefectSource`/`DefectKind`-style forward-compatible parsers elsewhere in this
    /// codebase, this one returns `None` on an unrecognized value rather than
    /// defaulting — a silently-mis-bucketed outcome would corrupt the calibration
    /// aggregate, so a bad value must surface as an error at the read site instead.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "redirected" => Some(DecisionOutcomeKind::Redirected),
            "defect_linked" => Some(DecisionOutcomeKind::DefectLinked),
            "survived" => Some(DecisionOutcomeKind::Survived),
            _ => None,
        }
    }
}

/// One recorded outcome for a decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionOutcome {
    /// Row id, assigned by the database. `None` until recorded.
    pub id: Option<i64>,
    /// The decision this outcome is about.
    pub decision_id: i64,
    /// What happened to the decision.
    pub outcome: DecisionOutcomeKind,
    /// RFC3339 UTC timestamp, stamped at construction.
    pub ts: String,
}

impl DecisionOutcome {
    /// Construct a new outcome. `ts` is stamped `Utc::now()` at construction.
    pub fn new(decision_id: i64, outcome: DecisionOutcomeKind) -> Self {
        Self {
            id: None,
            decision_id,
            outcome,
            ts: Utc::now().to_rfc3339(),
        }
    }
}

/// One class's calibration row: how many decisions of this class were recorded, and
/// how many of each outcome kind they resolved to. `redirect_rate` is the moat metric:
/// "measured override rate at max dial: X%" (per class).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClassCalibration {
    /// The decision class this row summarizes (the same plain-string vocabulary as
    /// [`OrchestratorDecision::class`]).
    pub class: String,
    /// Total decisions recorded for this class (regardless of whether an outcome has
    /// been recorded yet).
    pub total: i64,
    /// How many resolved to [`DecisionOutcomeKind::Redirected`].
    pub redirected: i64,
    /// How many resolved to [`DecisionOutcomeKind::DefectLinked`].
    pub defect_linked: i64,
    /// How many resolved to [`DecisionOutcomeKind::Survived`].
    pub survived: i64,
    /// `redirected / total`, or `0.0` when `total` is zero (never divides by zero).
    pub redirect_rate: f64,
}

// ---------------------------------------------------------------------------
// OrchestratorDecisionLog: SQLite-backed store (write + read)
// ---------------------------------------------------------------------------

/// SQLite-backed decision + outcome + calibration store. Mirrors
/// [`crate::governance_event::GovernanceLog`]'s shape: a write path (`record_decision`,
/// `record_outcome`) and a read path (`calibration`), both exercised from day one.
#[derive(Debug, Clone)]
pub struct OrchestratorDecisionLog {
    pool: SqlitePool,
}

impl OrchestratorDecisionLog {
    /// Open (or create) the decision database at `path`, creating both tables and
    /// their indexes if they don't already exist. Idempotent — safe to call on every
    /// startup.
    pub async fn open(path: &Path) -> Result<Self, PersistenceError> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);
        let pool = SqlitePool::connect_with(options).await?;
        let log = Self { pool };
        log.migrate().await?;
        Ok(log)
    }

    /// Open an in-memory decision database (tests / ephemeral runs).
    pub async fn open_in_memory() -> Result<Self, PersistenceError> {
        let pool = SqlitePool::connect("sqlite::memory:").await?;
        let log = Self { pool };
        log.migrate().await?;
        Ok(log)
    }

    /// Create / migrate both tables and their indexes. Idempotent.
    async fn migrate(&self) -> Result<(), PersistenceError> {
        sqlx::query(CREATE_ORCHESTRATOR_DECISIONS)
            .execute(&self.pool)
            .await?;
        sqlx::query(CREATE_IDX_ORCHESTRATOR_DECISIONS_RUN_ID)
            .execute(&self.pool)
            .await?;
        sqlx::query(CREATE_DECISION_OUTCOMES).execute(&self.pool).await?;
        sqlx::query(CREATE_IDX_DECISION_OUTCOMES_DECISION_ID)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Record one decision. Returns the auto-assigned row id.
    pub async fn record_decision(
        &self,
        decision: OrchestratorDecision,
    ) -> Result<i64, PersistenceError> {
        let alternatives_json = serde_json::to_string(&decision.alternatives)?;
        let row = sqlx::query(
            "INSERT INTO orchestrator_decisions
                 (run_id, ts, class, confidence, chosen, alternatives, assumption)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             RETURNING id",
        )
        .bind(&decision.run_id)
        .bind(&decision.ts)
        .bind(&decision.class)
        .bind(&decision.confidence)
        .bind(&decision.chosen)
        .bind(&alternatives_json)
        .bind(&decision.assumption)
        .fetch_one(&self.pool)
        .await?;
        let id: i64 = row.try_get("id")?;
        Ok(id)
    }

    /// Record one outcome for a previously-recorded decision. Returns the
    /// auto-assigned row id. Does NOT validate that `outcome.decision_id` refers to an
    /// existing decision — the caller owns that invariant (mirrors SQLite's default
    /// lack of a declared foreign key elsewhere in this crate).
    pub async fn record_outcome(&self, outcome: DecisionOutcome) -> Result<i64, PersistenceError> {
        let row = sqlx::query(
            "INSERT INTO decision_outcomes (decision_id, outcome, ts)
             VALUES (?1, ?2, ?3)
             RETURNING id",
        )
        .bind(outcome.decision_id)
        .bind(outcome.outcome.as_str())
        .bind(&outcome.ts)
        .fetch_one(&self.pool)
        .await?;
        let id: i64 = row.try_get("id")?;
        Ok(id)
    }

    /// The calibration query: for every class that has at least one recorded
    /// decision, how many decisions, how many of each outcome kind, and the
    /// resulting redirect rate. This is the moat metric ("measured override rate at
    /// max dial: X%").
    pub async fn calibration(&self) -> Result<Vec<ClassCalibration>, PersistenceError> {
        let rows = sqlx::query(
            "SELECT
                 d.class AS class,
                 COUNT(DISTINCT d.id) AS total,
                 SUM(CASE WHEN o.outcome = 'redirected' THEN 1 ELSE 0 END) AS redirected,
                 SUM(CASE WHEN o.outcome = 'defect_linked' THEN 1 ELSE 0 END) AS defect_linked,
                 SUM(CASE WHEN o.outcome = 'survived' THEN 1 ELSE 0 END) AS survived
             FROM orchestrator_decisions d
             LEFT JOIN decision_outcomes o ON o.decision_id = d.id
             GROUP BY d.class
             ORDER BY d.class ASC",
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| {
                let class: String = row.try_get("class")?;
                let total: i64 = row.try_get("total")?;
                let redirected: i64 = row.try_get("redirected")?;
                let defect_linked: i64 = row.try_get("defect_linked")?;
                let survived: i64 = row.try_get("survived")?;
                let redirect_rate = if total > 0 {
                    redirected as f64 / total as f64
                } else {
                    0.0
                };
                Ok(ClassCalibration {
                    class,
                    total,
                    redirected,
                    defect_linked,
                    survived,
                    redirect_rate,
                })
            })
            .collect()
    }
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
        let log = OrchestratorDecisionLog::open_in_memory().await.expect("open");
        log.migrate().await.expect("second migrate is a no-op");
    }

    #[tokio::test]
    async fn open_path_persists_across_reopen() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("orchestrator_decisions.db");

        {
            let log = OrchestratorDecisionLog::open(&path).await.expect("open");
            log.record_decision(OrchestratorDecision::new(
                "run-durable",
                "mechanically_verified",
                "high",
                "picked option A",
            ))
            .await
            .expect("record");
        }
        assert!(path.exists(), "open should create the database file");

        let log = OrchestratorDecisionLog::open(&path).await.expect("reopen");
        let calibration = log.calibration().await.expect("calibration");
        assert_eq!(calibration.len(), 1);
        assert_eq!(calibration[0].class, "mechanically_verified");
        assert_eq!(calibration[0].total, 1);
    }

    // ── constructors ─────────────────────────────────────────────────────────

    #[test]
    fn new_constructor_sets_fields_and_defaults() {
        let d = OrchestratorDecision::new("run-1", "preview_reversible", "medium", "used default copy");
        assert_eq!(d.run_id, "run-1");
        assert_eq!(d.class, "preview_reversible");
        assert_eq!(d.confidence, "medium");
        assert_eq!(d.chosen, "used default copy");
        assert!(d.alternatives.is_empty());
        assert!(d.assumption.is_none());
        assert!(d.id.is_none());
        assert!(!d.ts.is_empty());
    }

    #[test]
    fn builder_methods_attach_optional_fields() {
        let d = OrchestratorDecision::new("run-2", "irreversible", "low", "chose terraform apply")
            .with_alternatives(vec!["wait for human".to_string(), "skip".to_string()])
            .with_assumption("assumed staging environment");
        assert_eq!(
            d.alternatives,
            vec!["wait for human".to_string(), "skip".to_string()]
        );
        assert_eq!(d.assumption.as_deref(), Some("assumed staging environment"));
    }

    #[test]
    fn decision_outcome_kind_as_str_and_parse_round_trip() {
        for v in [
            DecisionOutcomeKind::Redirected,
            DecisionOutcomeKind::DefectLinked,
            DecisionOutcomeKind::Survived,
        ] {
            assert_eq!(DecisionOutcomeKind::parse(v.as_str()), Some(v));
        }
    }

    #[test]
    fn decision_outcome_kind_parse_rejects_unknown_string() {
        assert_eq!(DecisionOutcomeKind::parse("bogus"), None);
    }

    // ── record_decision + record_outcome ────────────────────────────────────

    #[tokio::test]
    async fn record_decision_returns_increasing_ids() {
        let log = OrchestratorDecisionLog::open_in_memory().await.expect("open");
        let id1 = log
            .record_decision(OrchestratorDecision::new("run-a", "mechanically_verified", "high", "x"))
            .await
            .expect("record 1");
        let id2 = log
            .record_decision(OrchestratorDecision::new("run-a", "preview_reversible", "high", "y"))
            .await
            .expect("record 2");
        assert!(id1 < id2);
    }

    #[tokio::test]
    async fn record_outcome_links_to_a_decision() {
        let log = OrchestratorDecisionLog::open_in_memory().await.expect("open");
        let decision_id = log
            .record_decision(OrchestratorDecision::new("run-a", "preview_reversible", "high", "used default copy"))
            .await
            .expect("record decision");
        let outcome_id = log
            .record_outcome(DecisionOutcome::new(decision_id, DecisionOutcomeKind::Redirected))
            .await
            .expect("record outcome");
        assert!(outcome_id > 0);
    }

    // ── calibration: the moat metric ────────────────────────────────────────

    #[tokio::test]
    async fn calibration_is_empty_with_no_decisions() {
        let log = OrchestratorDecisionLog::open_in_memory().await.expect("open");
        let calibration = log.calibration().await.expect("calibration");
        assert!(calibration.is_empty());
    }

    #[tokio::test]
    async fn calibration_counts_decisions_with_no_outcome_as_total_only() {
        let log = OrchestratorDecisionLog::open_in_memory().await.expect("open");
        log.record_decision(OrchestratorDecision::new("run-a", "mechanically_verified", "high", "x"))
            .await
            .expect("record");
        log.record_decision(OrchestratorDecision::new("run-a", "mechanically_verified", "high", "y"))
            .await
            .expect("record");

        let calibration = log.calibration().await.expect("calibration");
        assert_eq!(calibration.len(), 1);
        assert_eq!(calibration[0].class, "mechanically_verified");
        assert_eq!(calibration[0].total, 2);
        assert_eq!(calibration[0].redirected, 0);
        assert_eq!(calibration[0].defect_linked, 0);
        assert_eq!(calibration[0].survived, 0);
        assert_eq!(calibration[0].redirect_rate, 0.0);
    }

    #[tokio::test]
    async fn calibration_computes_redirect_rate_per_class() {
        let log = OrchestratorDecisionLog::open_in_memory().await.expect("open");

        // Class "preview_reversible": 4 decisions, 1 redirected, 1 defect-linked, 2 survived.
        let mut pr_ids = Vec::new();
        for _ in 0..4 {
            let id = log
                .record_decision(OrchestratorDecision::new(
                    "run-a",
                    "preview_reversible",
                    "high",
                    "x",
                ))
                .await
                .expect("record");
            pr_ids.push(id);
        }
        log.record_outcome(DecisionOutcome::new(pr_ids[0], DecisionOutcomeKind::Redirected))
            .await
            .expect("record outcome");
        log.record_outcome(DecisionOutcome::new(pr_ids[1], DecisionOutcomeKind::DefectLinked))
            .await
            .expect("record outcome");
        log.record_outcome(DecisionOutcome::new(pr_ids[2], DecisionOutcomeKind::Survived))
            .await
            .expect("record outcome");
        log.record_outcome(DecisionOutcome::new(pr_ids[3], DecisionOutcomeKind::Survived))
            .await
            .expect("record outcome");

        // Class "irreversible": 1 decision, 1 redirected (a red flag if this ever
        // happens for real — C should always be human-gated before it executes — but
        // the store itself does not enforce that; it just records what happened).
        let irr_id = log
            .record_decision(OrchestratorDecision::new("run-b", "irreversible", "low", "z"))
            .await
            .expect("record");
        log.record_outcome(DecisionOutcome::new(irr_id, DecisionOutcomeKind::Redirected))
            .await
            .expect("record outcome");

        let calibration = log.calibration().await.expect("calibration");
        assert_eq!(calibration.len(), 2);

        let pr = calibration
            .iter()
            .find(|c| c.class == "preview_reversible")
            .expect("preview_reversible row");
        assert_eq!(pr.total, 4);
        assert_eq!(pr.redirected, 1);
        assert_eq!(pr.defect_linked, 1);
        assert_eq!(pr.survived, 2);
        assert_eq!(pr.redirect_rate, 0.25);

        let irr = calibration
            .iter()
            .find(|c| c.class == "irreversible")
            .expect("irreversible row");
        assert_eq!(irr.total, 1);
        assert_eq!(irr.redirected, 1);
        assert_eq!(irr.redirect_rate, 1.0);
    }

    #[tokio::test]
    async fn alternatives_and_assumption_round_trip_through_storage() {
        let log = OrchestratorDecisionLog::open_in_memory().await.expect("open");
        let decision = OrchestratorDecision::new("run-a", "mechanically_verified", "high", "picked option B")
            .with_alternatives(vec!["option A".to_string(), "option C".to_string()])
            .with_assumption("assumed the default config applies");
        let id = log.record_decision(decision).await.expect("record");
        assert!(id > 0);
        // No direct read-back-by-id accessor is exposed today (only the aggregate
        // `calibration` query) — this test exists to prove the JSON round trip does
        // not error on write for a non-empty alternatives list + assumption, which is
        // the surface `record_decision` actually exercises.
    }
}
