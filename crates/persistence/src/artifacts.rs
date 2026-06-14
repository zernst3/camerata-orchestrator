//! Append-only, version-tracked artifact store.
//!
//! Every edit to a consumer-app living document (onboarding document, user
//! story, clarification, product suggestion, refinement session) is stored as
//! a new row in `artifact_revisions`.  Reading the "current" state means
//! fetching the highest-version non-deleted revision.  Reading the full history
//! means fetching every revision for that artifact ordered by version.  This
//! gives persistence, real-time update capture, and time-travel from a single
//! table.
//!
//! Conventions honored:
//! - RUST-DOMAIN-4: newtype IDs
//! - RUST-DOMAIN-5: async I/O throughout
//! - RUST-DOMAIN-6: thiserror error enum (reused from crate root)
//! - SQL-AUDIT-COLUMNS-1: `created_at` on every row
//! - SQL-DB-INDEX-1/2: FK and WHERE columns indexed
//! - RUST-PURE-STATE-TRANSITIONS-1: caller supplies `created_at`
//! - ORCH-NEW-PATH-TESTS-1: unit tests included in this file

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::{error::PersistenceError, store::SqliteStore};

// ---------------------------------------------------------------------------
// Migration SQL (idempotent, SQL-AUDIT-COLUMNS-1, SQL-DB-INDEX-1/2)
// ---------------------------------------------------------------------------

/// DDL for `artifact_revisions`.
///
/// `created_at` satisfies SQL-AUDIT-COLUMNS-1.
const CREATE_ARTIFACT_REVISIONS: &str = "
CREATE TABLE IF NOT EXISTS artifact_revisions (
    revision_id INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id  TEXT    NOT NULL,
    kind        TEXT    NOT NULL,
    artifact_id TEXT    NOT NULL,
    version     INTEGER NOT NULL,
    actor       TEXT    NOT NULL,
    op          TEXT    NOT NULL,
    payload     TEXT    NOT NULL,
    created_at  TEXT    NOT NULL
);
";

/// Uniqueness backstop: no two revisions for the same artifact may share a version.
/// Satisfies SQL-DB-INDEX-1 for the composite identity column.
const CREATE_UQ_ARTIFACT_REV_VERSION: &str = "
CREATE UNIQUE INDEX IF NOT EXISTS uq_artifact_rev_version
    ON artifact_revisions(project_id, kind, artifact_id, version);
";

/// Lookup index for fetching all revisions of a single artifact (SQL-DB-INDEX-2).
const CREATE_IDX_ARTIFACT_REV_LOOKUP: &str = "
CREATE INDEX IF NOT EXISTS idx_artifact_rev_lookup
    ON artifact_revisions(project_id, kind, artifact_id);
";

/// Index for listing all current artifacts within a project and kind (SQL-DB-INDEX-2).
const CREATE_IDX_ARTIFACT_REV_CURRENT: &str = "
CREATE INDEX IF NOT EXISTS idx_artifact_rev_current
    ON artifact_revisions(project_id, kind);
";

// ---------------------------------------------------------------------------
// ArtifactKind
// ---------------------------------------------------------------------------

/// The category of living document being tracked.
///
/// Each variant maps to a stable snake_case string stored in the database so
/// that the text is human-readable in raw SQL queries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    /// The initial onboarding document produced at project start.
    OnboardingDocument,
    /// A discrete user story scoped to a feature or workflow.
    UserStory,
    /// A clarification recorded in response to an ambiguous requirement.
    Clarification,
    /// A product suggestion proposed by the user or the AI.
    Suggestion,
    /// A refinement session transcript or summary.
    RefinementSession,
}

impl ArtifactKind {
    /// The stable snake_case string persisted in `artifact_revisions.kind`.
    pub fn as_str(&self) -> &'static str {
        match self {
            ArtifactKind::OnboardingDocument => "onboarding_document",
            ArtifactKind::UserStory => "user_story",
            ArtifactKind::Clarification => "clarification",
            ArtifactKind::Suggestion => "suggestion",
            ArtifactKind::RefinementSession => "refinement_session",
        }
    }

    /// Parse from the stored snake_case string. Returns `None` for unknown values.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "onboarding_document" => Some(ArtifactKind::OnboardingDocument),
            "user_story" => Some(ArtifactKind::UserStory),
            "clarification" => Some(ArtifactKind::Clarification),
            "suggestion" => Some(ArtifactKind::Suggestion),
            "refinement_session" => Some(ArtifactKind::RefinementSession),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// EditActor
