//! The Engine Check's verdict store, its hold rule and its sample (review-tree design A2, A2b and
//! A10; review UI spec section 7).
//!
//! The Engine Check is internal quality control of the engine: a fixed sample of rows per run,
//! each with one question for the reviewer, "did the engine get this right?". This module holds
//! what the read model ([`crate::review_view`]) needs and nothing the client ever sees:
//!
//! - [`EngineCheckStore`]: the verdicts, one file per project, the same shape as the decision and
//!   edit stores (append only, atomic writes, a damaged file is moved aside and never replaced).
//! - [`uncorrected_keys`]: the rows a No holds. A No with no correction yet is a row the engine got
//!   wrong and the client must not receive as it stands; this is the rule behind
//!   [`crate::decisions::held_keys`]'s Engine Check seam.
//! - [`draw_sample`]: which rows the run's sample holds. Deterministic from the run id and the
//!   rows, so a rerun of the screen and the gallery agree, and independent of what the reviewer has
//!   decided, so a decision never changes the sample.
//!
//! Nothing here reads a model, and nothing here writes into the sealed report or into any export.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use camerata_api_types::decisions::{effective_by_key, Decision};
use camerata_api_types::edits::Edit;
use camerata_api_types::review::EscalationState;
use camerata_api_types::review_view::{
    CorrectionRef, EngineCheckVerdict, EngineCheckVerdictReq, SampleStratum,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::onboard::{Finding, ScanReport};

/// On-disk schema version of one project's verdict file.
const FILE_VERSION: u32 = 1;

/// Who a verdict is recorded as given by when the request names no one.
pub const DEFAULT_CHECKER: &str = "reviewer";

/// The fewest rows a category with unread rows gets in the sample (design A2).
pub const CATEGORY_FLOOR: usize = 3;

/// The share of the sample, at least [`THROWN_OUT_MINIMUM`] rows, drawn from the rows the engine
/// threw out (design A10 item 2).
pub const THROWN_OUT_SHARE_PERCENT: usize = 10;
/// The fewest thrown-out rows a sample draws, or all of them when fewer exist.
pub const THROWN_OUT_MINIMUM: usize = 5;

/// Why a verdict was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineCheckStoreError {
    /// A No must say what was wrong.
    WrongRequired,
    /// A Yes carries neither a reason it was wrong nor a correction.
    YesCarriesNothing,
    /// The site key is blank.
    KeyRequired,
    /// The project's file was damaged when the store opened.
    Corrupt,
    /// The file could not be written.
    Persist(String),
}

impl EngineCheckStoreError {
    /// The short machine-readable code the route answers with.
    pub fn code(&self) -> &'static str {
        match self {
            Self::WrongRequired => "wrong_required",
            Self::YesCarriesNothing => "yes_carries_nothing",
            Self::KeyRequired => "key_required",
            Self::Corrupt => "store_corrupt",
            Self::Persist(_) => "persist_failed",
        }
    }
}

impl std::fmt::Display for EngineCheckStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongRequired => {
                write!(f, "A No says what was wrong, from the closed list.")
            }
            Self::YesCarriesNothing => write!(
                f,
                "A Yes records the verdict and nothing else: it names no error and no correction."
            ),
            Self::KeyRequired => write!(f, "A verdict names the site it is about."),
            Self::Corrupt => write!(
                f,
                "The Engine Check file for this project is damaged and was moved aside (it is kept \
                 with a .corrupt ending). Verdicts are not loaded and nothing will be written over \
                 it. Restore the file, then restart."
            ),
            Self::Persist(e) => write!(f, "could not save the verdict: {e}"),
        }
    }
}

impl std::error::Error for EngineCheckStoreError {}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ProjectVerdicts {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    verdicts: Vec<EngineCheckVerdict>,
}

#[derive(Deserialize)]
struct StoredFile {
    project_id: String,
    #[serde(flatten)]
    data: ProjectVerdicts,
}

#[derive(Serialize)]
struct StoredFileRef<'a> {
    project_id: &'a str,
    #[serde(flatten)]
    data: &'a ProjectVerdicts,
}

