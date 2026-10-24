//! The persisted inspection entry: everything one completed inspection produced, kept on disk
//! the moment it finishes.
//!
//! # Why this exists
//!
//! Until now the only copy of a completed scan was `AppState::last_scan`, an in-memory map. A
//! restart emptied it, and every export handler answers 404 "No scan results available" on an
//! empty map, so "regenerate the client package" quietly stopped working across a restart. That
//! is a client-visible defect today, and it is also the precondition for deleting client source:
//! once the checkout is gone, the stored inspection is the ONLY thing the exports, the review
//! screen and the package regeneration can be built from. See
//! `docs/design/2026-10-24_delete-and-refetch-client-source.md`, sections 2.1 and 6.3.
//!
//! # Shape
//!
//! One entry per completed inspection run, newest last, one FILE PER PROJECT
//! (`<data_dir>/camerata/inspections/<project_id>.json`) so a large report never rewrites
//! another project's file. V1 reads only the latest entry; V2's inspection-history view browses
//! the list. The store keeps an in-memory mirror (`Arc<Mutex<_>>`, same pattern as
//! `ProjectStore`) and writes through atomically (temp file, then rename) while holding the lock,
//! so a crash mid-write leaves either the old file or the new one, never half of each.
//!
//! The `report` inside an entry is the sealed, redacted report exactly as the exporters consume
//! it. Code excerpts inside its findings are part of the delivered product and stay (Zach,
//! 2026-10-22); they are why deletion's purge-completeness test distinguishes "kept" from "must
//! vanish".

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::llm::ModelSession;
use crate::onboard::{AuditedRef, ScanReport};

/// On-disk schema version of one project's inspection file.
const FILE_VERSION: u32 = 1;

/// Which kind of inspection produced an entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InspectionKind {
    /// A complete inspection of the tree as it stood.
    #[default]
    Full,
    /// A delta retest: only the files changed between two commits were re-audited.
    DeltaRetest {
        base_inspection_id: String,
        from_sha: String,
        to_sha: String,
    },
}

/// The client package generated from an inspection (V1 writes it from `export_product`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PackageRecord {
    /// RFC3339 UTC time the package was generated.
    pub generated_at: String,
    /// SHA-256 (lowercase hex) of the delivered zip bytes.
    pub zip_sha256: String,
    /// The package manifest as delivered (file names and hashes), kept so the package can be
    /// proven later without the zip.
    #[serde(default)]
    pub manifest_json: serde_json::Value,
}

/// Everything one inspection run produced. See the module docs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectionEntry {
    /// `insp-<project_id>-<started_at compact>-<4 hex>`.
    pub id: String,
    pub project_id: String,
    #[serde(default)]
    pub kind: InspectionKind,
    /// Copied from `report.provenance.started_at`.
    pub started_at: String,
    pub finished_at: String,
    /// `report.provenance.audited_refs`: per repo, the commit the inspection read.
    pub repos: Vec<AuditedRef>,
    pub rules_fingerprint: String,
    pub camerata_version: String,
    /// The sealed, redacted report exactly as the exporters consume it.
    pub report: ScanReport,
    /// Claude Code sessions this inspection's model calls created. Empty on the Api backend.
    #[serde(default)]
    pub model_sessions: Vec<ModelSession>,
    /// Set when this entry's recorded sessions were removed with a deleted checkout and their ids
    /// forgotten ([`InspectionStore::forget_model_sessions`]). An entry with no ids because they
    /// were cleared is not an entry that never recorded any.
    #[serde(default)]
    pub model_sessions_cleared: bool,
    /// Set when a client package was generated from this entry.
    #[serde(default)]
    pub package: Option<PackageRecord>,
}

impl InspectionEntry {
    /// Build an entry from a completed report. `id` is assigned by [`InspectionStore::append`]
    /// (it needs the existing list to stay unique); this leaves it empty.
    pub fn from_report(project_id: &str, report: ScanReport, kind: InspectionKind) -> Self {
        let p = &report.provenance;
        Self {
            id: String::new(),
            project_id: project_id.to_string(),
            kind,
            started_at: p.started_at.clone(),
            finished_at: p.finished_at.clone(),
            repos: p.audited_refs.clone(),
            rules_fingerprint: p.rules_fingerprint.clone(),
            camerata_version: p.camerata_version.clone(),
            model_sessions: p.model_sessions.clone(),
            model_sessions_cleared: false,
            package: None,
            report,
        }
    }

