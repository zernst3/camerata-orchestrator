//! The run store: every inspection is saved part by part, so an interruption or a failed part
//! costs only that part. Zach, 2026-10-28: if a true failure happens (the AI service is down)
//! he must be able to re-trigger only that portion later, and "any interruption should cost
//! minutes, never the run" (after a laptop crash). The client export unlocks once everything is
//! complete.
//!
//! # Layout
//!
//! `<state dir>/runs/<run-id>/` (mode 0700: it holds model replies derived from client code, the
//! same sensitivity as the scan cache):
//!
//! * `run.json` - the [`RunRecord`]: the commit and tree state of every repo, the models, the rules
//!   fingerprint, the [`AiScanKey`], the engine version, the arguments the run was started with
//!   and the plan of parts with their status.
//! * `lock.json` - the process id and a heartbeat time, present only while a process works on the
//!   run ([`RunLock`]).
//! * `calls/<key>.json` - one file per distinct model request: every reply the run got for it, in
//!   the order it asked ([`journal`]).
//! * `batches/<key>-<n>.json` - one file per submitted Message Batch: its id the moment the submit
//!   returned, and its results once fetched.
//!
//! Every file is written to a temporary name and renamed, so a process killed at any instant
//! leaves the previous complete file or the new complete file, never half of one.
//!
//! # How a resume works
//!
//! The pipeline is deterministic given the same model replies: the same code, the same prompts,
//! the same merge. A resume runs it again with the model seam ([`journal::JournalLlm`]) answering
//! from the stored replies. A reply that was stored as good is returned without a model call; a
//! call that failed, was never made, or whose reply a later step rejected goes to the model.
//! Merging, ranking, ordering and the report are therefore always recomputed from the stored
//! results, and a run resumed after a failure is the run that would have happened without it.
//!
//! The plan lists the stages of the run (per repo: tools, scan, calibration, alternatives, detail,
//! fix; then merge and export) and is written, every part pending, before any work starts. The
//! individual model calls of a stage are recorded as the stage reaches them, because how many
//! there are (files in a chunk, findings in a batch) depends on the stage before it.

pub mod journal;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::scan_cache::AiScanKey;

/// The layout version of `run.json`. A run written by another version is not resumed.
pub const STORE_SCHEMA_VERSION: u32 = 1;

/// A lock whose heartbeat is older than this is stale: the process holding it is gone.
pub const STALE_HEARTBEAT_SECS: i64 = 45;

/// How often a running process refreshes its heartbeat.
pub const HEARTBEAT_EVERY: std::time::Duration = std::time::Duration::from_secs(5);

/// How many runs per project the cleanup keeps by default.
pub const DEFAULT_KEEP_RUNS: usize = 5;

/// What the engine is, for the "same engine" check of a resume: the crate version and the version
/// of the question the scan asks the model. A prompt change makes a stored reply an answer to an
/// older question, so the run is not resumed.
pub fn engine_version() -> String {
    format!(
        "{}+prompt{}",
        env!("CARGO_PKG_VERSION"),
        crate::ai_audit::AUDIT_PROMPT_SCHEMA_VERSION
    )
}

/// The folder every run of this user lives under.
pub fn runs_dir() -> PathBuf {
    crate::scan_cache::state_dir::dir().join("runs")
}

/// RFC 3339 UTC now, to the second.
fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

// ── Records ──────────────────────────────────────────────────────────────────────────────────

/// What a part of a run is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartKind {
    /// The mechanical tools and the dependency audit of a repo.
    Tools,
    /// The AI scan passes of a repo.
    Scan,
    /// Calibration of a repo's findings (severity, confidence and the hour estimates).
    Calibration,
    /// The recommendation of an alternative per rule.
    Alternatives,
    /// Regeneration of the full detail of a finding.
    Detail,
    /// The finding-specific fix text.
    Fix,
    /// Any other model call (the clone judge, the deep tier).
    Other,
    /// Merging, ranking and the report, recomputed from the stored results.
    Merge,
    /// The export.
    Export,
}

impl PartKind {
    /// The kind of a failed pass, from the name it was recorded under.
    pub fn of_pass_name(pass: &str) -> Self {
        let p = pass.to_ascii_lowercase();
        if p.contains("alternative") {
            Self::Alternatives
        } else if p.contains("hour estimation") || p.contains("calibrat") {
            Self::Calibration
        } else if p.contains("detail") {
            Self::Detail
        } else if p.contains("fix") {
            Self::Fix
        } else if p.contains("preview pass")
            || p.contains("taint pass")
            || p.contains("repository read")
            || p.contains("dependency")
        {
            Self::Tools
        } else if p.contains("clone") || p.contains("deep") || p.contains("deferred") {
            Self::Other
        } else {
            Self::Scan
        }
    }

