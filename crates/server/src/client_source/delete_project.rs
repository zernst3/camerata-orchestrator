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

/// What one extra step does when executed.
enum Action {
    Path(PathBuf),
    ScanCache,
    Draft,
    /// Forget the stores' in-memory copies and the scan transcripts, jobs and last scans.
    Memory,
    ProjectRecord,
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
    not_reached: Vec<String>,
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
                blocked.push(ProjectDeletionItem {
                    category: "checkout".to_string(),
                    path: repo.clone(),
                    files: 0,
                    bytes: 0,
                    outcome: PurgeOutcome::Failed { error: r.message },
                });
                claim(state, &project.id, repo, &mut claims)?;
            }
        }
    }

    // Folders of this project's repos, and of every other project's.
    let own_dirs: Vec<PathBuf> = project
        .repos
        .iter()
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
        for repo in project.repos.iter().filter(|r| !shared_repo(r)) {
            let Some(dir) = resolve_dir(state, repo) else {
                continue;
            };
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
    let cache_bytes = state.scan_cache.project_bytes(&project.id);
    if cache_bytes > 0 {
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
    if draft_bytes > 0 {
        add(
            &mut extras,
            step(
                "onboarding draft",
                format!("onboarding-draft.json#{}", project.id),
                0,
                draft_bytes,
                Action::Draft,
            ),
        );
    }

    // The per-project stores (and their stray temp and corrupt files).
    let stores: [(&str, Option<PathBuf>); 4] = [
        ("inspection store", state.inspections.file_for(&project.id)),
        ("decision store", state.decisions.file_for(&project.id)),
        ("edit store", state.edits.file_for(&project.id)),
        (
            "engine check store",
            state.engine_check.file_for(&project.id),
        ),
    ];
    for (cat, main) in stores {
        for f in store_files(main) {
            if std::fs::symlink_metadata(&f).is_ok() {
                add(&mut extras, path_step(cat, f));
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
        not_reached,
    })
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
    let built = build(state, project_id, false).await?;
    let Built {
        project,
        targets,
        _claims,
        extras,
        blocked,
        kept,
        not_reached,
    } = built;

    let mut items: Vec<ProjectDeletionItem> = Vec::new();
    for (target, p) in targets {
        for done in execute(state, &target, p).await {
            items.push(from_purged(&done));
        }
    }
    items.extend(blocked);
    let mut project_step: Option<ProjectDeletionItem> = None;
    let mut memory_step: Option<ProjectDeletionItem> = None;
    for Step { mut item, action } in extras {
        match action {
            Action::ProjectRecord => {
                project_step = Some(item);
                continue;
            }
            Action::Memory => {
                memory_step = Some(item);
                continue;
            }
            Action::Path(path) => {
                item.outcome = run_blocking(move || remove_path(&path)).await;
            }
            Action::ScanCache => {
                state.scan_cache.clear(&project.id);
                item.outcome = PurgeOutcome::Removed;
            }
            Action::Draft => {
                state.draft.clear(&project.id);
                item.outcome = PurgeOutcome::Removed;
            }
        }
        items.push(item);
    }
    if let Some(mut item) = memory_step {
        state.inspections.forget_project(&project.id);
        state.decisions.forget_project(&project.id);
        state.edits.forget_project(&project.id);
        state.engine_check.forget_project(&project.id);
        state.transcripts.clear(crate::SCAN_AUDIT_KEY);
        state.jobs.remove_finished_for_project(&project.id);
        state.forget_project_scans(&project.id, &project.repos);
        item.outcome = PurgeOutcome::Removed;
        items.push(item);
    }
    // The project record goes only when everything else is clear, so a partial run can be retried.
    let all_clear = items.iter().all(|i| i.outcome.is_clear());
    let mut project_removed = false;
    if let Some(mut item) = project_step {
        if all_clear {
            state.projects.delete(&project.id);
            item.outcome = PurgeOutcome::Removed;
            project_removed = true;
            items.push(item);
        }
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
