//! The deletion record (the proof handed to a client) and the re-fetch record, plus the
//! machine-local store that keeps them.
//!
//! Records are NOT part of the project export (`export_project` is config-only by decision
//! `2026-06-21_project_config_vs_data_separation.md`); the deletion record reaches the client as
//! plain text through [`DeletionRecord::render_text`].
//!
//! # Ordering without a clock
//!
//! "Is this repo deleted or was it fetched back afterwards?" is decided by which record is
//! newer. Wall-clock seconds are not a safe tiebreak (a delete and an immediate re-fetch land in
//! the same second), so every record is stamped with a per-project, strictly increasing `seq` by
//! the store, and the newest record is the one with the highest `seq`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

/// What kind of location one purge item is. Order in a record follows the purge order of the
/// design (section 5): worktrees, the checkout, the server scan cache, the CLI cache and ledger,
/// model session files, then in-memory copies.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PurgedKind {
    Checkout,
    Worktree,
    ServerScanCache,
    CliScanCache,
    CliLedger,
    ModelSessionFile,
    InMemory,
}

impl PurgedKind {
    /// The label used in the rendered record.
    pub fn label(self) -> &'static str {
        match self {
            PurgedKind::Checkout => "checkout",
            PurgedKind::Worktree => "worktree",
            PurgedKind::ServerScanCache => "scan cache entry",
            PurgedKind::CliScanCache => "CLI scan cache",
            PurgedKind::CliLedger => "CLI ledger",
            PurgedKind::ModelSessionFile => "model session file",
            PurgedKind::InMemory => "in-memory copy",
        }
    }
}

/// What happened to one purge item.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum PurgeOutcome {
    /// Shown by `delete-preview`; nothing has been removed.
    Planned,
    Removed,
    /// It was not there when the purge reached it (already gone, or never created).
    AlreadyAbsent,
    Failed {
        error: String,
    },
}

impl PurgeOutcome {
    /// `true` for `Removed` and `AlreadyAbsent`: the location holds no client data any more.
    pub fn is_clear(&self) -> bool {
        matches!(self, PurgeOutcome::Removed | PurgeOutcome::AlreadyAbsent)
    }
}

/// One location the purge covered (or, in a preview, will cover).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PurgedItem {
    pub kind: PurgedKind,
    /// Exact path, or `scan-cache.json#<project_id>/<repo>` for the entry inside the server's
    /// scan cache file.
    pub path: String,
    /// Files counted BEFORE removal (a recursive walk, `.git/` included); 0 for an in-file entry.
    pub files: u64,
    pub bytes: u64,
    #[serde(flatten)]
    pub outcome: PurgeOutcome,
}

impl PurgedItem {
    /// What the item held, in the words the record and the confirmation dialog share:
    /// `1,204 files, 12.3 MB`, a bare size for a single file, `-` for an in-file entry.
    pub fn counts_text(&self) -> String {
        let size = format_bytes(self.bytes);
        if self.files > 0 {
            format!("{} files, {size}", group_thousands(self.files))
        } else if self.bytes > 0 {
            size
        } else {
            "-".to_string()
        }
    }
}

/// Proof that a repository's source was removed from this machine. See the design, section 2.3.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeletionRecord {
    /// `del-<project_id>-<repo sanitised>-<completed_at compact>`.
    pub id: String,
    /// Per-project strictly increasing sequence; assigned by the store.
    #[serde(default)]
    pub seq: u64,
    pub project_id: String,
    /// `owner/repo`.
    pub repo: String,
    /// Observed at deletion, before removal; credentials scrubbed.
    pub origin_url: Option<String>,
    /// The folder that was removed (canonical).
    pub path: String,
    pub inspected_sha: Option<String>,
    pub inspection_id: Option<String>,
    pub head_sha_at_deletion: Option<String>,
    /// RFC3339 UTC.
    pub requested_at: String,
    pub completed_at: String,
    /// The text the operator typed (always equal to `repo`).
    pub confirmed_with: String,
    pub include_uncommitted: bool,
    pub items: Vec<PurgedItem>,
    pub total_files: u64,
    pub total_bytes: u64,
    /// The fixed method sentence ([`METHOD_SENTENCE`]).
    pub method: String,
    /// Fixed sentences for what deletion does NOT cover.
    pub residuals: Vec<String>,
    pub camerata_version: String,
    /// The machine's host name, for the client's record.
    pub machine: String,
}

