//! "Delete this project and all its data": the second, separate deletion. The source-only delete
//! (`delete.rs`) keeps the findings on purpose; this one removes the whole project and every copy
//! of its data Camerata holds, so "your code is deleted" can be true at engagement end.
//!
//! It reuses the per-repo purge (worktrees, checkout, the CLI cache and ledger of the checkout)
//! and adds the project-wide stores: inspections, the onboarding draft entry, decisions, edits,
//! the Engine Check, every scan-cache slice, saved runs (with their finder rooms and traces),
//! finder run files, Claude Code session files recorded for the project, in-memory copies, the
//! stray `.tmp` and `.corrupt` files of those stores, and finally the project record itself.
//!
//! Never touched: anything outside Camerata's own folders and this project's own checkouts, and
//! any other project's data. A checkout, run or multi-repo cache set that another project also
//! uses is KEPT and named in the record.
//!
//! The deletion record (names, paths and counts, no content) is written to
//! `<data dir>/deletion-records/`, a folder no deletion removes. The project record is removed
//! only when every other item is clear, so a partial deletion can be retried.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use axum::extract::{Path as AxPath, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use super::guard::{guard, run_in_flight_refusal, GuardedTarget, Intent};
use super::ops::OpGuard;
use super::purge::{execute, find_session_paths, is_session_id, plan, remove_path, walk_totals};
use super::records::{
    format_bytes, group_thousands, now_rfc3339, PurgeOutcome, PurgedItem, PurgedKind,
};
use super::status::resolve_dir;
use super::Refusal;
use crate::project::Project;
use crate::scan_cache::state_dir::{project_set_stems_for, repo_state_key};
use crate::AppState;

/// Folder of the deletion records, under the data dir. No deletion removes it.
pub const RECORDS_FOLDER: &str = "deletion-records";

/// What a person must do by hand: Camerata cannot reach these.
const MANUAL_STEPS: [&str; 6] = [
    "Time Machine and other backups: delete all backups of the checkout folder, Camerata's data \
     folder, Camerata's cache folder and the Claude Code session folder, or let them age out.",
    "iCloud and cloud drives: if Desktop or Documents sync is on and any folder above sits under \
     it, delete there and empty Recently Deleted. Check other Macs too.",
    "Exports saved elsewhere: the client package (ZIP, PDF, workbook) is downloaded by the \
     browser, so Camerata does not know where it was saved. Delete every copy, including email \
     attachments and uploads.",
    "Claude Code sessions Camerata did not record (chat, governed development, finder rooms) \
     stay under the Claude Code session folder.",
    "Temporary folders and any clone made by hand elsewhere are not tracked by Camerata.",
    "Verify: quit the app, then search Camerata's folders and the workspace for a client \
     identifier (see the client data deletion runbook, step 18).",
];

/// Body of `POST .../delete-all`.
#[derive(Debug, Deserialize)]
pub struct DeleteAllRequest {
    /// The project name exactly as the dialog displays it.
    pub confirm: String,
}

/// One location the deletion covered (or, in a preview, will cover).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectDeletionItem {
    pub category: String,
    pub path: String,
    pub files: u64,
    pub bytes: u64,
    #[serde(flatten)]
    pub outcome: PurgeOutcome,
}

impl ProjectDeletionItem {
    fn counts_text(&self) -> String {
        if self.files > 0 {
            format!(
                "{} files, {}",
                group_thousands(self.files),
                format_bytes(self.bytes)
            )
        } else if self.bytes > 0 {
            format_bytes(self.bytes)
        } else {
            "-".to_string()
        }
    }
}

/// Something found and deliberately left alone, with the reason.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KeptItem {
    pub what: String,
    pub why: String,
}

/// The proof of a project deletion. Names, paths and counts only: no content, no code.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectDeletionRecord {
    pub id: String,
    pub project_id: String,
    pub project_name: String,
    pub client_name: String,
    pub repos: Vec<String>,
    pub requested_at: String,
    pub completed_at: String,
    pub camerata_build: String,
    pub items: Vec<ProjectDeletionItem>,
    pub kept: Vec<KeptItem>,
    /// Uncommitted work and unpushed commits that went with the checkouts (counts only).
    #[serde(default)]
    pub lost_work: Vec<String>,
    pub total_files: u64,
    pub total_bytes: u64,
    /// The project record itself was removed (false when some item could not be removed).
    pub project_removed: bool,
    pub not_reached: Vec<String>,
}

impl ProjectDeletionRecord {
    /// The record as plain text.
    pub fn render_text(&self) -> String {
        let mut out = String::new();
        out.push_str("Camerata project deletion record\n");
        out.push_str(&format!("Project      {}\n", self.project_name));
        out.push_str(&format!("Client       {}\n", self.client_name));
        out.push_str(&format!("Repositories {}\n", self.repos.join(", ")));
        out.push_str(&format!("Requested    {}\n", self.requested_at));
        out.push_str(&format!("Completed    {}\n", self.completed_at));
        out.push_str(&format!("Build        {}\n", self.camerata_build));
        out.push_str(&format!(
            "Total        {} files, {}\n",
            group_thousands(self.total_files),
            format_bytes(self.total_bytes)
        ));
        out.push_str(if self.project_removed {
            "The project record was removed.\n"
        } else {
            "The project record was NOT removed because some locations could not be removed; \
             fix them and delete again.\n"
        });
        out.push_str("\nLocations\n");
        for i in &self.items {
            let outcome = match &i.outcome {
                PurgeOutcome::Removed => "removed".to_string(),
                PurgeOutcome::AlreadyAbsent => "already absent".to_string(),
                PurgeOutcome::Planned => "planned".to_string(),
                PurgeOutcome::Failed { error } => format!("FAILED: {error}"),
            };
            out.push_str(&format!(
                "  [{}] {} ({}) {}\n",
                i.category,
                i.path,
                i.counts_text(),
                outcome
            ));
        }
        if !self.lost_work.is_empty() {
            out.push_str("\nWork that existed only in a checkout\n");
            for l in &self.lost_work {
                out.push_str(&format!("  {l}\n"));
            }
        }
        if !self.kept.is_empty() {
            out.push_str("\nKept\n");
            for k in &self.kept {
                out.push_str(&format!("  {}: {}\n", k.what, k.why));
            }
        }
        out.push_str("\nNot reached by Camerata (do these by hand)\n");
        for n in &self.not_reached {
            out.push_str(&format!("  - {n}\n"));
        }
        out
    }
}

