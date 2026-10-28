//! "Rescan changes": the delta retest between the inspected commit and now.
//!
//! After a re-fetch (or any time the checkout has moved past the inspection) the operator wants to
//! know what changed in the code, not to pay for the whole audit again. The engine already audits
//! only the files that differ from a prior scan manifest and carries forward the prior findings
//! for the rest (`onboard::audit_repos`, `scan_cache::partition`); the deterministic floor always
//! runs over everything. This module builds that prior from facts instead of from a cache:
//!
//! - which files changed comes from `git diff --name-status <inspected> HEAD` (plus whatever is
//!   uncommitted), so the answer is correct even though the server's own cache was purged when
//!   the source was deleted;
//! - what the unchanged files' findings were comes from the persisted inspection entry.
//!
//! Every repo of the inspection is re-run together, because the result REPLACES the project's
//! current inspection: a delta that covered one repo of two would drop the other repo's findings
//! from the review and the exports. So every repo the inspection covered must have its source on
//! disk; if one does not, the operator is told which to re-fetch.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use axum::extract::{Path as AxPath, State};
use axum::response::{IntoResponse, Response};
use axum::Json;

use super::git::{self, commit_exists, head_sha};
use super::resolution::source_deleted_refusal;
use super::status::{deleted_record, project_and_repo, resolve_dir, source_status};
use super::Refusal;
use crate::inspections::InspectionEntry;
use crate::onboard::{Finding, ScanReport};
use crate::scan_cache::{content_fingerprint, ScanManifest, MANIFEST_VERSION};
use crate::scan_ledger::RuleTier;
use crate::workspace::git as run_git;
use crate::AppState;

/// What a "Rescan changes" request produced.
#[derive(Debug)]
pub enum RescanOutcome {
    /// Nothing differs from the inspection in any repo: no job was started.
    NothingChanged,
    /// A delta job is running; poll it like any audit job.
    Started {
        job_id: String,
        notes: Vec<String>,
        /// Files that differ from the inspection, across every repo.
        changed_files: usize,
        /// The requested repo's commits, `from` the inspected one `to` HEAD.
        from_sha: String,
        to_sha: String,
        commits_newer: Option<u64>,
    },
}

/// Every path named by `git diff --name-status -z -M` output: both sides of a rename or copy,
/// the path of an add, modify, delete or type change. Pure.
pub fn parse_name_status_z(stdout: &[u8]) -> HashSet<String> {
    let mut parts = stdout
        .split(|b| *b == 0)
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .filter(|p| !p.is_empty());
    let mut paths = HashSet::new();
    while let Some(status) = parts.next() {
        // `R100` / `C75` carry two paths; everything else carries one.
        let takes = if status.starts_with(['R', 'C']) { 2 } else { 1 };
        for _ in 0..takes {
            if let Some(path) = parts.next() {
                paths.insert(path);
            }
        }
    }
    paths
}

/// Every path in `git status --porcelain -z --untracked-files=all` output (the uncommitted
/// changes), including the old side of a rename. Pure.
pub fn parse_porcelain_z(stdout: &[u8]) -> HashSet<String> {
    let mut entries = stdout
        .split(|b| *b == 0)
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .filter(|p| !p.is_empty());
    let mut paths = HashSet::new();
    while let Some(entry) = entries.next() {
        if entry.len() < 4 {
            continue;
        }
        let renamed = entry.starts_with(['R', 'C']) || entry[1..2].starts_with(['R', 'C']);
        paths.insert(entry[3..].to_string());
        if renamed {
            if let Some(old) = entries.next() {
                paths.insert(old);
            }
        }
    }
    paths
}

/// Whether the AI review ran and completed on the inspection that produced `report`. The same
/// meaning as `ScanManifest::ai_covered`: an AI-off, blocked or AI-errored run must never be
/// treated as "the AI looked and found nothing".
fn ai_covered(report: &ScanReport) -> bool {
    report.provenance.audit_model.is_some()
        && report.ai_error.is_none()
        && report.ai_blocked_reason.is_none()
        && !report.blocked
}