    /// A short plain label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Tools => "mechanical tools",
            Self::Scan => "AI scan",
            Self::Calibration => "calibration and estimates",
            Self::Alternatives => "rule alternatives",
            Self::Detail => "finding detail",
            Self::Fix => "fix text",
            Self::Other => "other model calls",
            Self::Merge => "merge and report",
            Self::Export => "export",
        }
    }

    /// True for a kind whose work is model calls.
    pub fn is_model_work(self) -> bool {
        !matches!(self, Self::Tools | Self::Merge | Self::Export)
    }
}

/// Where a part stands. Anything but `Done` is remaining work for a resume.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartStatus {
    Pending,
    Running,
    Done,
    Failed,
}

/// One part of the plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PartRecord {
    pub id: String,
    /// The repo it belongs to; empty for a part of the whole run.
    #[serde(default)]
    pub repo: String,
    pub kind: PartKind,
    pub status: PartStatus,
    /// Model calls of this part whose reply is stored and good.
    #[serde(default)]
    pub calls_done: u32,
    /// Model calls of this part that failed or whose reply was set aside.
    #[serde(default)]
    pub calls_not_done: u32,
    #[serde(default)]
    pub updated_at: String,
    /// Why the part is not done, for the operator.
    #[serde(default)]
    pub note: Option<String>,
}

/// Where a run stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// A process is working on it, or was and died ([`Liveness`] tells which).
    Running,
    /// Every part is done and the export was written.
    Complete,
    /// The process finished but a part failed, did not run, or the export was refused.
    Incomplete,
}

/// A repo of the run as it stood when the run began.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoRecord {
    pub name: String,
    pub dir: String,
    pub commit: Option<String>,
    /// Whether the working tree was clean (no change and no untracked file) at the start.
    pub clean: bool,
    /// A digest of the tree state at the start (status and diff).
    pub tree_at_start: String,
    /// The same at the end of the last attempt. The inspection's own build step can leave files
    /// behind, so a resume accepts either.
    #[serde(default)]
    pub tree_at_end: Option<String>,
}

/// The models and mode a run used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelsRecord {
    pub audit: String,
    pub calibration: String,
    pub fix: String,
    pub backend: String,
    pub mode: String,
}

/// Everything saved about one run except the model replies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub schema: u32,
    pub run_id: String,
    pub created_at: String,
    pub updated_at: String,
    pub status: RunStatus,
    /// How many times a process has worked on the run: 1 for the first, 2 after one resume.
    pub attempt: u32,
    pub engine_version: String,
    /// Names the project (the repos together) so the cleanup and `latest` can find its runs.
    pub project_key: String,
    pub repos: Vec<RepoRecord>,
    pub models: ModelsRecord,
    pub rules_fingerprint: String,
    pub ai_scan_key: AiScanKey,
    /// The secret the run draws its prompt delimiter tokens from (hex). See
    /// [`crate::prompt_frame::seed_tokens`].
    pub nonce_seed: String,
    /// The arguments the run was started with, for the resume (opaque to the store).
    pub args: serde_json::Value,
    /// The pre-flight estimate of the whole run, when one was made.
    #[serde(default)]
    pub estimate_usd: Option<f64>,
    pub plan: Vec<PartRecord>,
    /// Why the last attempt ended incomplete, in plain words.
    #[serde(default)]
    pub incomplete_reasons: Vec<String>,
}

/// What a process working on a run looks like from outside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Liveness {
    /// No process works on it.
    None,
    /// A live process holds the lock.
    Live,
    /// A lock exists but its process is gone or its heartbeat is stale.
    Stale,
}

/// The state of a run for the operator and the cockpit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Running,
    /// Marked running, but its process is gone: it was killed, crashed or the machine slept.
    Interrupted,
    Complete,
    Incomplete,
}

impl RunRecord {
    /// The run's state given who is working on it.
    pub fn state(&self, liveness: Liveness) -> RunState {
        match self.status {
            RunStatus::Complete => RunState::Complete,
            RunStatus::Incomplete => RunState::Incomplete,
            RunStatus::Running => {
                if liveness == Liveness::Live {
                    RunState::Running
                } else {
                    RunState::Interrupted
                }
            }
        }
    }

    /// Parts a resume has to do: every part that is not done (pending, running, failed).
    pub fn remaining_parts(&self) -> Vec<&PartRecord> {
        self.plan
            .iter()
            .filter(|p| p.status != PartStatus::Done)
            .collect()
    }
}

// ── Files ────────────────────────────────────────────────────────────────────────────────────