/// Which development console store a row set belongs to.
#[derive(Clone, Copy)]
enum Dev {
    Uow,
    Story,
    Clarification,
    Resume,
    Routine,
    Checkpoint,
    Escalation,
}

impl Dev {
    /// The file the store keeps in the data dir.
    fn file(self) -> &'static str {
        match self {
            Dev::Uow => "uow.json",
            Dev::Story => "stories.json",
            Dev::Clarification => "clarifications.json",
            Dev::Resume => "clarify-resume.json",
            Dev::Routine => "routines.json",
            Dev::Checkpoint => "checkpoints.json",
            Dev::Escalation => "escalations.json",
        }
    }
}

/// What one extra step does when executed. Steps run in the order of [`Action::phase`].
enum Action {
    Path(PathBuf),
    ScanCache,
    Draft,
    /// This project's development console rows (units of work, stories, questions, routines,
    /// checkpoints, escalations), by the ids found when the plan was built.
    Dev(Dev, Vec<String>),
    /// Forget the stores' in-memory copies and the scan transcripts, jobs and last scans.
    Memory,
    /// A decision, edit or engine-check file (after the memory copies are forgotten).
    StoreFile(PathBuf),
    /// An inspection file: kept until every other item is clear, because the Claude Code
    /// session ids it records are how a retry finds the session files.
    Inspection(PathBuf),
    ProjectRecord,
}

impl Action {
    fn phase(&self) -> u8 {
        match self {
            Action::Path(_) => 0,
            Action::ScanCache | Action::Draft | Action::Dev(..) => 1,
            Action::Memory => 2,
            Action::StoreFile(_) => 3,
            Action::Inspection(_) | Action::ProjectRecord => 4,
        }
    }
}

struct Step {
    item: ProjectDeletionItem,
    action: Action,
}

fn step(category: &str, path: String, files: u64, bytes: u64, action: Action) -> Step {
    Step {
        item: ProjectDeletionItem {
            category: category.to_string(),
            path,
            files,
            bytes,
            outcome: PurgeOutcome::Planned,
        },
        action,
    }
}

fn path_step(category: &str, path: PathBuf) -> Step {
    let (files, bytes) = walk_totals(&path, &[]);
    step(
        category,
        path.to_string_lossy().into_owned(),
        files,
        bytes,
        Action::Path(path),
    )
}

/// Everything found, ready to preview or execute. Holds the repos' operation claims.
struct Built {
    project: Project,
    targets: Vec<(GuardedTarget, super::purge::PurgePlan)>,
    _claims: Vec<OpGuard>,
    extras: Vec<Step>,
    /// Repos whose checkout the guard refused: reported as failed items.
    blocked: Vec<ProjectDeletionItem>,
    kept: Vec<KeptItem>,
    /// Work that exists only in a checkout and goes with it.
    losses: Vec<Loss>,
    not_reached: Vec<String>,
}

/// Uncommitted changes and unpushed commits in one checkout.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Loss {
    pub repo: String,
    pub uncommitted: u64,
    pub unpushed: u64,
}

impl Loss {
    fn sentence(&self) -> String {
        let n = self.uncommitted;
        let m = self.unpushed;
        format!(
            "{}: {n} uncommitted {} and {m} unpushed {} will be lost.",
            self.repo,
            if n == 1 { "change" } else { "changes" },
            if m == 1 { "commit" } else { "commits" },
        )
    }
}

impl Built {
    fn items(&self) -> Vec<ProjectDeletionItem> {
        let mut items: Vec<ProjectDeletionItem> = Vec::new();
        for (_, p) in &self.targets {
            items.extend(p.steps.iter().map(|s| from_purged(&s.item)));
        }
        items.extend(self.extras.iter().map(|s| s.item.clone()));
        items.extend(self.blocked.iter().cloned());
        items
    }
}

fn from_purged(item: &PurgedItem) -> ProjectDeletionItem {
    ProjectDeletionItem {
        category: item.kind.label().to_string(),
        path: item.path.clone(),
        files: item.files,
        bytes: item.bytes,
        outcome: item.outcome.clone(),
    }
}

fn data_dir(state: &AppState) -> Result<PathBuf, Refusal> {
    state.storage_roots.data_dir.clone().ok_or_else(|| {
        Refusal::conflict(
            "no_record_folder",
            "Camerata has no data folder here to keep the deletion record in, so nothing was \
             deleted.",
        )
    })
}

/// The files a file-backed store keeps for one project: the file, its `.tmp` and its `.corrupt`.
fn store_files(main: Option<PathBuf>) -> Vec<PathBuf> {
    let Some(main) = main else {
        return Vec::new();
    };
    vec![
        main.with_extension("json.tmp"),
        main.with_extension("json.corrupt"),
        main,
    ]
}