/// Whether `finding` came from the AI (semantic) review, which the delta carries forward for
/// unchanged files. The deterministic floor and the preview linters are recomputed over the
/// whole tree every run and must NOT be carried (they would duplicate); the ledger records the
/// tier each rule ran under, and an ad hoc `AI-` rule the model invented is semantic by nature.
fn is_ai_advisory(finding: &Finding, report: &ScanReport) -> bool {
    if finding.preview {
        return false;
    }
    match report.ledger.rule(&finding.rule_id) {
        Some(entry) => entry.tier == RuleTier::Semantic,
        None => finding.rule_id.starts_with("AI-"),
    }
}

/// One repo's contribution to the synthetic prior.
pub(super) struct RepoDelta {
    /// Fingerprints of every current file that did NOT change.
    pub(super) unchanged: BTreeMap<String, String>,
    /// How many current files differ from the inspection.
    pub(super) changed: usize,
    /// The inspection's AI findings for files that did not change.
    pub(super) carried: Vec<Finding>,
}

/// Build the prior for one repo from the current tree and the set of changed paths.
pub(super) fn repo_delta(
    repo: &str,
    current: &[(String, String)],
    changed: &HashSet<String>,
    entry: &InspectionEntry,
) -> RepoDelta {
    let unchanged: BTreeMap<String, String> = current
        .iter()
        .filter(|(path, _)| !changed.contains(path))
        .map(|(path, content)| (path.clone(), content_fingerprint(content)))
        .collect();
    let changed_count = current
        .iter()
        .filter(|(path, _)| changed.contains(path))
        .count();
    let carried = entry
        .report
        .findings
        .iter()
        .filter(|f| {
            f.repo.eq_ignore_ascii_case(repo)
                && !changed.contains(&f.path)
                && is_ai_advisory(f, &entry.report)
        })
        .cloned()
        .collect();
    RepoDelta {
        unchanged,
        changed: changed_count,
        carried,
    }
}

async fn changed_paths(dir: &Path, from: &str, to: &str) -> Option<HashSet<String>> {
    let out = run_git(Some(dir), &["diff", "--name-status", "-z", "-M", from, to])
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let mut paths = parse_name_status_z(&out.stdout);
    // Uncommitted edits and new files count as changed too: the scan reads the working tree.
    let status = run_git(
        Some(dir),
        &["status", "--porcelain", "-z", "--untracked-files=all"],
    )
    .await
    .ok()?;
    if status.status.success() {
        paths.extend(parse_porcelain_z(&status.stdout));
    }
    Some(paths)
}