/// Create `dir` (and parents) readable only by this user.
pub(crate) fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Write `bytes` to `path` so a reader (or a crash) sees the old file or the new one, whole.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp = dir.join(format!(
        ".{}.tmp-{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("file"),
        std::process::id()
    ));
    {
        let mut file = std::fs::File::create(&tmp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

// ── The lock ─────────────────────────────────────────────────────────────────────────────────

/// `lock.json`: who is working on the run and when they last said so.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LockFile {
    pid: u32,
    started_at: String,
    heartbeat_at: String,
}

/// True when a process with this id exists.
fn pid_is_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        std::process::Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(true)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

/// Whether anything works on the run in `run_dir`.
pub fn liveness_of(run_dir: &Path) -> Liveness {
    let Some(lock) = read_json::<LockFile>(&run_dir.join("lock.json")) else {
        return Liveness::None;
    };
    let fresh = chrono::DateTime::parse_from_rfc3339(&lock.heartbeat_at)
        .map(|t| (chrono::Utc::now() - t.with_timezone(&chrono::Utc)).num_seconds())
        .map(|age| age <= STALE_HEARTBEAT_SECS)
        .unwrap_or(false);
    if fresh && pid_is_alive(lock.pid) {
        Liveness::Live
    } else {
        Liveness::Stale
    }
}

/// The lock a process holds on a run while it works. A heartbeat task refreshes it; dropping the
/// lock stops the task and removes the file.
pub struct RunLock {
    path: PathBuf,
    heartbeat: Option<tokio::task::JoinHandle<()>>,
}

impl RunLock {
    /// Take the lock on `run_dir`. Refused while a live process holds it; a stale lock (its
    /// process died, or its heartbeat stopped) is taken over.
    pub fn acquire(run_dir: &Path) -> Result<Self, RunStoreError> {
        if liveness_of(run_dir) == Liveness::Live {
            let pid = read_json::<LockFile>(&run_dir.join("lock.json")).map(|l| l.pid);
            return Err(RunStoreError::Busy(match pid {
                Some(pid) => format!(
                    "this run is being worked on right now by process {pid}; wait for it to \
                     finish, or stop it first"
                ),
                None => "this run is being worked on right now".to_string(),
            }));
        }
        let path = run_dir.join("lock.json");
        let started_at = now();
        let write = {
            let path = path.clone();
            let started_at = started_at.clone();
            move || {
                let lock = LockFile {
                    pid: std::process::id(),
                    started_at: started_at.clone(),
                    heartbeat_at: now(),
                };
                if let Ok(bytes) = serde_json::to_vec(&lock) {
                    let _ = write_atomic(&path, &bytes);
                }
            }
        };
        write();
        let heartbeat = tokio::runtime::Handle::try_current().ok().map(|h| {
            h.spawn(async move {
                loop {
                    tokio::time::sleep(HEARTBEAT_EVERY).await;
                    write();
                }
            })
        });
        Ok(Self { path, heartbeat })
    }
}

impl Drop for RunLock {
    fn drop(&mut self) {
        if let Some(task) = self.heartbeat.take() {
            task.abort();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

// ── Errors ───────────────────────────────────────────────────────────────────────────────────

/// Why the store could not do what was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunStoreError {
    /// Another process is working on the run.
    Busy(String),
    /// A file could not be read or written.
    Io(String),
    /// No such run.
    NotFound(String),
    /// A resume was refused, with the plain reason.
    Refused(String),
    /// The run is complete; there is nothing to resume.
    AlreadyComplete(String),
}

impl std::fmt::Display for RunStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy(m)
            | Self::Io(m)
            | Self::NotFound(m)
            | Self::Refused(m)
            | Self::AlreadyComplete(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for RunStoreError {}

// ── The repo state ───────────────────────────────────────────────────────────────────────────

/// The state of a repo's working tree that decides whether stored replies still apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeState {
    pub commit: Option<String>,
    pub clean: bool,
    /// A digest of the status and the diff against HEAD.
    pub digest: String,
}

/// Read the commit and the tree state of `dir` with git. A folder that is not a git checkout has
/// no commit and a digest of nothing, so it can never be resumed against a different tree.
pub fn tree_state(dir: &Path) -> TreeState {
    use sha2::{Digest, Sha256};
    let git = |args: &[&str]| -> Option<Vec<u8>> {
        let out = std::process::Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .ok()?;
        out.status.success().then_some(out.stdout)
    };
    let commit = git(&["rev-parse", "HEAD"])
        .map(|b| String::from_utf8_lossy(&b).trim().to_string())
        .filter(|c| !c.is_empty());
    let status = git(&["status", "--porcelain", "-z", "--untracked-files=all"]);
    let diff = git(&["diff", "HEAD", "--no-ext-diff"]);
    let clean = status.as_ref().is_some_and(Vec::is_empty);
    let mut hasher = Sha256::new();
    hasher.update(status.unwrap_or_default());
    hasher.update([0u8]);
    hasher.update(diff.unwrap_or_default());
    TreeState {
        commit,
        clean,
        digest: hex(&hasher.finalize()),
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ── The store ────────────────────────────────────────────────────────────────────────────────

/// A run opened for work by this process.
pub struct RunStore {
    dir: PathBuf,
    state: Mutex<StoreState>,
    /// Held for the life of the store.
    _lock: RunLock,
}

pub(crate) struct StoreState {
    pub record: RunRecord,
    /// How many times each request key was asked in this attempt (the position of its reply).
    pub asked: std::collections::HashMap<String, usize>,
    /// Call files read or written in this attempt.
    pub calls: std::collections::HashMap<String, journal::CallFile>,
    /// How many times each batch submission key was made in this attempt.
    pub submitted: std::collections::HashMap<String, usize>,
}

/// What to create a run from.
pub struct NewRun {
    pub project_key: String,
    pub repos: Vec<(String, PathBuf)>,
    pub models: ModelsRecord,
    pub ai_scan_key: AiScanKey,
    pub args: serde_json::Value,
    pub estimate_usd: Option<f64>,
    pub include_alternatives: bool,
}

/// A fresh run id: the time, then six random hex digits.
fn new_run_id() -> String {
    let mut bytes = [0u8; 3];
    let _ = getrandom::getrandom(&mut bytes);
    format!(
        "{}-{}",
        chrono::Utc::now().format("%Y%m%dT%H%M%SZ"),
        hex(&bytes)
    )
}

/// The planned parts of a run: per repo, then the whole-run parts.
fn plan_for(repos: &[(String, PathBuf)], include_alternatives: bool) -> Vec<PartRecord> {
    let stamp = now();
    let mut plan = Vec::new();
    let mut part = |repo: &str, kind: PartKind| {
        plan.push(PartRecord {
            id: if repo.is_empty() {
                format!("{kind:?}").to_ascii_lowercase()
            } else {
                format!("{repo}:{}", format!("{kind:?}").to_ascii_lowercase())
            },
            repo: repo.to_string(),
            kind,
            status: PartStatus::Pending,
            calls_done: 0,
            calls_not_done: 0,
            updated_at: stamp.clone(),
            note: None,
        });
    };
    for (name, _) in repos {
        part(name, PartKind::Tools);
        part(name, PartKind::Scan);
        part(name, PartKind::Calibration);
        if include_alternatives {
            part(name, PartKind::Alternatives);
        }
        part(name, PartKind::Detail);
        part(name, PartKind::Fix);
    }
    part("", PartKind::Merge);
    part("", PartKind::Export);
    plan
}

impl RunStore {
    /// Begin a new run: the folder, the record with every part pending and the lock, all written
    /// before any work starts.
    pub fn create(new: NewRun) -> Result<Arc<Self>, RunStoreError> {
        let run_id = new_run_id();
        let dir = runs_dir().join(&run_id);
        create_private_dir(&dir).map_err(|e| RunStoreError::Io(e.to_string()))?;
        create_private_dir(&dir.join("calls")).map_err(|e| RunStoreError::Io(e.to_string()))?;
        create_private_dir(&dir.join("batches")).map_err(|e| RunStoreError::Io(e.to_string()))?;
        let mut seed = [0u8; crate::prompt_frame::NONCE_BYTES];
        getrandom::getrandom(&mut seed).map_err(|e| RunStoreError::Io(e.to_string()))?;
        let repos = new
            .repos
            .iter()
            .map(|(name, path)| {
                let t = tree_state(path);
                RepoRecord {
                    name: name.clone(),
                    dir: path.to_string_lossy().into_owned(),
                    commit: t.commit,
                    clean: t.clean,
                    tree_at_start: t.digest,
                    tree_at_end: None,
                }
            })
            .collect();
        let record = RunRecord {
            schema: STORE_SCHEMA_VERSION,
            run_id,
            created_at: now(),
            updated_at: now(),
            status: RunStatus::Running,
            attempt: 1,
            engine_version: engine_version(),
            project_key: new.project_key,
            repos,
            rules_fingerprint: new.ai_scan_key.directives_fingerprint.clone(),
            ai_scan_key: new.ai_scan_key,
            models: new.models,
            nonce_seed: hex(&seed),
            args: new.args,
            estimate_usd: new.estimate_usd,
            plan: plan_for(&new.repos, new.include_alternatives),
            incomplete_reasons: Vec::new(),
        };
        let lock = RunLock::acquire(&dir)?;
        let store = Arc::new(Self {
            dir,
            state: Mutex::new(StoreState {
                record,
                asked: Default::default(),
                calls: Default::default(),
                submitted: Default::default(),
            }),
            _lock: lock,
        });
        store.persist()?;
        Ok(store)
    }

    /// Open an existing run to work on it again: takes the lock (refused while another live
    /// process holds it), starts the next attempt and marks the run running. The caller has
    /// already decided the resume is allowed ([`check_resume`]).
    pub fn open(run_id: &str) -> Result<Arc<Self>, RunStoreError> {
        let dir = runs_dir().join(run_id);
        let mut record = load_record(&dir)?;
        let lock = RunLock::acquire(&dir)?;
        record.attempt += 1;
        record.status = RunStatus::Running;
        record.incomplete_reasons.clear();
        let store = Arc::new(Self {
            dir,
            state: Mutex::new(StoreState {
                record,
                asked: Default::default(),
                calls: Default::default(),
                submitted: Default::default(),
            }),
            _lock: lock,
        });
        store.persist()?;
        Ok(store)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Keep the incremental-scan baseline the run started from. A resume starts from this one,
    /// not from the cache file an attempt since may have replaced, so the resumed scan covers the
    /// same files as the run it continues.
    pub fn save_prior_manifest(&self, manifest: &crate::scan_cache::ScanManifest) {
        if let Ok(bytes) = serde_json::to_vec(manifest) {
            let _ = write_atomic(&self.dir.join("prior_manifest.json"), &bytes);
        }
    }

    /// The baseline [`Self::save_prior_manifest`] kept, if the run started from one.
    pub fn load_prior_manifest(&self) -> Option<crate::scan_cache::ScanManifest> {
        read_json(&self.dir.join("prior_manifest.json"))
    }

    pub fn run_id(&self) -> String {
        self.lock_state().record.run_id.clone()
    }

    /// The record as it stands.
    pub fn record(&self) -> RunRecord {
        self.lock_state().record.clone()
    }

    /// The secret the prompt tokens of this run are drawn from.
    pub fn nonce_seed(&self) -> [u8; crate::prompt_frame::NONCE_BYTES] {
        let mut out = [0u8; crate::prompt_frame::NONCE_BYTES];
        let hex = self.lock_state().record.nonce_seed.clone();
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = u8::from_str_radix(hex.get(i * 2..i * 2 + 2).unwrap_or("00"), 16).unwrap_or(0);
        }
        out
    }

    pub(crate) fn lock_state(&self) -> std::sync::MutexGuard<'_, StoreState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Write `run.json`.
    pub fn persist(&self) -> Result<(), RunStoreError> {
        let mut state = self.lock_state();
        state.record.updated_at = now();
        let bytes = serde_json::to_vec_pretty(&state.record)
            .map_err(|e| RunStoreError::Io(e.to_string()))?;
        write_atomic(&self.dir.join("run.json"), &bytes)
            .map_err(|e| RunStoreError::Io(e.to_string()))
    }

    /// Move a part to `status`, writing the record.
    pub fn set_part(&self, repo: &str, kind: PartKind, status: PartStatus, note: Option<String>) {
        {
            let mut state = self.lock_state();
            for part in state
                .record
                .plan
                .iter_mut()
                .filter(|p| p.kind == kind && (repo.is_empty() || p.repo == repo))
            {
                // A finished part is never moved back by a late "running".
                if part.status == PartStatus::Done && status == PartStatus::Running {
                    continue;
                }
                part.status = status;
                part.updated_at = now();
                part.note.clone_from(&note);
            }
        }
        let _ = self.persist();
    }

    /// Mark a part running the first time it is reached in this attempt.
    pub(crate) fn part_started(&self, repo: &str, kind: PartKind) {
        let needs = {
            let state = self.lock_state();
            state
                .record
                .plan
                .iter()
                .filter(|p| p.kind == kind && (repo.is_empty() || p.repo == repo))
                .any(|p| p.status != PartStatus::Running && p.status != PartStatus::Done)
        };
        if needs {
            self.set_part(repo, kind, PartStatus::Running, None);
        }
    }

    /// Record the working-tree state at the end of an attempt, so a resume accepts the tree the
    /// run itself left behind.
    pub fn record_tree_end(&self) {
        {
            let mut state = self.lock_state();
            for repo in &mut state.record.repos {
                repo.tree_at_end = Some(tree_state(Path::new(&repo.dir)).digest);
            }
        }
        let _ = self.persist();
    }

    /// The end of an attempt: sets aside replies a later step rejected, decides which parts are
    /// done, and marks the run complete or incomplete.
    pub fn finish(&self, outcome: &FinishOutcome) {
        journal::quarantine_rejected(self, &outcome.failures);
        journal::count_calls(self);
        {
            let mut state = self.lock_state();
            let stamp = now();
            let failed_kinds: Vec<(String, PartKind)> = outcome
                .failures
                .iter()
                .filter(|f| !f.scope_note)
                .map(|f| (f.repo.clone(), PartKind::of_pass_name(&f.pass)))
                .collect();
            let not_run_kinds: Vec<(String, PartKind)> = outcome
                .not_run
                .iter()
                .map(|n| (n.repo.clone(), PartKind::of_pass_name(&n.pass)))
                .collect();
            for part in &mut state.record.plan {
                let matches = |list: &[(String, PartKind)]| {
                    list.iter().any(|(repo, kind)| {
                        *kind == part.kind && (repo.is_empty() || *repo == part.repo)
                    })
                };
                let status = if part.kind == PartKind::Export {
                    if outcome.export_written {
                        PartStatus::Done
                    } else {
                        PartStatus::Failed
                    }
                } else if matches(&failed_kinds) {
                    PartStatus::Failed
                } else if matches(&not_run_kinds) {
                    PartStatus::Pending
                } else if outcome.scan_reached_end || part.status == PartStatus::Done {
                    PartStatus::Done
                } else {
                    part.status
                };
                part.status = status;
                part.updated_at.clone_from(&stamp);
                part.note = match status {
                    PartStatus::Failed => outcome
                        .failures
                        .iter()
                        .find(|f| {
                            !f.scope_note
                                && PartKind::of_pass_name(&f.pass) == part.kind
                                && (f.repo.is_empty() || f.repo == part.repo)
                        })
                        .map(|f| format!("{}: {}", f.pass, f.class)),
                    _ => None,
                };
            }
            state.record.status = if outcome.complete {
                RunStatus::Complete
            } else {
                RunStatus::Incomplete
            };
            state
                .record
                .incomplete_reasons
                .clone_from(&outcome.incomplete_reasons);
        }
        self.record_tree_end();
    }
}

/// What the end of an attempt looked like, from the report.
#[derive(Debug, Clone, Default)]
pub struct FinishOutcome {
    /// Every failed pass of the report.
    pub failures: Vec<FinishFailure>,
    /// Every pass that did not run (spend limit, outage stop).
    pub not_run: Vec<FinishNotRun>,
    /// Whether the scan ran to its end (false when the process stopped before the report).
    pub scan_reached_end: bool,
    pub export_written: bool,
    /// Whether the run is complete: no failed part, nothing not run, export written.
    pub complete: bool,
    pub incomplete_reasons: Vec<String>,
}

/// One failed pass of the report, as the store needs it.
#[derive(Debug, Clone)]
pub struct FinishFailure {
    pub repo: String,
    pub pass: String,
    /// The failure class as text (`transport`, `content` ...).
    pub class: String,
    pub files: Vec<String>,
    pub scope_note: bool,
}

/// One pass that did not run.
#[derive(Debug, Clone)]
pub struct FinishNotRun {
    pub repo: String,
    pub pass: String,
}

fn load_record(dir: &Path) -> Result<RunRecord, RunStoreError> {
    let record: RunRecord = read_json(&dir.join("run.json")).ok_or_else(|| {
        RunStoreError::NotFound(format!(
            "no saved run at {} (or its record could not be read)",
            dir.display()
        ))
    })?;
    if record.schema != STORE_SCHEMA_VERSION {
        return Err(RunStoreError::Refused(format!(
            "this run was saved by another version of Camerata (layout {} instead of {}) and \
             cannot be resumed",
            record.schema, STORE_SCHEMA_VERSION
        )));
    }
    Ok(record)
}

// ── Listing ──────────────────────────────────────────────────────────────────────────────────

/// A saved run as the operator sees it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunSummary {
    pub run_id: String,
    pub created_at: String,
    pub state: RunState,
    pub project_key: String,
    pub repos: Vec<String>,
    pub attempt: u32,
    pub remaining: Vec<String>,
    pub incomplete_reasons: Vec<String>,
}

/// Every saved run, newest first.
pub fn list_runs() -> Vec<RunSummary> {
    list_runs_in(&runs_dir())
}

pub fn list_runs_in(root: &Path) -> Vec<RunSummary> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out: Vec<RunSummary> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let dir = e.path();
            let record = load_record(&dir).ok()?;
            let state = record.state(liveness_of(&dir));
            Some(RunSummary {
                run_id: record.run_id.clone(),
                created_at: record.created_at.clone(),
                state,
                project_key: record.project_key.clone(),
                repos: record.repos.iter().map(|r| r.name.clone()).collect(),
                attempt: record.attempt,
                remaining: record
                    .remaining_parts()
                    .iter()
                    .map(|p| p.id.clone())
                    .collect(),
                incomplete_reasons: record.incomplete_reasons.clone(),
            })
        })
        .collect();
    out.sort_by(|a, b| b.run_id.cmp(&a.run_id));
    out
}

/// Load a saved run's record without taking its lock.
pub fn read_record(run_id: &str) -> Result<RunRecord, RunStoreError> {
    load_record(&runs_dir().join(run_id))
}

/// Resolve `run` (an id, or `latest`) to a run id. `latest` is the newest run that is not
/// complete; with `project_key` it is the newest of that project.
pub fn resolve_run(run: &str, project_key: Option<&str>) -> Result<String, RunStoreError> {
    if run != "latest" {
        return Ok(run.to_string());
    }
    list_runs()
        .into_iter()
        .find(|r| r.state != RunState::Complete && project_key.is_none_or(|k| r.project_key == k))
        .map(|r| r.run_id)
        .ok_or_else(|| {
            RunStoreError::NotFound(
                "there is no saved run that still has work left to do".to_string(),
            )
        })
}

// ── Resume checks ────────────────────────────────────────────────────────────────────────────

/// What a resume is about to run, for the refusal check.
pub struct ResumeCheck<'a> {
    pub record: &'a RunRecord,
    pub ai_scan_key: &'a AiScanKey,
    pub models: &'a ModelsRecord,
    /// The state of each repo folder as it stands now, by repo name.
    pub trees: Vec<(String, TreeState)>,
}