/// Find everything the deletion would remove. Refuses while a run of the project is in flight.
async fn build(state: &AppState, project_id: &str, preview: bool) -> Result<Built, Refusal> {
    let project = state.projects.get(project_id).ok_or_else(|| {
        Refusal::not_found(
            "unknown_project",
            format!("project not found: {project_id}"),
        )
    })?;
    data_dir(state)?;
    let cli_dir = state.storage_roots.cli_state_dir.clone();

    // A run of the project in flight (a scan, a governed run, a live saved run) refuses it all.
    if state.jobs.has_running_for_project(&project.id) || state.source_ops.scan_running(&project.id)
    {
        return Err(Refusal::conflict(
            "scan_in_progress",
            "A scan is running for this project. Wait for it to finish or cancel it first.",
        ));
    }

    let others: Vec<Project> = state
        .projects
        .list()
        .into_iter()
        .filter(|p| p.id != project.id)
        .collect();
    let shared_with = |repo: &str| -> Option<String> {
        others
            .iter()
            .find(|p| p.repos.iter().any(|r| r.eq_ignore_ascii_case(repo)))
            .map(|p| p.name.clone())
    };

    let mut targets = Vec::new();
    let mut claims = Vec::new();
    let mut blocked = Vec::new();
    let mut kept = Vec::new();
    let mut losses: Vec<Loss> = Vec::new();
    let mut blocked_repos: HashSet<String> = HashSet::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();

    for repo in &project.repos {
        if let Some(refusal) = run_in_flight_refusal(state, &project.id, repo) {
            return Err(refusal);
        }
        if let Some(other) = shared_with(repo) {
            kept.push(KeptItem {
                what: format!("the checkout of {repo}"),
                why: format!(
                    "the project {other} also uses this repository, so its checkout and its \
                     caches stay"
                ),
            });
            claim(state, &project.id, repo, &mut claims)?;
            continue;
        }
        let intent = if preview {
            Intent::Preview
        } else {
            Intent::Delete {
                confirm: repo,
                include_uncommitted: true,
            }
        };
        match guard(state, &project.id, repo, intent).await {
            Ok(target) => {
                let uncommitted = target.dirty_files + target.stashes;
                let unpushed: u64 = target.unpushed.iter().map(|u| u.ahead).sum();
                if uncommitted + unpushed > 0 {
                    losses.push(Loss {
                        repo: repo.clone(),
                        uncommitted,
                        unpushed,
                    });
                }
                let mut p = plan(state, &target).await;
                // The project-wide steps below own these kinds.
                p.steps.retain(|s| {
                    !matches!(
                        s.item.kind,
                        PurgedKind::ServerScanCache
                            | PurgedKind::SavedRun
                            | PurgedKind::ModelSessionFile
                            | PurgedKind::InMemory
                    ) && !is_set_file(&s.item.path)
                });
                p.steps.retain(|s| {
                    seen.insert((s.item.kind.label().to_string(), s.item.path.clone()))
                });
                targets.push((target, p));
            }
            Err(r) if matches!(r.code, "no_path" | "not_a_directory") => {
                claim(state, &project.id, repo, &mut claims)?;
            }
            Err(r) if matches!(r.code, "scan_in_progress" | "governed_run_in_progress") => {
                return Err(r);
            }
            Err(r) => {
                blocked_repos.insert(repo.to_ascii_lowercase());
                blocked.push(ProjectDeletionItem {
                    category: "checkout".to_string(),
                    path: repo.clone(),
                    files: 0,
                    bytes: 0,
                    outcome: PurgeOutcome::Failed {
                        error: format!(
                            "{} The way out: unlink {repo} in the project's repositories (or \
                             point it at the right folder), or remove the folder by hand, then \
                             delete again.",
                            r.message
                        ),
                    },
                });
                claim(state, &project.id, repo, &mut claims)?;
            }
        }
    }

    // Folders of this project's repos, and of every other project's.
    // (A repo whose folder the guard refused is not trusted to be this project's: its folder
    // counts for nothing below.)
    let own_dirs: Vec<PathBuf> = project
        .repos
        .iter()
        .filter(|r| !blocked_repos.contains(&r.to_ascii_lowercase()))
        .filter_map(|r| resolve_dir(state, r))
        .collect();
    let other_dirs: Vec<PathBuf> = others
        .iter()
        .flat_map(|p| p.repos.iter())
        .filter_map(|r| resolve_dir(state, r))
        .collect();
    let shared_repo = |repo: &str| shared_with(repo).is_some();
    let mut extras: Vec<Step> = Vec::new();
    let mut not_reached: Vec<String> = MANUAL_STEPS.iter().map(|s| (*s).to_string()).collect();
    let mut add = |extras: &mut Vec<Step>, s: Step| {
        if seen.insert((s.item.category.clone(), s.item.path.clone())) {
            extras.push(s);
        }
    };

    if let Some(cli) = &cli_dir {
        // Per-checkout bookkeeping that is not already in a repo plan.
        let canon = |p: &Path| crate::onboard::finding_context::canonicalize_existing_prefix(p);
        let other_canon: Vec<PathBuf> = other_dirs.iter().map(|d| canon(d)).collect();
        for repo in project
            .repos
            .iter()
            .filter(|r| !shared_repo(r) && !blocked_repos.contains(&r.to_ascii_lowercase()))
        {
            let Some(dir) = resolve_dir(state, repo) else {
                continue;
            };
            // A folder another project's repo also resolves to is not this project's to clean.
            if other_canon.contains(&canon(&dir)) {
                continue;
            }
            let key = repo_state_key(&dir);
            for (cat, name) in [
                ("CLI scan cache", format!("{key}.cache.json")),
                ("CLI ledger", format!("{key}.ledger.json")),
                ("finder run file", format!("{key}.finder-run.json")),
                ("run difference note", format!("{key}.run-difference.md")),
            ] {
                let p = cli.join(name);
                if std::fs::symlink_metadata(&p).is_ok() {
                    add(&mut extras, path_step(cat, p));
                }
            }
        }
        // Multi-repo sets that cover only this project's folders.
        let (mine, shared) = project_set_stems_for(cli, &own_dirs, &other_dirs);
        for stem in mine {
            for (cat, name) in [
                ("CLI scan cache", format!("{stem}.cache.json")),
                ("CLI ledger", format!("{stem}.ledger.json")),
                ("CLI folder list", format!("{stem}.repos.json")),
                ("finder run file", format!("{stem}.finder-run.json")),
                ("run difference note", format!("{stem}.run-difference.md")),
            ] {
                let p = cli.join(name);
                if std::fs::symlink_metadata(&p).is_ok() {
                    add(&mut extras, path_step(cat, p));
                }
            }
        }
        for stem in shared {
            kept.push(KeptItem {
                what: format!("the multi-repo scan files {stem}"),
                why: "they also cover another project's repository".to_string(),
            });
        }
        // Saved runs, with their finder rooms and traces inside them.
        let runs = crate::run_store::runs_of_project(
            &cli.join("runs"),
            &project.id,
            &own_dirs,
            &other_dirs,
        );
        if !runs.live.is_empty() {
            return Err(Refusal::conflict(
                "scan_in_progress",
                format!(
                    "A saved run of this project is still being worked on ({}). Wait for it to \
                     finish or stop it first.",
                    runs.live.join(", ")
                ),
            ));
        }
        for dir in runs.mine {
            add(&mut extras, path_step("saved run", dir));
        }
        for dir in runs.shared {
            kept.push(KeptItem {
                what: format!("the saved run {}", dir.display()),
                why: "it also covers another project's repository".to_string(),
            });
        }
        // Finder rooms and traces outside any run cannot be told apart by project.
        for name in ["finder-rooms", "finder-traces"] {
            let p = cli.join(name);
            if std::fs::symlink_metadata(&p).is_ok() {
                not_reached.push(format!(
                    "{} exists and cannot be attributed to a project; check it by hand.",
                    p.display()
                ));
            }
        }
    }

    // Claude Code session files recorded on the project's inspections.
    if let Some(claude) = &state.storage_roots.claude_projects_dir {
        let mut ids: Vec<String> = state
            .inspections
            .list(&project.id)
            .into_iter()
            .flat_map(|e| e.model_sessions)
            .map(|s| s.session_id)
            .filter(|id| is_session_id(id))
            .collect();
        ids.sort();
        ids.dedup();
        for id in ids {
            for (path, _) in find_session_paths(claude, &id) {
                add(&mut extras, path_step("model session file", path));
            }
        }
    }

    // Scan cache, draft entry.
    let data = data_dir(state)?;
    let cache_bytes = state.scan_cache.project_bytes(&project.id);
    let in_file =
        |name: &str| ids_in_file(&data.join(name)).map_or(true, |ids| ids.contains(&project.id));
    if cache_bytes > 0 || in_file("scan-cache.json") {
        add(
            &mut extras,
            step(
                "scan cache entry",
                format!("scan-cache.json#{}", project.id),
                0,
                cache_bytes,
                Action::ScanCache,
            ),
        );
    }
    let draft_bytes = state.draft.stored_bytes(&project.id);
    let draft_files = state.draft.files_of(&project.id);
    if draft_bytes > 0 || state.draft.holds_project(&project.id) || !draft_files.is_empty() {
        let source = state
            .draft
            .project_file(&project.id)
            .map_or_else(
                || format!("onboarding-draft.json#{}", project.id),
                |f| f.display().to_string(),
            );
        add(
            &mut extras,
            step(
                "onboarding draft",
                source,
                draft_files.len() as u64,
                draft_bytes,
                Action::Draft,
            ),
        );
    }

    // Each project's draft has its own file, removed above with its `.bak` and set-aside copies.
    // What stays are the shared files from before that split: the shared file and its `.bak`
    // (when a start has not migrated them yet), the one-time backup a migration keeps, and any
    // shared file set aside because it could not be read. A shared file that cannot be split by
    // project is never removed here; each is listed as a leftover when it may hold this
    // project's data.
    if let Ok(read) = std::fs::read_dir(&data) {
        let mut shared: Vec<PathBuf> = read
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name().is_some_and(|n| {
                    let n = n.to_string_lossy();
                    n == "onboarding-draft.json"
                        || n == "onboarding-draft.json.bak"
                        || n.starts_with("onboarding-draft.json.shared-backup")
                        || n.starts_with("onboarding-draft.json.unreadable-")
                })
            })
            .collect();
        shared.sort();
        for p in shared {
            let unreadable = p
                .file_name()
                .is_some_and(|n| n.to_string_lossy().contains(".unreadable-"));
            let names_project = std::fs::read(&p)
                .map_or(true, |bytes| String::from_utf8_lossy(&bytes).contains(&project.id));
            if !unreadable && !names_project {
                continue;
            }
            let why = if unreadable {
                "an onboarding draft file Camerata could not read and set aside; it names no \
                 project, so it may include this project's data. It is not deleted: check it by \
                 hand"
            } else {
                "a shared onboarding draft file from before each project had its own draft \
                 file (kept as the one-time backup of that change); it holds this project's \
                 draft. It is not deleted: check it by hand"
            };
            kept.push(KeptItem {
                what: p.display().to_string(),
                why: why.to_string(),
            });
        }
    }

    // The development console rows of this project (units of work, stories, questions,
    // routines, checkpoints, escalations).
    for (dev, ids, what) in dev_rows(state, &project, &others, &mut kept).await {
        if ids.is_empty() {
            continue;
        }
        add(
            &mut extras,
            step(
                "development console record",
                format!("{}#{} ({} {what})", dev.file(), project.id, ids.len()),
                0,
                0,
                Action::Dev(dev, ids),
            ),
        );
    }

    // The per-project stores (and their stray temp and corrupt files). Decisions, edits and
    // the Engine Check go with the memory step; the inspection files wait for everything else.
    let stores: [(&str, Option<PathBuf>, bool); 4] = [
        (
            "inspection store",
            state.inspections.file_for(&project.id),
            true,
        ),
        (
            "decision store",
            state.decisions.file_for(&project.id),
            false,
        ),
        ("edit store", state.edits.file_for(&project.id), false),
        (
            "engine check store",
            state.engine_check.file_for(&project.id),
            false,
        ),
    ];
    for (cat, main, last) in stores {
        for f in store_files(main) {
            if std::fs::symlink_metadata(&f).is_ok() {
                let st = path_step(cat, f.clone());
                let st = Step {
                    action: if last {
                        Action::Inspection(f)
                    } else {
                        Action::StoreFile(f)
                    },
                    ..st
                };
                add(&mut extras, st);
            }
        }
    }

    add(
        &mut extras,
        step(
            "in-memory copy",
            format!("memory:stores, scans and jobs of {}", project.id),
            0,
            0,
            Action::Memory,
        ),
    );
    add(
        &mut extras,
        step(
            "project record",
            format!("projects.json#{}", project.id),
            0,
            0,
            Action::ProjectRecord,
        ),
    );
    Ok(Built {
        project,
        targets,
        _claims: claims,
        extras,
        blocked,
        kept,
        losses,
        not_reached,
    })
}

