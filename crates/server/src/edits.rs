//! The server-side edit store (review-tree design section 5): a reviewer's changes to a finding's
//! text and fields, the findings they add, and the sites they move to another sub-group.
//!
//! Like the decision store ([`crate::decisions::DecisionStore`]), whose pattern it follows, an
//! edit is not part of the sealed report: it lives here, one file per project
//! (`<data_dir>/camerata/edits/<project_id>.json`), applied as an overlay at read time and at
//! export ([`crate::report_export::apply_overlay`]). The sealed report never changes. The store
//! is an in-memory mirror behind a mutex, written through atomically (temp file, then rename)
//! while the lock is held. A write that cannot be persisted leaves the mirror as it was and
//! returns the error.
//!
//! # Records
//!
//! The store is append-only. Two edits on one field of one site are both kept, in the order they
//! were written, and the LATER one applies. A discarded edit is stamped, never deleted.
//!
//! # Stale edits
//!
//! An edit names a site by its key and carries the fingerprint of the row's rule and snippet at
//! the time. On a rerun the overlay applies it only to a row whose key and fingerprint still
//! match ([`crate::report_export::apply_overlay`]); otherwise the edit is stale: shown with the
//! value it was made against, never applied silently. A reviewer reapplies it to the site it
//! moved to ([`EditStore::reapply`]) or discards it ([`EditStore::discard`]).
//!
//! # A damaged file
//!
//! A project file that does not parse is moved aside to `<name>.corrupt` and never replaced. The
//! project's edits are UNKNOWN, not empty: every read and write for it is refused (the routes
//! answer 503 `store_corrupt`) until the file is restored and the app restarted.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

pub use camerata_api_types::edits::*;

use crate::onboard::Finding;

/// On-disk schema version of one project's edit file.
const FILE_VERSION: u32 = 1;

/// Who an edit is recorded as made by when the request names no one.
pub const DEFAULT_EDITOR: &str = "reviewer";

/// The values the severity field takes, as the exports write them.
pub const SEVERITIES: [&str; 5] = ["critical", "high", "medium", "low", "info"];

/// A finding a reviewer added because the engine missed it, with who added it and why.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AddedFinding {
    pub finding: Finding,
    pub reason: String,
    pub by: String,
    pub at: String,
    /// The inspection the finding was added to.
    #[serde(default)]
    pub run_id: String,
}

/// What a write needs to stamp on the records it makes.
#[derive(Debug, Clone, Copy)]
pub struct EditCtx<'a> {
    /// The inspection (entry id) the change is made against.
    pub run_id: &'a str,
    /// RFC3339 time of the write. Passed in so the store never reads the clock.
    pub now: &'a str,
}

/// One edit as the route works it out from the row it is about: the value the field has now and
/// the row's fingerprint.
#[derive(Debug, Clone)]
pub struct EditDraft {
    pub key: String,
    pub field: EditField,
    pub old: String,
    pub new: String,
    pub reason: String,
    pub by: String,
    /// The inspection the reviewer was looking at; blank means [`EditCtx::run_id`].
    pub run_id: String,
    pub fingerprint: String,
}