/// A successful re-fetch, appended so the deleted state can be told from a later fetch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RefetchRecord {
    #[serde(default)]
    pub seq: u64,
    pub repo: String,
    pub path: String,
    /// HEAD after the fetch.
    pub sha: Option<String>,
    /// `latest` or `commit:<sha>`.
    pub target: String,
    pub completed_at: String,
}

/// The fixed method sentence every record carries.
pub const METHOD_SENTENCE: &str = "Ordinary file removal (std::fs::remove_dir_all) on a \
FileVault-encrypted APFS volume. Not a forensic wipe; no overwrite pass.";

/// Residual sentence: operator backups are outside the record (always present).
pub const RESIDUAL_BACKUPS: &str =
    "Backups made by the operator's own tools (Time Machine or similar) are outside this record.";
/// Residual sentence: finding excerpts are kept (always present).
pub const RESIDUAL_EXCERPTS: &str =
    "Code excerpts inside the inspection findings are kept as part of the delivered report.";
/// Residual sentence: present when model sessions were recorded.
pub const RESIDUAL_MODEL_SESSIONS: &str = "This repository was inspected on the Cli backend; \
Claude Code session files recorded by Camerata were removed, any session not recorded by \
Camerata was not.";
/// Residual sentence: present when the operator ticked the uncommitted-work box.
pub const RESIDUAL_UNCOMMITTED: &str =
    "Uncommitted changes and unpushed commits were removed at the operator's request.";

/// A deletion record that is held in memory (and returned to the operator) but could not be
/// written to disk.
#[derive(Debug)]
pub struct RecordNotSaved {
    pub record: DeletionRecord,
    pub error: std::io::Error,
}

/// Everything stored for one project.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ProjectRecords {
    #[serde(default)]
    next_seq: u64,
    #[serde(default)]
    deletions: Vec<DeletionRecord>,
    #[serde(default)]
    refetches: Vec<RefetchRecord>,
}

/// File-backed store of deletion and re-fetch records, `{ project_id: ProjectRecords }` in
/// `<data_dir>/camerata/source-records.json`. `Clone` is a shallow handle.
#[derive(Clone, Default)]
pub struct SourceRecordStore {
    path: Option<Arc<PathBuf>>,
    inner: Arc<Mutex<BTreeMap<String, ProjectRecords>>>,
}

impl SourceRecordStore {
    /// In-memory only.
    pub fn new() -> Self {
        Self::default()
    }

