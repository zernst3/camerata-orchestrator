//! AI-assisted "Update branch" runner — the GitHub PR "Update branch" pattern, gated.
//!
//! "Update branch" merges a user-selected SOURCE branch (local or origin) INTO the
//! UoW's working branch, exactly like clicking "Update branch" on a GitHub PR — but
//! when the merge conflicts, a SINGLE gated agent resolves the conflict markers instead
//! of dumping the work on the developer. The server orchestrates; the agent only
//! resolves, through the gate.
//!
//! # The flow
//!
//! In the UoW's local clone (resolved via `resolve_repo_dir`), on the UoW's branch:
//!
//! 1. Ensure the UoW branch is checked out. If the source is an origin branch, fetch it
//!    first (token injected ONLY into the fetch, per the `workspace` token-handling rule).
//! 2. `git merge <source>` into the UoW branch.
//!    - **Clean** → git auto-commits the merge; the run reports success.
//!    - **Conflict** → spawn ONE gated agent (see below) to resolve the markers + `git add`
//!      the resolved files, then the SERVER completes the merge commit.
//! 3. Fail-closed: if the agent can't resolve (conflicts remain, or the merge commit
//!    won't complete), the merge is ABORTED and the run reports failure. It never leaves a
//!    half-merged claimed-success tree.
//!
//! # The gate is preserved (identical to investigation_run.rs)
//!
//! The conflict-resolution agent is built from the SAME [`camerata_fleet::governed_role`]
//! and [`camerata_agent::prepare_session`] machinery the fleet uses, so it carries the
//! identical `--allowedTools` = gated tools only and the identical denylist (`Task`,
//! `Write`, `Bash`, …). Its ONLY mutation path is the governance gate (`gated_write`,
//! layer-1); it cannot spawn sub-agents (`Task` is disallowed). The repo dir is passed as
//! the session worktree so the gate jails the agent's writes to the repo. Spawning is the
//! server's job, never the agent's.
//!
//! # Token-free fallback
//!
//! When live mode is off (`CAMERATA_LIVE_BUILD != 1`, the default), the merge itself still
//! runs (it is local git, no token, no model). A CLEAN merge completes for real. A
//! CONFLICTING merge cannot be resolved without an agent, so the run aborts the merge and
//! reports an honest "conflicts need live mode" failure — never a faked resolution.

use std::sync::atomic::{AtomicUsize, Ordering};

use camerata_agent::prepare_session;
use camerata_core::AgentDriver;
use camerata_fleet::{governed_role, locate_gateway_bin};

use crate::run::{live_mode_enabled, GateEvent, RunStatus, RunStore};
use crate::uow::UowStore;
use crate::workspace::{self, MergeOutcome};

/// Where a merge source branch lives. Mirrors the `source` field on the request body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeSourceKind {
    /// A branch already in the local working copy (`git branch`).
    Local,
    /// A remote-tracking branch under `origin/` — fetched before the merge.
    Origin,
}

impl MergeSourceKind {
    /// Parse the wire string the API accepts (`"local"` / `"origin"`).
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "local" => Some(Self::Local),
            "origin" => Some(Self::Origin),
            _ => None,
        }
    }
}

/// Resolve the merge ref to pass to `git merge` for a source branch + kind. A local
/// source merges the branch by name; an origin source merges the remote-tracking ref
/// `origin/<branch>` (the just-fetched one), never a local branch of the same name.
/// Pure + testable.
pub fn merge_ref(source_branch: &str, kind: MergeSourceKind) -> String {
    match kind {
        MergeSourceKind::Local => source_branch.to_string(),
        MergeSourceKind::Origin => format!("origin/{source_branch}"),
    }
}