/// Why a write to the store was refused.
#[derive(Debug, thiserror::Error)]
pub enum EditStoreError {
    #[error("{0}")]
    ReasonRequired(&'static str),
    #[error("an edit needs the key of the site it is about")]
    KeyRequired,
    #[error("{0}")]
    InvalidValue(String),
    #[error("the new value is the one the field already has")]
    NoChange,
    #[error("a finding the reviewer adds needs {0}")]
    AddedFindingIncomplete(&'static str),
    #[error("there is no edit {0}")]
    NotFound(String),
    #[error("edit {0} is already discarded")]
    AlreadyDiscarded(String),
    #[error("the edit file for this project is damaged and was moved aside; nothing was changed")]
    Corrupt,
    #[error("the edits could not be saved: {0}")]
    Persist(#[from] std::io::Error),
}

impl EditStoreError {
    /// The short machine-readable code the routes put in their error body.
    pub fn code(&self) -> &'static str {
        match self {
            Self::ReasonRequired(_) => "reason_required",
            Self::KeyRequired => "key_required",
            Self::InvalidValue(_) => "invalid_value",
            Self::NoChange => "no_change",
            Self::AddedFindingIncomplete(_) => "added_finding_incomplete",
            Self::NotFound(_) => "not_found",
            Self::AlreadyDiscarded(_) => "already_discarded",
            Self::Corrupt => "store_corrupt",
            Self::Persist(_) => "persist_failed",
        }
    }
}

/// One project's records.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ProjectEdits {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    edits: Vec<Edit>,
    #[serde(default)]
    added: Vec<AddedFinding>,
}

/// File-backed store of edits and added findings, one file per project. `Clone` is a shallow
/// handle.
#[derive(Clone, Default)]
pub struct EditStore {
    /// `None` = in-memory only (tests, no resolvable data dir).
    dir: Option<Arc<PathBuf>>,
    mem: Arc<Mutex<BTreeMap<String, ProjectEdits>>>,
    /// File stems of project files that did not parse when the store opened (see the module
    /// doc). Never auto-created over.
    corrupt: Arc<BTreeSet<String>>,
}

impl EditStore {
    /// An empty, non-persisted store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Open `dir`, loading every project file in it, and persist every change back. A file that
    /// does not parse is moved aside to `<name>.json.corrupt` (never silently overwritten) and
    /// reported with `tracing::warn!`.
    pub fn at(dir: PathBuf) -> Self {
        let mut mem: BTreeMap<String, ProjectEdits> = BTreeMap::new();
        let mut corrupt: BTreeSet<String> = BTreeSet::new();
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
                            "edit file did not parse; moving it aside"
                        );
                        let _ = std::fs::rename(&path, &aside);
                        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                            corrupt.insert(stem.to_string());
                        }
                    }
                }
            }
        }
        Self {
            dir: Some(Arc::new(dir)),
            mem: Arc::new(Mutex::new(mem)),
            corrupt: Arc::new(corrupt),
        }
    }

    /// The file one project's edits are persisted in, if the store is file-backed.
    pub fn file_for(&self, project_id: &str) -> Option<PathBuf> {
        self.dir.as_ref().map(|d| {
            d.join(format!(
                "{}.json",
                crate::inspections::file_stem(project_id)
            ))
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, ProjectEdits>> {
        self.mem
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Whether this project's edit file was damaged when the store opened.
    pub fn is_corrupt(&self, project_id: &str) -> bool {
        self.corrupt
            .contains(&crate::inspections::file_stem(project_id))
    }

    /// The directory the store persists into, if any.
    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref().map(PathBuf::as_path)
    }

    /// Every edit of a project (discarded ones too), in the order they were written.
    pub fn list(&self, project_id: &str) -> Vec<Edit> {
        self.lock()
            .get(project_id)
            .map(|p| p.edits.clone())
            .unwrap_or_default()
    }

    /// The edits that have not been discarded, in the order they were written. These are the
    /// ones the overlay applies (and calls stale when their site changed).
    pub fn active(&self, project_id: &str) -> Vec<Edit> {
        self.lock()
            .get(project_id)
            .map(|p| p.edits.iter().filter(|e| e.is_active()).cloned().collect())
            .unwrap_or_default()
    }

    /// The findings reviewers added, in the order they were added.
    pub fn added(&self, project_id: &str) -> Vec<AddedFinding> {
        self.lock()
            .get(project_id)
            .map(|p| p.added.clone())
            .unwrap_or_default()
    }

    /// Record one edit. Appends; an earlier edit on the same field is kept.
    pub fn record_edit(
        &self,
        project_id: &str,
        draft: EditDraft,
        ctx: EditCtx<'_>,
    ) -> Result<Edit, EditStoreError> {
        validate_draft(&draft)?;
        self.mutate(project_id, |pd| {
            let edit = make_edit(&pd.edits, draft, ctx, None);
            pd.edits.push(edit.clone());
            Ok(edit)
        })
    }

    /// Record a split: one Subgroup edit per site, all moving to `subgroup`, all with `reason`,
    /// written together or not at all. `drafts` carry each site's key, current sub-group id and
    /// fingerprint.
    pub fn record_split(
        &self,
        project_id: &str,
        drafts: Vec<EditDraft>,
        ctx: EditCtx<'_>,
    ) -> Result<Vec<Edit>, EditStoreError> {
        for d in &drafts {
            validate_draft(d)?;
        }
        self.mutate(project_id, |pd| {
            let mut written = Vec::with_capacity(drafts.len());
            for draft in drafts {
                let edit = make_edit(&pd.edits, draft, ctx, None);
                pd.edits.push(edit.clone());
                written.push(edit);
            }
            Ok(written)
        })
    }

    /// Discard an edit: stamp `discarded_at`. Nothing is deleted. Returns the edit as stamped.
    pub fn discard(
        &self,
        project_id: &str,
        edit_id: &str,
        now: &str,
    ) -> Result<Edit, EditStoreError> {
        self.mutate(project_id, |pd| {
            let Some(edit) = pd.edits.iter_mut().find(|e| e.id == edit_id) else {
                return Err(EditStoreError::NotFound(edit_id.to_string()));
            };
            if edit.discarded_at.is_some() {
                return Err(EditStoreError::AlreadyDiscarded(edit_id.to_string()));
            }
            edit.discarded_at = Some(now.to_string());
            Ok(edit.clone())
        })
    }

    /// Reapply a stale edit as a new edit on the site it now belongs to: the same field, new
    /// value and reason, with the site's current key, value and fingerprint. The stale edit is
    /// stamped discarded (it is replaced, not deleted).
    pub fn reapply(
        &self,
        project_id: &str,
        edit_id: &str,
        target: EditDraft,
        ctx: EditCtx<'_>,
    ) -> Result<Edit, EditStoreError> {
        validate_draft(&target)?;
        self.mutate(project_id, |pd| {
            let Some(old) = pd.edits.iter_mut().find(|e| e.id == edit_id) else {
                return Err(EditStoreError::NotFound(edit_id.to_string()));
            };
            if old.discarded_at.is_some() {
                return Err(EditStoreError::AlreadyDiscarded(edit_id.to_string()));
            }
            old.discarded_at = Some(ctx.now.to_string());
            let from = old.id.clone();
            let edit = make_edit(&pd.edits, target, ctx, Some(from));
            pd.edits.push(edit.clone());
            Ok(edit)
        })
    }

    /// Record a finding the reviewer added. The caller has already given it its site key, its
    /// `provenance: reviewer` mark and its evidence check. A finding with the same site key as
    /// one already added is refused as a duplicate.
    pub fn add_finding(
        &self,
        project_id: &str,
        added: AddedFinding,
    ) -> Result<AddedFinding, EditStoreError> {
        validate_added(&added)?;
        self.mutate(project_id, |pd| {
            if pd
                .added
                .iter()
                .any(|a| a.finding.site_key == added.finding.site_key)
            {
                return Err(EditStoreError::InvalidValue(
                    "this finding was already added".to_string(),
                ));
            }
            pd.added.push(added.clone());
            Ok(added)
        })
    }

    /// Run `change` on a copy of the project's data, persist the copy, and only then make it the
    /// mirror. A failure to persist leaves the mirror untouched.
    fn mutate<T>(
        &self,
        project_id: &str,
        change: impl FnOnce(&mut ProjectEdits) -> Result<T, EditStoreError>,
    ) -> Result<T, EditStoreError> {
        if self.is_corrupt(project_id) {
            return Err(EditStoreError::Corrupt);
        }
        let mut guard = self.lock();
        let mut working = guard.get(project_id).cloned().unwrap_or_default();
        let out = change(&mut working)?;
        self.persist_locked(project_id, &working)?;
        guard.insert(project_id.to_string(), working);
        Ok(out)
    }

    /// Write one project's data atomically while the caller holds the lock.
    fn persist_locked(&self, project_id: &str, data: &ProjectEdits) -> Result<(), std::io::Error> {
        let (Some(dir), Some(path)) = (self.dir.as_ref(), self.file_for(project_id)) else {
            return Ok(());
        };
        std::fs::create_dir_all(dir.as_path())?;
        let json = serde_json::to_vec(&StoredFileRef {
            project_id,
            data: &ProjectEdits {
                version: FILE_VERSION,
                ..data.clone()
            },
        })
        .map_err(std::io::Error::other)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &path)
    }
}

/// What the evidence check said about an added finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceOutcome {
    /// The snippet was found near the cited line (or the row is of a kind the check does not
    /// apply to).
    pub verified: bool,
    /// Why not, in words; blank when verified.
    pub note: String,
}