/// Decide whether `check.record` may be resumed. The refusal is a plain sentence naming what
/// differs, never a code. A complete run says so ([`RunStoreError::AlreadyComplete`]).
pub fn check_resume(check: &ResumeCheck<'_>) -> Result<(), RunStoreError> {
    let record = check.record;
    if record.status == RunStatus::Complete {
        return Err(RunStoreError::AlreadyComplete(format!(
            "run {} is already complete: every part is done and the export was written, so \
             there is nothing to resume",
            record.run_id
        )));
    }
    let refuse = |why: String| Err(RunStoreError::Refused(why));
    if record.engine_version != engine_version() {
        return refuse(format!(
            "run {} was made by another version of Camerata ({} instead of {}); its saved \
             replies answer a different question, so start a new run",
            record.run_id,
            record.engine_version,
            engine_version()
        ));
    }
    for saved in &record.repos {
        let Some((_, now)) = check.trees.iter().find(|(name, _)| *name == saved.name) else {
            return refuse(format!(
                "repository {} was part of run {} but is not part of this one",
                saved.name, record.run_id
            ));
        };
        if now.commit != saved.commit {
            return refuse(format!(
                "repository {} is at commit {} now; run {} was made at commit {}. Check out \
                 that commit to resume it, or start a new run",
                saved.name,
                now.commit.as_deref().map_or("(none)".to_string(), short),
                record.run_id,
                saved.commit.as_deref().map_or("(none)".to_string(), short),
            ));
        }
        let unchanged = now.digest == saved.tree_at_start
            || saved.tree_at_end.as_deref() == Some(now.digest.as_str());
        if !unchanged {
            return refuse(format!(
                "repository {} has changes that were not there when run {} was made (an edited \
                 or new file); put the tree back as it was, or start a new run",
                saved.name, record.run_id
            ));
        }
    }
    if check.models != &record.models {
        return refuse(format!(
            "run {} used different models or mode ({} / {} / {}, {}); the stored replies came \
             from those, so resume it with the same ones",
            record.run_id,
            record.models.audit,
            record.models.calibration,
            record.models.fix,
            record.models.mode
        ));
    }
    if check.ai_scan_key.directives_fingerprint != record.rules_fingerprint {
        return refuse(format!(
            "the rules changed since run {} was made, so its stored replies answer different \
             questions; start a new run",
            record.run_id
        ));
    }
    if check.ai_scan_key != &record.ai_scan_key {
        return refuse(format!(
            "the way the scan asks the model changed since run {} was made (prompt layout, \
             measures or models); start a new run",
            record.run_id
        ));
    }
    if liveness_of(&runs_dir().join(&record.run_id)) == Liveness::Live {
        return Err(RunStoreError::Busy(format!(
            "run {} is being worked on right now; wait for it to finish",
            record.run_id
        )));
    }
    Ok(())
}