/// Build the conflict-resolution agent's task prompt. Resolution-oriented: resolve the
/// conflict markers so the tree is coherent + builds, then `git add` the resolved files.
/// It MUST NOT commit (the server completes the merge commit) and MUST NOT push. Pure +
/// testable.
pub fn conflict_prompt(
    story_id: &str,
    target_branch: &str,
    merge_ref: &str,
    conflicts: &[String],
) -> String {
    let files = if conflicts.is_empty() {
        "(the conflicted files are listed by `git status`)".to_string()
    } else {
        conflicts.join(", ")
    };
    format!(
        "You are the MERGE CONFLICT RESOLVER for story `{story_id}`. A `git merge \
         {merge_ref}` into the working branch `{target_branch}` left conflicts. Your job \
         is to resolve them so the merged tree is coherent and builds.\n\n\
         Conflicted files: {files}\n\n\
         For each conflicted file:\n\
         1. Open it and find the conflict markers (`<<<<<<<`, `=======`, `>>>>>>>`).\n\
         2. Reconcile BOTH sides into a single correct version that preserves the intent \
            of each change. Do NOT just pick one side blindly; integrate them.\n\
         3. Remove ALL conflict markers — none may remain.\n\
         4. Make sure the result is syntactically valid and the project still builds.\n\
         5. `git add` each resolved file so it is staged.\n\n\
         Do NOT run `git commit` — the server completes the merge commit after you finish. \
         Do NOT push. Do NOT change unrelated files. When every conflict is resolved and \
         staged, you are done."
    )
}

/// Run an AI-assisted update-branch: merge `source_branch` (local/origin) into the UoW's
/// branch in its local clone, resolving any conflicts with ONE gated agent.
///
/// `repo` is `owner/repo` (derived from the story id by the caller); `dir` is the
/// resolved local clone path; `target_branch` is the UoW's working branch; `token` is the
/// GitHub token (used ONLY for the origin fetch — `None` when unavailable). `model` pins
/// the conflict-resolution agent's model.
///
/// The run walks: Executing → (merge; on conflict, agent resolves) → AwaitingQa. Poll
/// `GET /api/runs/:id` to watch it. Fail-closed: a merge that can't be completed aborts
/// and reports failure (never a half-merged success).
#[allow(clippy::too_many_arguments)]
pub async fn execute_update_branch_run(
    runs: RunStore,
    uow: UowStore,
    run_id: String,
    story_id: String,
    repo: String,
    dir: std::path::PathBuf,
    target_branch: String,
    source_branch: String,
    source_kind: MergeSourceKind,
    token: Option<String>,
    model: String,
) {
    runs.set_status(&run_id, RunStatus::Executing, false);
    let seq = AtomicUsize::new(0);
    let next_seq = || seq.fetch_add(1, Ordering::SeqCst) + 1;

    let info = |runs: &RunStore, detail: String| {
        runs.push_event(
            &run_id,
            GateEvent {
                seq: next_seq(),
                layer: "update-branch".to_string(),
                verdict: "info".to_string(),
                rule: None,
                detail,
            },
        );
    };
    let fail = |runs: &RunStore, uow: &UowStore, detail: String| {
        runs.push_event(
            &run_id,
            GateEvent {
                seq: next_seq(),
                layer: "update-branch".to_string(),
                verdict: "error".to_string(),
                rule: None,
                detail: detail.clone(),
            },
        );
        uow.append_history(&story_id, "update_branch", &format!("Update branch failed: {detail}"));
        runs.set_status(&run_id, RunStatus::AwaitingQa, true);
    };

    // 1. Ensure the UoW branch is checked out.
    if let Err(e) = workspace::switch_branch(&dir, &target_branch).await {
        fail(&runs, &uow, format!("could not check out the UoW branch `{target_branch}`: {e}"));
        return;
    }

    // For an origin source, fetch the branch first so origin/<branch> is current. The token
    // is injected ONLY into this network command (workspace token-handling rule). With no
    // token we cannot refresh origin; fall back to whatever origin/<branch> already exists.
    if source_kind == MergeSourceKind::Origin {
        match token.as_deref() {
            Some(t) => {
                if let Err(e) = workspace::fetch_branch(&dir, &repo, &source_branch, t).await {
                    fail(&runs, &uow, format!("could not fetch origin branch `{source_branch}`: {e}"));
                    return;
                }
                info(&runs, format!("Fetched origin/{source_branch} for the merge."));
            }
            None => {
                info(
                    &runs,
                    format!(
                        "No GitHub token: merging the already-fetched origin/{source_branch} \
                         (not refreshed from remote)."
                    ),
                );
            }
        }
    }

    let mref = merge_ref(&source_branch, source_kind);
    info(&runs, format!("Merging `{mref}` into `{target_branch}`."));

    // 2. Merge.
    let outcome = match workspace::merge_source(&dir, &mref).await {
        Ok(o) => o,
        Err(e) => {
            // A hard merge error (unknown ref, dirty tree) — nothing to abort (merge_source
            // only returns Err when the tree is NOT mid-merge), so just report.
            fail(&runs, &uow, format!("merge `{mref}` failed: {e}"));
            return;
        }
    };

    match outcome {
        MergeOutcome::Clean(summary) => {
            // Clean merge → git already created the merge commit (--no-edit). Done.
            info(&runs, format!("Clean merge — no conflicts. {summary}"));
            uow.append_history(
                &story_id,
                "update_branch",
                &format!("Updated `{target_branch}` from `{mref}` (clean merge)."),
            );
            info(&runs, "Branch updated successfully.".to_string());
            runs.set_status(&run_id, RunStatus::AwaitingQa, true);
        }
        MergeOutcome::Conflicts(conflicts) => {
            info(
                &runs,
                format!(
                    "Merge has {} conflicted file(s): {}. Resolving with a gated agent.",
                    conflicts.len(),
                    conflicts.join(", ")
                ),
            );
            // Continue the seq counter from where the merge phase left off.
            let start_seq = seq.load(Ordering::SeqCst);
            resolve_conflicts_and_commit(
                &runs, &uow, &run_id, &story_id, &dir, &target_branch, &mref, &conflicts, &model,
                start_seq,
            )
            .await;
        }
    }
}