/// Build the finding a reviewer adds from their request and run it through the evidence check
/// like any row (M5): the snippet must be found near the line in `files` (the repo's scanned
/// files) or under `root`. A snippet that is not found does not refuse the finding: it is held
/// (the row carries the `evidence_unverified` mark), and the release gate refuses it until the
/// reviewer decides it. `existing` are the rows of the report it joins, so its site key is the
/// normal one with the lowest free ordinal.
pub fn added_finding_from_request(
    req: &AddFindingReq,
    existing: &[Finding],
    files: &[(String, String)],
    root: Option<&Path>,
    ctx: EditCtx<'_>,
) -> Result<(AddedFinding, EvidenceOutcome), EditStoreError> {
    let headline = req
        .headline
        .trim()
        .replace(crate::ai_audit::HEADLINE_SEP, " - ");
    let mut f = Finding {
        repo: req.repo.trim().to_string(),
        path: req.path.trim().to_string(),
        line: req.line,
        rule_id: req.rule_id.trim().to_string(),
        severity: req.severity.trim().to_ascii_lowercase(),
        snippet: req.snippet.trim().to_string(),
        detail: if headline.is_empty() {
            req.detail.trim().to_string()
        } else {
            format!(
                "{headline}{}{}",
                crate::ai_audit::HEADLINE_SEP,
                req.detail.trim()
            )
        },
        fix_specific: Some(req.fix.trim().to_string()).filter(|s| !s.is_empty()),
        kind: req.kind.clone(),
        category: req
            .category
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .map(str::to_string),
        ..Finding::default()
    };
    f.captures.insert(
        crate::report_export::overlay::PROVENANCE_CAPTURE.to_string(),
        crate::report_export::overlay::REVIEWER_PROVENANCE.to_string(),
    );
    let by = req.by.trim();
    let added = AddedFinding {
        finding: f.clone(),
        reason: req.reason.clone(),
        by: if by.is_empty() {
            DEFAULT_EDITOR.to_string()
        } else {
            by.to_string()
        },
        at: ctx.now.to_string(),
        run_id: ctx.run_id.to_string(),
    };
    validate_added(&added)?;
    f.site_key = crate::report_export::key_for_added(existing, &f);
    crate::onboard::evidence_check::verify_evidence(std::slice::from_mut(&mut f), files, root);
    let note = f
        .captures
        .get(crate::onboard::evidence_check::EVIDENCE_UNVERIFIED_CAPTURE)
        .cloned()
        .unwrap_or_default();
    Ok((
        AddedFinding {
            finding: f,
            ..added
        },
        EvidenceOutcome {
            verified: note.is_empty(),
            note,
        },
    ))
}

