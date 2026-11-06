//! The `seed-review` command's engine: load a fixture run and a reviewer's work on it into a data
//! directory, so the liveview build and the desktop smoke checklist open a realistic review
//! project without a paid scan (review UI spec sections 13 and 14; work order WO15).
//!
//! # What is written, and how
//!
//! The fixture is two JSON files of inputs (see `fixtures/review-demo/`): `run.json` describes
//! the inspection (its sites and the group reviewer's stage 2 data) and `overlays.json` what the
//! reviewer did to it (settings, decisions, edits, a merge and Engine Check verdicts). Nothing
//! in the data directory is written by hand:
//!
//! - The project, its client name and its review settings go through the project store, by the
//!   same routes the cockpit uses.
//! - The inspection is built from the fixture's sites with the engine's own types and goes into
//!   the inspection store through [`crate::inspections::InspectionStore::append`], which keys
//!   the sites and builds the review tree the way a scan's result does.
//! - Every decision, edit, merge and Engine Check verdict is sent to the server's own route
//!   (`POST /api/projects/:id/decisions` and its neighbours), in process, so each record has the
//!   shape, validation and fingerprint the server gives it, and a seed can never hold a record
//!   the server would not have written.
//!
//! The code the findings point at is real too: the seed makes a small git repository under the data
//! folder (`<data dir>/clones/<owner>-<name>`) whose files hold each finding's snippet at its line,
//! commits it, records it as the repo's checkout and records the commit as the one the inspection
//! read, so the code viewer opens real code and its permalink names a real commit. A rerun (the
//! `moved` variant) is a second commit with the moved lines. It needs `git` on the path, and the
//! commits carry a fixed identity and date so the same fixture gives the same commit ids.
//!
//! The `clean` variant is the base project with every held row released: `clean.json` lists the
//! decisions the reviewer then makes on them and the Engine Check correction. The release gate
//! refuses nothing, so the export panel shows its Export button (the click-through tests need it).
//!
//! The `moved` variant then appends a second inspection built from `moved.json`: the same
//! project after the code changed. Records made against the first run stay where they were
//! written, so some become stale and one site is new since a group decision, exactly as after a
//! real rerun.
//!
//! No model is called and nothing leaves the machine. Nothing is ever deleted: a data directory
//! with anything in it is refused unless `force` is set, and then the project is added beside
//! what is there.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use camerata_api_types::decisions::{DecisionRecorded, DecisionState, NewDecisionReq, Scope};
use camerata_api_types::edits::{EditField, MergeReq, NewEditReq};
use camerata_api_types::review::{
    re_mark, EscalationBudget, EscalationCandidate, EscalationReason, OutlierReason,
};
use camerata_api_types::review_view::{
    CorrectionRef, EngineCheckError, EngineCheckLane, EngineCheckVerdictReq, ExportGateView,
    HoldReason, ReviewShellView,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::inspections::{InspectionEntry, InspectionKind};
use crate::onboard::{Finding, ScanReport};
use crate::review_tree::{DuplicateLink, FlaggedBy, NodeLevel, Outlier, OutlierBy};
use crate::{router, AppState};

// ── the fixture's files ──────────────────────────────────────────────────────────────────

/// `run.json`: the inspection.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunFile {
    #[serde(default)]
    pub about: String,
    pub project: ProjectSpec,
    pub run: RunSpec,
    pub sites: Vec<SiteSpec>,
    /// Sites the engine threw out (each has a `reason`).
    #[serde(default)]
    pub thrown_out: Vec<SiteSpec>,
    pub stage2: Stage2Spec,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectSpec {
    pub name: String,
    pub client_name: String,
    pub repos: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunSpec {
    pub started_at: String,
    pub finished_at: String,
    pub code_lines: usize,
}

/// One site of the run. Its `id` is how the overlays and the moved variant name it.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SiteSpec {
    pub id: String,
    pub path: String,
    pub line: usize,
    pub rule_id: String,
    pub severity: String,
    pub title: String,
    pub body: String,
    pub snippet: String,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub confidence: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub stage1: Option<Stage1Spec>,
    /// The evidence check could not find the quoted code (the row is held until decided).
    #[serde(default)]
    pub evidence_unverified: Option<String>,
    #[serde(default)]
    pub duplicate_of: Option<DuplicateSpec>,
    #[serde(default)]
    pub flagged_by: Vec<FlaggedSpec>,
    /// The row exports as merged into this site, whatever the decisions say.
    #[serde(default)]
    pub merged_into: Option<String>,
    /// A thrown-out site: why the engine threw it out.
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stage1Spec {
    pub real: String,
    pub reason: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DuplicateSpec {
    pub site: String,
    pub hint: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlaggedSpec {
    pub rule_id: String,
    #[serde(default)]
    pub tool: Option<String>,
    pub path: String,
    pub line: usize,
}

/// What the group reviewer wrote.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stage2Spec {
    pub group_reviewed: bool,
    /// In the order of the global rank.
    pub candidates: Vec<CandidateSpec>,
    pub summaries: Vec<SummarySpec>,
    pub outliers: Vec<OutlierSpec>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateSpec {
    pub site: String,
    pub reason: EscalationReason,
    pub severity: String,
    pub why: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SummarySpec {
    /// The sub-group the named site is in.
    pub subgroup_of: String,
    pub text: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutlierSpec {
    pub site: String,
    pub reason: OutlierReason,
    pub by: OutlierBy,
    pub why: String,
}

/// `overlays.json`: the reviewer's work.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OverlayFile {
    #[serde(default)]
    pub about: String,
    pub settings: SettingsSpec,
    pub decisions: Vec<DecisionSpec>,
    pub edits: Vec<EditSpec>,
    pub merges: Vec<MergeSpec>,
    pub engine_check: Vec<EngineCheckSpec>,
    /// The rows the release gate must refuse once the seed is written, per variant. The seed
    /// checks its own result against this, so a fixture that drifts fails loudly.
    pub expected_held: ExpectedHeld,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsSpec {
    pub rate_per_kloc: f32,
    pub floor: usize,
    pub sample_size: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionSpec {
    /// A site decision: the site's id. Exactly one of `site` and `node_of` is set.
    #[serde(default)]
    pub site: Option<String>,
    /// A node decision: the node of `level` that holds this site.
    #[serde(default)]
    pub node_of: Option<String>,
    #[serde(default)]
    pub level: Option<String>,
    pub state: DecisionState,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub decided_by: String,
    #[serde(default)]
    pub decided_at: String,
    #[serde(default)]
    pub needed_me: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditSpec {
    pub site: String,
    pub field: EditField,
    pub new: String,
    pub reason: String,
    #[serde(default)]
    pub by: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MergeSpec {
    pub site: String,
    pub into: String,
    pub reason: String,
    #[serde(default)]
    pub by: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineCheckSpec {
    pub site: String,
    pub right: bool,
    #[serde(default)]
    pub wrong: Option<EngineCheckError>,
    /// A decision the reviewer makes to correct the row; the verdict names it.
    #[serde(default)]
    pub correct_with: Option<CorrectionSpec>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorrectionSpec {
    pub state: DecisionState,
    pub reason: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedHeld {
    pub base: Vec<HeldSpec>,
    pub moved: Vec<HeldSpec>,
    pub clean: Vec<HeldSpec>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeldSpec {
    pub site: String,
    pub reason: HoldReason,
}

/// `moved.json`: how the code changed before the second run.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MovedFile {
    #[serde(default)]
    pub about: String,
    pub run: MovedRun,
    pub moves: Vec<MoveSpec>,
    pub new_sites: Vec<SiteSpec>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MovedRun {
    pub started_at: String,
    pub finished_at: String,
}

/// One site that is at a different place in the second run. A new `snippet` gives it a new key.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MoveSpec {
    pub site: String,
    #[serde(default)]
    pub line: Option<usize>,
    #[serde(default)]
    pub snippet: Option<String>,
}

/// The whole fixture.
#[derive(Debug, Clone)]
pub struct Fixture {
    pub run: RunFile,
    pub overlays: OverlayFile,
    pub moved: MovedFile,
    pub clean: CleanFile,
}

/// `clean.json`: what the reviewer does on top of the base project to release every held row.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CleanFile {
    #[serde(default)]
    pub about: String,
    /// Decisions on the held rows, after the base overlays.
    pub decisions: Vec<DecisionSpec>,
    /// Merges the reviewer records for rows the engine merged on its own (a merge with no
    /// decision record is held).
    pub merges: Vec<MergeSpec>,
    /// Engine Check verdicts that replace the base ones (a No gets its correction).
    pub engine_check: Vec<EngineCheckSpec>,
}

fn read_json<T: DeserializeOwned>(dir: &Path, file: &str) -> Result<T, SeedError> {
    let path = dir.join(file);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| SeedError::Fixture(format!("cannot read {}: {e}", path.display())))?;
    serde_json::from_str(&text)
        .map_err(|e| SeedError::Fixture(format!("{} does not parse: {e}", path.display())))
}

/// Read the fixture directory: `run.json`, `overlays.json` and `moved.json`.
pub fn load_fixture(dir: &Path) -> Result<Fixture, SeedError> {
    Ok(Fixture {
        run: read_json(dir, "run.json")?,
        overlays: read_json(dir, "overlays.json")?,
        moved: read_json(dir, "moved.json")?,
        clean: read_json(dir, "clean.json")?,
    })
}

// ── the command's public face ────────────────────────────────────────────────────────────

/// Which version of the project to write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Variant {
    /// One inspection and the reviewer's work on it.
    #[default]
    Base,
    /// The base project, then a second inspection after the code moved.
    Moved,
    /// The base project with every held row released, so the release gate refuses nothing.
    Clean,
}

/// What the caller asked for besides the two paths.
#[derive(Debug, Clone, Copy, Default)]
pub struct SeedOptions {
    pub variant: Variant,
    /// Seed even though the data directory is not empty. Adds a project beside what is there.
    pub force: bool,
}

/// Why a seed did not happen.
#[derive(Debug)]
pub enum SeedError {
    /// The data directory has something in it and `force` was not set.
    NotEmpty(PathBuf),
    /// The fixture is unreadable, or does not say what the server then produced.
    Fixture(String),
    /// A store or a route refused a write.
    Store(String),
}

impl std::fmt::Display for SeedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotEmpty(p) => write!(
                f,
                "{} is not empty. Seed into a new folder, or pass --force to add the demo \
                 project beside what is there (nothing is deleted or overwritten)",
                p.display()
            ),
            Self::Fixture(m) => write!(f, "fixture: {m}"),
            Self::Store(m) => write!(f, "store: {m}"),
        }
    }
}

impl std::error::Error for SeedError {}

/// What a seed wrote.
#[derive(Debug, Clone)]
pub struct Seeded {
    /// Where the stores are (`<data dir>/camerata`).
    pub store_dir: PathBuf,
    pub project_id: String,
    /// The inspections written, oldest first.
    pub run_ids: Vec<String>,
    /// The review shell as the server reads it back from the stores that were written.
    pub shell: ReviewShellView,
    /// The small git repository the findings' code is read from (under the data folder).
    pub clone_dir: PathBuf,
}

/// The directory the stores live in for a data directory: `<data dir>/camerata`, the same place
/// `CAMERATA_DATA_DIR=<data dir>` makes the server look.
pub fn store_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("camerata")
}

/// Every file under `dir`, relative to it and sorted, for the command to list.
pub fn written_files(dir: &Path) -> Vec<PathBuf> {
    fn walk(root: &Path, at: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(at) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else if let Ok(rel) = path.strip_prefix(root) {
                out.push(rel.to_path_buf());
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

/// Seed `fixture` into `data_dir`.
pub async fn seed_review(
    fixture: &Path,
    data_dir: &Path,
    options: &SeedOptions,
) -> Result<Seeded, SeedError> {
    let fixture = load_fixture(fixture)?;
    check_destination(data_dir, options.force)?;
    let dir = store_dir(data_dir);
    std::fs::create_dir_all(&dir)
        .map_err(|e| SeedError::Store(format!("cannot create {}: {e}", dir.display())))?;
    let state = AppState::from_data_dir(&dir);
    Seeding::new(state, dir, fixture).run(options.variant).await
}

/// Refuse a data directory with anything in it, unless `force`. Never removes anything.
fn check_destination(data_dir: &Path, force: bool) -> Result<(), SeedError> {
    if !data_dir.exists() {
        return Ok(());
    }
    if !data_dir.is_dir() {
        return Err(SeedError::Store(format!(
            "{} is not a folder",
            data_dir.display()
        )));
    }
    let occupied = std::fs::read_dir(data_dir)
        .map_err(|e| SeedError::Store(format!("cannot read {}: {e}", data_dir.display())))?
        .next()
        .is_some();
    if occupied && !force {
        return Err(SeedError::NotEmpty(data_dir.to_path_buf()));
    }
    Ok(())
}

// ── the work ─────────────────────────────────────────────────────────────────────────────

/// A site id to the key the site has in one run.
type Keys = HashMap<String, String>;

struct Seeding {
    state: AppState,
    app: axum::Router,
    store_dir: PathBuf,
    fixture: Fixture,
    project_id: String,
}

impl Seeding {
    fn new(state: AppState, store_dir: PathBuf, fixture: Fixture) -> Self {
        let app = router(state.clone());
        Self {
            state,
            app,
            store_dir,
            fixture,
            project_id: String::new(),
        }
    }

    async fn run(mut self, variant: Variant) -> Result<Seeded, SeedError> {
        let project = self
            .state
            .projects()
            .create(
                &self.fixture.run.project.name,
                self.fixture.run.project.repos.clone(),
            )
            .ok_or_else(|| SeedError::Store("the project store refused a new project".into()))?;
        self.project_id = project.id.clone();

        self.send(
            "set the client name",
            "POST",
            "client-name",
            Some(json!({ "client_name": self.fixture.run.project.client_name })),
        )
        .await?;
        let settings = self.fixture.overlays.settings.clone();
        self.send(
            "set the escalation line",
            "POST",
            "escalation-budget",
            Some(json!({
                "rate_per_kloc": settings.rate_per_kloc,
                "floor": settings.floor,
                "over_budget_reasons": EscalationReason::default_over_budget_set(),
            })),
        )
        .await?;
        self.send(
            "set the Engine Check sample size",
            "POST",
            "sample-size",
            Some(json!({ "sample_size_per_run": settings.sample_size })),
        )
        .await?;

        let first = self.append_run(None)?;
        let mut run_ids = vec![first.run_id.clone()];
        self.apply_overlays(&first.keys).await?;

        let last_keys = match variant {
            Variant::Base => first.keys,
            Variant::Clean => {
                self.apply_clean(&first.keys).await?;
                first.keys
            }
            Variant::Moved => {
                let second = self.append_run(Some(&self.fixture.moved.clone()))?;
                run_ids.push(second.run_id);
                second.keys
            }
        };

        self.check_expected_held(variant, &last_keys).await?;
        let shell: ReviewShellView = self.fetch("the review shell", "review").await?;
        let clone_dir = self.clone_dir();
        Ok(Seeded {
            store_dir: self.store_dir,
            project_id: self.project_id,
            run_ids,
            shell,
            clone_dir,
        })
    }

    /// Build the run (the first, or the moved one) and append it to the inspection store.
    fn append_run(&self, moved: Option<&MovedFile>) -> Result<RunWritten, SeedError> {
        let run = &self.fixture.run;
        let mut sites = run.sites.clone();
        let thrown = run.thrown_out.clone();
        let (started_at, finished_at) = match moved {
            None => (run.run.started_at.clone(), run.run.finished_at.clone()),
            Some(m) => {
                for mv in &m.moves {
                    let site = sites
                        .iter_mut()
                        .find(|s| s.id == mv.site)
                        .ok_or_else(|| unknown_site("moved.json", &mv.site))?;
                    if let Some(line) = mv.line {
                        site.line = line;
                    }
                    if let Some(snippet) = &mv.snippet {
                        site.snippet.clone_from(snippet);
                    }
                }
                sites.extend(m.new_sites.iter().cloned());
                (m.run.started_at.clone(), m.run.finished_at.clone())
            }
        };
        let mut report = ScanReport::gated(&run.project.repos);
        report.gated = false;
        let all_sites: Vec<&SiteSpec> = sites.iter().chain(thrown.iter()).collect();
        report.provenance.audited_refs = self.commit_clone(&all_sites, &started_at)?;
        report.code_lines = run.run.code_lines;
        report.provenance.started_at = started_at;
        report.provenance.finished_at = finished_at;
        for site in &sites {
            report.findings.push(finding_of(&run.project.repos, site));
        }
        for site in &thrown {
            let mut f = finding_of(&run.project.repos, site);
            crate::engine_discard::mark(&mut f, site.reason.as_deref().unwrap_or_default());
            report.discarded.push(f);
        }
        let mut entry =
            InspectionEntry::from_report(&self.project_id, report, InspectionKind::Full);

        // The keys the engine gave the rows.
        let all: Vec<&SiteSpec> = sites.iter().chain(thrown.iter()).collect();
        let keys = resolve_keys(&entry, &all)?;

        link_rows(&mut entry, &all, &keys)?;
        self.apply_stage2(&mut entry, &keys)?;

        let entry = self
            .state
            .inspections()
            .append(entry)
            .map_err(|e| SeedError::Store(format!("cannot save the inspection: {e}")))?;
        Ok(RunWritten {
            run_id: entry.id,
            keys,
        })
    }

    /// Write the sites' files into the clone (making it on the first run), commit them and return
    /// the audited ref the inspection records: the repo, the commit, a clean tree.
    fn commit_clone(
        &self,
        sites: &[&SiteSpec],
        when: &str,
    ) -> Result<Vec<crate::onboard::AuditedRef>, SeedError> {
        let Some(repo) = self.fixture.run.project.repos.first() else {
            return Ok(Vec::new());
        };
        let dir = self.clone_dir();
        let fresh = !dir.join(".git").exists();
        std::fs::create_dir_all(&dir)
            .map_err(|e| SeedError::Store(format!("cannot create {}: {e}", dir.display())))?;
        let origin = format!("https://github.com/{repo}.git");
        let git = |args: &[&str]| -> Result<String, SeedError> {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&dir)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .env("GIT_AUTHOR_NAME", "Camerata seed")
                .env("GIT_AUTHOR_EMAIL", "seed@example.com")
                .env("GIT_COMMITTER_NAME", "Camerata seed")
                .env("GIT_COMMITTER_EMAIL", "seed@example.com")
                .env("GIT_AUTHOR_DATE", when)
                .env("GIT_COMMITTER_DATE", when)
                .output()
                .map_err(|e| {
                    SeedError::Store(format!("git is needed to make the demo repo: {e}"))
                })?;
            if !out.status.success() {
                return Err(SeedError::Store(format!(
                    "git {args:?}: {}",
                    String::from_utf8_lossy(&out.stderr)
                )));
            }
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        };
        if fresh {
            git(&["init", "-q", "-b", "main"])?;
            git(&["remote", "add", "origin", &origin])?;
        }
        for (path, text) in clone_files(sites) {
            let full = dir.join(&path);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    SeedError::Store(format!("cannot create {}: {e}", parent.display()))
                })?;
            }
            std::fs::write(&full, text)
                .map_err(|e| SeedError::Store(format!("cannot write {}: {e}", full.display())))?;
        }
        git(&["add", "-A"])?;
        git(&[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            &format!("The code the inspection of {when} read"),
        ])?;
        let sha = git(&["rev-parse", "HEAD"])?;
        // The folder is this repo's checkout on this machine, exactly as a linked clone is.
        self.state
            .settings
            .set_repo_path(repo, Some(dir.to_string_lossy().into_owned()));
        Ok(vec![crate::onboard::AuditedRef {
            repo: repo.clone(),
            sha: Some(sha),
            branch: Some("main".to_string()),
            dirty: false,
            origin_url: Some(origin),
            ..Default::default()
        }])
    }

    /// Where the demo repo is: `<data dir>/clones/<owner>-<name>`.
    fn clone_dir(&self) -> PathBuf {
        let data_dir = self.store_dir.parent().unwrap_or(&self.store_dir);
        let repo = self
            .fixture
            .run
            .project
            .repos
            .first()
            .map_or("repo".to_string(), |r| r.replace('/', "-"));
        data_dir.join("clones").join(repo)
    }

    /// The group reviewer's stage 2 data: candidates and the line, summaries, outliers.
    fn apply_stage2(&self, entry: &mut InspectionEntry, keys: &Keys) -> Result<(), SeedError> {
        let spec = &self.fixture.run.stage2;
        let settings = &self.fixture.overlays.settings;
        let rule_of: HashMap<&str, &str> = self
            .fixture
            .run
            .sites
            .iter()
            .map(|s| (s.id.as_str(), s.rule_id.as_str()))
            .collect();
        let tree = entry
            .report
            .tree
            .as_mut()
            .ok_or_else(|| SeedError::Fixture("the run has no review tree".into()))?;

        let mut per_node: HashMap<String, u32> = HashMap::new();
        let mut candidates: Vec<EscalationCandidate> = Vec::new();
        for (i, c) in spec.candidates.iter().enumerate() {
            let key = key_of(keys, &c.site, "run.json stage2 candidates")?;
            let rule = rule_of
                .get(c.site.as_str())
                .ok_or_else(|| unknown_site("run.json stage2 candidates", &c.site))?;
            let node_id = tree
                .nodes
                .iter()
                .find(|n| n.level == NodeLevel::Rule && n.id == format!("rule:{rule}"))
                .map(|n| n.id.clone())
                .ok_or_else(|| {
                    SeedError::Fixture(format!("the tree has no rule node for {rule}"))
                })?;
            let rank = per_node.entry(node_id.clone()).or_insert(0);
            *rank += 1;
            candidates.push(EscalationCandidate {
                key,
                node_id,
                reason: c.reason,
                severity: c.severity.clone(),
                stage2_rank: *rank,
                global_rank: u32::try_from(i + 1).unwrap_or(u32::MAX),
                stage2_severity: None,
                slot: 0,
                escalated: false,
                over_budget_reason: None,
                why: c.why.clone(),
            });
        }
        let mut budget = EscalationBudget::compute(
            settings.rate_per_kloc,
            settings.floor,
            self.fixture.run.run.code_lines,
        );
        re_mark(
            &mut candidates,
            &mut budget,
            &EscalationReason::default_over_budget_set(),
        );
        // What a real stage 2 records in the scan ledger beside the tree (`record_ledger` in the
        // group reviewer): the export prints this block in its methodology, and it is worked out
        // again from the tree whenever the reviewer changes the rate.
        let mut by_reason: std::collections::BTreeMap<String, usize> = Default::default();
        let mut over_by_reason: std::collections::BTreeMap<String, usize> = Default::default();
        for c in candidates.iter().filter(|c| c.escalated) {
            *by_reason.entry(c.reason.as_str().to_string()).or_default() += 1;
            if let Some(why) = c.over_budget_reason {
                *over_by_reason.entry(why.as_str().to_string()).or_default() += 1;
            }
        }
        let total_sites = entry.report.findings.len();
        let block = crate::scan_ledger::EscalationBlock {
            model: Some("seed-fixture".to_string()),
            rate_per_kloc: settings.rate_per_kloc,
            floor: settings.floor,
            counted_lines: self.fixture.run.run.code_lines,
            budget: budget.budget,
            candidates: candidates.len(),
            escalated: budget.escalated,
            over_budget: budget.over_budget,
            total_sites,
            share_escalated: if total_sites == 0 {
                0.0
            } else {
                budget.escalated as f64 / total_sites as f64
            },
            by_reason,
            over_budget_by_reason: over_by_reason,
            over_budget_reasons: EscalationReason::default_over_budget_set()
                .iter()
                .map(|r| r.as_str().to_string())
                .collect(),
            unplaced: Vec::new(),
            rules_reviewed: tree
                .nodes
                .iter()
                .filter(|n| n.level == NodeLevel::Rule)
                .count(),
            rules_failed: 0,
        };
        tree.candidates = candidates;
        tree.budget = budget;
        tree.group_reviewed = spec.group_reviewed;

        for s in &spec.summaries {
            let key = key_of(keys, &s.subgroup_of, "run.json stage2 summaries")?;
            let node = tree
                .nodes
                .iter_mut()
                .find(|n| n.level == NodeLevel::Subgroup && n.members.contains(&key))
                .ok_or_else(|| {
                    SeedError::Fixture(format!("{} is in no sub-group", s.subgroup_of))
                })?;
            node.summary = Some(s.text.clone());
        }
        for o in &spec.outliers {
            let key = key_of(keys, &o.site, "run.json stage2 outliers")?;
            let node = tree
                .nodes
                .iter_mut()
                .find(|n| n.level == NodeLevel::Subgroup && n.members.contains(&key))
                .ok_or_else(|| SeedError::Fixture(format!("{} is in no sub-group", o.site)))?;
            node.outliers.push(Outlier {
                key,
                reason: o.reason,
                by: o.by,
                why: o.why.clone(),
            });
        }
        let tree = entry
            .report
            .tree
            .take()
            .ok_or_else(|| SeedError::Fixture("the run has no review tree".into()))?;
        tree.sync_rows(&mut entry.report.findings);
        entry.report.tree = Some(tree);
        entry.report.ledger.record_escalation(block);
        Ok(())
    }

    /// The reviewer's work, in the file's order, each through the route the screens use.
    async fn apply_overlays(&self, keys: &Keys) -> Result<(), SeedError> {
        let overlays = self.fixture.overlays.clone();
        for d in &overlays.decisions {
            self.decide(d, keys).await?;
        }
        for e in &overlays.edits {
            let req = NewEditReq {
                key: key_of(keys, &e.site, "overlays.json edits")?,
                field: e.field,
                new: e.new.clone(),
                reason: e.reason.clone(),
                by: e.by.clone(),
                run_id: String::new(),
            };
            self.send("record an edit", "POST", "edits", Some(to_value(&req)?))
                .await?;
        }
        for m in &overlays.merges {
            let req = MergeReq {
                key: key_of(keys, &m.site, "overlays.json merges")?,
                target: key_of(keys, &m.into, "overlays.json merges")?,
                reason: m.reason.clone(),
                by: m.by.clone(),
            };
            self.send("record a merge", "POST", "merge", Some(to_value(&req)?))
                .await?;
        }
        for v in &overlays.engine_check {
            self.judge(v, keys).await?;
        }
        Ok(())
    }

    /// What the reviewer does after the base overlays to release every held row: the decisions on
    /// them, and the Engine Check verdicts posted again with their corrections.
    async fn apply_clean(&self, keys: &Keys) -> Result<(), SeedError> {
        let clean = self.fixture.clean.clone();
        for d in &clean.decisions {
            self.decide(d, keys).await?;
        }
        for m in &clean.merges {
            let req = MergeReq {
                key: key_of(keys, &m.site, "clean.json merges")?,
                target: key_of(keys, &m.into, "clean.json merges")?,
                reason: m.reason.clone(),
                by: m.by.clone(),
            };
            self.send("record a merge", "POST", "merge", Some(to_value(&req)?))
                .await?;
        }
        for v in &clean.engine_check {
            self.judge(v, keys).await?;
        }
        Ok(())
    }

    async fn decide(&self, d: &DecisionSpec, keys: &Keys) -> Result<DecisionRecorded, SeedError> {
        let (scope, key) = match (&d.site, &d.node_of) {
            (Some(site), None) => (Scope::Site, key_of(keys, site, "overlays.json decisions")?),
            (None, Some(site)) => {
                let key = key_of(keys, site, "overlays.json decisions")?;
                let level = match d.level.as_deref() {
                    Some("subgroup") => NodeLevel::Subgroup,
                    Some("rule") => NodeLevel::Rule,
                    Some("category") => NodeLevel::Category,
                    other => {
                        return Err(SeedError::Fixture(format!(
                            "a node decision names its level (subgroup, rule or category), got \
                             {other:?}"
                        )))
                    }
                };
                let latest = self
                    .state
                    .inspections()
                    .latest(&self.project_id)
                    .ok_or_else(|| SeedError::Store("the inspection is not saved".into()))?;
                let node = latest
                    .report
                    .tree
                    .as_ref()
                    .and_then(|t| {
                        t.nodes
                            .iter()
                            .find(|n| n.level == level && n.members.contains(&key))
                    })
                    .ok_or_else(|| {
                        SeedError::Fixture(format!("{site} is in no node of that level"))
                    })?;
                (Scope::Node(node.id.clone()), String::new())
            }
            _ => {
                return Err(SeedError::Fixture(
                    "a decision names `site` or `node_of`, not both and not neither".into(),
                ))
            }
        };
        let req = NewDecisionReq {
            scope,
            key,
            state: d.state.clone(),
            reason: d.reason.clone(),
            decided_by: d.decided_by.clone(),
            needed_me: d.needed_me,
            sample_verdict: None,
            except: Vec::new(),
            run_id: String::new(),
            decided_at: d.decided_at.clone(),
        };
        self.fetch_from(
            "record a decision",
            "POST",
            "decisions",
            Some(to_value(&req)?),
        )
        .await
    }

    async fn judge(&self, v: &EngineCheckSpec, keys: &Keys) -> Result<(), SeedError> {
        let key = key_of(keys, &v.site, "overlays.json engine_check")?;
        let correction = match &v.correct_with {
            None => None,
            Some(c) => {
                let recorded = self
                    .decide(
                        &DecisionSpec {
                            site: Some(v.site.clone()),
                            node_of: None,
                            level: None,
                            state: c.state.clone(),
                            reason: c.reason.clone(),
                            decided_by: String::new(),
                            decided_at: String::new(),
                            needed_me: None,
                        },
                        keys,
                    )
                    .await?;
                Some(CorrectionRef::Decision(recorded.decision.id))
            }
        };
        let req = EngineCheckVerdictReq {
            right: v.right,
            wrong: v.wrong,
            correction,
            by: String::new(),
        };
        let lane: EngineCheckLane = self
            .fetch("the Engine Check lane", "review/engine-check")
            .await?;
        if !lane.cards.iter().any(|c| c.site.site_key == key) {
            let mut drawn: Vec<&str> = keys
                .iter()
                .filter(|(_, k)| lane.cards.iter().any(|c| c.site.site_key == **k))
                .map(|(id, _)| id.as_str())
                .collect();
            drawn.sort_unstable();
            return Err(SeedError::Fixture(format!(
                "{} is not one of the {} Engine Check cards drawn for this run. Name a site that \
                 is ({}), or change the sample size in overlays.json",
                v.site,
                lane.drawn,
                drawn.join(", ")
            )));
        }
        self.send(
            "record an Engine Check verdict",
            "POST",
            &format!("review/engine-check/{}", path_key(&key)),
            Some(to_value(&req)?),
        )
        .await?;
        Ok(())
    }

    /// The rows the release gate refuses must be the rows the fixture says it does.
    async fn check_expected_held(&self, variant: Variant, keys: &Keys) -> Result<(), SeedError> {
        let expected = match variant {
            Variant::Base => &self.fixture.overlays.expected_held.base,
            Variant::Moved => &self.fixture.overlays.expected_held.moved,
            Variant::Clean => &self.fixture.overlays.expected_held.clean,
        };
        let mut want: Vec<(String, String)> = Vec::new();
        for h in expected {
            want.push((
                h.reason.phrase().to_string(),
                key_of(keys, &h.site, "overlays.json expected_held")?,
            ));
        }
        want.sort();
        let gate: ExportGateView = self.fetch("the export gate", "review/export-gate").await?;
        let mut got: Vec<(String, String)> = gate
            .held
            .iter()
            .chain(&gate.merged_without_decision)
            .map(|h| (h.reason.phrase().to_string(), h.key.clone()))
            .collect();
        got.sort();
        if got != want {
            return Err(SeedError::Fixture(format!(
                "the release gate refuses {got:?} but overlays.json expects {want:?}"
            )));
        }
        Ok(())
    }

    // ── in-process requests ──────────────────────────────────────────────────────────────

    async fn send(
        &self,
        what: &str,
        method: &str,
        tail: &str,
        body: Option<Value>,
    ) -> Result<String, SeedError> {
        let uri = format!("/api/projects/{}/{tail}", self.project_id);
        let mut builder = Request::builder().method(method).uri(uri);
        let body = match body {
            Some(v) => {
                builder = builder.header("content-type", "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        let request = builder
            .body(body)
            .map_err(|e| SeedError::Store(format!("{what}: {e}")))?;
        let response = self
            .app
            .clone()
            .oneshot(request)
            .await
            .map_err(|e| SeedError::Store(format!("{what}: {e}")))?;
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .map_err(|e| SeedError::Store(format!("{what}: {e}")))?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        // The client-name route answers 200 with `ok: false` for a name it refuses.
        let refused_in_body = serde_json::from_str::<Value>(&text)
            .ok()
            .is_some_and(|v| v["ok"] == Value::Bool(false));
        if status != StatusCode::OK || refused_in_body {
            return Err(SeedError::Store(format!("{what}: {status} {text}")));
        }
        Ok(text)
    }

    async fn fetch_from<T: DeserializeOwned>(
        &self,
        what: &str,
        method: &str,
        tail: &str,
        body: Option<Value>,
    ) -> Result<T, SeedError> {
        let text = self.send(what, method, tail, body).await?;
        serde_json::from_str(&text)
            .map_err(|e| SeedError::Store(format!("{what}: the reply does not parse: {e}")))
    }

    async fn fetch<T: DeserializeOwned>(&self, what: &str, tail: &str) -> Result<T, SeedError> {
        self.fetch_from(what, "GET", tail, None).await
    }
}

/// An inspection that was appended: its id and the key of each site in it.
struct RunWritten {
    run_id: String,
    keys: Keys,
}

fn to_value<T: Serialize>(v: &T) -> Result<Value, SeedError> {
    serde_json::to_value(v).map_err(|e| SeedError::Store(e.to_string()))
}

fn unknown_site(file: &str, id: &str) -> SeedError {
    SeedError::Fixture(format!(
        "{file} names a site {id} that run.json does not have"
    ))
}

fn key_of(keys: &Keys, id: &str, file: &str) -> Result<String, SeedError> {
    keys.get(id).cloned().ok_or_else(|| unknown_site(file, id))
}

/// A path segment for a site key (a key holds NUL bytes): every byte percent-encoded.
fn path_key(key: &str) -> String {
    key.bytes().map(|b| format!("%{b:02X}")).collect()
}

/// A finding row for `site`, before the engine has keyed it.
/// The files of the demo repo: for each path, a file whose lines hold the sites' snippets at the
/// sites' lines and a comment everywhere else.
fn clone_files(sites: &[&SiteSpec]) -> std::collections::BTreeMap<String, String> {
    let mut by_path: std::collections::BTreeMap<&str, std::collections::BTreeMap<usize, &str>> =
        Default::default();
    for site in sites {
        let lines = by_path.entry(site.path.as_str()).or_default();
        for (i, line) in site.snippet.lines().enumerate() {
            lines.insert(site.line + i, line);
        }
    }
    by_path
        .into_iter()
        .map(|(path, lines)| {
            let last = lines.keys().next_back().copied().unwrap_or(1) + 2;
            let mut text = String::new();
            for n in 1..=last {
                match lines.get(&n) {
                    Some(code) => text.push_str(code),
                    None => text.push_str(&format!("// {path} line {n}")),
                }
                text.push('\n');
            }
            (path.to_string(), text)
        })
        .collect()
}

fn finding_of(repos: &[String], site: &SiteSpec) -> Finding {
    let mut f = Finding {
        repo: repos.first().cloned().unwrap_or_default(),
        path: site.path.clone(),
        line: site.line,
        rule_id: site.rule_id.clone(),
        severity: site.severity.clone(),
        snippet: site.snippet.clone(),
        detail: format!("{} \u{2014} {}", site.title, site.body),
        kind: Some(camerata_api_types::audit::FindingKind::Defect),
        effort: site.effort.clone(),
        confidence: site.confidence.clone(),
        category: site.category.clone(),
        ..Finding::default()
    };
    if let Some(s1) = &site.stage1 {
        // The shape the stage 1 verdict is stored in on a scanned row.
        f.captures.insert(
            "stage1_verdict".to_string(),
            json!({ "real": s1.real, "reason": s1.reason }).to_string(),
        );
    }
    if let Some(note) = &site.evidence_unverified {
        f.captures.insert(
            crate::onboard::evidence_check::EVIDENCE_UNVERIFIED_CAPTURE.to_string(),
            note.clone(),
        );
    }
    f
}

/// The key of each site in the entry's rows, by site id. A site is found by its place and rule,
/// so two sites of the fixture may not share all three.
fn resolve_keys(entry: &InspectionEntry, sites: &[&SiteSpec]) -> Result<Keys, SeedError> {
    let mut keys = Keys::new();
    for site in sites {
        let mut found = entry
            .report
            .findings
            .iter()
            .chain(&entry.report.discarded)
            .filter(|f| f.path == site.path && f.line == site.line && f.rule_id == site.rule_id);
        let key = match (found.next(), found.next()) {
            (Some(f), None) => f.site_key.clone(),
            (None, _) => {
                return Err(SeedError::Fixture(format!(
                    "site {} is not in the run the engine built (it was merged into another \
                     row, or dropped)",
                    site.id
                )))
            }
            (Some(_), Some(_)) => {
                return Err(SeedError::Fixture(format!(
                    "site {} shares its path, line and rule with another site",
                    site.id
                )))
            }
        };
        if keys.insert(site.id.clone(), key).is_some() {
            return Err(SeedError::Fixture(format!(
                "two sites are called {}",
                site.id
            )));
        }
    }
    Ok(keys)
}

/// The fields of a row that name another row's key: a merge pointer, a possible duplicate and
/// the rows a tool flagged. They need the keys, so they are set after the engine has keyed.
fn link_rows(
    entry: &mut InspectionEntry,
    sites: &[&SiteSpec],
    keys: &Keys,
) -> Result<(), SeedError> {
    let place: HashMap<&str, (&str, usize)> = sites
        .iter()
        .map(|s| (s.id.as_str(), (s.path.as_str(), s.line)))
        .collect();
    for site in sites {
        let own = key_of(keys, &site.id, "run.json")?;
        let Some(row) = entry
            .report
            .findings
            .iter_mut()
            .chain(entry.report.discarded.iter_mut())
            .find(|f| f.site_key == own)
        else {
            continue;
        };
        if let Some(target) = &site.merged_into {
            let (path, line) = place
                .get(target.as_str())
                .ok_or_else(|| unknown_site("run.json merged_into", target))?;
            row.captures.insert(
                crate::report_export::overlay::MERGED_INTO_CAPTURE.to_string(),
                key_of(keys, target, "run.json merged_into")?,
            );
            row.captures.insert(
                crate::report_export::overlay::MERGED_LABEL_CAPTURE.to_string(),
                format!("Merged into {path}:{line}"),
            );
        }
        if let Some(dup) = &site.duplicate_of {
            row.possible_duplicate_of = Some(DuplicateLink {
                key: key_of(keys, &dup.site, "run.json duplicate_of")?,
                hint: dup.hint.clone(),
            });
        }
        for flag in &site.flagged_by {
            row.flagged_by.push(FlaggedBy {
                rule_id: flag.rule_id.clone(),
                tool: flag.tool.clone(),
                site_key: camerata_api_types::site_key::site_key(
                    &row.repo,
                    &flag.path,
                    &flag.rule_id,
                    &row.snippet,
                    0,
                ),
                path: flag.path.clone(),
                line: flag.line,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