    /// Whether Camerata has no record of the Claude Code sessions this inspection created: it
    /// recorded none and none were cleared. True for every inspection made before session ids
    /// were recorded (and for one that made no model calls), whose session files cannot be found.
    pub fn sessions_unrecorded(&self) -> bool {
        self.model_sessions.is_empty() && !self.model_sessions_cleared
    }

    /// The commit the inspection read for `repo` (case-insensitive), if it was a git folder.
    pub fn sha_for(&self, repo: &str) -> Option<&str> {
        self.audited_ref(repo).and_then(|r| r.sha.as_deref())
    }

    /// The [`AuditedRef`] for `repo` (case-insensitive).
    pub fn audited_ref(&self, repo: &str) -> Option<&AuditedRef> {
        self.repos
            .iter()
            .find(|r| r.repo.eq_ignore_ascii_case(repo.trim()))
    }
}

/// The on-disk form of one project's file.
#[derive(Serialize, Deserialize)]
struct ProjectFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    entries: Vec<InspectionEntry>,
}

/// File-backed store of inspection entries, one file per project. `Clone` is a shallow handle.
#[derive(Clone, Default)]
pub struct InspectionStore {
    /// `None` = in-memory only (tests, no resolvable data dir).
    dir: Option<Arc<PathBuf>>,
    /// project id -> entries, oldest first.
    mem: Arc<Mutex<BTreeMap<String, Vec<InspectionEntry>>>>,
}