fn short(commit: &str) -> String {
    commit.chars().take(10).collect()
}

// ── The estimate of what a resume would spend ────────────────────────────────────────────────

/// What a resume is expected to cost, and what it will do.
#[derive(Debug, Clone, Serialize)]
pub struct ResumeEstimate {
    /// Parts that will run again, in plain words.
    pub remaining: Vec<String>,
    /// Model calls that are stored and good, and will not be sent again.
    pub calls_stored: usize,
    /// Model calls that failed or were set aside, and will be sent again.
    pub calls_to_send_again: usize,
    /// The upper estimate in US dollars of the work that remains.
    pub usd: f64,
    /// How the figure was reached, in one sentence.
    pub basis: String,
}

impl ResumeEstimate {
    /// The text the CLI prints and the cockpit shows before a resume.
    pub fn render(&self) -> String {
        let mut out = String::from("Resume: what remains\n");
        if self.remaining.is_empty() {
            out.push_str("  nothing; every part is done\n");
        }
        for part in &self.remaining {
            out.push_str(&format!("  - {part}\n"));
        }
        out.push_str(&format!(
            "  {} model call(s) stored and not sent again; {} to send again\n",
            self.calls_stored, self.calls_to_send_again
        ));
        out.push_str(&format!(
            "  estimated cost of the remaining work: up to ${:.2} ({})\n",
            self.usd, self.basis
        ));
        out
    }
}

