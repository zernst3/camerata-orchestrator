//! The gated "resolve PR feedback" runner (per-UoW PR lifecycle, Decision 2).
//!
//! "Resolve with agent" feeds a PR's open review comments + failing CI check names to ONE
//! governed agent that fixes the code IN THE UoW WORKTREE, then the SERVER commits the
//! fix and (when a token is available) pushes it so the PR re-runs its checks.
//!
//! # The gate is universal + unchanged
//!
//! Resolving PR feedback is still CODE-WRITING, so it is gated EXACTLY like the dev run
//! and the update-branch run: the agent is built from the SAME
//! [`camerata_fleet::governed_role`] + [`camerata_agent::prepare_session`] machinery, so
//! it carries the identical `--allowedTools` = gated tools only and the identical
//! denylist (`Task`, `Write`, `Bash`, …). Its only mutation path is the governance gate
//! (layer-1); it cannot spawn sub-agents (`Task` is disallowed); the layer-2 post-task
//! bounce applies. The repo dir passed as the session worktree jails its writes. Reading
//! the PR feedback is not a write — FIXING it is, and it goes through the gate. Worktrees
//! change WHERE the agent works, not WHETHER it is gated.
//!
//! # Token-free fallback
//!
//! When live mode is off (`CAMERATA_LIVE_BUILD != 1`, the default), no `claude` process
//! is spawned: the run records an honest "resolve needs live mode" note and completes.
//! Nothing is faked.

use std::sync::atomic::{AtomicUsize, Ordering};

use camerata_agent::prepare_session;
use camerata_core::AgentDriver;
use camerata_fleet::{governed_role, locate_gateway_bin};

use crate::run::{live_mode_enabled, GateEvent, RunStatus, RunStore};
use crate::uow::UowStore;