/// The ids a file-backed store keeps, read from its file: a list of objects with an `id`, a
/// `units`, `by_project` or `projects` section, or a plain map keyed by id. A missing file holds
/// none; an unreadable one is an error (so it is treated as "may still hold the project").
fn ids_in_file(path: &Path) -> Result<HashSet<String>, String> {
    use serde_json::Value;
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(HashSet::new()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let value: Value =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let from_array = |a: &Vec<Value>| -> HashSet<String> {
        a.iter()
            .filter_map(|v| v.get("id").and_then(Value::as_str).map(str::to_string))
            .collect()
    };
    Ok(match &value {
        Value::Array(a) => from_array(a),
        Value::Object(o) => {
            if let Some(Value::Object(m)) = o.get("units").or_else(|| o.get("by_project")) {
                m.keys().cloned().collect()
            } else if let Some(Value::Array(a)) = o.get("projects") {
                from_array(a)
            } else {
                o.keys().cloned().collect()
            }
        }
        _ => HashSet::new(),
    })
}

/// The development console rows of `project`, by store: attributed by the project id the row
/// records or by its repositories, and only when every repository is this project's alone. A row
/// tied to a repository another project also uses, or to none, is left and listed as kept.
async fn dev_rows(
    state: &AppState,
    project: &Project,
    others: &[Project],
    kept: &mut Vec<KeptItem>,
) -> Vec<(Dev, Vec<String>, &'static str)> {
    enum Who {
        Mine,
        Shared,
        Unassigned,
        Foreign,
    }
    let lower = |r: &String| r.to_ascii_lowercase();
    let own: HashSet<String> = project.repos.iter().map(lower).collect();
    let other: HashSet<String> = others
        .iter()
        .flat_map(|p| p.repos.iter())
        .map(lower)
        .collect();
    let classify = |repos: &[String]| -> Who {
        let repos: Vec<String> = repos.iter().map(lower).collect();
        if repos.is_empty() {
            Who::Unassigned
        } else if repos.iter().all(|r| own.contains(r) && !other.contains(r)) {
            Who::Mine
        } else if repos.iter().any(|r| own.contains(r)) {
            Who::Shared
        } else {
            Who::Foreign
        }
    };

    let mut shared = 0usize;
    let mut unassigned = 0usize;
    let mut uow_ids: Vec<String> = Vec::new();
    for u in state.uow.list() {
        let mut repos: Vec<String> = u.repo().map(str::to_string).into_iter().collect();
        repos.extend(u.intake.repos.iter().map(|s| s.repo.clone()));
        match u.project_id.as_deref() {
            Some(id) if id == project.id => uow_ids.push(u.story_id.clone()),
            Some(_) => {}
            None => match classify(&repos) {
                Who::Mine => uow_ids.push(u.story_id.clone()),
                Who::Shared => shared += 1,
                Who::Unassigned => unassigned += 1,
                Who::Foreign => {}
            },
        }
    }
    let mut story_ids: Vec<String> = Vec::new();
    for story in state.stories.list().await.unwrap_or_default() {
        let repos: Vec<String> = story.targets.iter().map(|t| t.repo.clone()).collect();
        if uow_ids.contains(&story.id) {
            story_ids.push(story.id.clone());
            continue;
        }
        match classify(&repos) {
            Who::Mine => story_ids.push(story.id.clone()),
            Who::Shared => shared += 1,
            Who::Unassigned => unassigned += 1,
            Who::Foreign => {}
        }
    }
    let mut story_scope = uow_ids.clone();
    story_scope.extend(story_ids.iter().cloned());
    story_scope.sort();
    story_scope.dedup();

    let routine_ids: Vec<String> = state
        .routines
        .list_for_project(&project.id)
        .into_iter()
        .map(|r| r.id)
        .collect();
    let checkpoint_ids: Vec<String> = state
        .checkpoints
        .list()
        .into_iter()
        .filter(|c| {
            c.project_id.as_deref() == Some(project.id.as_str())
                || story_scope.contains(&c.story_id)
        })
        .map(|c| c.id)
        .collect();
    let escalation_ids: Vec<String> = state
        .escalations
        .list()
        .into_iter()
        .filter(|e| match e.subject_kind {
            camerata_app_core::escalation::SubjectKind::Uow => story_scope.contains(&e.routine_id),
            camerata_app_core::escalation::SubjectKind::Routine => {
                routine_ids.contains(&e.routine_id)
            }
        })
        .map(|e| e.id)
        .collect();

    if shared > 0 {
        kept.push(KeptItem {
            what: format!("{shared} development console record(s)"),
            why: "they are tied to a repository another project also uses, so they cannot be \
                  attributed to this project alone"
                .to_string(),
        });
    }
    if unassigned > 0 {
        kept.push(KeptItem {
            what: format!("{unassigned} development console record(s)"),
            why: "they name no project and no repository, so Camerata cannot tell whether they \
                  are this project's: they may include this project's, so check them by hand"
                .to_string(),
        });
    }
    kept.push(KeptItem {
        what: "the SQLite logs (artifacts.db, enforcement_catches.db, governance_events.db, \
               orchestrator_decisions.db, feedback.db)"
            .to_string(),
        why: "they hold ids, rule ids and descriptions, not client code, and are not kept per \
              project, so they are not deleted"
            .to_string(),
    });
    kept.push(KeptItem {
        what: "source-records.json".to_string(),
        why: "it holds the paths, counts and dates of earlier source deletions, kept as proof; \
              no code"
            .to_string(),
    });
    // Dependents first, then the stories and units of work they were found through, then the
    // routines their escalations were found through: a write that fails leaves the rows the
    // retry needs to find the rest.
    vec![
        (Dev::Escalation, escalation_ids, "escalations"),
        (Dev::Checkpoint, checkpoint_ids, "checkpoints"),
        (
            Dev::Resume,
            state.clarify_resume.ids_for_stories(&story_scope),
            "resume points",
        ),
        (
            Dev::Clarification,
            state.clarifications.ids_for_stories(&story_scope),
            "questions",
        ),
        (Dev::Story, story_ids, "stories"),
        (Dev::Uow, uow_ids, "units of work"),
        (Dev::Routine, routine_ids, "routines"),
    ]
}

/// Why the last phase (the inspections, then the project record) must not run yet, or `None`:
/// another location is not clear, or a scan or job started on the project during the deletion
/// (the marker refuses new starts; this catches one that began before it was set).
pub(super) fn final_block(
    state: &AppState,
    project: &Project,
    items: &[ProjectDeletionItem],
) -> Option<String> {
    if state.jobs.has_running_for_project(&project.id) || state.source_ops.scan_running(&project.id)
    {
        return Some(
            "a scan started during the deletion; the project is kept so you can retry".to_string(),
        );
    }
    if items.iter().any(|i| !i.outcome.is_clear()) {
        return Some(
            "kept because another location could not be removed; delete again to retry".to_string(),
        );
    }
    None
}

fn failed(file: &Path, e: &std::io::Error) -> PurgeOutcome {
    PurgeOutcome::Failed {
        error: format!("{} could not be written: {e}", file.display()),
    }
}

/// Forget the in-memory copies of the decision, edit and Engine Check stores, the scan
/// transcripts, finished jobs and last scans of the project. (Inspections wait for the end.)
fn forget_console_memory(state: &AppState, project: &Project) {
    state.decisions.forget_project(&project.id);
    state.edits.forget_project(&project.id);
    state.engine_check.forget_project(&project.id);
    state.transcripts.clear(crate::SCAN_AUDIT_KEY);
    state.jobs.remove_finished_for_project(&project.id);
    state.forget_project_scans(&project.id, &project.repos);
}

/// Remove one set of development console rows (the write is checked: on a failure nothing is
/// removed and the item is failed, so a retry finds the rows again).
fn remove_dev(state: &AppState, dev: Dev, ids: &[String], data: &Path) -> PurgeOutcome {
    let result: std::io::Result<()> = match dev {
        Dev::Uow => state.uow.remove_many(ids).map(|_| ()),
        Dev::Story => state.stories.remove_many(ids).map(|_| ()),
        Dev::Routine => state.routines.remove_ids(ids).map(|_| ()),
        Dev::Checkpoint => state.checkpoints.remove_ids(ids).map(|_| ()),
        Dev::Escalation => state.escalations.remove_ids(ids).map(|_| ()),
        Dev::Clarification => state.clarifications.remove_ids(ids).map(|_| ()),
        Dev::Resume => state.clarify_resume.remove_ids(ids).map(|_| ()),
    };
    match result {
        Ok(()) => PurgeOutcome::Removed,
        Err(e) => failed(&data.join(dev.file()), &e),
    }
}

fn is_set_file(path: &str) -> bool {
    Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("project-"))
}