/// The on-disk form: the project id is inside the file because the file name is an encoding.
#[derive(Deserialize)]
struct StoredFile {
    project_id: String,
    #[serde(flatten)]
    data: ProjectEdits,
}

#[derive(Serialize)]
struct StoredFileRef<'a> {
    project_id: &'a str,
    #[serde(flatten)]
    data: &'a ProjectEdits,
}

/// `edit-000001`, `edit-000002`, ...: one more than the number of edits ever written. Edits are
/// never deleted, so an id is never reused.
fn next_edit_id(edits: &[Edit]) -> String {
    format!("edit-{:06}", edits.len() + 1)
}

fn make_edit(existing: &[Edit], draft: EditDraft, ctx: EditCtx<'_>, from: Option<String>) -> Edit {
    let by = draft.by.trim();
    Edit {
        id: next_edit_id(existing),
        key: draft.key.trim().to_string(),
        field: draft.field,
        old: draft.old,
        new: draft.new,
        reason: draft.reason.trim().to_string(),
        by: if by.is_empty() {
            DEFAULT_EDITOR.to_string()
        } else {
            by.to_string()
        },
        at: ctx.now.to_string(),
        run_id: if draft.run_id.trim().is_empty() {
            ctx.run_id.to_string()
        } else {
            draft.run_id.trim().to_string()
        },
        fingerprint_at_edit: draft.fingerprint,
        stale: false,
        discarded_at: None,
        reapplied_from: from,
    }
}