// ---------------------------------------------------------------------------

/// Who authored a particular revision.
///
/// Stored as a snake_case string in the database so the audit trail is
/// human-readable without decoding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditActor {
    /// The revision was authored by the end user.
    User,
    /// The revision was authored by the AI orchestrator.
    Ai,
}

impl EditActor {
    /// The stable snake_case string persisted in `artifact_revisions.actor`.
    pub fn as_str(&self) -> &'static str {
        match self {
            EditActor::User => "user",
            EditActor::Ai => "ai",
        }
    }

    /// Parse from the stored snake_case string. Returns `None` for unknown values.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "user" => Some(EditActor::User),
            "ai" => Some(EditActor::Ai),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// RevisionOp
// ---------------------------------------------------------------------------

/// The operation recorded by a revision row.
///
/// The log is append-only: a logical delete is a new row with `op = Delete`,
/// not a physical row removal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevisionOp {
    /// The first revision of an artifact.
    Create,
    /// A subsequent content edit.
    Update,
    /// A logical deletion. The `payload` field may be empty for delete revisions.
    Delete,
}

impl RevisionOp {
    /// The stable snake_case string persisted in `artifact_revisions.op`.
    pub fn as_str(&self) -> &'static str {
        match self {
            RevisionOp::Create => "create",
            RevisionOp::Update => "update",
            RevisionOp::Delete => "delete",
        }
    }

    /// Parse from the stored snake_case string. Returns `None` for unknown values.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "create" => Some(RevisionOp::Create),
            "update" => Some(RevisionOp::Update),
            "delete" => Some(RevisionOp::Delete),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// NewRevision (input DTO)
// ---------------------------------------------------------------------------

/// The input required to record a single edit to an artifact.
///
/// `payload` is an opaque JSON snapshot of the artifact at this version.  The
/// persistence crate does not inspect or validate the payload; higher layers
/// are responsible for serializing their typed artifacts via [`encode`].
///
/// `created_at` is caller-supplied so tests can inject deterministic
/// timestamps (RUST-PURE-STATE-TRANSITIONS-1).
///
/// For a [`RevisionOp::Delete`] revision, `payload` may be an empty string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewRevision {
    /// The project this artifact belongs to.
    pub project_id: String,
    /// The category of living document.
    pub kind: ArtifactKind,
    /// A caller-assigned stable identifier for the artifact within the project.
    /// Arbitrary string, e.g. a UUID or a slug.
    pub artifact_id: String,
    /// Who is authoring this revision.
    pub actor: EditActor,
    /// Whether this is a creation, update, or logical deletion.
    pub op: RevisionOp,
    /// Opaque JSON snapshot of the artifact at this version.
    pub payload: String,
    /// Audit timestamp (SQL-AUDIT-COLUMNS-1). Caller-supplied for determinism.
    pub created_at: DateTime<Utc>,
}

impl NewRevision {
    /// Construct a new (un-persisted) revision descriptor.
    pub fn new(
        project_id: impl Into<String>,
        kind: ArtifactKind,
        artifact_id: impl Into<String>,
        actor: EditActor,
        op: RevisionOp,
        payload: impl Into<String>,
        created_at: DateTime<Utc>,
    ) -> Self {
        Self {
            project_id: project_id.into(),
            kind,
            artifact_id: artifact_id.into(),
            actor,
            op,
            payload: payload.into(),
            created_at,
        }
    }
}

// ---------------------------------------------------------------------------
// ArtifactRevision (stored row)
// ---------------------------------------------------------------------------

/// A stored revision row from `artifact_revisions`.
///
/// `revision_id` is the global autoincrement primary key.
/// `version` is the per-(project, kind, artifact_id) monotonic counter
/// starting at 1, incremented by one for every new revision appended to the
/// same artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRevision {
    /// Global autoincrement primary key.
    pub revision_id: i64,
    /// The project this artifact belongs to.
    pub project_id: String,
    /// The category of living document.
    pub kind: ArtifactKind,
    /// Stable artifact identifier within the project.
    pub artifact_id: String,
    /// Monotonic version counter scoped to (project_id, kind, artifact_id),
    /// starting at 1.
    pub version: i64,
    /// Who authored this revision.
    pub actor: EditActor,
    /// The operation recorded by this revision.
    pub op: RevisionOp,
    /// Opaque JSON snapshot of the artifact at this version.
    pub payload: String,
    /// Audit timestamp (SQL-AUDIT-COLUMNS-1).
    pub created_at: DateTime<Utc>,
}