/// Build the resolve agent's task prompt from the PR feedback. Fix-oriented: address the
/// open review comments + the failing CI checks so the tree is coherent and builds. The
/// SERVER commits + pushes after, so the agent must NOT commit or push. Pure + testable.
pub fn resolve_prompt(
    story_id: &str,
    pr_number: u64,
    target_branch: &str,
    review_comments: &[String],
    failing_checks: &[String],
) -> String {
    let comments = if review_comments.is_empty() {
        "(no open review comments)".to_string()
    } else {
        review_comments
            .iter()
            .map(|c| format!("- {c}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let checks = if failing_checks.is_empty() {
        "(no failing checks)".to_string()
    } else {
        failing_checks.join(", ")
    };
    format!(
        "You are the PR FEEDBACK RESOLVER for story `{story_id}` (pull request #{pr_number}, \
         working branch `{target_branch}`). Address the reviewer feedback and make the failing \
         CI checks pass.\n\n\
         Open review comments:\n{comments}\n\n\
         Failing CI checks: {checks}\n\n\
         For each item:\n\
         1. Understand what the reviewer asked for / why the check is failing.\n\
         2. Make the smallest correct change in the code that resolves it.\n\
         3. Keep the project building and the existing tests passing.\n\
         4. Do NOT change unrelated files.\n\n\
         Do NOT run `git commit` — the server commits your changes after you finish. \
         Do NOT push. When the feedback is addressed and the project builds, you are done."
    )
}

/// Run the gated PR-feedback resolution for a UoW.
///
/// `dir` is the UoW's WORKTREE (resolved by the caller via `resolve_uow_worktree`);
/// `target_branch` is the UoW's branch (the PR head); `repo` is `owner/repo`; `token` is
/// the GitHub token used ONLY for the post-fix push (`None` → commit locally, no push).
/// `model` pins the resolution agent's model.
///
/// The run walks: Executing → (agent fixes) → server commits → optional push → AwaitingQa.
/// Poll `GET /api/runs/:id` to watch it. It surfaces events in the run stream like the dev
/// run.
#[allow(clippy::too_many_arguments)]
pub async fn execute_pr_resolve_run(
    runs: RunStore,
    uow: UowStore,
    run_id: String,
    story_id: String,
    repo: String,
    dir: std::path::PathBuf,
    target_branch: String,
    pr_number: u64,
    review_comments: Vec<String>,
    failing_checks: Vec<String>,
    token: Option<String>,
    model: String,
) {
    runs.set_status(&run_id, RunStatus::Executing, false);
    let seq = AtomicUsize::new(0);
    let next_seq = || seq.fetch_add(1, Ordering::SeqCst) + 1;

    let event = |runs: &RunStore, verdict: &str, detail: String| {
        runs.push_event(
            &run_id,
            GateEvent {
                seq: next_seq(),
                layer: "pr-resolve".to_string(),
                verdict: verdict.to_string(),
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
                layer: "pr-resolve".to_string(),
                verdict: "error".to_string(),
                rule: None,
                detail: detail.clone(),
            },
        );
        uow.append_history(&story_id, "pr_resolve", &format!("Resolve PR feedback failed: {detail}"));
        runs.set_status(&run_id, RunStatus::AwaitingQa, true);
    };

    event(
        &runs,
        "info",
        format!(
            "Resolving PR #{pr_number} feedback on `{target_branch}`: {} review comment(s), {} failing check(s).",
            review_comments.len(),
            failing_checks.len()
        ),
    );

    // Token-free fallback: no agent can run, so we cannot fix anything. Report honestly.
    if !live_mode_enabled() {
        fail(
            &runs,
            &uow,
            "resolving PR feedback needs the AI resolver, but live mode is off (set CAMERATA_LIVE_BUILD=1)"
                .to_string(),
        );
        return;
    }

    // Ensure the UoW branch is checked out in this worktree before the agent edits.
    if let Err(e) = crate::workspace::switch_branch(&dir, &target_branch).await {
        fail(&runs, &uow, format!("could not check out the UoW branch `{target_branch}`: {e}"));
        return;
    }

    // ── Spawn ONE gated resolution agent (mirrors update_branch_run.rs) ──────────
    let gateway_bin = match locate_gateway_bin() {
        Ok(bin) => bin,
        Err(e) => {
            fail(&runs, &uow, format!("gateway binary missing: {e}"));
            return;
        }
    };
    // The SAME governed role the fleet uses: allowedTools = gated tools only, Task disallowed.
    let role = match governed_role("PrFeedbackResolver").await {
        Ok(r) => r,
        Err(e) => {
            fail(&runs, &uow, format!("could not build the governed resolver role: {e}"));
            return;
        }
    };
    let session_dir = std::env::temp_dir()
        .join(format!("camerata-prresolve-{}-{}", std::process::id(), run_id));
    // Jail the agent's writes to the worktree via the session worktree: gated_write
    // (layer-1) is its only mutation path, confined to this UoW's worktree.
    let spawn = match prepare_session(&session_dir, &gateway_bin, &role, Some(dir.as_path())) {
        Ok(s) => s,
        Err(e) => {
            fail(&runs, &uow, format!("could not prepare the resolver session: {e}"));
            return;
        }
    };
    let driver = spawn.driver.with_model(&model);

    event(
        &runs,
        "info",
        format!(
            "Spawning single gated PR-feedback resolution agent on model `{}`.",
            if model.trim().is_empty() { "<cli default>" } else { &model }
        ),
    );

    let task = resolve_prompt(&story_id, pr_number, &target_branch, &review_comments, &failing_checks);
    if let Err(e) = driver.run(&role, &task).await {
        fail(&runs, &uow, format!("resolution agent failed: {e}"));
        return;
    }

    // The SERVER commits the agent's fix (commit stays server-side, never the agent).
    let commit_msg = format!("fix: resolve PR #{pr_number} feedback for {story_id}");
    match crate::workspace::commit_all(&dir, &commit_msg).await {
        Ok(out) => {
            event(&runs, "allow", format!("Committed the resolution. {out}"));
        }
        Err(e) => {
            fail(&runs, &uow, format!("could not commit the resolution: {e}"));
            return;
        }
    }

    // Optionally push so the PR re-runs CI. Token-gated: with no token, the fix is
    // committed locally and the operator pushes manually.
    match token.as_deref() {
        Some(t) => match crate::workspace::push_branch(&dir, &repo, &target_branch, t).await {
            Ok(()) => {
                event(&runs, "info", format!("Pushed `{target_branch}` — the PR will re-run its checks."));
                uow.append_history(
                    &story_id,
                    "pr_resolve",
                    &format!("Resolved PR #{pr_number} feedback and pushed `{target_branch}`."),
                );
            }
            Err(e) => {
                // The fix IS committed locally; only the push failed. Report it but the
                // run still completes (the local commit is real).
                event(
                    &runs,
                    "error",
                    format!("Committed locally but the push failed: {e} (push `{target_branch}` manually)."),
                );
                uow.append_history(
                    &story_id,
                    "pr_resolve",
                    &format!("Resolved PR #{pr_number} feedback (committed locally; push failed)."),
                );
            }
        },
        None => {
            event(
                &runs,
                "info",
                format!("No GitHub token: committed locally — push `{target_branch}` to update the PR."),
            );
            uow.append_history(
                &story_id,
                "pr_resolve",
                &format!("Resolved PR #{pr_number} feedback (committed locally; no token to push)."),
            );
        }
    }

    runs.set_status(&run_id, RunStatus::AwaitingQa, true);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_prompt_includes_feedback_and_forbids_commit_and_push() {
        let p = resolve_prompt(
            "o/r#1",
            7,
            "camerata/story-1",
            &["src/a.rs: fix the off-by-one".to_string()],
            &["clippy".to_string(), "build".to_string()],
        );
        assert!(p.contains("o/r#1"));
        assert!(p.contains("#7"));
        assert!(p.contains("camerata/story-1"));
        assert!(p.contains("fix the off-by-one"));
        assert!(p.contains("clippy"));
        assert!(p.contains("build"));
        // It must NOT commit or push (the server commits + pushes).
        assert!(p.contains("Do NOT run `git commit`"));
        assert!(p.contains("Do NOT push"));
    }

    #[test]
    fn resolve_prompt_handles_empty_feedback() {
        let p = resolve_prompt("o/r#1", 3, "b", &[], &[]);
        assert!(p.contains("(no open review comments)"));
        assert!(p.contains("(no failing checks)"));
    }

    /// Live-mode-off → the run reports the honest "needs live mode" failure and completes
    /// AwaitingQa. Token-free, no network, no agent. This also asserts the gate posture is
    /// unchanged: the run never claims a fix it did not make.
    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_with_live_mode_off_fails_honestly() {
        std::env::remove_var("CAMERATA_LIVE_BUILD");
        let dir = std::env::temp_dir().join(format!("cam-prres-off-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let runs = RunStore::new();
        let uow = UowStore::new();
        let run_id = runs.create("o/r#1", "pr-resolve");
        execute_pr_resolve_run(
            runs.clone(),
            uow.clone(),
            run_id.clone(),
            "o/r#1".to_string(),
            "o/r".to_string(),
            dir.clone(),
            "camerata/story-1".to_string(),
            7,
            vec!["fix this".to_string()],
            vec!["clippy".to_string()],
            None,
            "claude-opus-4-8".to_string(),
        )
        .await;

        let run = runs.get(&run_id).expect("run exists");
        assert_eq!(run.status, RunStatus::AwaitingQa);
        assert!(run.done);
        assert!(run.events.iter().any(|e| e.verdict == "error"));
        assert!(run.events.iter().any(|e| e.detail.contains("live mode is off")));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