/// Start the delta rescan for the project that owns `repo_param`. See the module docs.
pub async fn rescan_changes(
    state: &AppState,
    project_id: &str,
    repo_param: &str,
) -> Result<RescanOutcome, Refusal> {
    let (project, repo) = project_and_repo(state, project_id, repo_param)?;
    if let Some(record) = deleted_record(state, &project.id, &repo) {
        return Err(source_deleted_refusal(&repo, &record.completed_at));
    }
    let Some(entry) = state.inspections.latest(&project.id) else {
        return Err(no_inspection(&repo));
    };
    if entry.sha_for(&repo).is_none() {
        return Err(no_inspection(&repo));
    }
    if state.jobs.has_running_for_project(&project.id) || state.source_ops.scan_running(&project.id)
    {
        return Err(Refusal::conflict(
            "scan_in_progress",
            "A scan is running for this project. Wait for it to finish or cancel it first.",
        ));
    }
    let Some(_claim) = state.source_ops.try_begin(&project.id, &repo) else {
        return Err(Refusal::conflict(
            "scan_in_progress",
            format!("Another operation on {repo} is already running. Wait for it to finish."),
        ));
    };

    // Every repo the inspection covered must be on disk with its inspected commit reachable.
    let mut prior = ScanManifest {
        version: MANIFEST_VERSION,
        // The inspection's own fingerprint: if the rule selection changed since, the engine's
        // `matches_rules` fails and it does a full re-scan, which is the right answer.
        rules_fingerprint: entry.rules_fingerprint.clone(),
        ai_covered: ai_covered(&entry.report),
        // The question the inspection's AI findings answered. An inspection that predates the
        // key has none, so the engine re-scans in full rather than trust answers it cannot
        // place.
        ai_key: entry.report.ledger.ai_key().cloned().unwrap_or_default(),
        clone_verdicts: state
            .scan_cache
            .get(&project.id)
            .map(|m| m.clone_verdicts)
            .unwrap_or_default(),
        ..ScanManifest::default()
    };
    let mut notes = Vec::new();
    let mut changed_files = 0usize;
    let mut anything_differs = false;
    let mut requested: Option<(String, String, Option<u64>)> = None;
    let mut repo_names = Vec::new();

    for covered in &entry.repos {
        let Some(inspected) = covered.sha.as_deref() else {
            continue;
        };
        let name = covered.repo.clone();
        let Some(dir) = resolve_dir(state, &name) else {
            return Err(not_present(&name));
        };
        if let Some(record) = deleted_record(state, &project.id, &name) {
            return Err(source_deleted_refusal(&name, &record.completed_at));
        }
        if !matches!(
            source_status(state, &project.id, &name).await,
            super::status::RepoSourceStatus::Present { .. }
        ) {
            return Err(not_present(&name));
        }
        if !commit_exists(&dir, inspected).await {
            return Err(Refusal::conflict(
                "inspected_commit_unreachable",
                format!(
                    "The inspected commit {inspected} of {name} is not in this clone (history was \
                     rewritten). Run a full re-inspection instead."
                ),
            ));
        }
        let Some(head) = head_sha(&dir).await else {
            return Err(not_present(&name));
        };
        let Some(changed) = changed_paths(&dir, inspected, &head).await else {
            return Err(Refusal::conflict(
                "inspected_commit_unreachable",
                format!("Could not compare {name} with the inspected commit {inspected}."),
            ));
        };
        let current = {
            let dir = dir.clone();
            tokio::task::spawn_blocking(move || crate::onboard::read_local_repo_files(&dir))
                .await
                .map_err(|e| {
                    Refusal::conflict("scan_in_progress", format!("read task failed: {e}"))
                })?
                .map_err(|e| not_present_because(&name, &e.to_string()))?
                .files
        };
        let delta = repo_delta(&name, &current, &changed, &entry);
        if !changed.is_empty() || head != inspected {
            anything_differs = true;
        }
        changed_files += delta.changed;
        prior.files.insert(name.clone(), delta.unchanged);
        prior.findings.extend(delta.carried);
        if covered.dirty {
            notes.push(format!(
                "{name}: the inspection ran on a tree with uncommitted changes; the delta is \
                 computed from the committed state ({inspected}). Consider a full re-inspection."
            ));
        }
        if name.eq_ignore_ascii_case(&repo) {
            let newer = git::commits_between(&dir, inspected, &head).await;
            requested = Some((inspected.to_string(), head.clone(), newer));
        }
        repo_names.push(name);
    }
    let Some((from_sha, to_sha, commits_newer)) = requested else {
        return Err(no_inspection(&repo));
    };
    if !anything_differs {
        return Ok(RescanOutcome::NothingChanged);
    }

    let request = project_scan_request(&project, &repo_names)
        .await
        .map_err(|e| {
            Refusal::conflict("scan_in_progress", format!("could not build the scan: {e}"))
        })?;

    let job_id = crate::start_audit_job(
        state,
        request,
        crate::AuditJobOptions {
            project_id: Some(project.id.clone()),
            prior_override: Some(prior),
            extra_notes: notes.clone(),
            delta: Some(crate::DeltaRetest {
                base_inspection_id: entry.id.clone(),
                from_sha: from_sha.clone(),
                to_sha: to_sha.clone(),
            }),
        },
    )
    .await;
    Ok(RescanOutcome::Started {
        job_id,
        notes,
        changed_files,
        from_sha,
        to_sha,
        commits_newer,
    })
}

