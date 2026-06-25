//! The gated "brownfield dev implement" runner — in-place story implementation on an
//! existing local repo.
//!
//! When the UoW's repo is LOCAL (its worktree resolves), this path is chosen over the
//! greenfield [`live_fleet`] scaffolder. The agent edits the EXISTING codebase in the
//! UoW's worktree — on the UoW's branch — rather than scaffolding a fresh app from a
//! plan in a throwaway temp dir. After the agent finishes, the SERVER commits the
//! changes. Layer-2 checks (real repo toolchain via `runner_for_worktree`) run
//! post-task; failing checks bounce the agent for a revise pass up to `max_iterations`.
//!
//! # The gate is universal + unchanged
//!
//! Implementing brownfield code is still CODE-WRITING. The agent is built from the SAME
//! [`camerata_fleet::governed_role`] + [`camerata_agent::prepare_session`] machinery as
//! pr_resolve_run and update_branch_run:
//!
//! - `--allowedTools` = gated tools only (`gated_write` is the only write path).
//! - `Task`, `Write`, `Bash`, `Edit`, `MultiEdit`, `NotebookEdit` are DISALLOWED.
//! - The repo dir passed as the session worktree jails writes to the UoW's worktree.
//!
//! Worktrees change WHERE the agent works, not WHETHER it is gated.
//!
//! # No-code-first gate
//!
//! `ensure_development_gate` (enforced in the caller, `start_governed_run`) guarantees
//! that at least one decision is `Approved` before this function is ever called. The
//! approved decisions ARE the spec: this function builds the agent's task from them.
//!
//! # Commit / push
//!
//! The SERVER commits after the agent finishes (the agent is explicitly forbidden from
//! committing or pushing). Push follows when a GitHub token is available; otherwise the
//! commit stays local for the architect to push manually.
//!
//! # Dispatch predicate
//!
//! `start_governed_run` (lib.rs) calls `execute_dev_implement_run` when:
//!   1. Live mode is on (`CAMERATA_LIVE_BUILD=1`).
//!   2. The UoW's repo worktree is RESOLVABLE (`resolve_uow_worktree` returns `Some`).
//!
//! When the worktree is NOT resolvable (no local clone), the greenfield scaffolder
//! (`execute_live_run` / `execute_live_run_tiered`) is used as the fallback.

use std::sync::atomic::{AtomicUsize, Ordering};

use camerata_agent::prepare_session;
use camerata_checks::runner_for_worktree;
use camerata_core::{AgentDriver, RuleId};
use camerata_fleet::{governed_role, locate_gateway_bin};
use camerata_worktracker::investigation::DecisionRecord;

use crate::run::{live_mode_enabled, GateEvent, RunStatus, RunStore};
use crate::uow::UowStore;

/// Build the implement-run agent's task prompt from the story + approved decisions.
///
/// The decisions ARE the spec: every `Approved` decision is included verbatim so the
/// agent understands the architect-approved constraints before touching any code.
/// Pure + testable: no I/O, no async.
pub fn implement_prompt(
    story_id: &str,
    story_title: &str,
    story_desc: &str,
    target_branch: &str,
    decisions: &[DecisionRecord],
    grounding: Option<&str>,
) -> String {
    // GROUNDING (the invariant): the implementer can read the repo clone, but still hand
    // it the project's rule context + repo digest up front and tell it to consult the real
    // code. See docs/decisions/2026-06-25_all-agents-grounded-in-repo-and-rules.md.
    let grounding_block = match grounding {
        Some(g) if !g.trim().is_empty() => format!(
            "{}\n\nApply the project rules above, and ground every change in the ACTUAL \
             repo code you can read from the working directory.\n\n",
            g.trim()
        ),
        _ => String::new(),
    };
    let decisions_text = {
        let approved: Vec<&DecisionRecord> = decisions
            .iter()
            .filter(|d| d.outcome.is_approved())
            .collect();
        if approved.is_empty() {
            "(no approved decisions — implement directly from the story description)".to_string()
        } else {
            approved
                .iter()
                .enumerate()
                .map(|(i, d)| {
                    format!(
                        "Decision {n}. {label}\n  Question: {q}\n  Rationale: {r}",
                        n = i + 1,
                        label = d.label,
                        q = d.question,
                        r = d.rationale,
                    )
                })
                .collect::<Vec<_>>()
                .join("\n\n")
        }
    };

    format!(
        "You are the BROWNFIELD IMPLEMENTER for story `{story_id}` (branch `{target_branch}`).\n\n\
         {grounding_block}\
         ## Story\n\n\
         Title: {story_title}\n\
         Description: {story_desc}\n\n\
         ## Architect-approved decisions (the spec)\n\n\
         {decisions_text}\n\n\
         ## Your job\n\n\
         Read the existing codebase, then make the minimal correct changes that satisfy \
         the story and every approved decision above.\n\n\
         Rules:\n\
         1. Keep the project building and the existing tests passing.\n\
         2. Add tests for any new behaviour you introduce.\n\
         3. Do NOT change unrelated files.\n\
         4. Do NOT run `git commit` — the server commits your changes after you finish.\n\
         5. Do NOT push.\n\
         6. When the changes are complete and the project builds, you are done."
    )
}

