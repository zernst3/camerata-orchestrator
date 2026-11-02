//! The server-side decision store (review-tree design D7, D9, D10 and sections 3.5 and 3.6).
//!
//! A reviewer's decision on a site is not part of the sealed report and not part of the UI's
//! draft: it lives here, one file per project
//! (`<data_dir>/camerata/decisions/<project_id>.json`), beside the inspection store. The store
//! follows [`crate::inspections::InspectionStore`]'s pattern: an in-memory mirror behind a
//! mutex, written through atomically (temp file, then rename) while the lock is held, so a crash
//! leaves the old file or the new one and never half of each. A write that cannot be persisted
//! leaves the in-memory copy as it was and returns the error.
//!
//! # Records
//!
//! The store is append-only. A decision at a site is one record. A decision at a tree node is
//! the node record plus one per-site record per member, each carrying `inherited_from`, so every
//! site holds the decision and its reason in the file. Revoking a node decision revokes every
//! record it wrote. For a site, the LATEST record that is not revoked is its decision, so a
//! reviewer's later ruling on one site overrides an earlier batch, and revoking that ruling
//! brings the batch's back.
//!
//! # Reruns
//!
//! [`apply_decisions`] reads the store against a new run's tree. A decision applies to a site
//! only on an EXACT key match, and an inherited record only when its key is in the node
//! decision's `applies_to`. A member of a decided node that is not in `applies_to` is Unresolved
//! and carries the `new_since_decision` marker. A site that moved (the neighbourhood match
//! [`crate::review_tree::match_runs`] writes to `key_links.json`) shows its prior decision as
//! stale and stays Unresolved (D10): shown, never applied silently.
//!
//! # The release gate
//!
//! [`StoreDecisions`] answers the release gate's [`ReviewerDecisions`] question from the store.
//! The gate still reads the export request's dispositions; the swap is a later work order.

mod apply;
mod migrate;
pub(crate) mod routes;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

pub use apply::{
    apply_decisions, apply_to, effective_dispositions, read_key_links, AppliedDecisions,
    DispositionDisagreement, EffectiveDispositions, StoreDecisions,
};
pub use camerata_api_types::decisions::*;
pub use migrate::{
    disposition_state, legacy_finding_key, translate_draft, DraftTranslation, MIGRATED_BY,
};

/// On-disk schema version of one project's decision file.
const FILE_VERSION: u32 = 1;

/// Who a decision is recorded as made by when the request names no one.
pub const DEFAULT_DECIDER: &str = "reviewer";

/// What a write needs to stamp on the records it makes.
#[derive(Debug, Clone, Copy)]
pub struct DecisionCtx<'a> {
    /// The inspection (entry id) the decision is made against.
    pub run_id: &'a str,
    pub engine_version: &'a str,
    /// RFC3339 time of the write. Passed in so the store never reads the clock.
    pub now: &'a str,
}

/// Why a write to the store was refused.
#[derive(Debug, thiserror::Error)]
pub enum DecisionStoreError {
    #[error("{0}")]
    ReasonRequired(&'static str),
    #[error("a decision needs the key of the site it is about")]
    KeyRequired,
    #[error("merging a site into another needs the other site's key, and not its own")]
    MergeTargetInvalid,
    #[error("a merge is decided one site at a time, never for a whole group")]
    NodeMerge,
    #[error("there is no tree node {0}")]
    UnknownNode(String),
    #[error("the tree node {0} has no sites")]
    EmptyNode(String),
    #[error("there is no decision {0}")]
    NotFound(String),
    #[error("decision {0} is already revoked")]
    AlreadyRevoked(String),
    #[error("{0} was written by a group decision: revoke that decision instead")]
    InheritedRecord(String),
    #[error("the decisions could not be saved: {0}")]
    Persist(#[from] std::io::Error),
}

impl DecisionStoreError {
    /// The short machine-readable code the routes put in their error body.
    pub fn code(&self) -> &'static str {
        match self {
            Self::ReasonRequired(_) => "reason_required",
            Self::KeyRequired => "key_required",
            Self::MergeTargetInvalid => "merge_target_invalid",
            Self::NodeMerge => "merge_is_per_site",
            Self::UnknownNode(_) => "unknown_node",
            Self::EmptyNode(_) => "empty_node",
            Self::NotFound(_) => "not_found",
            Self::AlreadyRevoked(_) => "already_revoked",
            Self::InheritedRecord(_) => "revoke_the_group_decision",
            Self::Persist(_) => "persist_failed",
        }
    }
}

/// One project's records and whether the draft migration has run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ProjectDecisions {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    decisions: Vec<Decision>,
    /// When the UI draft's dispositions were migrated into this file (once, ever).
    #[serde(default)]
    migrated_from_draft_at: Option<String>,
}