/// Claim a repo that has no guarded checkout, so a scan cannot start on it meanwhile.
fn claim(
    state: &AppState,
    project_id: &str,
    repo: &str,
    claims: &mut Vec<OpGuard>,
) -> Result<(), Refusal> {
    match state.source_ops.try_begin(project_id, repo) {
        Some(c) => {
            claims.push(c);
            Ok(())
        }
        None => Err(Refusal::conflict(
            "scan_in_progress",
            format!("Another operation on {repo} is already running. Wait for it to finish."),
        )),
    }
}

async fn run_blocking(f: impl FnOnce() -> PurgeOutcome + Send + 'static) -> PurgeOutcome {
    tokio::task::spawn_blocking(f)
        .await
        .unwrap_or_else(|e| PurgeOutcome::Failed {
            error: format!("removal task failed: {e}"),
        })
}

fn build_id() -> String {
    format!(
        "{} ({})",
        env!("CARGO_PKG_VERSION"),
        crate::ENGINE_COMMIT.map_or("no commit recorded", |c| c)
    )
}

/// Whether the project still holds data Camerata keeps on this machine: a checkout, scan caches,
/// saved runs, inspections, review stores, session files or development console rows (the
/// project record and the in-memory copies do not count). A run in flight counts as data.
/// The plain "Remove from list" is refused while this is true.
pub async fn project_holds_data(state: &AppState, project_id: &str) -> Option<Refusal> {
    const BASE: &str = "This project still has data on this machine. Use Delete source code from \
                        this machine or Delete project and all its data first.";
    let Ok(built) = build(state, project_id, true).await else {
        return Some(Refusal::conflict("project_holds_data", BASE));
    };
    let holds = built
        .items()
        .iter()
        .any(|i| !matches!(i.category.as_str(), "in-memory copy" | "project record"));
    if !holds {
        return None;
    }
    // A checkout Camerata will not delete is a dead end for the delete too: say how to get out.
    let mut message = BASE.to_string();
    for b in &built.blocked {
        if let PurgeOutcome::Failed { error } = &b.outcome {
            message.push_str(&format!(
                " One folder cannot be deleted by Camerata: {error}"
            ));
        }
    }
    Some(Refusal::conflict("project_holds_data", message))
}