/// The estimate for resuming the run in `run_id`, from what its folder holds.
pub fn estimate_resume(run_id: &str) -> Result<ResumeEstimate, RunStoreError> {
    let dir = runs_dir().join(run_id);
    let record = load_record(&dir)?;
    Ok(journal::estimate_remaining(&dir, &record))
}

/// Open the saved run `run_id` to resume it: refuse in plain words when anything its stored
/// replies depend on differs now, and say what remains and what it should cost. Returns the opened
/// store and the text to show before any work starts. THE one implementation of "resume": the
/// command line (`camerata inspect --resume`) and the cockpit's "Run what remains" both come
/// through it.
pub fn open_for_resume(
    run_id: &str,
    sources: &[(String, PathBuf)],
    ai_scan_key: &AiScanKey,
    models: &ModelsRecord,
) -> Result<(Arc<RunStore>, String), RunStoreError> {
    let record = read_record(run_id)?;
    let trees = sources
        .iter()
        .map(|(name, dir)| (name.clone(), tree_state(dir)))
        .collect();
    check_resume(&ResumeCheck {
        record: &record,
        ai_scan_key,
        models,
        trees,
    })?;
    let estimate = estimate_resume(run_id)?;
    let store = RunStore::open(run_id)?;
    Ok((store, estimate.render()))
}