    /// Load `path` (a missing file is an empty store) and persist every change back to it. A file
    /// that does not parse is moved aside to `<name>.corrupt`, never overwritten.
    pub fn load_or_new(path: PathBuf) -> Self {
        let map = match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<BTreeMap<String, ProjectRecords>>(&text) {
                Ok(map) => map,
                Err(e) => {
                    let aside = path.with_extension("json.corrupt");
                    tracing::warn!(
                        file = %path.display(),
                        error = %e,
                        moved_to = %aside.display(),
                        "source-records file did not parse; moving it aside"
                    );
                    let _ = std::fs::rename(&path, &aside);
                    BTreeMap::new()
                }
            },
            Err(_) => BTreeMap::new(),
        };
        Self {
            path: Some(Arc::new(path)),
            inner: Arc::new(Mutex::new(map)),
        }
    }

    /// The file the store persists into, if any.
    pub fn path(&self) -> Option<&std::path::Path> {
        self.path.as_deref().map(PathBuf::as_path)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, ProjectRecords>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn persist_locked(&self, map: &BTreeMap<String, ProjectRecords>) -> std::io::Result<()> {
        let Some(path) = self.path.as_deref() else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_vec(map).map_err(std::io::Error::other)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path)
    }

    /// Append a deletion record, stamping its `seq`. The in-memory copy is updated even when
    /// persisting fails (the error is returned so the caller can say so): the record is the proof
    /// of what happened and must never be lost to a full disk mid-response.
    pub fn add_deletion(
        &self,
        project_id: &str,
        mut record: DeletionRecord,
    ) -> Result<DeletionRecord, Box<RecordNotSaved>> {
        let mut map = self.lock();
        let entry = map.entry(project_id.to_string()).or_default();
        entry.next_seq += 1;
        record.seq = entry.next_seq;
        entry.deletions.push(record.clone());
        match self.persist_locked(&map) {
            Ok(()) => Ok(record),
            Err(error) => Err(Box::new(RecordNotSaved { record, error })),
        }
    }

    /// Append a re-fetch record, stamping its `seq`.
    pub fn add_refetch(
        &self,
        project_id: &str,
        mut record: RefetchRecord,
    ) -> std::io::Result<RefetchRecord> {
        let mut map = self.lock();
        let entry = map.entry(project_id.to_string()).or_default();
        entry.next_seq += 1;
        record.seq = entry.next_seq;
        entry.refetches.push(record.clone());
        self.persist_locked(&map).map(|()| record)
    }

    /// Every deletion record of a project, newest first.
    pub fn deletions(&self, project_id: &str) -> Vec<DeletionRecord> {
        self.lock()
            .get(project_id)
            .map(|p| p.deletions.iter().rev().cloned().collect())
            .unwrap_or_default()
    }

    /// Every re-fetch record of a project, newest first.
    pub fn refetches(&self, project_id: &str) -> Vec<RefetchRecord> {
        self.lock()
            .get(project_id)
            .map(|p| p.refetches.iter().rev().cloned().collect())
            .unwrap_or_default()
    }

    /// The newest deletion record for `repo` (case-insensitive).
    pub fn latest_deletion(&self, project_id: &str, repo: &str) -> Option<DeletionRecord> {
        self.lock().get(project_id).and_then(|p| {
            p.deletions
                .iter()
                .rev()
                .find(|d| d.repo.eq_ignore_ascii_case(repo.trim()))
                .cloned()
        })
    }

    /// The newest re-fetch record for `repo` (case-insensitive).
    pub fn latest_refetch(&self, project_id: &str, repo: &str) -> Option<RefetchRecord> {
        self.lock().get(project_id).and_then(|p| {
            p.refetches
                .iter()
                .rev()
                .find(|r| r.repo.eq_ignore_ascii_case(repo.trim()))
                .cloned()
        })
    }

    /// One deletion record by id.
    pub fn deletion(&self, project_id: &str, record_id: &str) -> Option<DeletionRecord> {
        self.lock()
            .get(project_id)
            .and_then(|p| p.deletions.iter().find(|d| d.id == record_id).cloned())
    }
}

impl DeletionRecord {
    /// Total of the items that are clear (removed or already absent), as `(files, bytes)`.
    pub fn totals_of(items: &[PurgedItem]) -> (u64, u64) {
        items
            .iter()
            .fold((0, 0), |(f, b), i| (f + i.files, b + i.bytes))
    }

    /// The record as the client sees it: plain text, no markup (design 2.3.1).
    pub fn render_text(&self) -> String {
        let mut out = String::new();
        let label = |name: &str| format!("{name:<18}");
        out.push_str("Camerata source deletion record\n");
        let origin = self
            .origin_url
            .as_deref()
            .map(|u| format!("  (origin {u})"))
            .unwrap_or_default();
        out.push_str(&format!("{}{}{origin}\n", label("Repository"), self.repo));
        let inspected = match (&self.inspected_sha, &self.inspection_id) {
            (Some(sha), Some(id)) => format!("{sha} (inspection {id})"),
            (Some(sha), None) => sha.clone(),
            (None, Some(id)) => format!("not recorded (inspection {id})"),
            (None, None) => "no inspection was recorded".to_string(),
        };
        out.push_str(&format!("{}{inspected}\n", label("Inspected commit")));
        if let Some(head) = &self.head_sha_at_deletion {
            out.push_str(&format!("{}{head}\n", label("HEAD at deletion")));
        }
        out.push_str(&format!(
            "{}{}   on   {}\n",
            label("Removed from"),
            self.path,
            self.machine
        ));
        out.push_str(&format!(
            "{}{}     Completed  {}\n",
            label("Requested"),
            display_time(&self.requested_at),
            display_time(&self.completed_at)
        ));
        out.push_str("Removed\n");
        for item in &self.items {
            let counts = item.counts_text();
            let outcome = match &item.outcome {
                PurgeOutcome::Planned => " [planned]".to_string(),
                PurgeOutcome::Removed => String::new(),
                PurgeOutcome::AlreadyAbsent => " [already absent]".to_string(),
                PurgeOutcome::Failed { error } => format!(" [NOT REMOVED: {error}]"),
            };
            out.push_str(&format!(
                "  {:<20}{}  ({counts}){outcome}\n",
                item.kind.label(),
                item.path
            ));
        }
        out.push_str(&format!(
            "{}{} files, {}\n",
            label("Total"),
            group_thousands(self.total_files),
            format_bytes(self.total_bytes)
        ));
        out.push_str(&format!(
            "{}code excerpts inside the inspection findings (part of the delivered report)\n",
            label("Kept")
        ));
        out.push_str(&format!("{}{}\n", label("Method"), self.method));
        for residual in &self.residuals {
            out.push_str(&format!("{}{residual}\n", label("Note")));
        }
        out.push_str(&format!("{}{}\n", label("Camerata"), self.camerata_version));
        out
    }
}