/// The scan the project's own settings describe: its selected rules (each with its directive and
/// placement, resolved from the corpus exactly as Apply resolves them), AI review on, the
/// deterministic floor on, incremental. The project's own backend applies when the job runs.
///
/// The rules' placement is part of the incremental cache's rule fingerprint, so the request must
/// be built the same way every time; a delta and the inspection it is computed from only share a
/// fingerprint (and so only carry findings forward) if both came from this.
pub(super) async fn project_scan_request(
    project: &crate::project::Project,
    repos: &[String],
) -> Result<crate::AuditReq, serde_json::Error> {
    let rules: Vec<serde_json::Value> = crate::resolve_project_selection_rules(project)
        .await
        .into_iter()
        .map(|r| serde_json::json!({ "id": r.id, "directive": r.directive, "repos": r.repos }))
        .collect();
    serde_json::from_value(serde_json::json!({
        "repos": repos,
        "rules": rules,
        "incremental": true,
        "run_ai_review": true,
        "run_deterministic": true,
    }))
}

fn no_inspection(repo: &str) -> Refusal {
    Refusal::conflict(
        "no_inspection",
        format!("No inspection has run for {repo} yet; run a scan instead."),
    )
}

fn not_present(repo: &str) -> Refusal {
    Refusal::conflict(
        "source_not_present",
        format!(
            "{repo} has no usable local clone, and the delta rescan re-runs every repo of the \
             inspection. Link or re-fetch it first."
        ),
    )
}

fn not_present_because(repo: &str, why: &str) -> Refusal {
    Refusal::conflict(
        "source_not_present",
        format!("{repo} could not be read ({why}). Link or re-fetch it first."),
    )
}

/// `POST /api/projects/:id/repos/:repo/rescan-changes`.
pub async fn rescan_changes_handler(
    State(state): State<AppState>,
    AxPath((id, repo)): AxPath<(String, String)>,
) -> Response {
    match rescan_changes(&state, &id, &repo).await {
        Err(refusal) => refusal.into_response(),
        Ok(RescanOutcome::NothingChanged) => Json(serde_json::json!({
            "ok": true,
            "nothing_changed": true,
        }))
        .into_response(),
        Ok(RescanOutcome::Started {
            job_id,
            notes,
            changed_files,
            from_sha,
            to_sha,
            commits_newer,
        }) => Json(serde_json::json!({
            "ok": true,
            "job_id": job_id,
            "notes": notes,
            "changed_files": changed_files,
            "from_sha": from_sha,
            "to_sha": to_sha,
            "commits_newer": commits_newer,
        }))
        .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_status_z_names_every_path_including_both_sides_of_a_rename() {
        let out = b"M\0src/a.rs\0A\0src/new.rs\0D\0src/gone.rs\0R100\0old/name.rs\0new/name.rs\0C75\0orig.rs\0copy.rs\0T\0link\0";
        let paths = parse_name_status_z(out);
        let mut sorted: Vec<&str> = paths.iter().map(String::as_str).collect();
        sorted.sort_unstable();
        assert_eq!(
            sorted,
            [
                "copy.rs",
                "link",
                "new/name.rs",
                "old/name.rs",
                "orig.rs",
                "src/a.rs",
                "src/gone.rs",
                "src/new.rs"
            ]
        );
        assert!(parse_name_status_z(b"").is_empty());
    }

    #[test]
    fn porcelain_z_names_edits_untracked_files_and_renames() {
        let out = b" M src/a.rs\0?? new.rs\0R  renamed.rs\0original.rs\0A  added.rs\0";
        let paths = parse_porcelain_z(out);
        let mut sorted: Vec<&str> = paths.iter().map(String::as_str).collect();
        sorted.sort_unstable();
        assert_eq!(
            sorted,
            [
                "added.rs",
                "new.rs",
                "original.rs",
                "renamed.rs",
                "src/a.rs"
            ]
        );
    }

    #[test]
    fn paths_with_spaces_and_unicode_survive_the_nul_separated_form() {
        let out = "M\0dir with space/файл.rs\0".as_bytes();
        assert!(parse_name_status_z(out).contains("dir with space/файл.rs"));
    }
}