/// What the cockpit shows before a resume is confirmed: what remains, what it should cost, and
/// the reason the resume is not possible when it is not.
#[derive(Debug, Clone, Serialize)]
pub struct ResumePreview {
    pub run_id: String,
    pub state: RunState,
    pub estimate: ResumeEstimate,
    /// The text of [`ResumeEstimate::render`], ready to show.
    pub text: String,
    /// Why the resume cannot start now, in plain words; `None` when it can.
    pub refusal: Option<String>,
}

/// The preview for `run_id`. The checks that need only the run and the folders (complete, in use,
/// another version of Camerata, a commit or file that changed) are made here; the models and the
/// rules are compared when the resume starts, and refuse then.
pub fn preview_resume(run_id: &str) -> Result<ResumePreview, RunStoreError> {
    let dir = runs_dir().join(run_id);
    let record = load_record(&dir)?;
    let estimate = journal::estimate_remaining(&dir, &record);
    let trees = record
        .repos
        .iter()
        .map(|r| (r.name.clone(), tree_state(Path::new(&r.dir))))
        .collect();
    let refusal = check_resume(&ResumeCheck {
        record: &record,
        ai_scan_key: &record.ai_scan_key,
        models: &record.models,
        trees,
    })
    .err()
    .map(|e| e.to_string());
    Ok(ResumePreview {
        run_id: record.run_id.clone(),
        state: record.state(liveness_of(&dir)),
        text: estimate.render(),
        estimate,
        refusal,
    })
}

