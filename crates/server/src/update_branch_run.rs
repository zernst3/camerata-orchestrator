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

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use camerata_agent::{HeartbeatFn, prepare_session};
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
    grounding: Option<&str>,
) -> String {
    // GROUNDING (the invariant): the resolver can read the clone, but hand it the project's
    // rule context + repo digest so reconciliation respects the real conventions/stack.
    let grounding_block = match grounding {
        Some(g) if !g.trim().is_empty() => format!("{}\n\n", g.trim()),
        _ => String::new(),
    };
    let files = if conflicts.is_empty() {
        "(the conflicted files are listed by `git status`)".to_string()
    } else {
        conflicts.join(", ")
    };
    format!(
        "You are the MERGE CONFLICT RESOLVER for story `{story_id}`. A `git merge \
         {merge_ref}` into the working branch `{target_branch}` left conflicts. Your job \
         is to resolve them so the merged tree is coherent and builds.\n\n\
         {grounding_block}\
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
    grounding: Option<String>,
    // MULTI-REPO READ scope: the local clones of ALL the active project's repos, added
    // READ-ONLY via `--add-dir`. The resolver writes only to `dir` (the repo being merged);
    // sibling repos are readable so it can reconcile cross-repo conflicts.
    read_dirs: Vec<std::path::PathBuf>,
    // GAP-2: the active project's VCS-action process rules, used to gate the server-side
    // merge commit at its chokepoint. Defaulted by callers with no active project.
    vcs_config: camerata_checks::vcs_action::ProcessRuleConfig,
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
                content_hash: None,
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
                content_hash: None,
            },
        );
        uow.append_history(&story_id, "update_branch", &format!("Update branch failed: {detail}"));
        // LIFECYCLE-2: a failure is a genuine FAILED terminal, not a silent AwaitingQa.
        runs.fail_with_reason(&run_id, detail);
    };
    // LIFECYCLE-1: honor a cancel mid-run. The terminal Cancelled state is already set by
    // RunStore::cancel; record it and stop before any git mutation (merge / commit).
    let cancelled_stop = |runs: &RunStore, uow: &UowStore, where_: &str| {
        runs.push_event(
            &run_id,
            GateEvent {
                seq: next_seq(),
                layer: "update-branch".to_string(),
                verdict: "info".to_string(),
                rule: None,
                detail: format!("Run cancelled {where_}; stopped before any git mutation."),
                content_hash: None,
            },
        );
        uow.append_history(&story_id, "update_branch", &format!("Cancelled {where_}."));
    };

    // Honor a cancel that arrived before the executor got scheduled.
    if runs.is_cancelled(&run_id) {
        cancelled_stop(&runs, &uow, "before start");
        return;
    }

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

    // LIFECYCLE-1: cancel check IMMEDIATELY before the merge (a git mutation).
    if runs.is_cancelled(&run_id) {
        cancelled_stop(&runs, &uow, "before merge");
        return;
    }

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
                grounding.as_deref(),
                &read_dirs,
                start_seq,
                &vcs_config,
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
    grounding: Option<&str>,
    // MULTI-REPO READ scope: ALL the active project's local repo clones, added read-only.
    read_dirs: &[std::path::PathBuf],
    start_seq: usize,
    // GAP-2: the project's VCS-action process rules for the server-side merge-commit gate.
    vcs_config: &camerata_checks::vcs_action::ProcessRuleConfig,
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
            content_hash: None,
            },
        );
        uow.append_history(
            story_id,
            "update_branch",
            &format!("Update branch failed: {detail} (merge aborted)."),
        );
        // LIFECYCLE-2: a failed conflict resolution is a genuine FAILED terminal.
        runs.fail_with_reason(run_id, detail);
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
    // Jail the agent's writes to the repo dir via the session worktree: gated_write (layer-1)
    // is its only mutation path, and it is confined to the repo being merged.
    // The session temp dir is RAII-managed inside SessionSpawn._dir (ARCH-RESOURCE-LIFECYCLE-1).
    // MULTI-REPO READ: sibling project-repo clones are added READ-ONLY; they don't widen the
    // write jail (still `dir`). Drop `dir` to avoid a dup `--add-dir`.
    let sibling_read_dirs: Vec<std::path::PathBuf> = read_dirs
        .iter()
        .filter(|d| d.as_path() != dir)
        .cloned()
        .collect();
    let spawn = match prepare_session(&gateway_bin, &role, Some(dir), &sibling_read_dirs, None) {
        Ok(s) => s,
        Err(e) => {
            abort_and_fail(format!("could not prepare the resolver session: {e}")).await;
            return;
        }
    };
    // Wire the run's activity heartbeat so the conflict-resolution agent's
    // streamed output keeps last_activity_ms fresh throughout its execution.
    let store_hb = runs.clone();
    let rid_hb = run_id.to_owned();
    let on_activity: HeartbeatFn = Arc::new(move || store_hb.touch_activity(&rid_hb, None));
    let driver = spawn.driver.with_model(model).with_on_activity(on_activity);

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
            content_hash: None,
        },
    );

    let task = conflict_prompt(story_id, target_branch, merge_ref, conflicts, grounding);
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

    // LIFECYCLE-1: cancel check IMMEDIATELY before the merge commit. A Stop that arrived
    // while the agent resolved conflicts must halt BEFORE the server commits the merge. We
    // abort the in-progress merge so no half-merged tree lingers, then stop (the terminal
    // Cancelled state is already set by RunStore::cancel).
    if runs.is_cancelled(run_id) {
        let _ = workspace::merge_abort(dir).await;
        runs.push_event(
            run_id,
            GateEvent {
                seq: next_seq(),
                layer: "update-branch".to_string(),
                verdict: "info".to_string(),
                rule: None,
                detail: "Run cancelled before the merge commit; merge aborted, tree restored."
                    .to_string(),
                content_hash: None,
            },
        );
        uow.append_history(
            story_id,
            "update_branch",
            "Cancelled before the merge commit (merge aborted).",
        );
        return;
    }

    // 3. The SERVER completes the merge commit (never the agent — Task/commit stay server-side).
    //
    // GAP-2 chokepoint. Rather than let git author an ungated `Merge branch ...` subject with
    // `--no-edit` and bypass the gate, Camerata authors a COMPLIANT merge message (conventional
    // shape + substantive body + story-id reference in the project's format) and completes the
    // merge with `git commit -m`. The HARD-BLOCK path then guarantees a non-compliant machine
    // message surfaces as a real error rather than a silent bypass.
    let numeric_id = crate::vcs_choke::numeric_story_id(story_id);
    let merge_msg = crate::vcs_choke::compliant_machine_commit_message(
        vcs_config,
        "chore",
        &format!("merge {merge_ref} into {target_branch} for {story_id}"),
        &numeric_id,
    );
    if let Err(e) = crate::vcs_choke::gated_commit(vcs_config, &merge_msg) {
        abort_and_fail(format!("VCS-action gate blocked the merge commit: {e}")).await;
        return;
    }

    match workspace::commit_merge_with_message(dir, &merge_msg).await {
        Ok(out) => {
            runs.push_event(
                run_id,
                GateEvent {
                    seq: next_seq(),
                    layer: "update-branch".to_string(),
                    verdict: "allow".to_string(),
                    rule: None,
                    detail: format!("Conflicts resolved by the gated agent; merge committed. {out}"),
                    content_hash: None,
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
        let p = conflict_prompt("o/r#1", "camerata/work", "origin/main", &["src/a.rs".into()], None);
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
        let run_id = runs.create("o/r#1", "update-branch", crate::run::RunKind::Watched);
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
            None,
            Vec::new(),
            camerata_checks::vcs_action::ProcessRuleConfig::default(),
        )
        .await;

        let run = runs.get(&run_id).expect("run exists");
        // LIFECYCLE-2: fail-closed means a genuine FAILED terminal, not AwaitingQa.
        assert!(matches!(run.status, RunStatus::Failed { .. }));
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
        let run_id = runs.create("o/r#2", "update-branch", crate::run::RunKind::Watched);
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
            None,
            Vec::new(),
            camerata_checks::vcs_action::ProcessRuleConfig::default(),
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

    /// LIFECYCLE-1 (e2e): a run cancelled BEFORE it executes must NOT perform the merge
    /// (a git mutation). The run stays in its terminal Cancelled state (never AwaitingQa),
    /// and HEAD is unchanged — no merge commit landed.
    #[tokio::test(flavor = "multi_thread")]
    async fn cancelled_run_does_not_merge_or_advance() {
        std::env::remove_var("CAMERATA_LIVE_BUILD");
        let base = std::env::temp_dir().join(format!("cam-upd-cancel-{}", std::process::id()));
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
        g(&base, &["checkout", "-q", "-b", "src"]);
        std::fs::write(base.join("other.txt"), "new\n").unwrap();
        g(&base, &["add", "."]);
        g(&base, &["commit", "-q", "-m", "add other"]);
        g(&base, &["checkout", "-q", "main"]);

        // Record HEAD on main BEFORE the run so we can prove no merge commit was created.
        let head_before = String::from_utf8(
            g(&base, &["rev-parse", "HEAD"]).stdout,
        )
        .unwrap();

        let runs = RunStore::new();
        let uow = UowStore::new();
        let run_id = runs.create("o/r#9", "update-branch", crate::run::RunKind::Watched);
        // Cancel BEFORE executing — the terminal Cancelled state is set here.
        runs.cancel(&run_id);

        execute_update_branch_run(
            runs.clone(),
            uow.clone(),
            run_id.clone(),
            "o/r#9".to_string(),
            "o/r".to_string(),
            base.clone(),
            "main".to_string(),
            "src".to_string(),
            MergeSourceKind::Local,
            None,
            String::new(),
            None,
            Vec::new(),
        )
        .await;

        let run = runs.get(&run_id).expect("run exists");
        // LIFECYCLE-1: cancel wins — the run did NOT advance to AwaitingQa.
        assert_eq!(run.status, RunStatus::Cancelled);
        assert!(run.done);
        // No merge happened: HEAD is unchanged and the tree is not mid-merge.
        let head_after = String::from_utf8(g(&base, &["rev-parse", "HEAD"]).stdout).unwrap();
        assert_eq!(head_before, head_after, "no merge commit on a cancelled run");
        assert!(!workspace::is_merge_in_progress(&base).await);
        // other.txt (only on src) never landed on main.
        assert!(!base.join("other.txt").exists(), "source file not merged in");

        let _ = std::fs::remove_dir_all(&base);
    }
}