/// File-backed store of decisions, one file per project. `Clone` is a shallow handle.
#[derive(Clone, Default)]
pub struct DecisionStore {
    /// `None` = in-memory only (tests, no resolvable data dir).
    dir: Option<Arc<PathBuf>>,
    mem: Arc<Mutex<BTreeMap<String, ProjectDecisions>>>,
}

impl DecisionStore {
    /// An empty, non-persisted store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Open `dir`, loading every project file in it, and persist every change back. A file that
    /// does not parse is moved aside to `<name>.corrupt` (never silently overwritten) and
    /// reported with `tracing::warn!`.
    pub fn at(dir: PathBuf) -> Self {
        let mut mem: BTreeMap<String, ProjectDecisions> = BTreeMap::new();
        if let Ok(read) = std::fs::read_dir(&dir) {
            let mut files: Vec<PathBuf> = read
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "json"))
                .collect();
            files.sort();
            for path in files {
                let parsed = std::fs::read_to_string(&path)
                    .map_err(|e| e.to_string())
                    .and_then(|s| {
                        serde_json::from_str::<StoredFile>(&s).map_err(|e| e.to_string())
                    });
                match parsed {
                    Ok(file) => {
                        mem.insert(file.project_id, file.data);
                    }
                    Err(e) => {
                        let aside = path.with_extension("json.corrupt");
                        tracing::warn!(
                            file = %path.display(),
                            error = %e,
                            moved_to = %aside.display(),
                            "decision file did not parse; moving it aside"
                        );
                        let _ = std::fs::rename(&path, &aside);
                    }
                }
            }
        }
        Self {
            dir: Some(Arc::new(dir)),
            mem: Arc::new(Mutex::new(mem)),
        }
    }

    /// The file one project's decisions are persisted in, if the store is file-backed. Named by
    /// the same injective encoding the inspection store uses.
    pub fn file_for(&self, project_id: &str) -> Option<PathBuf> {
        self.dir.as_ref().map(|d| {
            d.join(format!(
                "{}.json",
                crate::inspections::file_stem(project_id)
            ))
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, ProjectDecisions>> {
        self.mem
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Every record of a project (revoked ones too), in the order they were written.
    pub fn list(&self, project_id: &str) -> Vec<Decision> {
        self.lock()
            .get(project_id)
            .map(|p| p.decisions.clone())
            .unwrap_or_default()
    }

    /// The records that have not been revoked, in the order they were written.
    pub fn active(&self, project_id: &str) -> Vec<Decision> {
        self.lock()
            .get(project_id)
            .map(|p| {
                p.decisions
                    .iter()
                    .filter(|d| d.is_active())
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether the project has any record that is not revoked. The export request's own
    /// dispositions are ignored once it does.
    pub fn has_active_records(&self, project_id: &str) -> bool {
        self.lock()
            .get(project_id)
            .is_some_and(|p| p.decisions.iter().any(Decision::is_active))
    }

    /// When the draft migration ran for this project, if it has.
    pub fn migrated_from_draft_at(&self, project_id: &str) -> Option<String> {
        self.lock()
            .get(project_id)
            .and_then(|p| p.migrated_from_draft_at.clone())
    }

    /// Record one decision on one site. A site-scope Confirmed may carry no reason; every other
    /// decision needs one.
    pub fn record_site(
        &self,
        project_id: &str,
        req: &NewDecisionReq,
        ctx: DecisionCtx<'_>,
    ) -> Result<Decision, DecisionStoreError> {
        let key = req.key.trim();
        if key.is_empty() {
            return Err(DecisionStoreError::KeyRequired);
        }
        if let DecisionState::MergedInto(target) = &req.state {
            if target.trim().is_empty() || target == key {
                return Err(DecisionStoreError::MergeTargetInvalid);
            }
        }
        if req.state != DecisionState::Confirmed && req.reason.trim().is_empty() {
            return Err(DecisionStoreError::ReasonRequired(
                "a decision other than Confirm needs a reason",
            ));
        }
        self.mutate(project_id, |pd| {
            let id = next_top_level_id(&pd.decisions);
            let decision = Decision {
                id,
                key: key.to_string(),
                state: req.state.clone(),
                reason: req.reason.trim().to_string(),
                scope: Scope::Site,
                applies_to: Vec::new(),
                inherited_from: None,
                decided_by: decider(&req.decided_by),
                decided_at: ctx.now.to_string(),
                run_id: ctx.run_id.to_string(),
                engine_version: ctx.engine_version.to_string(),
                needed_me: req.needed_me,
                sample_verdict: req.sample_verdict.clone(),
                confirmed_by_client: None,
                revoked_at: None,
            };
            pd.decisions.push(decision.clone());
            Ok(decision)
        })
    }

    /// Record a decision at a tree node: the node record, and one per-site record for each of
    /// `members` (the node's site keys, sorted and distinct), each with `inherited_from`. A node
    /// decision always needs a reason, Confirm included. Returns the node record and how many
    /// per-site records it wrote.
    pub fn record_node(
        &self,
        project_id: &str,
        node_id: &str,
        members: &[String],
        req: &NewDecisionReq,
        ctx: DecisionCtx<'_>,
    ) -> Result<(Decision, usize), DecisionStoreError> {
        if matches!(req.state, DecisionState::MergedInto(_)) {
            return Err(DecisionStoreError::NodeMerge);
        }
        // A group decision carries a reason always, Confirm included (D9): a batch is the place a
        // wrong call hides the most rows, and the reason is what the reviewer can be held to.
        if req.reason.trim().is_empty() {
            return Err(DecisionStoreError::ReasonRequired(
                "a decision on a group always needs a reason, Confirm included",
            ));
        }
        if members.is_empty() {
            return Err(DecisionStoreError::EmptyNode(node_id.to_string()));
        }
        self.mutate(project_id, |pd| {
            let id = next_top_level_id(&pd.decisions);
            let reason = req.reason.trim().to_string();
            let decided_by = decider(&req.decided_by);
            let node = Decision {
                id: id.clone(),
                key: node_id.to_string(),
                state: req.state.clone(),
                reason: reason.clone(),
                scope: Scope::Node(node_id.to_string()),
                applies_to: members.to_vec(),
                inherited_from: None,
                decided_by: decided_by.clone(),
                decided_at: ctx.now.to_string(),
                run_id: ctx.run_id.to_string(),
                engine_version: ctx.engine_version.to_string(),
                needed_me: None,
                sample_verdict: None,
                confirmed_by_client: None,
                revoked_at: None,
            };
            pd.decisions.reserve(members.len() + 1);
            pd.decisions.push(node.clone());
            // One record per member, so every site holds the decision and its reason in the file.
            for (i, key) in members.iter().enumerate() {
                pd.decisions.push(Decision {
                    id: format!("{id}.{i:05}"),
                    key: key.clone(),
                    state: req.state.clone(),
                    reason: reason.clone(),
                    scope: Scope::Site,
                    applies_to: Vec::new(),
                    inherited_from: Some(id.clone()),
                    decided_by: decided_by.clone(),
                    decided_at: ctx.now.to_string(),
                    run_id: ctx.run_id.to_string(),
                    engine_version: ctx.engine_version.to_string(),
                    needed_me: None,
                    sample_verdict: None,
                    confirmed_by_client: None,
                    revoked_at: None,
                });
            }
            Ok((node, members.len()))
        })
    }

    /// Revoke a decision: stamp `revoked_at` on it, and on a node decision on every per-site
    /// record it wrote. Nothing is deleted. Returns the ids revoked. A per-site record written
    /// by a group decision cannot be revoked alone (rule on that site instead, or revoke the
    /// group decision).
    pub fn revoke(
        &self,
        project_id: &str,
        decision_id: &str,
        now: &str,
    ) -> Result<Vec<String>, DecisionStoreError> {
        self.mutate(project_id, |pd| {
            let Some(target) = pd.decisions.iter().find(|d| d.id == decision_id) else {
                return Err(DecisionStoreError::NotFound(decision_id.to_string()));
            };
            if target.inherited_from.is_some() {
                return Err(DecisionStoreError::InheritedRecord(decision_id.to_string()));
            }
            if target.revoked_at.is_some() {
                return Err(DecisionStoreError::AlreadyRevoked(decision_id.to_string()));
            }
            let mut revoked = Vec::new();
            for d in pd.decisions.iter_mut().filter(|d| {
                d.revoked_at.is_none()
                    && (d.id == decision_id || d.inherited_from.as_deref() == Some(decision_id))
            }) {
                d.revoked_at = Some(now.to_string());
                revoked.push(d.id.clone());
            }
            Ok(revoked)
        })
    }

    /// Write the records a draft migration produced and remember that it has run. Nothing is
    /// written, and `Ok(false)` returned, when the project was migrated before.
    pub fn record_migration(
        &self,
        project_id: &str,
        records: Vec<Decision>,
        now: &str,
    ) -> Result<bool, DecisionStoreError> {
        self.mutate(project_id, |pd| {
            if pd.migrated_from_draft_at.is_some() {
                return Ok(false);
            }
            pd.decisions.reserve(records.len());
            let first = top_level_count(&pd.decisions) + 1;
            for (i, mut record) in records.into_iter().enumerate() {
                record.id = format!("dec-{:06}", first + i);
                pd.decisions.push(record);
            }
            pd.migrated_from_draft_at = Some(now.to_string());
            Ok(true)
        })
    }

    /// Run `change` on a copy of the project's data, persist the copy, and only then make it the
    /// mirror. A failure to persist leaves the mirror untouched.
    fn mutate<T>(
        &self,
        project_id: &str,
        change: impl FnOnce(&mut ProjectDecisions) -> Result<T, DecisionStoreError>,
    ) -> Result<T, DecisionStoreError> {
        let mut guard = self.lock();
        let mut working = guard.get(project_id).cloned().unwrap_or_default();
        let out = change(&mut working)?;
        self.persist_locked(project_id, &working)?;
        guard.insert(project_id.to_string(), working);
        Ok(out)
    }

    /// Write one project's data atomically while the caller holds the lock.
    fn persist_locked(
        &self,
        project_id: &str,
        data: &ProjectDecisions,
    ) -> Result<(), std::io::Error> {
        let (Some(dir), Some(path)) = (self.dir.as_ref(), self.file_for(project_id)) else {
            return Ok(());
        };
        std::fs::create_dir_all(dir.as_path())?;
        let json = serde_json::to_vec(&StoredFileRef {
            project_id,
            data: &ProjectDecisions {
                version: FILE_VERSION,
                ..data.clone()
            },
        })
        .map_err(std::io::Error::other)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &path)
    }

    /// The directory the store persists into, if any.
    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref().map(PathBuf::as_path)
    }
}

/// The on-disk form: the project id is inside the file because the file name is an encoding.
#[derive(Deserialize)]
struct StoredFile {
    project_id: String,
    #[serde(flatten)]
    data: ProjectDecisions,
}

#[derive(Serialize)]
struct StoredFileRef<'a> {
    project_id: &'a str,
    #[serde(flatten)]
    data: &'a ProjectDecisions,
}

fn decider(by: &str) -> String {
    let by = by.trim();
    if by.is_empty() {
        DEFAULT_DECIDER.to_string()
    } else {
        by.to_string()
    }
}

/// `dec-000001`, `dec-000002`, ...: one more than the number of records that were not written by
/// a group decision. Records are never deleted, so an id is never reused.
fn next_top_level_id(decisions: &[Decision]) -> String {
    format!("dec-{:06}", top_level_count(decisions) + 1)
}

fn top_level_count(decisions: &[Decision]) -> usize {
    decisions
        .iter()
        .filter(|d| d.inherited_from.is_none())
        .count()
}