/// The end of an attempt, from the finished report: tells the store which parts failed and
/// whether the run is complete. Shared by the command line and the cockpit's scan job.
pub fn finish_run(store: &RunStore, report: &crate::onboard::ScanReport, export_written: bool) {
    let reasons = report.incomplete_reasons();
    store.finish(&FinishOutcome {
        failures: report
            .failed_passes
            .iter()
            .map(|f| FinishFailure {
                repo: f.repo.clone(),
                pass: f.pass.clone(),
                class: f.class.as_str().to_string(),
                files: f.files.clone(),
                scope_note: f.scope_note,
            })
            .collect(),
        not_run: Vec::new(),
        scan_reached_end: true,
        export_written,
        complete: reasons.is_empty() && export_written,
        incomplete_reasons: reasons,
    });
}

// ── Cleanup ──────────────────────────────────────────────────────────────────────────────────

/// Delete the oldest runs of `project_key` beyond the newest `keep`. A run that is not complete
/// is deleted only when a newer complete run exists for the same commits, and a run a live
/// process works on is never deleted. Returns the ids deleted.
pub fn prune_runs(project_key: &str, keep: usize) -> Vec<String> {
    prune_runs_in(&runs_dir(), project_key, keep)
}

pub fn prune_runs_in(root: &Path, project_key: &str, keep: usize) -> Vec<String> {
    let mut runs: Vec<RunRecord> = list_runs_in(root)
        .into_iter()
        .filter(|r| r.project_key == project_key)
        .filter_map(|r| load_record(&root.join(&r.run_id)).ok())
        .collect();
    runs.sort_by(|a, b| b.run_id.cmp(&a.run_id));
    let commits = |r: &RunRecord| -> Vec<Option<String>> {
        r.repos.iter().map(|x| x.commit.clone()).collect()
    };
    let mut deleted = Vec::new();
    for (idx, run) in runs.iter().enumerate().skip(keep) {
        let dir = root.join(&run.run_id);
        if liveness_of(&dir) == Liveness::Live {
            continue;
        }
        let allowed = run.status == RunStatus::Complete
            || runs[..idx]
                .iter()
                .any(|newer| newer.status == RunStatus::Complete && commits(newer) == commits(run));
        if allowed && std::fs::remove_dir_all(&dir).is_ok() {
            deleted.push(run.run_id.clone());
        }
    }
    deleted
}

#[cfg(test)]
mod tests;