/// Spawn the single gated agent to resolve the conflicts, then have the SERVER complete the
/// merge commit. Fail-closed: any failure aborts the merge and reports an error.
#[allow(clippy::too_many_arguments)]
async fn resolve_conflicts_and_commit(
    runs: &RunStore,
    uow: &UowStore,
    run_id: &str,
    story_id: &str,
    dir: &std::path::Path,
    target_branch: &str,
    merge_ref: &str,
    conflicts: &[String],
    model: &str,
    start_seq: usize,
) {
    let seq = AtomicUsize::new(start_seq);
    let next_seq = || seq.fetch_add(1, Ordering::SeqCst) + 1;

    // Fail-closed helper: abort the merge so no half-merged tree is left behind, record
    // an honest error event + history, and complete the run.
    let abort_and_fail = |detail: String| async move {
        let _ = workspace::merge_abort(dir).await;
        runs.push_event(
            run_id,
            GateEvent {
                seq: next_seq(),
                layer: "update-branch".to_string(),
                verdict: "error".to_string(),
                rule: None,
                detail: format!("{detail} (merge aborted — tree restored)."),
            },
        );
        uow.append_history(
            story_id,
            "update_branch",
            &format!("Update branch failed: {detail} (merge aborted)."),
        );
        runs.set_status(run_id, RunStatus::AwaitingQa, true);
    };

    // Token-free fallback: no agent can run, so a conflicting merge cannot be resolved.
    // Abort + report honestly (never fake a resolution).
    if !live_mode_enabled() {
        abort_and_fail(
            "conflicts need the AI resolver, but live mode is off (set CAMERATA_LIVE_BUILD=1)"
                .to_string(),
        )
        .await;
        return;
    }

    // ── Spawn ONE gated conflict-resolution agent (mirrors investigation_run.rs) ──
    let gateway_bin = match locate_gateway_bin() {
        Ok(bin) => bin,
        Err(e) => {
            abort_and_fail(format!("gateway binary missing: {e}")).await;
            return;
        }
    };
    // The SAME governed role the fleet uses: allowedTools = gated tools only, Task disallowed.
    let role = match governed_role("ConflictResolver").await {
        Ok(r) => r,
        Err(e) => {
            abort_and_fail(format!("could not build the governed resolver role: {e}")).await;
            return;
        }
    };
    let session_dir = std::env::temp_dir()
        .join(format!("camerata-updatebranch-{}-{}", std::process::id(), run_id));
    // Jail the agent's writes to the repo dir via the session worktree: gated_write (layer-1)
    // is its only mutation path, and it is confined to the repo being merged.
    let spawn = match prepare_session(&session_dir, &gateway_bin, &role, Some(dir)) {
        Ok(s) => s,
        Err(e) => {
            abort_and_fail(format!("could not prepare the resolver session: {e}")).await;
            return;
        }
    };
    let driver = spawn.driver.with_model(model);

    runs.push_event(
        run_id,
        GateEvent {
            seq: next_seq(),
            layer: "update-branch".to_string(),
            verdict: "info".to_string(),
            rule: None,
            detail: format!(
                "Spawning single gated conflict-resolution agent on model `{}`.",
                if model.trim().is_empty() { "<cli default>" } else { model }
            ),
        },
    );

    let task = conflict_prompt(story_id, target_branch, merge_ref, conflicts);
    if let Err(e) = driver.run(&role, &task).await {
        abort_and_fail(format!("conflict-resolution agent failed: {e}")).await;
        return;
    }

    // The agent claims to have resolved + staged. VERIFY before completing: any conflicted
    // path still unresolved means the resolution failed — fail closed.
    let remaining = workspace::conflicted_paths(dir).await;
    if !remaining.is_empty() {
        abort_and_fail(format!(
            "agent left unresolved conflicts: {}",
            remaining.join(", ")
        ))
        .await;
        return;
    }

    // 3. The SERVER completes the merge commit (never the agent — Task/commit stay server-side).
    match workspace::commit_merge(dir).await {
        Ok(out) => {
            runs.push_event(
                run_id,
                GateEvent {
                    seq: next_seq(),
                    layer: "update-branch".to_string(),
                    verdict: "allow".to_string(),
                    rule: None,
                    detail: format!("Conflicts resolved by the gated agent; merge committed. {out}"),
                },
            );
            uow.append_history(
                story_id,
                "update_branch",
                &format!(
                    "Updated `{target_branch}` from `{merge_ref}` (AI-resolved {} conflict(s)).",
                    conflicts.len()
                ),
            );
            runs.set_status(run_id, RunStatus::AwaitingQa, true);
        }
        Err(e) => {
            // The commit itself failed (e.g. still-unstaged conflicts git detected). Fail closed.
            abort_and_fail(format!("could not complete the merge commit: {e}")).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_source_kind_parses_wire() {
        assert_eq!(MergeSourceKind::from_wire("local"), Some(MergeSourceKind::Local));
        assert_eq!(MergeSourceKind::from_wire("origin"), Some(MergeSourceKind::Origin));
        assert_eq!(MergeSourceKind::from_wire("bogus"), None);
    }

    #[test]
    fn merge_ref_local_is_branch_name_origin_is_prefixed() {
        assert_eq!(merge_ref("feature/x", MergeSourceKind::Local), "feature/x");
        assert_eq!(merge_ref("main", MergeSourceKind::Origin), "origin/main");
    }

    #[test]
    fn conflict_prompt_is_resolution_oriented_and_forbids_commit() {
        let p = conflict_prompt("o/r#1", "camerata/work", "origin/main", &["src/a.rs".into()]);
        assert!(p.contains("o/r#1"));
        assert!(p.contains("camerata/work"));
        assert!(p.contains("origin/main"));
        assert!(p.contains("src/a.rs"));
        // It must resolve markers + add, but NOT commit or push (server commits).
        assert!(p.contains("git add"));
        assert!(p.to_lowercase().contains("do not run `git commit`") || p.contains("Do NOT run `git commit`"));
        assert!(p.contains("Do NOT push"));
    }

    /// Live-mode-off + a conflicting merge → the run fails closed (merge aborted, honest
    /// error, run completes). Token-free, no network, no agent: a real local git repo is
    /// built with a guaranteed conflict and the runner is driven end to end.
    #[tokio::test(flavor = "multi_thread")]
    async fn conflict_with_live_mode_off_aborts_and_fails_closed() {
        std::env::remove_var("CAMERATA_LIVE_BUILD");
        let base =
            std::env::temp_dir().join(format!("cam-upd-run-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let g = |dir: &std::path::Path, args: &[&str]| {
            std::process::Command::new("git")
                .current_dir(dir)
                .args(args)
                .output()
                .expect("git runs")
        };
        g(&base, &["init", "-q", "-b", "main"]);
        g(&base, &["config", "user.email", "t@example.com"]);
        g(&base, &["config", "user.name", "Test"]);
        std::fs::write(base.join("f.txt"), "base\n").unwrap();
        g(&base, &["add", "."]);
        g(&base, &["commit", "-q", "-m", "init"]);
        // A source branch that conflicts on f.txt.
        g(&base, &["checkout", "-q", "-b", "src"]);
        std::fs::write(base.join("f.txt"), "from-src\n").unwrap();
        g(&base, &["add", "."]);
        g(&base, &["commit", "-q", "-m", "src edits"]);
        g(&base, &["checkout", "-q", "main"]);
        std::fs::write(base.join("f.txt"), "from-main\n").unwrap();
        g(&base, &["add", "."]);
        g(&base, &["commit", "-q", "-m", "main edits"]);

        let runs = RunStore::new();
        let uow = UowStore::new();
        let run_id = runs.create("o/r#1", "update-branch");
        execute_update_branch_run(
            runs.clone(),
            uow.clone(),
            run_id.clone(),
            "o/r#1".to_string(),
            "o/r".to_string(),
            base.clone(),
            "main".to_string(),
            "src".to_string(),
            MergeSourceKind::Local,
            None,
            "claude-opus-4-8".to_string(),
        )
        .await;

        let run = runs.get(&run_id).expect("run exists");
        assert_eq!(run.status, RunStatus::AwaitingQa);
        assert!(run.done);
        // It must have reported the live-mode-off failure and aborted the merge.
        assert!(run.events.iter().any(|e| e.verdict == "error"));
        assert!(run.events.iter().any(|e| e.detail.contains("live mode is off")));
        // Fail-closed: the tree is NOT mid-merge after the abort.
        assert!(!workspace::is_merge_in_progress(&base).await);

        let _ = std::fs::remove_dir_all(&base);
    }

    /// A CLEAN merge completes for real even with live mode off (it is local git — no
    /// token, no model, no agent needed).
    #[tokio::test(flavor = "multi_thread")]
    async fn clean_merge_succeeds_with_live_mode_off() {
        std::env::remove_var("CAMERATA_LIVE_BUILD");
        let base =
            std::env::temp_dir().join(format!("cam-upd-clean-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let g = |dir: &std::path::Path, args: &[&str]| {
            std::process::Command::new("git")
                .current_dir(dir)
                .args(args)
                .output()
                .expect("git runs")
        };
        g(&base, &["init", "-q", "-b", "main"]);
        g(&base, &["config", "user.email", "t@example.com"]);
        g(&base, &["config", "user.name", "Test"]);
        std::fs::write(base.join("f.txt"), "base\n").unwrap();
        g(&base, &["add", "."]);
        g(&base, &["commit", "-q", "-m", "init"]);
        // A source branch touching a different file → clean merge.
        g(&base, &["checkout", "-q", "-b", "src"]);
        std::fs::write(base.join("other.txt"), "new\n").unwrap();
        g(&base, &["add", "."]);
        g(&base, &["commit", "-q", "-m", "add other"]);
        g(&base, &["checkout", "-q", "main"]);

        let runs = RunStore::new();
        let uow = UowStore::new();
        let run_id = runs.create("o/r#2", "update-branch");
        execute_update_branch_run(
            runs.clone(),
            uow.clone(),
            run_id.clone(),
            "o/r#2".to_string(),
            "o/r".to_string(),
            base.clone(),
            "main".to_string(),
            "src".to_string(),
            MergeSourceKind::Local,
            None,
            String::new(),
        )
        .await;

        let run = runs.get(&run_id).expect("run exists");
        assert_eq!(run.status, RunStatus::AwaitingQa);
        assert!(run.done);
        assert!(run.events.iter().all(|e| e.verdict != "error"), "no error events on a clean merge");
        assert!(run.events.iter().any(|e| e.detail.contains("Clean merge")));
        // The merge produced a commit and the tree is not mid-merge.
        assert!(!workspace::is_merge_in_progress(&base).await);

        let _ = std::fs::remove_dir_all(&base);
    }
}