impl ArtifactRevision {
    /// Deserialize the `payload` field into a typed value.
    ///
    /// Returns a [`PersistenceError::Json`] if the payload cannot be decoded
    /// as `T`.
    pub fn decode<T: serde::de::DeserializeOwned>(&self) -> Result<T, PersistenceError> {
        serde_json::from_str(&self.payload).map_err(PersistenceError::Json)
    }
}

/// Serialize a value to the JSON string form expected in [`NewRevision::payload`].
///
/// Returns a [`PersistenceError::Json`] on serialization failure.
pub fn encode<T: serde::Serialize>(value: &T) -> Result<String, PersistenceError> {
    serde_json::to_string(value).map_err(PersistenceError::Json)
}

// ---------------------------------------------------------------------------
// ArtifactStore trait
// ---------------------------------------------------------------------------

/// Async persistence seam for the append-only artifact revision log.
///
/// Every method is async (RUST-DOMAIN-5). The trait is object-safe and
/// `Send + Sync` so it can be stored in `Arc<dyn ArtifactStore>`.
#[async_trait]
pub trait ArtifactStore: Send + Sync {
    /// Create or migrate the `artifact_revisions` schema. Idempotent: safe to
    /// call on every startup.
    async fn migrate_artifacts(&self) -> Result<(), PersistenceError>;

    /// Append one revision to the log and return the fully-populated stored row.
    ///
    /// The next version number is computed as
    /// `SELECT COALESCE(MAX(version), 0) + 1` scoped to the artifact, inside a
    /// transaction so concurrent appends cannot produce duplicate version numbers.
    /// The unique index on (project_id, kind, artifact_id, version) is the final
    /// backstop.
    async fn record_revision(
        &self,
        new: &NewRevision,
    ) -> Result<ArtifactRevision, PersistenceError>;

    /// The latest live (non-deleted) revision for each artifact of the given
    /// kind within the project, ordered by `artifact_id ASC`.
    ///
    /// An artifact whose most recent revision has `op = 'delete'` is excluded.
    async fn current(
        &self,
        project_id: &str,
        kind: ArtifactKind,
    ) -> Result<Vec<ArtifactRevision>, PersistenceError>;

    /// The single latest revision for one artifact.
    ///
    /// Returns `None` if the artifact has never been created, or if its most
    /// recent revision is a deletion.
    async fn current_artifact(
        &self,
        project_id: &str,
        kind: ArtifactKind,
        artifact_id: &str,
    ) -> Result<Option<ArtifactRevision>, PersistenceError>;

    /// Every revision for one artifact ordered by `version ASC`.
    ///
    /// Includes all operations (create, update, delete) so the caller has the
    /// full audit trail.
    async fn history(
        &self,
        project_id: &str,
        kind: ArtifactKind,
        artifact_id: &str,
    ) -> Result<Vec<ArtifactRevision>, PersistenceError>;

    /// A specific historical revision by its version number (time-travel).
    ///
    /// Returns `None` if no revision exists at that version for the artifact.
    async fn revision_at(
        &self,
        project_id: &str,
        kind: ArtifactKind,
        artifact_id: &str,
        version: i64,
    ) -> Result<Option<ArtifactRevision>, PersistenceError>;
}

// ---------------------------------------------------------------------------
// Row mapping helper (private)
// ---------------------------------------------------------------------------

/// Parse one sqlx row into an [`ArtifactRevision`].
fn row_to_revision(row: &sqlx::sqlite::SqliteRow) -> Result<ArtifactRevision, PersistenceError> {
    let revision_id: i64 = row.try_get("revision_id")?;
    let project_id: String = row.try_get("project_id")?;
    let kind_str: String = row.try_get("kind")?;
    let artifact_id: String = row.try_get("artifact_id")?;
    let version: i64 = row.try_get("version")?;
    let actor_str: String = row.try_get("actor")?;
    let op_str: String = row.try_get("op")?;
    let payload: String = row.try_get("payload")?;
    let created_at_str: String = row.try_get("created_at")?;

    let kind = ArtifactKind::from_str(&kind_str).ok_or_else(|| {
        sqlx::Error::Decode(format!("unknown artifact kind: {kind_str}").into())
    })?;
    let actor = EditActor::from_str(&actor_str).ok_or_else(|| {
        sqlx::Error::Decode(format!("unknown edit actor: {actor_str}").into())
    })?;
    let op = RevisionOp::from_str(&op_str).ok_or_else(|| {
        sqlx::Error::Decode(format!("unknown revision op: {op_str}").into())
    })?;
    let created_at: DateTime<Utc> = created_at_str.parse().unwrap_or_else(|_| Utc::now());

    Ok(ArtifactRevision {
        revision_id,
        project_id,
        kind,
        artifact_id,
        version,
        actor,
        op,
        payload,
        created_at,
    })
}