/// Whether a field's change is a claim beyond the wording that the reviewer must justify. A
/// reviewer rewording a headline owes no reason; one changing the severity, the rule, the kind,
/// the category or the sub-group does.
pub fn field_needs_reason(field: EditField) -> bool {
    matches!(
        field,
        EditField::Severity
            | EditField::RuleId
            | EditField::Kind
            | EditField::Category
            | EditField::Subgroup
    )
}

fn validate_draft(d: &EditDraft) -> Result<(), EditStoreError> {
    if d.key.trim().is_empty() {
        return Err(EditStoreError::KeyRequired);
    }
    if field_needs_reason(d.field) && d.reason.trim().is_empty() {
        return Err(EditStoreError::ReasonRequired(
            "changing the severity, rule, kind, category or sub-group of a finding needs a reason",
        ));
    }
    validate_value(d.field, &d.new)?;
    if d.old == d.new {
        return Err(EditStoreError::NoChange);
    }
    Ok(())
}

/// Whether `new` is a value `field` can take.
pub fn validate_value(field: EditField, new: &str) -> Result<(), EditStoreError> {
    let bad = |why: &str| Err(EditStoreError::InvalidValue(why.to_string()));
    let new = new.trim();
    match field {
        EditField::Headline | EditField::Detail | EditField::Fix | EditField::RuleId => {
            if new.is_empty() {
                return bad("the new value cannot be empty");
            }
        }
        EditField::Subgroup => {
            if new.is_empty() {
                return bad("a sub-group needs a name");
            }
        }
        EditField::Severity => {
            if !SEVERITIES.contains(&new.to_ascii_lowercase().as_str()) {
                return bad("the severity must be critical, high, medium, low or info");
            }
        }
        EditField::FixType => {
            if new != crate::report_export::TAILORED_FIX_LABEL
                && new != crate::report_export::STANDARD_FIX_LABEL
            {
                return bad("the fix type must be Tailored fix or Standard fix");
            }
        }
        EditField::Kind => {
            if parse_kind(new).is_none() {
                return bad(
                    "the kind must be defect, or a kind written as JSON with its details (for \
                     example {\"kind\":\"defect\"})",
                );
            }
        }
        // A category is a free label; blank clears it.
        EditField::Category => {}
    }
    Ok(())
}

/// The finding kind an edit's new value names: the tag `defect`, or the kind as JSON
/// (`{"kind":"defect"}`, `{"kind":"deviation",...}`). A tag whose kind carries details cannot be
/// written as a bare tag.
pub fn parse_kind(new: &str) -> Option<camerata_api_types::audit::FindingKind> {
    let new = new.trim();
    if new.starts_with('{') {
        return serde_json::from_str(new).ok();
    }
    serde_json::from_value(serde_json::json!({ "kind": new })).ok()
}

fn validate_added(a: &AddedFinding) -> Result<(), EditStoreError> {
    let f = &a.finding;
    if f.repo.trim().is_empty() || f.path.trim().is_empty() {
        return Err(EditStoreError::AddedFindingIncomplete(
            "a repo and a file path",
        ));
    }
    if f.rule_id.trim().is_empty() {
        return Err(EditStoreError::AddedFindingIncomplete("a rule id"));
    }
    if f.snippet.trim().is_empty() {
        return Err(EditStoreError::AddedFindingIncomplete(
            "the offending code quoted as its snippet",
        ));
    }
    if f.detail.trim().is_empty() {
        return Err(EditStoreError::AddedFindingIncomplete(
            "a headline and an explanation",
        ));
    }
    if f.kind.is_none() {
        return Err(EditStoreError::AddedFindingIncomplete(
            "a kind (what the row is)",
        ));
    }
    if !SEVERITIES.contains(&f.severity.trim().to_ascii_lowercase().as_str()) {
        return Err(EditStoreError::AddedFindingIncomplete(
            "a severity (critical, high, medium, low or info)",
        ));
    }
    if a.reason.trim().is_empty() {
        return Err(EditStoreError::ReasonRequired(
            "adding a finding needs a reason (why the engine missed it)",
        ));
    }
    Ok(())
}

pub(crate) mod routes;
#[cfg(test)]
mod tests;