/// True when the dispatch predicate chooses the brownfield implement path: the UoW's
/// repo worktree must be resolvable (i.e. a local clone exists on disk). Pure + testable.
///
/// This is the routing discriminant between brownfield (in-place) and greenfield
/// (scaffold-from-plan): brownfield when `Some`, greenfield fallback when `None`.
pub fn is_brownfield(worktree: Option<&std::path::Path>) -> bool {
    worktree.is_some()
}

/// Run the gated brownfield story-implementation on an existing local repo.
///
/// `dir` is the UoW's WORKTREE (resolved by the caller via `resolve_uow_worktree`);
/// `target_branch` is the UoW's branch; `repo` is `owner/repo`; `token` is the GitHub
/// token used ONLY for the post-implement push (`None` → commit locally, no push).
/// `decisions` are the UoW's APPROVED decisions — they drive the agent's task prompt.
/// `model` pins the implementation agent's model. `max_iterations` is the layer-2
/// bounce-and-revise ceiling (from the active project, default 1).
///
/// The run walks: Executing → (agent implements) → server commits → optional push →
/// AwaitingQa. Poll `GET /api/runs/:id` to watch it. Events surface layer-1 gate
/// decisions, layer-2 check results, StageStarted/Finished, and the bounce loop.
#[allow(clippy::too_many_arguments)]
pub async fn execute_dev_implement_run(
    runs: RunStore,
    uow: UowStore,
    run_id: String,
    story_id: String,
    story_title: String,
    story_desc: String,
    repo: String,
    dir: std::path::PathBuf,
    target_branch: String,
    decisions: Vec<DecisionRecord>,
    token: Option<String>,
    model: String,
    max_iterations: usize,
    skip_layer2: bool,
    grounding: Option<String>,
) {
    runs.set_status(&run_id, RunStatus::Executing, false);
    let seq = AtomicUsize::new(0);
    let next_seq = || seq.fetch_add(1, Ordering::SeqCst) + 1;

    // Helpers — mirrors pr_resolve_run and update_branch_run exactly.
    let event = |runs: &RunStore, verdict: &str, detail: String| {
        runs.push_event(
            &run_id,
            GateEvent {
                seq: next_seq(),
                layer: "dev-implement".to_string(),
                verdict: verdict.to_string(),
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
                layer: "dev-implement".to_string(),
                verdict: "error".to_string(),
                rule: None,
                detail: detail.clone(),
                content_hash: None,
            },
        );
        uow.append_history(
            &story_id,
            "dev_implement",
            &format!("Brownfield implement failed: {detail}"),
        );
        runs.set_status(&run_id, RunStatus::AwaitingQa, true);
    };

    let approved_count = decisions
        .iter()
        .filter(|d| d.outcome.is_approved())
        .count();
    event(
        &runs,
        "info",
        format!(
            "Brownfield implement for story `{story_id}` on branch `{target_branch}` \
             ({approved_count} approved decision(s) → spec)."
        ),
    );

    // Token-free fallback: no `claude` process can be spawned. Report honestly and
    // complete AwaitingQa. Nothing is faked — mirrors pr_resolve_run.
    if !live_mode_enabled() {
        fail(
            &runs,
            &uow,
            "brownfield implementation requires the AI agent, but live mode is off \
             (set CAMERATA_LIVE_BUILD=1)"
                .to_string(),
        );
        return;
    }

    // Ensure the UoW branch is checked out in this worktree before the agent edits.
    // `create_branch_at` creates the branch if absent, then switches to it — exactly
    // the pattern update_branch_run uses via `switch_branch` (after the clone already
    // has the branch) plus the `workspace::create_branch` create-if-absent path.
    if let Err(e) = crate::workspace::create_branch_at(&dir, &target_branch).await {
        fail(
            &runs,
            &uow,
            format!("could not check out the UoW branch `{target_branch}`: {e}"),
        );
        return;
    }

    // ── Spawn ONE gated implementation agent (identical gate machinery as
    //    pr_resolve_run, update_branch_run, investigation_run) ────────────────────────
    let gateway_bin = match locate_gateway_bin() {
        Ok(bin) => bin,
        Err(e) => {
            fail(&runs, &uow, format!("gateway binary missing: {e}"));
            return;
        }
    };
    // THE SAME governed role the fleet uses: allowedTools = gated tools only,
    // Task / Write / Bash / Edit / MultiEdit / NotebookEdit disallowed.
    let role = match governed_role("BrownfieldImplementer").await {
        Ok(r) => r,
        Err(e) => {
            fail(
                &runs,
                &uow,
                format!("could not build the governed implementer role: {e}"),
            );
            return;
        }
    };
    // Jail the agent's writes to the worktree via the session worktree: gated_write
    // (layer-1) is its ONLY mutation path, confined to this UoW's worktree.
    // The session temp dir is RAII-managed inside SessionSpawn._dir (ARCH-RESOURCE-LIFECYCLE-1).
    let spawn = match prepare_session(&gateway_bin, &role, Some(dir.as_path())) {
        Ok(s) => s,
        Err(e) => {
            fail(
                &runs,
                &uow,
                format!("could not prepare the implementer session: {e}"),
            );
            return;
        }
    };
    let driver = spawn.driver.with_model(&model);

    event(
        &runs,
        "info",
        format!(
            "Spawning gated brownfield-implement agent on model `{}`.",
            if model.trim().is_empty() {
                "<cli default>"
            } else {
                &model
            }
        ),
    );

    event(
        &runs,
        "info",
        format!(
            "Stage 1/1: BrownfieldImplementer running under the gate (skip_layer2={skip_layer2})."
        ),
    );

    let task = implement_prompt(
        &story_id,
        &story_title,
        &story_desc,
        &target_branch,
        &decisions,
        grounding.as_deref(),
    );

    // Bounce-and-revise loop: up to `max_iterations` passes. On each pass, run the
    // agent (layer-1 gate enforced by the gateway), then run layer-2 checks (real
    // toolchain via runner_for_worktree). A clean layer-2 → break. A failing
    // layer-2 within budget → revise pass. Matches live_fleet's bounce semantics.
    let mut iteration = 0usize;
    let layer2_result = loop {
        if let Err(e) = driver.run(&role, &task).await {
            fail(&runs, &uow, format!("implementation agent failed: {e}"));
            return;
        }

        // Layer-2: real toolchain checks (skip when bootstrap-escaping).
        if skip_layer2 {
            event(
                &runs,
                "info",
                "Layer-2 checks skipped (bootstrap escape hatch active). \
                 The security gate (layer 1) still applied."
                    .to_string(),
            );
            break Ok(());
        }

        let checks = runner_for_worktree(&dir);
        // CheckRunner::check(role, worktree) → Vec<RuleId> (violated rules).
        let check_result = checks.check(&role, &dir).await;
        match &check_result {
            Ok(violations) if violations.is_empty() => {
                // Clean pass.
                event(&runs, "pass", "Stage 1/1 passed layer-2 checks.".to_string());
                runs.push_event(
                    &run_id,
                    GateEvent {
                        seq: next_seq(),
                        layer: "stage".to_string(),
                        verdict: "info".to_string(),
                        rule: None,
                        detail: format!(
                            "Stage 1/1 finished: clean=true, bounced={iteration}."
                        ),
                        content_hash: None,
                    },
                );
                break Ok(());
            }
            Ok(violations) => {
                let rule_ids: Vec<String> =
                    violations.iter().map(|RuleId(id)| id.clone()).collect();
                let rule_summary = rule_ids.join(", ");
                runs.push_event(
                    &run_id,
                    GateEvent {
                        seq: next_seq(),
                        layer: "layer-2".to_string(),
                        verdict: "fail".to_string(),
                        rule: Some(rule_summary.clone()),
                        detail: format!(
                            "Stage 1/1 failed layer-2: {rule_summary}."
                        ),
                        content_hash: None,
                    },
                );
                iteration += 1;
                if iteration >= max_iterations {
                    runs.push_event(
                        &run_id,
                        GateEvent {
                            seq: next_seq(),
                            layer: "stage".to_string(),
                            verdict: "fail".to_string(),
                            rule: None,
                            detail: format!(
                                "Stage 1/1 finished: clean=false, bounced={}, \
                                 residual violations: {rule_summary}.",
                                iteration - 1
                            ),
                            content_hash: None,
                        },
                    );
                    break Err(format!(
                        "layer-2 still failing after {iteration} pass(es): {rule_summary}"
                    ));
                }
                // Revise pass.
                runs.push_event(
                    &run_id,
                    GateEvent {
                        seq: next_seq(),
                        layer: "layer-2".to_string(),
                        verdict: "revise".to_string(),
                        rule: Some(rule_summary.clone()),
                        detail: format!(
                            "Stage 1: bounce-and-revise — sent back to the agent to fix {rule_summary}."
                        ),
                        content_hash: None,
                    },
                );
            }
            Err(e) => {
                // A hard check-runner error (e.g. toolchain not found) is surfaced as a
                // layer-2 failure; we skip the bounce and treat it as a permanent error
                // so the run doesn't loop forever waiting for a missing tool.
                runs.push_event(
                    &run_id,
                    GateEvent {
                        seq: next_seq(),
                        layer: "layer-2".to_string(),
                        verdict: "fail".to_string(),
                        rule: None,
                        detail: format!("Layer-2 runner error: {e}"),
                        content_hash: None,
                    },
                );
                break Err(format!("layer-2 runner error: {e}"));
            }
        }
    };

    if let Err(detail) = layer2_result {
        // Record the layer-2 failure in history and complete. The changes are left in the
        // worktree so the architect can inspect; no commit is made on a failing tree.
        fail(
            &runs,
            &uow,
            format!("brownfield implementation incomplete — {detail}"),
        );
        return;
    }

    // The SERVER commits the agent's implementation (commit stays server-side, never
    // the agent — mirrors pr_resolve_run exactly).
    let commit_msg = format!("feat: implement story {story_id} on {target_branch}");
    match crate::workspace::commit_all(&dir, &commit_msg).await {
        Ok(out) => {
            event(
                &runs,
                "allow",
                format!("Committed the implementation. {out}"),
            );
        }
        Err(e) => {
            fail(
                &runs,
                &uow,
                format!("could not commit the implementation: {e}"),
            );
            return;
        }
    }

    // Optionally push so the branch is available for CI / PR opening. Token-gated:
    // with no token, the commit is local and the operator pushes manually.
    match token.as_deref() {
        Some(t) => {
            match crate::workspace::push_branch(&dir, &repo, &target_branch, t).await {
                Ok(()) => {
                    event(
                        &runs,
                        "info",
                        format!("Pushed `{target_branch}` — ready for review / CI."),
                    );
                    uow.append_history(
                        &story_id,
                        "dev_implement",
                        &format!(
                            "Implemented story on `{target_branch}` and pushed ({approved_count} \
                             approved decision(s) honoured)."
                        ),
                    );
                }
                Err(e) => {
                    // The implementation IS committed locally; only the push failed.
                    event(
                        &runs,
                        "error",
                        format!(
                            "Committed locally but the push failed: {e} \
                             (push `{target_branch}` manually)."
                        ),
                    );
                    uow.append_history(
                        &story_id,
                        "dev_implement",
                        &format!(
                            "Implemented story on `{target_branch}` (committed locally; push failed)."
                        ),
                    );
                }
            }
        }
        None => {
            event(
                &runs,
                "info",
                format!(
                    "No GitHub token: committed locally — push `{target_branch}` when ready."
                ),
            );
            uow.append_history(
                &story_id,
                "dev_implement",
                &format!(
                    "Implemented story on `{target_branch}` (committed locally; no token to push)."
                ),
            );
        }
    }

    runs.set_status(&run_id, RunStatus::AwaitingQa, true);
}

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_worktracker::investigation::{DecisionOutcome, DecisionRecord};

    // ── Helper: build a minimal approved DecisionRecord ───────────────────────

    fn approved_decision(label: &str, question: &str, rationale: &str) -> DecisionRecord {
        use camerata_worktracker::investigation::{RevisionActor, RevisionProvenance};
        use chrono::Utc;
        DecisionRecord {
            artifact_id: format!("story-1/decision/{label}"),
            story_id: "story-1".to_string(),
            label: label.to_string(),
            question: question.to_string(),
            rationale: rationale.to_string(),
            alternatives_considered: vec![],
            outcome: DecisionOutcome::Approved,
            provenance: RevisionProvenance::new(RevisionActor::Ai, Utc::now()),
        }
    }

    fn pending_decision(label: &str) -> DecisionRecord {
        use camerata_worktracker::investigation::{RevisionActor, RevisionProvenance};
        use chrono::Utc;
        DecisionRecord {
            artifact_id: format!("story-1/decision/{label}"),
            story_id: "story-1".to_string(),
            label: label.to_string(),
            question: "Q?".to_string(),
            rationale: "R".to_string(),
            alternatives_considered: vec![],
            outcome: DecisionOutcome::Pending,
            provenance: RevisionProvenance::new(RevisionActor::Ai, Utc::now()),
        }
    }

    // ── 1. Dispatch routing predicate ──────────────────────────────────────────

    /// When the worktree resolves to Some → brownfield path is chosen.
    #[test]
    fn dispatch_routing_brownfield_when_worktree_resolves() {
        let dir = std::env::temp_dir().join("cam-route-test");
        let path: Option<&std::path::Path> = Some(dir.as_path());
        assert!(
            is_brownfield(path),
            "is_brownfield should be true when worktree is Some"
        );
    }

    /// When the worktree is None (not resolvable) → greenfield scaffolder is chosen.
    #[test]
    fn dispatch_routing_greenfield_when_worktree_not_resolvable() {
        assert!(
            !is_brownfield(None),
            "is_brownfield should be false when worktree is None (greenfield fallback)"
        );
    }

    // ── 2. Prompt construction ─────────────────────────────────────────────────

    /// The implement prompt carries the story title, description, and all approved
    /// decisions. It explicitly forbids committing and pushing.
    #[test]
    fn implement_prompt_contains_story_and_approved_decisions_and_forbids_commit_push() {
        let decisions = vec![
            approved_decision(
                "auth-strategy",
                "JWT or session cookies?",
                "JWT for stateless scalability.",
            ),
            approved_decision(
                "pagination",
                "Cursor or offset?",
                "Cursor for stable page order.",
            ),
        ];
        let p = implement_prompt(
            "acme/api#42",
            "Add user login",
            "Support email/password login with remember-me.",
            "camerata/story-42",
            &decisions,
            None,
        );

        // Story identity.
        assert!(p.contains("acme/api#42"), "prompt must include story_id");
        assert!(p.contains("camerata/story-42"), "prompt must include branch");
        assert!(p.contains("Add user login"), "prompt must include story title");
        assert!(
            p.contains("Support email/password login"),
            "prompt must include story description"
        );

        // Approved decisions are the spec.
        assert!(
            p.contains("auth-strategy"),
            "prompt must include decision label"
        );
        assert!(
            p.contains("JWT or session cookies?"),
            "prompt must include decision question"
        );
        assert!(
            p.contains("JWT for stateless scalability"),
            "prompt must include decision rationale"
        );
        assert!(
            p.contains("pagination"),
            "prompt must include second decision"
        );

        // The agent must NOT commit or push.
        assert!(
            p.contains("Do NOT run `git commit`"),
            "prompt must forbid agent commit"
        );
        assert!(p.contains("Do NOT push"), "prompt must forbid agent push");
    }

    /// Only APPROVED decisions appear in the prompt; Pending decisions are excluded.
    #[test]
    fn implement_prompt_only_includes_approved_decisions() {
        let decisions = vec![
            approved_decision("approved-one", "Q1", "R1"),
            pending_decision("pending-two"),
        ];
        let p = implement_prompt(
            "s/r#1",
            "Title",
            "Desc",
            "camerata/s-r-1",
            &decisions,
            None,
        );
        assert!(
            p.contains("approved-one"),
            "approved decision must appear in prompt"
        );
        assert!(
            !p.contains("pending-two"),
            "pending decision must NOT appear in prompt"
        );
    }

    /// When there are no decisions at all, a clear note replaces the list.
    #[test]
    fn implement_prompt_handles_empty_decisions() {
        let p = implement_prompt("s/r#1", "T", "D", "b", &[], None);
        assert!(p.contains("no approved decisions"));
    }

    // ── 3. GATE UNCHANGED assertion ────────────────────────────────────────────

    /// The brownfield implement run uses `governed_role` + `prepare_session(..., Some(worktree))`
    /// — the IDENTICAL gate machinery as pr_resolve_run and update_branch_run. This test
    /// asserts the compile-time guarantee: we call `governed_role` and `prepare_session`
    /// from the same crates (`camerata_fleet` / `camerata_agent`) with the same
    /// signatures.
    ///
    /// Additionally the live-mode-off path proves the gate never skips: with no token
    /// and live mode off, the run errors rather than faking an implementation.
    #[tokio::test(flavor = "multi_thread")]
    async fn gate_unchanged_live_mode_off_fails_honestly_without_fake_implementation() {
        std::env::remove_var("CAMERATA_LIVE_BUILD");

        let dir = std::env::temp_dir().join(format!(
            "cam-devimpl-gate-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let runs = RunStore::new();
        let uow = UowStore::new();
        let run_id = runs.create("acme/api#42", "dev-implement", crate::run::RunKind::Watched);

        execute_dev_implement_run(
            runs.clone(),
            uow.clone(),
            run_id.clone(),
            "acme/api#42".to_string(),
            "Add user login".to_string(),
            "Support email/password login.".to_string(),
            "acme/api".to_string(),
            dir.clone(),
            "camerata/story-42".to_string(),
            vec![approved_decision("auth", "JWT?", "Use JWT.")],
            None,
            "claude-opus-4-8".to_string(),
            1,
            false,
            None,
        )
        .await;

        let run = runs.get(&run_id).expect("run exists");

        // The run must complete (AwaitingQa).
        assert_eq!(run.status, RunStatus::AwaitingQa);
        assert!(run.done);

        // It must report an honest error — never fake a resolution.
        assert!(
            run.events.iter().any(|e| e.verdict == "error"),
            "must have an error event"
        );
        assert!(
            run.events
                .iter()
                .any(|e| e.detail.contains("live mode is off")),
            "error must mention live mode being off"
        );

        // Critically: NO event should claim the implementation was committed or pushed,
        // because nothing was done.
        assert!(
            !run.events
                .iter()
                .any(|e| e.detail.contains("Committed")),
            "must not claim a commit happened in live-mode-off"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── 4. Branch handling ──────────────────────────────────────────────────────

    /// The run targets the UoW branch in the worktree: create-if-absent is the
    /// expected behaviour. With live mode off the run short-circuits before touching
    /// the branch; test the `is_brownfield` predicate + prompt instead of a
    /// full-worktree round-trip which would require a real git repo + live fleet.
    #[test]
    fn branch_handling_prompt_includes_target_branch() {
        let p = implement_prompt(
            "s/r#7",
            "T",
            "D",
            "camerata/story-7",
            &[approved_decision("opt-a", "Q", "R")],
            None,
        );
        assert!(
            p.contains("camerata/story-7"),
            "prompt must include the target branch name so the agent knows where it is working"
        );
    }
}