/// `2,314` style grouping.
pub fn group_thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// `148.2 MB` style size (decimal units, one decimal place from KB up).
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// `2026-10-24T17:40:12Z` -> `2026-10-24 17:40:12 UTC`; an unparseable string is shown as given.
fn display_time(rfc3339: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .map(|t| {
            t.with_timezone(&chrono::Utc)
                .format("%Y-%m-%d %H:%M:%S UTC")
                .to_string()
        })
        .unwrap_or_else(|_| rfc3339.to_string())
}

/// `2026-10-24T17:40:19Z` -> `24 Oct 2026`; an unparseable string is shown as given.
pub fn long_date(rfc3339: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(rfc3339)
        .map(|t| {
            t.with_timezone(&chrono::Utc)
                .format("%-d %b %Y")
                .to_string()
        })
        .unwrap_or_else(|_| rfc3339.to_string())
}

/// The current time as RFC3339 UTC with second precision (`2026-10-24T17:40:12Z`).
pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn record(repo: &str, completed: &str) -> DeletionRecord {
        DeletionRecord {
            id: format!("del-p-{}-{completed}", repo.replace('/', "-")),
            seq: 0,
            project_id: "p".to_string(),
            repo: repo.to_string(),
            origin_url: Some(format!("https://github.com/{repo}.git")),
            path: format!("/work/{repo}"),
            inspected_sha: Some("3f2a9c14e0b7".to_string()),
            inspection_id: Some("insp-p-1-abcd".to_string()),
            head_sha_at_deletion: Some("9b7e21c0000".to_string()),
            requested_at: "2026-10-24T17:40:12Z".to_string(),
            completed_at: completed.to_string(),
            confirmed_with: repo.to_string(),
            include_uncommitted: false,
            items: vec![
                PurgedItem {
                    kind: PurgedKind::Checkout,
                    path: format!("/work/{repo}"),
                    files: 2314,
                    bytes: 148_200_000,
                    outcome: PurgeOutcome::Removed,
                },
                PurgedItem {
                    kind: PurgedKind::CliLedger,
                    path: "/cache/camerata/app-3f2a9c14.ledger.json".to_string(),
                    files: 1,
                    bytes: 12_100,
                    outcome: PurgeOutcome::Removed,
                },
                PurgedItem {
                    kind: PurgedKind::ModelSessionFile,
                    path: "/claude/projects/-work/9d1c.jsonl".to_string(),
                    files: 0,
                    bytes: 0,
                    outcome: PurgeOutcome::AlreadyAbsent,
                },
            ],
            total_files: 2315,
            total_bytes: 148_212_100,
            method: METHOD_SENTENCE.to_string(),
            residuals: vec![RESIDUAL_BACKUPS.to_string(), RESIDUAL_EXCERPTS.to_string()],
            camerata_version: "0.4.0".to_string(),
            machine: "MacBook-Pro.local".to_string(),
        }
    }

    #[test]
    fn counts_and_sizes_read_the_way_the_record_shows_them() {
        assert_eq!(group_thousands(0), "0");
        assert_eq!(group_thousands(999), "999");
        assert_eq!(group_thousands(2314), "2,314");
        assert_eq!(group_thousands(1_234_567), "1,234,567");
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(999), "999 B");
        assert_eq!(format_bytes(12_100), "12.1 KB");
        assert_eq!(format_bytes(148_200_000), "148.2 MB");
        assert_eq!(format_bytes(2_300_000_000), "2.3 GB");
    }

    /// The golden rendering the client receives. Changing it is a deliberate act.
    #[test]
    fn delete_record_text_rendering_matches_fixture() {
        let got = record("acme/app", "2026-10-24T17:40:19Z").render_text();
        let want = "\
Camerata source deletion record
Repository        acme/app  (origin https://github.com/acme/app.git)
Inspected commit  3f2a9c14e0b7 (inspection insp-p-1-abcd)
HEAD at deletion  9b7e21c0000
Removed from      /work/acme/app   on   MacBook-Pro.local
Requested         2026-10-24 17:40:12 UTC     Completed  2026-10-24 17:40:19 UTC
Removed
  checkout            /work/acme/app  (2,314 files, 148.2 MB)
  CLI ledger          /cache/camerata/app-3f2a9c14.ledger.json  (1 files, 12.1 KB)
  model session file  /claude/projects/-work/9d1c.jsonl  (-) [already absent]
Total             2,315 files, 148.2 MB
Kept              code excerpts inside the inspection findings (part of the delivered report)
Method            Ordinary file removal (std::fs::remove_dir_all) on a FileVault-encrypted APFS volume. Not a forensic wipe; no overwrite pass.
Note              Backups made by the operator's own tools (Time Machine or similar) are outside this record.
Note              Code excerpts inside the inspection findings are kept as part of the delivered report.
Camerata          0.4.0
";
        assert_eq!(got, want);
    }

    #[test]
    fn a_failed_item_is_named_in_the_text_so_the_proof_is_honest() {
        let mut r = record("acme/app", "2026-10-24T17:40:19Z");
        r.items[1].outcome = PurgeOutcome::Failed {
            error: "permission denied".to_string(),
        };
        assert!(r.render_text().contains("[NOT REMOVED: permission denied]"));
    }

    #[test]
    fn store_stamps_increasing_seq_across_both_record_kinds() {
        let store = SourceRecordStore::new();
        let d = store
            .add_deletion("p", record("acme/app", "2026-10-24T17:40:19Z"))
            .unwrap();
        let f = store
            .add_refetch(
                "p",
                RefetchRecord {
                    seq: 0,
                    repo: "acme/app".to_string(),
                    path: "/work/acme/app".to_string(),
                    sha: Some("abc".to_string()),
                    target: "latest".to_string(),
                    // Same second as the deletion: only seq can order them.
                    completed_at: "2026-10-24T17:40:19Z".to_string(),
                },
            )
            .unwrap();
        assert!(f.seq > d.seq);
        assert_eq!(store.latest_deletion("p", "ACME/APP").unwrap().seq, d.seq);
        assert_eq!(store.latest_refetch("p", "acme/app").unwrap().seq, f.seq);
        assert!(store.latest_deletion("p", "acme/other").is_none());
        assert!(store.latest_deletion("q", "acme/app").is_none());
    }

    #[test]
    fn records_survive_a_restart_and_a_corrupt_file_is_moved_aside() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("source-records.json");
        let store = SourceRecordStore::load_or_new(path.clone());
        let d = store
            .add_deletion("p", record("acme/app", "2026-10-24T17:40:19Z"))
            .unwrap();
        drop(store);

        let again = SourceRecordStore::load_or_new(path.clone());
        assert_eq!(again.deletion("p", &d.id).unwrap().repo, "acme/app");
        assert_eq!(again.deletions("p").len(), 1);
        drop(again);

        std::fs::write(&path, "not json").unwrap();
        let broken = SourceRecordStore::load_or_new(path.clone());
        assert!(broken.deletions("p").is_empty());
        assert!(tmp.path().join("source-records.json.corrupt").is_file());
    }

    #[test]
    fn items_serialize_with_a_flat_outcome_string() {
        let item = PurgedItem {
            kind: PurgedKind::Worktree,
            path: "/w".to_string(),
            files: 3,
            bytes: 4,
            outcome: PurgeOutcome::Planned,
        };
        let v = serde_json::to_value(&item).unwrap();
        assert_eq!(v["kind"], "worktree");
        assert_eq!(v["outcome"], "planned");
        let failed = PurgedItem {
            outcome: PurgeOutcome::Failed {
                error: "x".to_string(),
            },
            ..item
        };
        let v = serde_json::to_value(&failed).unwrap();
        assert_eq!(v["outcome"], "failed");
        assert_eq!(v["error"], "x");
        let back: PurgedItem = serde_json::from_value(v).unwrap();
        assert!(!back.outcome.is_clear());
    }
}