impl InspectionStore {
    /// An empty, non-persisted store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Open `dir`, loading every project file in it, and persist every change back. A file that
    /// does not parse is moved aside to `<name>.corrupt` (never silently overwritten) and
    /// reported with `tracing::warn!`.
    pub fn at(dir: PathBuf) -> Self {
        let mut mem: BTreeMap<String, Vec<InspectionEntry>> = BTreeMap::new();
        if let Ok(read) = std::fs::read_dir(&dir) {
            let mut files: Vec<PathBuf> = read
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "json"))
                .collect();
            files.sort();
            for path in files {
                match std::fs::read_to_string(&path)
                    .map_err(|e| e.to_string())
                    .and_then(|s| {
                        serde_json::from_str::<ProjectFile>(&s).map_err(|e| e.to_string())
                    }) {
                    Ok(file) => {
                        for entry in file.entries {
                            mem.entry(entry.project_id.clone()).or_default().push(entry);
                        }
                    }
                    Err(e) => {
                        let aside = path.with_extension("json.corrupt");
                        tracing::warn!(
                            file = %path.display(),
                            error = %e,
                            moved_to = %aside.display(),
                            "inspection file did not parse; moving it aside"
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

    /// The directory the store persists into, if any.
    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref().map(PathBuf::as_path)
    }

    /// The file one project's entries are persisted in, if the store is file-backed. The
    /// project id is encoded so any id is a safe single path component and distinct ids can
    /// never collide.
    pub fn file_for(&self, project_id: &str) -> Option<PathBuf> {
        self.dir
            .as_ref()
            .map(|d| d.join(format!("{}.json", file_stem(project_id))))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Vec<InspectionEntry>>> {
        self.mem
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Append a new entry for `entry.project_id`, assigning its unique id. Returns the stored
    /// entry. A persistence failure is returned as `Err` AFTER the in-memory copy is updated, so
    /// the running app keeps working; the caller logs it.
    pub fn append(&self, mut entry: InspectionEntry) -> Result<InspectionEntry, std::io::Error> {
        let mut guard = self.lock();
        let list = guard.entry(entry.project_id.clone()).or_default();
        entry.id = unique_id(&entry, list);
        list.push(entry.clone());
        let result = self.persist_locked(&entry.project_id, list);
        result.map(|()| entry)
    }

    /// The newest entry for a project.
    pub fn latest(&self, project_id: &str) -> Option<InspectionEntry> {
        self.lock().get(project_id).and_then(|l| l.last()).cloned()
    }

    /// Every entry for a project, newest first.
    pub fn list(&self, project_id: &str) -> Vec<InspectionEntry> {
        self.lock()
            .get(project_id)
            .map(|l| l.iter().rev().cloned().collect())
            .unwrap_or_default()
    }

    /// Replace the latest entry's report in place (an alternatives rescan merges into the
    /// current inspection rather than starting a new one). `Ok(false)` when there is no entry.
    pub fn update_latest_report(
        &self,
        project_id: &str,
        report: ScanReport,
    ) -> Result<bool, std::io::Error> {
        self.update_latest(project_id, |e| e.report = report)
    }

    /// Remember one more Claude Code session on the latest entry (an alternatives rescan makes
    /// model calls after the inspection finished). Skips an id already recorded.
    pub fn record_model_session(
        &self,
        project_id: &str,
        session: ModelSession,
    ) -> Result<bool, std::io::Error> {
        self.update_latest(project_id, |e| {
            if !e
                .model_sessions
                .iter()
                .any(|s| s.session_id == session.session_id)
            {
                e.model_sessions.push(session);
            }
        })
    }

    /// Record the client package generated from the latest entry.
    pub fn set_package(
        &self,
        project_id: &str,
        package: PackageRecord,
    ) -> Result<bool, std::io::Error> {
        self.update_latest(project_id, |e| e.package = Some(package))
    }

    fn update_latest(
        &self,
        project_id: &str,
        change: impl FnOnce(&mut InspectionEntry),
    ) -> Result<bool, std::io::Error> {
        let mut guard = self.lock();
        let Some(list) = guard.get_mut(project_id) else {
            return Ok(false);
        };
        let Some(last) = list.last_mut() else {
            return Ok(false);
        };
        change(last);
        self.persist_locked(project_id, list).map(|()| true)
    }

    /// Remove the sessions in `removed` from every entry's `model_sessions` (used after deletion
    /// so a second delete does not report files that are already gone), marking each entry that
    /// lost any as `model_sessions_cleared`. Entries and findings are untouched.
    pub fn forget_model_sessions(
        &self,
        project_id: &str,
        removed: &[String],
    ) -> Result<(), std::io::Error> {
        let mut guard = self.lock();
        let Some(list) = guard.get_mut(project_id) else {
            return Ok(());
        };
        for e in list.iter_mut() {
            let before = e.model_sessions.len();
            e.model_sessions
                .retain(|s| !removed.iter().any(|r| r == &s.session_id));
            // Remember that the ids were cleared, not never recorded: a later deletion must still
            // tell an entry that recorded its sessions from one that predates the recording.
            e.model_sessions_cleared |= e.model_sessions.len() < before;
        }
        self.persist_locked(project_id, list)
    }

    /// Write one project's list atomically while the caller holds the lock.
    fn persist_locked(
        &self,
        project_id: &str,
        list: &[InspectionEntry],
    ) -> Result<(), std::io::Error> {
        let (Some(dir), Some(path)) = (self.dir.as_ref(), self.file_for(project_id)) else {
            return Ok(());
        };
        std::fs::create_dir_all(dir.as_path())?;
        let json = serde_json::to_vec(&ProjectFileRef {
            version: FILE_VERSION,
            entries: list,
        })
        .map_err(std::io::Error::other)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &path)
    }
}

/// Borrowing twin of [`ProjectFile`] for writing without cloning the list.
#[derive(Serialize)]
struct ProjectFileRef<'a> {
    version: u32,
    entries: &'a [InspectionEntry],
}

/// A project id as a file-name stem: ASCII letters, digits, `-` and `_` pass through; every
/// other byte becomes `%XX`. Injective (`%` itself is encoded), so two ids never share a file.
pub fn file_stem(project_id: &str) -> String {
    let mut out = String::with_capacity(project_id.len());
    for b in project_id.bytes() {
        if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    if out.is_empty() {
        out.push_str("%00empty");
    }
    out
}

/// `insp-<project_id>-<started_at digits>-<4 hex>`, bumped until no entry in `existing` has it.
fn unique_id(entry: &InspectionEntry, existing: &[InspectionEntry]) -> String {
    let compact: String = entry
        .started_at
        .chars()
        .filter(char::is_ascii_digit)
        .collect();
    let compact = if compact.is_empty() {
        "0".to_string()
    } else {
        compact
    };
    let mut n = existing.len() as u64;
    loop {
        let seed = format!(
            "{}|{}|{}|{n}",
            entry.project_id, entry.started_at, entry.finished_at
        );
        let hex = format!("{:04x}", crate::suppression::fnv1a(&seed) & 0xffff);
        let id = format!("insp-{}-{compact}-{hex}", entry.project_id);
        if !existing.iter().any(|e| e.id == id) {
            return id;
        }
        n += 1;
    }
}

#[cfg(test)]
mod state_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// A minimal real report with one audited repo at `sha`.
    fn report(repo: &str, sha: &str, started: &str) -> ScanReport {
        let mut r = ScanReport::gated(&[repo.to_string()]);
        r.gated = false;
        r.provenance.audited_refs = vec![AuditedRef {
            repo: repo.to_string(),
            sha: Some(sha.to_string()),
            branch: Some("main".to_string()),
            dirty: false,
            ..AuditedRef::default()
        }];
        r.provenance.started_at = started.to_string();
        r.provenance.finished_at = started.to_string();
        r.provenance.rules_fingerprint = "fp".to_string();
        r.provenance.camerata_version = "9.9.9".to_string();
        r
    }

    fn entry(project: &str, sha: &str, started: &str) -> InspectionEntry {
        InspectionEntry::from_report(
            project,
            report("acme/app", sha, started),
            InspectionKind::Full,
        )
    }

    #[test]
    fn append_assigns_ids_and_latest_is_the_newest() {
        let store = InspectionStore::new();
        let a = store
            .append(entry("proj-1", "aaa", "2026-10-24T10:00:00Z"))
            .unwrap();
        let b = store
            .append(entry("proj-1", "bbb", "2026-10-24T11:00:00Z"))
            .unwrap();
        assert!(a.id.starts_with("insp-proj-1-20261024100000-"), "{}", a.id);
        assert_ne!(a.id, b.id);
        assert_eq!(
            store.latest("proj-1").unwrap().sha_for("acme/app"),
            Some("bbb")
        );
        let listed: Vec<String> = store.list("proj-1").into_iter().map(|e| e.id).collect();
        assert_eq!(listed, vec![b.id, a.id], "list is newest first");
        assert!(store.latest("proj-2").is_none());
    }

    #[test]
    fn identical_timestamps_still_get_distinct_ids() {
        let store = InspectionStore::new();
        let ids: std::collections::HashSet<String> = (0..50)
            .map(|_| {
                store
                    .append(entry("p", "s", "2026-10-24T10:00:00Z"))
                    .unwrap()
                    .id
            })
            .collect();
        assert_eq!(ids.len(), 50);
    }

    #[test]
    fn entries_survive_a_restart() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("inspections");
        let first = InspectionStore::at(dir.clone());
        let stored = first
            .append(entry("proj-1", "abc123", "2026-10-24T10:00:00Z"))
            .unwrap();
        drop(first);

        let second = InspectionStore::at(dir);
        let got = second.latest("proj-1").expect("entry reloaded");
        assert_eq!(got.id, stored.id);
        assert_eq!(got.sha_for("ACME/APP"), Some("abc123"), "case-insensitive");
        assert_eq!(got.camerata_version, "9.9.9");
    }

    #[test]
    fn each_project_has_its_own_file_and_one_does_not_rewrite_the_other() {
        let tmp = TempDir::new().unwrap();
        let store = InspectionStore::at(tmp.path().to_path_buf());
        store
            .append(entry("proj-a", "1", "2026-10-24T10:00:00Z"))
            .unwrap();
        let a_file = store.file_for("proj-a").unwrap();
        let before = std::fs::read(&a_file).unwrap();
        store
            .append(entry("proj-b", "2", "2026-10-24T10:00:00Z"))
            .unwrap();
        assert_eq!(std::fs::read(&a_file).unwrap(), before);
        assert!(store.file_for("proj-b").unwrap().is_file());
    }

    #[test]
    fn hostile_project_ids_cannot_escape_the_directory_or_collide() {
        assert_eq!(file_stem("proj-3"), "proj-3");
        assert_eq!(file_stem("../x"), "%2E%2E%2Fx");
        assert_ne!(file_stem("a/b"), file_stem("a%2Fb"));
        assert_eq!(file_stem(""), "%00empty");
        let store = InspectionStore::at(PathBuf::from("/data/inspections"));
        let f = store.file_for("../../etc/passwd").unwrap();
        assert_eq!(f.parent().unwrap(), Path::new("/data/inspections"));
    }

    #[test]
    fn a_corrupt_file_is_moved_aside_not_overwritten() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("proj-1.json");
        std::fs::write(&path, "{ this is not json").unwrap();
        let store = InspectionStore::at(tmp.path().to_path_buf());
        assert!(store.latest("proj-1").is_none());
        assert!(!path.exists());
        assert!(tmp.path().join("proj-1.json.corrupt").is_file());
        // And the store still works.
        store
            .append(entry("proj-1", "x", "2026-10-24T10:00:00Z"))
            .unwrap();
        assert!(path.is_file());
    }

    #[test]
    fn update_latest_report_changes_the_newest_entry_only() {
        let store = InspectionStore::new();
        store
            .append(entry("p", "old", "2026-10-24T10:00:00Z"))
            .unwrap();
        store
            .append(entry("p", "new", "2026-10-24T11:00:00Z"))
            .unwrap();
        let mut merged = report("acme/app", "new", "2026-10-24T11:00:00Z");
        merged.message = Some("merged".to_string());
        assert!(store.update_latest_report("p", merged).unwrap());
        let all = store.list("p");
        assert_eq!(all[0].report.message.as_deref(), Some("merged"));
        assert_ne!(all[1].report.message.as_deref(), Some("merged"));
        assert!(!store
            .update_latest_report("nobody", report("a/b", "s", "t"))
            .unwrap());
    }

    #[test]
    fn model_sessions_and_package_are_recorded_on_the_latest_entry() {
        let tmp = TempDir::new().unwrap();
        let store = InspectionStore::at(tmp.path().to_path_buf());
        store
            .append(entry("p", "s", "2026-10-24T10:00:00Z"))
            .unwrap();
        let session = ModelSession {
            session_id: "sid-1".into(),
            cwd: "/x".into(),
            created_at: "2026-10-24T10:01:00Z".into(),
        };
        assert!(store.record_model_session("p", session.clone()).unwrap());
        assert!(
            store.record_model_session("p", session).unwrap(),
            "recording the same id twice is not an error"
        );
        assert!(store
            .set_package(
                "p",
                PackageRecord {
                    generated_at: "t".into(),
                    zip_sha256: "ab".into(),
                    manifest_json: serde_json::json!({"files": []}),
                },
            )
            .unwrap());

        let reopened = InspectionStore::at(tmp.path().to_path_buf());
        let e = reopened.latest("p").unwrap();
        assert_eq!(e.model_sessions.len(), 1, "no duplicate session id");
        assert_eq!(e.package.unwrap().zip_sha256, "ab");
    }

    #[test]
    fn forget_model_sessions_removes_only_the_named_ids() {
        let store = InspectionStore::new();
        let mut e = entry("p", "s", "2026-10-24T10:00:00Z");
        e.model_sessions = vec![
            ModelSession {
                session_id: "keep".into(),
                ..ModelSession::default()
            },
            ModelSession {
                session_id: "drop".into(),
                ..ModelSession::default()
            },
        ];
        store.append(e).unwrap();
        store
            .forget_model_sessions("p", &["drop".to_string()])
            .unwrap();
        let ids: Vec<String> = store
            .latest("p")
            .unwrap()
            .model_sessions
            .into_iter()
            .map(|s| s.session_id)
            .collect();
        assert_eq!(ids, ["keep"]);
    }

    #[test]
    fn the_kind_round_trips_with_its_shas() {
        let tmp = TempDir::new().unwrap();
        let store = InspectionStore::at(tmp.path().to_path_buf());
        let mut e = entry("p", "s", "2026-10-24T10:00:00Z");
        e.kind = InspectionKind::DeltaRetest {
            base_inspection_id: "insp-base".into(),
            from_sha: "aaa".into(),
            to_sha: "bbb".into(),
        };
        store.append(e).unwrap();
        let back = InspectionStore::at(tmp.path().to_path_buf())
            .latest("p")
            .unwrap();
        assert_eq!(
            back.kind,
            InspectionKind::DeltaRetest {
                base_inspection_id: "insp-base".into(),
                from_sha: "aaa".into(),
                to_sha: "bbb".into(),
            }
        );
    }
}