// ---------------------------------------------------------------------------
// SqliteStore impl
// ---------------------------------------------------------------------------

#[async_trait]
impl ArtifactStore for SqliteStore {
    async fn migrate_artifacts(&self) -> Result<(), PersistenceError> {
        sqlx::query(CREATE_ARTIFACT_REVISIONS)
            .execute(self.pool())
            .await?;
        sqlx::query(CREATE_UQ_ARTIFACT_REV_VERSION)
            .execute(self.pool())
            .await?;
        sqlx::query(CREATE_IDX_ARTIFACT_REV_LOOKUP)
            .execute(self.pool())
            .await?;
        sqlx::query(CREATE_IDX_ARTIFACT_REV_CURRENT)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    async fn record_revision(
        &self,
        new: &NewRevision,
    ) -> Result<ArtifactRevision, PersistenceError> {
        let kind_str = new.kind.as_str();
        let actor_str = new.actor.as_str();
        let op_str = new.op.as_str();

        let mut tx = self.pool().begin().await?;

        // Compute the next version inside the transaction to prevent concurrent
        // races from producing duplicate version numbers. The unique index on
        // (project_id, kind, artifact_id, version) provides the final backstop.
        let version_row = sqlx::query(
            "SELECT COALESCE(MAX(version), 0) + 1 AS next_version
             FROM artifact_revisions
             WHERE project_id = ?1 AND kind = ?2 AND artifact_id = ?3",
        )
        .bind(&new.project_id)
        .bind(kind_str)
        .bind(&new.artifact_id)
        .fetch_one(&mut *tx)
        .await?;

        let next_version: i64 = version_row.try_get("next_version")?;

        let inserted_row = sqlx::query(
            "INSERT INTO artifact_revisions
                 (project_id, kind, artifact_id, version, actor, op, payload, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             RETURNING revision_id",
        )
        .bind(&new.project_id)
        .bind(kind_str)
        .bind(&new.artifact_id)
        .bind(next_version)
        .bind(actor_str)
        .bind(op_str)
        .bind(&new.payload)
        .bind(new.created_at.to_rfc3339())
        .fetch_one(&mut *tx)
        .await?;

        let revision_id: i64 = inserted_row.try_get("revision_id")?;

        tx.commit().await?;

        Ok(ArtifactRevision {
            revision_id,
            project_id: new.project_id.clone(),
            kind: new.kind.clone(),
            artifact_id: new.artifact_id.clone(),
            version: next_version,
            actor: new.actor.clone(),
            op: new.op.clone(),
            payload: new.payload.clone(),
            created_at: new.created_at,
        })
    }

    async fn current(
        &self,
        project_id: &str,
        kind: ArtifactKind,
    ) -> Result<Vec<ArtifactRevision>, PersistenceError> {
        let kind_str = kind.as_str();

        // Join the per-artifact MAX(version) back to the table, then exclude
        // rows whose op is 'delete'.
        let rows = sqlx::query(
            "SELECT r.revision_id, r.project_id, r.kind, r.artifact_id,
                    r.version, r.actor, r.op, r.payload, r.created_at
             FROM artifact_revisions r
             INNER JOIN (
                 SELECT artifact_id, MAX(version) AS max_version
                 FROM artifact_revisions
                 WHERE project_id = ?1 AND kind = ?2
                 GROUP BY artifact_id
             ) latest
                 ON r.artifact_id = latest.artifact_id
                 AND r.version    = latest.max_version
                 AND r.project_id = ?1
                 AND r.kind       = ?2
             WHERE r.op != 'delete'
             ORDER BY r.artifact_id ASC",
        )
        .bind(project_id)
        .bind(kind_str)
        .fetch_all(self.pool())
        .await?;

        rows.iter().map(row_to_revision).collect()
    }

    async fn current_artifact(
        &self,
        project_id: &str,
        kind: ArtifactKind,
        artifact_id: &str,
    ) -> Result<Option<ArtifactRevision>, PersistenceError> {
        let kind_str = kind.as_str();

        let maybe_row = sqlx::query(
            "SELECT revision_id, project_id, kind, artifact_id,
                    version, actor, op, payload, created_at
             FROM artifact_revisions
             WHERE project_id = ?1 AND kind = ?2 AND artifact_id = ?3
             ORDER BY version DESC
             LIMIT 1",
        )
        .bind(project_id)
        .bind(kind_str)
        .bind(artifact_id)
        .fetch_optional(self.pool())
        .await?;

        match maybe_row {
            None => Ok(None),
            Some(row) => {
                let rev = row_to_revision(&row)?;
                // If the latest op is a deletion, the artifact is logically gone.
                if rev.op == RevisionOp::Delete {
                    Ok(None)
                } else {
                    Ok(Some(rev))
                }
            }
        }
    }

    async fn history(
        &self,
        project_id: &str,
        kind: ArtifactKind,
        artifact_id: &str,
    ) -> Result<Vec<ArtifactRevision>, PersistenceError> {
        let kind_str = kind.as_str();

        let rows = sqlx::query(
            "SELECT revision_id, project_id, kind, artifact_id,
                    version, actor, op, payload, created_at
             FROM artifact_revisions
             WHERE project_id = ?1 AND kind = ?2 AND artifact_id = ?3
             ORDER BY version ASC",
        )
        .bind(project_id)
        .bind(kind_str)
        .bind(artifact_id)
        .fetch_all(self.pool())
        .await?;

        rows.iter().map(row_to_revision).collect()
    }

    async fn revision_at(
        &self,
        project_id: &str,
        kind: ArtifactKind,
        artifact_id: &str,
        version: i64,
    ) -> Result<Option<ArtifactRevision>, PersistenceError> {
        let kind_str = kind.as_str();

        let maybe_row = sqlx::query(
            "SELECT revision_id, project_id, kind, artifact_id,
                    version, actor, op, payload, created_at
             FROM artifact_revisions
             WHERE project_id = ?1 AND kind = ?2 AND artifact_id = ?3
               AND version = ?4",
        )
        .bind(project_id)
        .bind(kind_str)
        .bind(artifact_id)
        .bind(version)
        .fetch_optional(self.pool())
        .await?;

        match maybe_row {
            None => Ok(None),
            Some(row) => row_to_revision(&row).map(Some),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests (ORCH-NEW-PATH-TESTS-1)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    /// Opens a fresh in-memory store with ALL tables created (including
    /// artifact_revisions). Mirrors the pattern in store.rs.
    async fn in_memory_store() -> SqliteStore {
        SqliteStore::open("sqlite::memory:")
            .await
            .expect("in-memory store")
    }

    // -----------------------------------------------------------------------
    // Test 1: Create then Update; current_artifact returns latest payload
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_create_then_update_version_sequence() {
        let store = in_memory_store().await;
        let t = Utc::now();

        let r1 = store
            .record_revision(&NewRevision::new(
                "proj-1",
                ArtifactKind::UserStory,
                "us-001",
                EditActor::User,
                RevisionOp::Create,
                r#"{"title":"As a user I can log in"}"#,
                t,
            ))
            .await
            .expect("create");

        assert_eq!(r1.version, 1, "first revision must be version 1");

        let r2 = store
            .record_revision(&NewRevision::new(
                "proj-1",
                ArtifactKind::UserStory,
                "us-001",
                EditActor::Ai,
                RevisionOp::Update,
                r#"{"title":"As a user I can log in with SSO"}"#,
                t,
            ))
            .await
            .expect("update");

        assert_eq!(r2.version, 2, "second revision must be version 2");

        let current = store
            .current_artifact("proj-1", ArtifactKind::UserStory, "us-001")
            .await
            .expect("current_artifact")
            .expect("should exist");

        assert_eq!(current.version, 2);
        assert!(current.payload.contains("SSO"));
    }

    // -----------------------------------------------------------------------
    // Test 2: history returns all revisions in version order
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_history_order() {
        let store = in_memory_store().await;
        let t = Utc::now();

        for (i, content) in ["v1", "v2", "v3"].iter().enumerate() {
            let op = if i == 0 {
                RevisionOp::Create
            } else {
                RevisionOp::Update
            };
            store
                .record_revision(&NewRevision::new(
                    "proj-h",
                    ArtifactKind::Clarification,
                    "cl-001",
                    EditActor::User,
                    op,
                    format!(r#"{{"body":"{}"}}"#, content),
                    t,
                ))
                .await
                .expect("record");
        }

        let hist = store
            .history("proj-h", ArtifactKind::Clarification, "cl-001")
            .await
            .expect("history");

        assert_eq!(hist.len(), 3);
        assert_eq!(hist[0].version, 1);
        assert_eq!(hist[1].version, 2);
        assert_eq!(hist[2].version, 3);
        assert!(hist[0].payload.contains("v1"));
        assert!(hist[2].payload.contains("v3"));
    }

    // -----------------------------------------------------------------------
    // Test 3: Two different artifact_ids get independent version sequences
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_independent_version_sequences_per_artifact() {
        let store = in_memory_store().await;
        let t = Utc::now();

        let a = store
            .record_revision(&NewRevision::new(
                "proj-v",
                ArtifactKind::Suggestion,
                "sug-aaa",
                EditActor::User,
                RevisionOp::Create,
                r#"{"text":"Add dark mode"}"#,
                t,
            ))
            .await
            .expect("a create");

        let b = store
            .record_revision(&NewRevision::new(
                "proj-v",
                ArtifactKind::Suggestion,
                "sug-bbb",
                EditActor::Ai,
                RevisionOp::Create,
                r#"{"text":"Add keyboard shortcuts"}"#,
                t,
            ))
            .await
            .expect("b create");

        // Update artifact A
        let a2 = store
            .record_revision(&NewRevision::new(
                "proj-v",
                ArtifactKind::Suggestion,
                "sug-aaa",
                EditActor::User,
                RevisionOp::Update,
                r#"{"text":"Add dark mode v2"}"#,
                t,
            ))
            .await
            .expect("a update");

        assert_eq!(a.version, 1, "sug-aaa first revision");
        assert_eq!(b.version, 1, "sug-bbb first revision is independent");
        assert_eq!(a2.version, 2, "sug-aaa second revision");
    }

    // -----------------------------------------------------------------------
    // Test 4: current() excludes deleted artifacts; re-created artifact is visible
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_current_excludes_deleted_includes_recreated() {
        let store = in_memory_store().await;
        let t = Utc::now();

        // live-artifact: Create only
        store
            .record_revision(&NewRevision::new(
                "proj-c",
                ArtifactKind::OnboardingDocument,
                "doc-live",
                EditActor::User,
                RevisionOp::Create,
                r#"{"content":"Live doc"}"#,
                t,
            ))
            .await
            .expect("live create");

        // dead-artifact: Create then Delete
        store
            .record_revision(&NewRevision::new(
                "proj-c",
                ArtifactKind::OnboardingDocument,
                "doc-dead",
                EditActor::User,
                RevisionOp::Create,
                r#"{"content":"Dead doc"}"#,
                t,
            ))
            .await
            .expect("dead create");

        store
            .record_revision(&NewRevision::new(
                "proj-c",
                ArtifactKind::OnboardingDocument,
                "doc-dead",
                EditActor::User,
                RevisionOp::Delete,
                "",
                t,
            ))
            .await
            .expect("dead delete");

        // reborn-artifact: Create, Delete, then Create again (latest is Create)
        for op in [RevisionOp::Create, RevisionOp::Delete, RevisionOp::Create] {
            store
                .record_revision(&NewRevision::new(
                    "proj-c",
                    ArtifactKind::OnboardingDocument,
                    "doc-reborn",
                    EditActor::Ai,
                    op,
                    r#"{"content":"Reborn"}"#,
                    t,
                ))
                .await
                .expect("reborn step");
        }

        let live = store
            .current("proj-c", ArtifactKind::OnboardingDocument)
            .await
            .expect("current");

        let ids: Vec<&str> = live.iter().map(|r| r.artifact_id.as_str()).collect();
        assert!(ids.contains(&"doc-live"), "live artifact must appear");
        assert!(!ids.contains(&"doc-dead"), "deleted artifact must be excluded");
        assert!(ids.contains(&"doc-reborn"), "re-created artifact must appear");

        let reborn = live.iter().find(|r| r.artifact_id == "doc-reborn").unwrap();
        assert_eq!(reborn.op, RevisionOp::Create);
        assert_eq!(reborn.version, 3, "version keeps climbing after re-create");
    }

    // -----------------------------------------------------------------------
    // Test 5: revision_at returns old payload; current_artifact returns newest
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_revision_at_time_travel() {
        let store = in_memory_store().await;
        let t = Utc::now();

        store
            .record_revision(&NewRevision::new(
                "proj-t",
                ArtifactKind::RefinementSession,
                "rs-001",
                EditActor::User,
                RevisionOp::Create,
                r#"{"notes":"initial notes"}"#,
                t,
            ))
            .await
            .expect("v1");

        store
            .record_revision(&NewRevision::new(
                "proj-t",
                ArtifactKind::RefinementSession,
                "rs-001",
                EditActor::Ai,
                RevisionOp::Update,
                r#"{"notes":"refined notes"}"#,
                t,
            ))
            .await
            .expect("v2");

        let v1 = store
            .revision_at("proj-t", ArtifactKind::RefinementSession, "rs-001", 1)
            .await
            .expect("revision_at")
            .expect("v1 must exist");

        assert!(v1.payload.contains("initial"));

        let current = store
            .current_artifact("proj-t", ArtifactKind::RefinementSession, "rs-001")
            .await
            .expect("current_artifact")
            .expect("must exist");

        assert!(current.payload.contains("refined"));
        assert_eq!(current.version, 2);
    }

    // -----------------------------------------------------------------------
    // Test 6: AI and User edits both persist with correct actor
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_actor_audit_trail() {
        let store = in_memory_store().await;
        let t = Utc::now();

        store
            .record_revision(&NewRevision::new(
                "proj-a",
                ArtifactKind::UserStory,
                "us-audit",
                EditActor::User,
                RevisionOp::Create,
                r#"{"body":"user-authored"}"#,
                t,
            ))
            .await
            .expect("user create");

        store
            .record_revision(&NewRevision::new(
                "proj-a",
                ArtifactKind::UserStory,
                "us-audit",
                EditActor::Ai,
                RevisionOp::Update,
                r#"{"body":"ai-refined"}"#,
                t,
            ))
            .await
            .expect("ai update");

        let hist = store
            .history("proj-a", ArtifactKind::UserStory, "us-audit")
            .await
            .expect("history");

        assert_eq!(hist.len(), 2);
        assert_eq!(hist[0].actor, EditActor::User);
        assert_eq!(hist[1].actor, EditActor::Ai);
    }

    // -----------------------------------------------------------------------
    // Test 7: decode/encode round-trip a serde struct through a revision payload
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_encode_decode_round_trip() {
        #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
        struct StoryPayload {
            title: String,
            priority: u32,
        }

        let store = in_memory_store().await;
        let t = Utc::now();
        let original = StoryPayload {
            title: "User can reset password".to_string(),
            priority: 2,
        };
        let json = encode(&original).expect("encode");

        store
            .record_revision(&NewRevision::new(
                "proj-rt",
                ArtifactKind::UserStory,
                "us-rt",
                EditActor::User,
                RevisionOp::Create,
                json,
                t,
            ))
            .await
            .expect("record");

        let rev = store
            .current_artifact("proj-rt", ArtifactKind::UserStory, "us-rt")
            .await
            .expect("current_artifact")
            .expect("must exist");

        let decoded: StoryPayload = rev.decode().expect("decode");
        assert_eq!(decoded, original);
    }

    // -----------------------------------------------------------------------
    // Test 8: Different ArtifactKinds in the same project do not interfere
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_different_kinds_do_not_interfere() {
        let store = in_memory_store().await;
        let t = Utc::now();

        // Record a UserStory with artifact_id "x"
        let us = store
            .record_revision(&NewRevision::new(
                "proj-k",
                ArtifactKind::UserStory,
                "x",
                EditActor::User,
                RevisionOp::Create,
                r#"{"type":"user_story"}"#,
                t,
            ))
            .await
            .expect("user story");

        // Record a Clarification with the same artifact_id "x"
        let cl = store
            .record_revision(&NewRevision::new(
                "proj-k",
                ArtifactKind::Clarification,
                "x",
                EditActor::Ai,
                RevisionOp::Create,
                r#"{"type":"clarification"}"#,
                t,
            ))
            .await
            .expect("clarification");

        assert_eq!(us.version, 1, "UserStory x starts at version 1");
        assert_eq!(cl.version, 1, "Clarification x has its own version 1");

        // Update just the UserStory
        let us2 = store
            .record_revision(&NewRevision::new(
                "proj-k",
                ArtifactKind::UserStory,
                "x",
                EditActor::User,
                RevisionOp::Update,
                r#"{"type":"user_story","v":2}"#,
                t,
            ))
            .await
            .expect("user story update");

        assert_eq!(us2.version, 2, "UserStory x increments independently");

        // Clarification must still be at version 1
        let cl_current = store
            .current_artifact("proj-k", ArtifactKind::Clarification, "x")
            .await
            .expect("current clarification")
            .expect("exists");
        assert_eq!(cl_current.version, 1);

        // history for each kind is isolated
        let us_hist = store
            .history("proj-k", ArtifactKind::UserStory, "x")
            .await
            .expect("us history");
        let cl_hist = store
            .history("proj-k", ArtifactKind::Clarification, "x")
            .await
            .expect("cl history");

        assert_eq!(us_hist.len(), 2);
        assert_eq!(cl_hist.len(), 1);
    }

    // -----------------------------------------------------------------------
    // Test 9: as_str/from_str round-trips for all discriminants
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_enum_round_trips() {
        // ArtifactKind
        for kind in [
            ArtifactKind::OnboardingDocument,
            ArtifactKind::UserStory,
            ArtifactKind::Clarification,
            ArtifactKind::Suggestion,
            ArtifactKind::RefinementSession,
        ] {
            let s = kind.as_str();
            let parsed = ArtifactKind::from_str(s).expect("ArtifactKind round-trip");
            assert_eq!(parsed, kind, "ArtifactKind::{s}");
        }

        // EditActor
        for actor in [EditActor::User, EditActor::Ai] {
            let s = actor.as_str();
            let parsed = EditActor::from_str(s).expect("EditActor round-trip");
            assert_eq!(parsed, actor, "EditActor::{s}");
        }

        // RevisionOp
        for op in [RevisionOp::Create, RevisionOp::Update, RevisionOp::Delete] {
            let s = op.as_str();
            let parsed = RevisionOp::from_str(s).expect("RevisionOp round-trip");
            assert_eq!(parsed, op, "RevisionOp::{s}");
        }

        // Serde JSON round-trip via the enums' serde impls
        let kind_json = serde_json::to_string(&ArtifactKind::RefinementSession).unwrap();
        let kind_back: ArtifactKind = serde_json::from_str(&kind_json).unwrap();
        assert_eq!(kind_back, ArtifactKind::RefinementSession);

        let actor_json = serde_json::to_string(&EditActor::Ai).unwrap();
        let actor_back: EditActor = serde_json::from_str(&actor_json).unwrap();
        assert_eq!(actor_back, EditActor::Ai);

        let op_json = serde_json::to_string(&RevisionOp::Delete).unwrap();
        let op_back: RevisionOp = serde_json::from_str(&op_json).unwrap();
        assert_eq!(op_back, RevisionOp::Delete);
    }

    // -----------------------------------------------------------------------
    // Test 10 (bonus): revision_at returns None for nonexistent version
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_revision_at_nonexistent_returns_none() {
        let store = in_memory_store().await;
        let t = Utc::now();

        store
            .record_revision(&NewRevision::new(
                "proj-n",
                ArtifactKind::Suggestion,
                "sug-n",
                EditActor::User,
                RevisionOp::Create,
                r#"{}"#,
                t,
            ))
            .await
            .expect("record");

        let none = store
            .revision_at("proj-n", ArtifactKind::Suggestion, "sug-n", 99)
            .await
            .expect("revision_at ok");
        assert!(none.is_none());
    }

    // -----------------------------------------------------------------------
    // Test 11 (bonus): current_artifact returns None for logically deleted artifact
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_current_artifact_deleted_returns_none() {
        let store = in_memory_store().await;
        let t = Utc::now();

        store
            .record_revision(&NewRevision::new(
                "proj-d",
                ArtifactKind::Clarification,
                "cl-del",
                EditActor::User,
                RevisionOp::Create,
                r#"{"q":"why?"}"#,
                t,
            ))
            .await
            .expect("create");

        store
            .record_revision(&NewRevision::new(
                "proj-d",
                ArtifactKind::Clarification,
                "cl-del",
                EditActor::User,
                RevisionOp::Delete,
                "",
                t,
            ))
            .await
            .expect("delete");

        let result = store
            .current_artifact("proj-d", ArtifactKind::Clarification, "cl-del")
            .await
            .expect("current_artifact ok");

        assert!(result.is_none(), "deleted artifact must return None");
    }
}