/// The refusal for starting governed work on `story_id` while the project that owns its
/// repository (or that created the unit) is being deleted, or `None`.
pub(crate) fn story_deleting_refusal(state: &AppState, story_id: &str) -> Option<String> {
    let unit = state.uow.get_or_default(story_id);
    let repo = unit.repo().map(str::to_string);
    state
        .projects
        .list()
        .into_iter()
        .filter(|p| {
            unit.project_id.as_deref() == Some(p.id.as_str())
                || repo
                    .as_deref()
                    .is_some_and(|r| p.repos.iter().any(|x| x.eq_ignore_ascii_case(r)))
        })
        .find_map(|p| state.source_ops.deleting_refusal(&p.id))
}

/// What a project deletion ended in.
pub struct DeleteAllOutcome {
    pub record: ProjectDeletionRecord,
    pub partial: bool,
    pub record_path: Option<PathBuf>,
    pub record_not_saved: Option<String>,
}

/// Delete the project and all its data. See the module docs.
pub async fn delete_all(
    state: &AppState,
    project_id: &str,
    req: &DeleteAllRequest,
) -> Result<DeleteAllOutcome, Refusal> {
    let project = state.projects.get(project_id).ok_or_else(|| {
        Refusal::not_found(
            "unknown_project",
            format!("project not found: {project_id}"),
        )
    })?;
    if req.confirm != project.name {
        return Err(Refusal::conflict(
            "confirmation_mismatch",
            format!("Type the project name exactly as shown: {}.", project.name),
        ));
    }
    let requested_at = now_rfc3339();
    // From here until this function ends no scan or job may start on the project; set BEFORE the
    // in-flight checks so nothing slips in between the check and the removal.
    let Some(_deleting) = state.source_ops.begin_deleting(&project.id) else {
        return Err(Refusal::conflict(
            "scan_in_progress",
            "This project is already being deleted. Wait for that to finish.",
        ));
    };
    let built = build(state, project_id, false).await?;
    let Built {
        project,
        targets,
        _claims,
        mut extras,
        blocked,
        kept,
        losses,
        not_reached,
    } = built;

    let mut items: Vec<ProjectDeletionItem> = Vec::new();
    for (target, p) in targets {
        for done in execute(state, &target, p).await {
            items.push(from_purged(&done));
        }
    }
    items.extend(blocked);
    let data = state.storage_roots.data_dir.clone().unwrap_or_default();
    extras.sort_by_key(|s| s.action.phase());
    let mut memory_forgotten = false;
    let mut inspections_forgotten = false;
    let mut dev_failed = false;
    let mut project_item: Option<ProjectDeletionItem> = None;
    for Step { mut item, action } in extras {
        // The last phase (inspections, then the project record) runs only when everything
        // before it is clear, and a scan that slipped in refuses it too.
        if action.phase() == 4 {
            if let Some(why) = final_block(state, &project, &items) {
                item.outcome = PurgeOutcome::Failed { error: why };
                items.push(item);
                continue;
            }
        }
        match action {
            Action::Path(path) | Action::StoreFile(path) => {
                if matches!(
                    item.category.as_str(),
                    "decision store" | "edit store" | "engine check store"
                ) && !memory_forgotten
                {
                    forget_console_memory(state, &project);
                    memory_forgotten = true;
                }
                item.outcome = run_blocking(move || remove_path(&path)).await;
            }
            Action::ScanCache => {
                item.outcome = match state.scan_cache.clear_checked(&project.id) {
                    Ok(()) => PurgeOutcome::Removed,
                    Err(e) => failed(&data.join("scan-cache.json"), &e),
                };
            }
            Action::Draft => {
                item.outcome = match state.draft.clear_checked(&project.id) {
                    Ok(()) => PurgeOutcome::Removed,
                    Err(e) => failed(
                        &state
                            .draft
                            .project_file(&project.id)
                            .unwrap_or_else(|| data.join("onboarding-draft.json")),
                        &e,
                    ),
                };
            }
            Action::Dev(dev, ids) => {
                // Stop at the first failure: later sets are found through earlier ones' rows.
                item.outcome = if dev_failed {
                    PurgeOutcome::Failed {
                        error: "not attempted because an earlier removal failed; delete again to \
                                retry"
                            .to_string(),
                    }
                } else {
                    remove_dev(state, dev, &ids, &data)
                };
                dev_failed |= !item.outcome.is_clear();
            }
            Action::Memory => {
                if !memory_forgotten {
                    forget_console_memory(state, &project);
                    memory_forgotten = true;
                }
                item.outcome = PurgeOutcome::Removed;
            }
            Action::Inspection(path) => {
                if !inspections_forgotten {
                    state.inspections.forget_project(&project.id);
                    inspections_forgotten = true;
                }
                item.outcome = run_blocking(move || remove_path(&path)).await;
            }
            Action::ProjectRecord => {
                project_item = Some(item);
                continue;
            }
        }
        items.push(item);
    }
    if !inspections_forgotten && items.iter().all(|i| i.outcome.is_clear()) {
        state.inspections.forget_project(&project.id);
    }
    // The project record itself goes last, only when everything else is clear.
    let mut project_removed = false;
    if let Some(mut item) = project_item {
        item.outcome = if let Some(why) = final_block(state, &project, &items) {
            PurgeOutcome::Failed { error: why }
        } else {
            match state.projects.delete_checked(&project.id) {
                Ok(true) => {
                    project_removed = true;
                    PurgeOutcome::Removed
                }
                Ok(false) => {
                    project_removed = true;
                    PurgeOutcome::AlreadyAbsent
                }
                Err(e) => failed(&data.join("projects.json"), &e),
            }
        };
        items.push(item);
    }

    let removed = items.iter().filter(|i| i.outcome == PurgeOutcome::Removed);
    let (total_files, total_bytes) =
        removed.fold((0u64, 0u64), |(f, b), i| (f + i.files, b + i.bytes));
    let completed_at = now_rfc3339();
    let compact: String = completed_at.chars().filter(char::is_ascii_digit).collect();
    let record = ProjectDeletionRecord {
        id: format!(
            "project-{}-{compact}",
            crate::inspections::file_stem(&project.id)
        ),
        project_id: project.id.clone(),
        project_name: project.name.clone(),
        client_name: project.client_name.clone(),
        repos: project.repos.clone(),
        requested_at,
        completed_at,
        camerata_build: build_id(),
        items,
        kept,
        lost_work: losses.iter().map(Loss::sentence).collect(),
        total_files,
        total_bytes,
        project_removed,
        not_reached,
    };
    let (record_path, record_not_saved) = match write_record(state, &record) {
        Ok(path) => (Some(path), None),
        Err(e) => {
            tracing::error!(error = %e, "could not persist the project deletion record");
            (None, Some(e.to_string()))
        }
    };
    Ok(DeleteAllOutcome {
        partial: !project_removed,
        record,
        record_path,
        record_not_saved,
    })
}