/// File-backed store of Engine Check verdicts, one file per project. `Clone` is a shallow handle.
#[derive(Clone, Default)]
pub struct EngineCheckStore {
    /// `None` = in-memory only (tests, no resolvable data dir).
    dir: Option<Arc<PathBuf>>,
    mem: Arc<Mutex<BTreeMap<String, ProjectVerdicts>>>,
    corrupt: Arc<BTreeSet<String>>,
    /// The project's inspections, to find where a site went between runs. Without them a verdict
    /// holds only the run it was given on.
    inspections: Option<crate::inspections::InspectionStore>,
}

impl EngineCheckStore {
    /// An empty, non-persisted store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Open `dir`, loading every project file in it. A file that does not parse is moved aside to
    /// `<name>.json.corrupt` and never replaced.
    pub fn at(dir: PathBuf) -> Self {
        let mut mem: BTreeMap<String, ProjectVerdicts> = BTreeMap::new();
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
                            "Engine Check file did not parse; moving it aside"
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
            inspections: None,
        }
    }

    /// The same store, reading earlier runs from `inspections` so an uncorrected No is carried
    /// forward to the same site in later runs ([`carry_forward`]).
    #[must_use]
    pub fn with_inspections(mut self, inspections: crate::inspections::InspectionStore) -> Self {
        self.inspections = Some(inspections);
        self
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, ProjectVerdicts>> {
        self.mem
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The file one project's verdicts are persisted in, if the store is file-backed.
    pub fn file_for(&self, project_id: &str) -> Option<PathBuf> {
        self.dir.as_ref().map(|d| {
            d.join(format!(
                "{}.json",
                crate::inspections::file_stem(project_id)
            ))
        })
    }

    /// Whether this project's file was damaged when the store opened.
    pub fn is_corrupt(&self, project_id: &str) -> bool {
        self.corrupt
            .contains(&crate::inspections::file_stem(project_id))
    }

    /// The directory the store persists into, if any.
    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref().map(PathBuf::as_path)
    }

    /// Every verdict of a project, oldest first.
    pub fn list(&self, project_id: &str) -> Vec<EngineCheckVerdict> {
        self.lock()
            .get(project_id)
            .map(|p| p.verdicts.clone())
            .unwrap_or_default()
    }

    /// The verdicts given on `run_id`, the LATEST one per site key.
    pub fn latest_for_run(&self, project_id: &str, run_id: &str) -> Vec<EngineCheckVerdict> {
        latest_per_key(&self.list(project_id), run_id)
    }

    /// Record one verdict. Appends; a later verdict on the same site supersedes the earlier one.
    pub fn record(
        &self,
        project_id: &str,
        key: &str,
        req: &EngineCheckVerdictReq,
        run_id: &str,
        now: &str,
    ) -> Result<EngineCheckVerdict, EngineCheckStoreError> {
        if key.trim().is_empty() {
            return Err(EngineCheckStoreError::KeyRequired);
        }
        if req.right && (req.wrong.is_some() || req.correction.is_some()) {
            return Err(EngineCheckStoreError::YesCarriesNothing);
        }
        if !req.right && req.wrong.is_none() {
            return Err(EngineCheckStoreError::WrongRequired);
        }
        if self.is_corrupt(project_id) {
            return Err(EngineCheckStoreError::Corrupt);
        }
        let by = req.by.trim();
        let verdict = EngineCheckVerdict {
            key: key.to_string(),
            run_id: run_id.to_string(),
            right: req.right,
            wrong: req.wrong,
            correction: req.correction.clone(),
            by: if by.is_empty() { DEFAULT_CHECKER } else { by }.to_string(),
            at: now.to_string(),
        };
        let mut guard = self.lock();
        let mut working = guard.get(project_id).cloned().unwrap_or_default();
        working.verdicts.push(verdict.clone());
        self.persist_locked(project_id, &working)
            .map_err(|e| EngineCheckStoreError::Persist(e.to_string()))?;
        guard.insert(project_id.to_string(), working);
        Ok(verdict)
    }

    /// What the gate and the read model need to know of this project's Engine Check on `run_id`.
    /// The store is read twice over: the copy it holds in memory (and whether the file was
    /// damaged when it opened), and the file itself as it is on disk now. A file that exists and
    /// cannot be read or parsed makes the evidence [`EngineCheckEvidence::Unreadable`], so the
    /// gate refuses rather than guess that no verdict was a No.
    pub fn evidence_for(
        &self,
        project_id: &str,
        run_id: &str,
        edits: Vec<Edit>,
    ) -> EngineCheckEvidence {
        let unreadable = || {
            let name = self.file_for(project_id).map_or_else(
                || format!("{}.json", crate::inspections::file_stem(project_id)),
                |p| {
                    let part = |s: Option<&std::ffi::OsStr>| {
                        s.map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default()
                    };
                    format!(
                        "{}/{}",
                        part(p.parent().and_then(Path::file_name)),
                        part(p.file_name())
                    )
                },
            );
            EngineCheckEvidence::Unreadable { file: name }
        };
        if self.is_corrupt(project_id) {
            return unreadable();
        }
        if let Some(path) = self.file_for(project_id).filter(|p| p.exists()) {
            let parses = std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|s| serde_json::from_str::<StoredFile>(&s).map_err(|e| e.to_string()));
            if parses.is_err() {
                return unreadable();
            }
        }
        EngineCheckEvidence::Recorded {
            run_id: run_id.to_string(),
            verdicts: self.verdicts_in_force(project_id, run_id),
            edits,
        }
    }

    /// The verdicts the hold rule reads for `run_id`: the run's own, and an earlier run's No that
    /// nothing on this run has answered, carried to the site's key here ([`carry_forward`]). A
    /// verdict is per run, but the wrong finding it names does not stop being wrong because the
    /// engine ran again. Without the project's inspections, only the run's own.
    fn verdicts_in_force(&self, project_id: &str, run_id: &str) -> Vec<EngineCheckVerdict> {
        let all = self.list(project_id);
        let Some(entries) = self
            .inspections
            .as_ref()
            .map(|i| i.list(project_id))
            .filter(|_| all.iter().any(|v| v.run_id != run_id))
        else {
            return latest_per_key(&all, run_id);
        };
        let Some(latest) = entries.iter().find(|e| e.id == run_id) else {
            return latest_per_key(&all, run_id);
        };
        let here: HashSet<&str> = latest
            .report
            .findings
            .iter()
            .chain(&latest.report.discarded)
            .map(|f| f.site_key.as_str())
            .collect();
        // Where each earlier run's moved sites went in this one.
        let mut moved: HashMap<&str, HashMap<String, String>> = HashMap::new();
        for run in all
            .iter()
            .map(|v| v.run_id.as_str())
            .filter(|r| *r != run_id)
        {
            if moved.contains_key(run) {
                continue;
            }
            let links = entries
                .iter()
                .find(|e| e.id == run)
                .map(|prev| {
                    crate::review_tree::match_runs(&prev.report.findings, &latest.report.findings)
                        .moved
                        .into_iter()
                        .map(|l| (l.old_key, l.new_key))
                        .collect()
                })
                .unwrap_or_default();
            moved.insert(run, links);
        }
        carry_forward(&all, run_id, |v| {
            if here.contains(v.key.as_str()) {
                Some(v.key.clone())
            } else {
                moved.get(v.run_id.as_str())?.get(&v.key).cloned()
            }
        })
    }

    fn persist_locked(&self, project_id: &str, data: &ProjectVerdicts) -> std::io::Result<()> {
        let (Some(dir), Some(path)) = (self.dir.as_ref(), self.file_for(project_id)) else {
            return Ok(());
        };
        std::fs::create_dir_all(dir.as_path())?;
        let json = serde_json::to_vec(&StoredFileRef {
            project_id,
            data: &ProjectVerdicts {
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

/// What the Engine Check store holds about one run, as the release gate and the review read model
/// need it. Built from the store by [`EngineCheckStore::evidence_for`]; a caller that holds no
/// store (the headless `camerata inspect`, a test of something else) carries
/// [`EngineCheckEvidence::NotRecorded`], which holds nothing.
#[derive(Debug, Clone, Default)]
pub enum EngineCheckEvidence {
    /// No Engine Check store was consulted. Nothing is held on the Engine Check's account.
    #[default]
    NotRecorded,
    /// The store was read: the verdicts of `run_id` and the live edits of the project.
    Recorded {
        run_id: String,
        verdicts: Vec<EngineCheckVerdict>,
        edits: Vec<Edit>,
    },
    /// The store exists but could not be read or parsed. The gate refuses, naming `file`: with
    /// the verdicts unknown, no No can be ruled out.
    Unreadable { file: String },
}

impl EngineCheckEvidence {
    /// The site keys an Engine Check No holds, given the live `decisions`
    /// ([`uncorrected_keys`]). Empty when nothing was recorded or the store was unreadable (the
    /// unreadable store is its own refusal, see [`Self::unreadable_file`]).
    pub fn uncorrected(&self, decisions: &[Decision]) -> Vec<String> {
        match self {
            Self::Recorded {
                run_id,
                verdicts,
                edits,
            } => uncorrected_keys(verdicts, run_id, decisions, edits),
            Self::NotRecorded | Self::Unreadable { .. } => Vec::new(),
        }
    }

    /// The file that could not be read, when the store was unreadable.
    pub fn unreadable_file(&self) -> Option<&str> {
        match self {
            Self::Unreadable { file } => Some(file),
            Self::NotRecorded | Self::Recorded { .. } => None,
        }
    }
}

/// The latest verdict per site key among those given on `run_id`, in the order the keys were first
/// judged.
pub fn latest_per_key(verdicts: &[EngineCheckVerdict], run_id: &str) -> Vec<EngineCheckVerdict> {
    let mut order: Vec<&str> = Vec::new();
    let mut latest: HashMap<&str, &EngineCheckVerdict> = HashMap::new();
    for v in verdicts.iter().filter(|v| v.run_id == run_id) {
        if latest.insert(v.key.as_str(), v).is_none() {
            order.push(v.key.as_str());
        }
    }
    order.into_iter().map(|k| latest[k].clone()).collect()
}

/// The verdicts that stand for `run_id`: for each site of that run, its latest verdict in the
/// order they were given, whichever run it was given on. `site_in_run` says where a verdict's
/// site is in `run_id` (its own key when the site is unchanged, its new key when the engine found
/// it moved, `None` when it is gone). A verdict of this run stands as it is; a No from an earlier
/// run stands as a No on the new key, with its original time, so a correction or a decision made
/// since still clears it. A Yes from an earlier run stands for nothing. A later verdict on the
/// site, on any run, replaces an earlier one, so a Yes on this run clears an old No.
pub fn carry_forward(
    all: &[EngineCheckVerdict],
    run_id: &str,
    site_in_run: impl Fn(&EngineCheckVerdict) -> Option<String>,
) -> Vec<EngineCheckVerdict> {
    let mut order: Vec<String> = Vec::new();
    let mut latest: HashMap<String, &EngineCheckVerdict> = HashMap::new();
    for v in all {
        let key = if v.run_id == run_id {
            Some(v.key.clone())
        } else {
            site_in_run(v)
        };
        if let Some(key) = key {
            if latest.insert(key.clone(), v).is_none() {
                order.push(key);
            }
        }
    }
    order
        .into_iter()
        .filter_map(|key| {
            let v = latest[&key];
            if v.run_id == run_id {
                Some(v.clone())
            } else if v.right {
                None
            } else {
                Some(EngineCheckVerdict {
                    key,
                    run_id: run_id.to_string(),
                    ..v.clone()
                })
            }
        })
        .collect()
}

// ── the hold rule ─────────────────────────────────────────────────────────────────────────

/// The site keys an Engine Check No holds from the client export: the latest verdict on the site
/// is a No, and nothing corrects it. A No is corrected when it names a correction that exists, or
/// when the reviewer has decided the site or edited it LATER than the verdict. `decisions` and
/// `edits` are the live (not revoked, not discarded) records.
///
/// "Later" is a strictly later time. A record made in the same second as the verdict, or one whose
/// time cannot be read, does not count: the row stays held until a correction is named or a later
/// record exists, so a doubt never lets a wrong finding through.
pub fn uncorrected_keys(
    verdicts: &[EngineCheckVerdict],
    run_id: &str,
    decisions: &[Decision],
    edits: &[Edit],
) -> Vec<String> {
    let decided = effective_by_key(decisions);
    let later = |record_at: &str, verdict_at: &str| match (
        chrono::DateTime::parse_from_rfc3339(record_at),
        chrono::DateTime::parse_from_rfc3339(verdict_at),
    ) {
        (Ok(record), Ok(verdict)) => record > verdict,
        _ => false,
    };
    latest_per_key(verdicts, run_id)
        .into_iter()
        .filter(|v| !v.right)
        .filter(|v| {
            let named = match &v.correction {
                Some(CorrectionRef::Decision(id)) => decisions.iter().any(|d| d.id == *id),
                Some(CorrectionRef::Edit(id)) => edits.iter().any(|e| e.id == *id),
                None => false,
            };
            let decided_since = decided
                .get(v.key.as_str())
                .is_some_and(|d| later(&d.decided_at, &v.at));
            let edited_since = edits.iter().any(|e| e.key == v.key && later(&e.at, &v.at));
            !(named || decided_since || edited_since)
        })
        .map(|v| v.key)
        .collect()
}

// ── the sample ────────────────────────────────────────────────────────────────────────────

/// The rows a run's sample holds (design A2 and A10).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sample {
    /// Kept rows to check, by category (alphabetical), each category's keys in draw order.
    pub kept: Vec<(String, Vec<String>)>,
    /// Thrown-out rows to check, in draw order.
    pub thrown_out: Vec<String>,
    /// Unread kept rows per category (the pool each category drew from).
    pub unread: BTreeMap<String, usize>,
    /// Unread kept rows in all.
    pub unread_total: usize,
    /// Thrown-out rows in all.
    pub thrown_out_total: usize,
}

impl Sample {
    /// Rows drawn in all.
    pub fn drawn(&self) -> usize {
        self.kept.iter().map(|(_, k)| k.len()).sum::<usize>() + self.thrown_out.len()
    }

    /// Every drawn key with its stratum, in card order: kept rows by category, then thrown-out.
    pub fn cards(&self) -> Vec<(String, String, SampleStratum)> {
        let mut out = Vec::with_capacity(self.drawn());
        for (category, keys) in &self.kept {
            for key in keys {
                out.push((category.clone(), key.clone(), SampleStratum::Kept));
            }
        }
        for key in &self.thrown_out {
            out.push((String::new(), key.clone(), SampleStratum::ThrownOut));
        }
        out
    }
}

/// The category a row belongs to for the sample: its tree category, else its own category.
pub fn category_of(f: &Finding) -> String {
    f.group_path
        .first()
        .and_then(|id| id.strip_prefix("cat:"))
        .map(|c| c.trim_end_matches(crate::review_tree::THROWN_OUT_SUFFIX))
        .map(str::to_string)
        .or_else(|| f.category.clone())
        .filter(|c| !c.trim().is_empty())
        .unwrap_or_else(|| "uncategorised".to_string())
}

/// A row's place in the draw order: a hash of the run and the row, so the order is fixed for a run
/// and unrelated to anything the reviewer does.
fn draw_order(run_id: &str, key: &str) -> u64 {
    let digest = Sha256::digest(format!("{run_id}\u{0}{key}").as_bytes());
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(bytes)
}

/// Rows the sample draws the kept rows from: every row the group reviewer did not put in the
/// queue (an escalated row is read by the reviewer by construction). The pool does not look at
/// decisions, so deciding a row never changes the sample.
fn is_unread(f: &Finding) -> bool {
    f.review.escalation_state != EscalationState::Escalated
}

/// Draw the run's sample of `size` rows from `report`.
///
/// - The thrown-out stratum comes first: `max(5, 10 percent of size)` thrown-out rows, or all of
///   them when fewer exist (design A10 item 2).
/// - The rest is drawn from the unread kept rows, allocated across categories in proportion to
///   their unread rows, with at least [`CATEGORY_FLOOR`] for every category that has that many
///   (design A2). A category's floor is honoured even when that puts the total above `size`.
/// - Within a category rows are taken in [`draw_order`].
///
/// A `size` of 0 draws nothing (the lane is off).
pub fn draw_sample(report: &ScanReport, size: usize, run_id: &str) -> Sample {
    let mut by_category: BTreeMap<String, Vec<&Finding>> = BTreeMap::new();
    for f in report.findings.iter().filter(|f| is_unread(f)) {
        by_category.entry(category_of(f)).or_default().push(f);
    }
    let unread: BTreeMap<String, usize> = by_category
        .iter()
        .map(|(c, rows)| (c.clone(), rows.len()))
        .collect();
    let unread_total = unread.values().sum();
    let thrown_out_total = report.discarded.len();
    let mut sample = Sample {
        unread,
        unread_total,
        thrown_out_total,
        ..Sample::default()
    };
    if size == 0 {
        return sample;
    }

    // The thrown-out stratum.
    let want_thrown = THROWN_OUT_MINIMUM.max((size * THROWN_OUT_SHARE_PERCENT).div_ceil(100));
    let mut thrown: Vec<&Finding> = report.discarded.iter().collect();
    thrown.sort_by_key(|f| (draw_order(run_id, &f.site_key), f.site_key.clone()));
    sample.thrown_out = thrown
        .into_iter()
        .take(want_thrown)
        .map(|f| f.site_key.clone())
        .collect();

    // The kept stratum: the rest of the size.
    let kept_size = size.saturating_sub(sample.thrown_out.len());
    let counts: Vec<(&String, usize)> = by_category.iter().map(|(c, r)| (c, r.len())).collect();
    let alloc = allocate(&counts, kept_size);
    for (category, rows) in &mut by_category {
        let take = alloc.get(category).copied().unwrap_or(0);
        rows.sort_by_key(|f| (draw_order(run_id, &f.site_key), f.site_key.clone()));
        sample.kept.push((
            category.clone(),
            rows.iter().take(take).map(|f| f.site_key.clone()).collect(),
        ));
    }
    sample
}

/// Split `size` across categories by unread rows, with a floor of [`CATEGORY_FLOOR`] each (or all
/// of a category's rows when it has fewer), never more than a category has.
///
/// The floors come first. What is left of `size` is shared out in proportion to each category's
/// rows beyond its floor, by the largest remainder, ties broken by category name, so the same
/// counts always give the same split. When the floors alone reach `size` they are all a category
/// gets.
fn allocate(counts: &[(&String, usize)], size: usize) -> BTreeMap<String, usize> {
    let mut out: BTreeMap<String, usize> = counts
        .iter()
        .map(|(c, n)| ((*c).clone(), (*n).min(CATEGORY_FLOOR)))
        .collect();
    let floors: usize = out.values().sum();
    // Rows each category has beyond its floor.
    let room: Vec<(&String, usize)> = counts.iter().map(|(c, n)| (*c, n - out[*c])).collect();
    let total_room: usize = room.iter().map(|(_, r)| r).sum();
    let spare = size.saturating_sub(floors).min(total_room);
    if spare == 0 {
        return out;
    }
    // Whole-number shares of the spare: a quotient and a remainder for each category.
    let mut shares: Vec<(&String, usize, usize)> = room
        .iter()
        .map(|(c, r)| (*c, spare * r / total_room, spare * r % total_room))
        .collect();
    let mut left = spare - shares.iter().map(|(_, q, _)| q).sum::<usize>();
    shares.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(b.0)));
    for (c, quotient, _) in &mut shares {
        if left > 0 {
            *quotient += 1;
            left -= 1;
        }
        *out.get_mut(c.as_str()).expect("allocated") += *quotient;
    }
    out
}

#[cfg(test)]
mod tests;