/// Write the record (JSON and text) into the records folder; returns the JSON file's path.
fn write_record(state: &AppState, record: &ProjectDeletionRecord) -> std::io::Result<PathBuf> {
    let dir = state
        .storage_roots
        .data_dir
        .clone()
        .ok_or_else(|| std::io::Error::other("no data folder"))?
        .join(RECORDS_FOLDER);
    std::fs::create_dir_all(&dir)?;
    let json = dir.join(format!("{}.json", record.id));
    std::fs::write(
        &json,
        serde_json::to_vec_pretty(record).map_err(std::io::Error::other)?,
    )?;
    std::fs::write(dir.join(format!("{}.txt", record.id)), record.render_text())?;
    Ok(json)
}

/// `POST /api/projects/:id/delete-all-preview`: run the checks and show what would be removed.
pub async fn delete_all_preview(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
) -> Response {
    let built = match build(&state, &id, true).await {
        Ok(b) => b,
        Err(r) => return r.into_response(),
    };
    let items = built.items();
    let total_files: u64 = items.iter().map(|i| i.files).sum();
    let total_bytes: u64 = items.iter().map(|i| i.bytes).sum();
    let item_view: Vec<serde_json::Value> = items
        .iter()
        .map(|i| {
            serde_json::json!({
                "label": i.category,
                "path": i.path,
                "counts": i.counts_text(),
            })
        })
        .collect();
    Json(serde_json::json!({
        "ok": true,
        "plan": {
            "project": built.project.name,
            "item_view": item_view,
            "total_summary": format!(
                "{} files, {}", group_thousands(total_files), format_bytes(total_bytes)
            ),
            "kept": built.kept,
            "losses": built.losses,
            "loss_sentences": built.losses.iter().map(Loss::sentence).collect::<Vec<_>>(),
            "warnings": built.blocked.iter().map(|b| match &b.outcome {
                PurgeOutcome::Failed { error } => format!("{}: {error}", b.path),
                _ => b.path.clone(),
            }).collect::<Vec<_>>(),
            "not_reached": built.not_reached,
        },
    }))
    .into_response()
}

/// `POST /api/projects/:id/delete-all`: delete the project and all its data.
pub async fn delete_all_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Json(req): Json<DeleteAllRequest>,
) -> Response {
    match delete_all(&state, &id, &req).await {
        Err(r) => r.into_response(),
        Ok(out) => {
            let failed = out
                .record
                .items
                .iter()
                .filter(|i| !i.outcome.is_clear())
                .count();
            let body = serde_json::json!({
                "ok": !out.partial,
                "code": if out.partial { "partial" } else { "deleted" },
                "message": if out.partial {
                    format!(
                        "{failed} {} could not be removed, so the project was kept; see the record.",
                        if failed == 1 { "location" } else { "locations" }
                    )
                } else {
                    "The project and all its data were deleted.".to_string()
                },
                "record": out.record,
                "record_text": out.record.render_text(),
                "record_path": out.record_path.as_ref().map(|p| p.to_string_lossy().into_owned()),
                "record_not_saved": out.record_not_saved,
            });
            Json(body).into_response()
        }
    }
}
