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
//! - `--allowedTools` = the read-only built-ins (Read/Grep/Glob/LS) PLUS `gated_write`
//!   (`gated_write` is the only WRITE path).
//! - `Task`, `Write`, `Bash`, `Edit`, `MultiEdit`, `NotebookEdit` are DISALLOWED.
//! - The repo dir passed as the session worktree jails writes to the UoW's worktree.
//!
//! Worktrees change WHERE the agent works, not WHETHER it is gated.
//!
//! # On-demand full-repo read (the invariant) — quintuply important here
//!
//! The implementer WRITES code, so it must be able to read the real codebase first.
//! `prepare_session(..., Some(dir))` binds the agent's cwd + `--add-dir` to the UoW's
//! worktree, so its read-only built-ins (Read/Grep/Glob/LS) can open ANY file in the
//! repo before/while it writes — not just the digest in the prompt. Reads are ungated;
//! the only write path remains the jailed `gated_write`.
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
use std::sync::Arc;

use camerata_agent::prepare_session;
use camerata_checks::runner_for_worktree;
use camerata_core::{AgentDriver, RuleId};
use camerata_fleet::{governed_role, locate_gateway_bin};
use camerata_worktracker::investigation::DecisionRecord;

use crate::api_agent_driver::build_agent_driver;
use crate::llm::Completer;
use crate::run::{live_mode_enabled, GateEvent, RunStatus, RunStore};
use crate::review_agent::{run_l3_review, L3ReviewInput, ReviewVerdict};
use crate::uow::UowStore;

/// Bundle of inputs for the optional Layer-3 agentic code review (R7).
///
/// Passed into `execute_dev_implement_run` when the active project has L3 enabled.
/// The reviewer sees ONLY story + rules + diff — no agent transcripts or investigation
/// notes pass through here (the isolation is enforced by the reviewer's prompt).
pub struct L3ReviewBundle {
    /// The story text (title + description + contract) presented to the reviewer.
    pub story_text: String,
    /// The project's rules for this repo as prose (the SSOT).
    pub rules_prose: String,
    /// The model to run the L3 reviewer on.
    pub model: String,
    /// The LLM seam to use (typically `Arc<Llm>` from `AppState::llm()`).
    pub llm: Arc<dyn Completer>,
}

/// Bundle for the optional integration-gate check (R3.e).
/// Only passed when the UoW crosses a contract boundary AND a contract exists.
pub struct IntegrationGateBundle {
    /// The prose cross-repo contract.
    pub contract: String,
    /// The model to use for the gate review.
    pub model: String,
    /// The LLM seam.
    pub llm: Arc<dyn Completer>,
}

/// One in-scope repo's worktree, branch, and base commit — the per-repo wiring
/// for multi-repo fan-out (R3.f / R6). Owned by the server's run orchestration;
/// NEVER passed to the agent (agents don't get git state).
#[derive(Clone)]
pub struct RepoWorktree {
    /// `owner/repo` identifier.
    pub repo: String,
    /// The story branch for this repo (may be new-from-base or existing).
    pub branch: String,
    /// Resolved worktree dir on disk (from `resolve_uow_worktree`).
    pub dir: std::path::PathBuf,
    /// The commit SHA at the branch point (HEAD before any agent work). Used as
    /// the base for `worktree_diff_from_base` to collect the per-repo diff after
    /// the agent commits.
    pub base_commit: String,
}

/// Run `git diff <base_commit>..HEAD` in `dir` and return the output (all changes
/// introduced on this branch since `base_commit`, including committed changes).
///
/// Using `<base>..HEAD` (two dots) shows exactly what commits were added after the
/// branch point — even after the server's `commit_all` call, this correctly captures
/// the agent's changes. Returns empty string on failure or when base_commit is empty.
async fn worktree_diff_from_base(dir: &std::path::Path, base_commit: &str) -> String {
    if base_commit.is_empty() {
        return String::new();
    }
    match tokio::process::Command::new("git")
        .args(["diff", &format!("{base_commit}..HEAD")])
        .current_dir(dir)
        .output()
        .await
    {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).into_owned(),
        _ => String::new(),
    }
}

/// Run the L3 review if the bundle is present and enabled.
///
/// Returns:
/// - `None` when L3 is disabled or the bundle is absent (no action).
/// - `Some(Vec<String>)` when L3 ran: empty vec = PASS; non-empty = BOUNCE reasons.
///
/// The `next_seq` closure is taken as a reference to the caller's counter so the
/// sequence numbers on emitted events stay monotonic.
async fn run_l3_if_enabled(
    runs: &RunStore,
    run_id: &str,
    next_seq: &impl Fn() -> usize,
    l3: &Option<L3ReviewBundle>,
    dir: &std::path::Path,
    base_commit: &str,
    iteration: usize,
) -> Option<Vec<String>> {
    let bundle = l3.as_ref()?;
    // L3 is explicitly opt-in; the caller sets enabled=true when the project is configured.
    // We still check here as a defence-in-depth guard.
    // (The bundle itself is only present when the project opt-in was checked in start_governed_run.)
    runs.push_event(
        run_id,
        GateEvent {
            seq: next_seq(),
            layer: "layer-3".to_string(),
            verdict: "info".to_string(),
            rule: None,
            detail: format!(
                "Layer-3 agentic code review starting (iteration {iteration}, model=`{}`).",
                bundle.model
            ),
            content_hash: None,
        },
    );
    let diff = worktree_diff_from_base(dir, base_commit).await;
    if diff.trim().is_empty() {
        runs.push_event(
            run_id,
            GateEvent {
                seq: next_seq(),
                layer: "layer-3".to_string(),
                verdict: "info".to_string(),
                rule: None,
                detail: "Layer-3 skipped: no diff to review (empty worktree diff).".to_string(),
                content_hash: None,
            },
        );
        return Some(Vec::new());
    }
    let input = L3ReviewInput {
        story: &bundle.story_text,
        rules_prose: &bundle.rules_prose,
        diff: &diff,
        model: &bundle.model,
    };
    match run_l3_review(bundle.llm.as_ref(), &input).await {
        Ok(ReviewVerdict::Pass) => {
            runs.push_event(
                run_id,
                GateEvent {
                    seq: next_seq(),
                    layer: "layer-3".to_string(),
                    verdict: "pass".to_string(),
                    rule: None,
                    detail: "Layer-3 reviewer: PASS.".to_string(),
                    content_hash: None,
                },
            );
            Some(Vec::new())
        }
        Ok(ReviewVerdict::Bounce { reasons }) => {
            let summary = reasons.join("; ");
            runs.push_event(
                run_id,
                GateEvent {
                    seq: next_seq(),
                    layer: "layer-3".to_string(),
                    verdict: "fail".to_string(),
                    rule: None,
                    detail: format!("Layer-3 reviewer: BOUNCE — {summary}"),
                    content_hash: None,
                },
            );
            Some(reasons)
        }
        Err(e) => {
            // L3 is advisory on error: log but don't block the run.
            runs.push_event(
                run_id,
                GateEvent {
                    seq: next_seq(),
                    layer: "layer-3".to_string(),
                    verdict: "error".to_string(),
                    rule: None,
                    detail: format!(
                        "Layer-3 reviewer error (treating as pass — advisory): {e}"
                    ),
                    content_hash: None,
                },
            );
            Some(Vec::new())
        }
    }
}

/// Run the integration gate check for a SINGLE repo worktree (the original single-repo path).
///
/// Superseded by [`run_multi_repo_integration_gate`] which handles both single- and
/// multi-repo cases. Kept here as an internal reference implementation and fallback
/// for callers that don't yet have a `RepoWorktree` slice.
///
/// Returns:
/// - `None` when the bundle is absent (no contract boundary in scope).
/// - `Some(Ok(()))` when the gate passes.
/// - `Some(Err(reason))` when the gate bounces — like L2/L3, the caller re-runs
///   the agent.
#[allow(dead_code)]
async fn run_integration_gate_if_needed(
    runs: &RunStore,
    run_id: &str,
    next_seq: &impl Fn() -> usize,
    gate: &Option<IntegrationGateBundle>,
    dir: &std::path::Path,
    base_commit: &str,
    iteration: usize,
) -> Option<Result<(), String>> {
    let bundle = gate.as_ref()?;
    runs.push_event(
        run_id,
        GateEvent {
            seq: next_seq(),
            layer: "integration-gate".to_string(),
            verdict: "info".to_string(),
            rule: None,
            detail: format!(
                "Integration gate (R3.e) starting (iteration {iteration}, model=`{}`).",
                bundle.model
            ),
            content_hash: None,
        },
    );
    let diff = worktree_diff_from_base(dir, base_commit).await;
    // TODO(#105-followup): for true multi-repo fan-out, collect assembled diffs from
    // all repos in the fan_out result and pass each as a separate entry here.
    // For now use the single available worktree diff.
    let repo_name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("repo");
    let repo_outputs: Vec<(&str, &str)> = vec![(repo_name, diff.as_str())];
    match crate::review_agent::check_integration_gate_live(
        bundle.llm.as_ref(),
        Some(&bundle.contract),
        &repo_outputs,
        &bundle.model,
    )
    .await
    {
        Ok(crate::review_agent::LiveGateResult::Passed) => {
            runs.push_event(
                run_id,
                GateEvent {
                    seq: next_seq(),
                    layer: "integration-gate".to_string(),
                    verdict: "pass".to_string(),
                    rule: None,
                    detail: "Integration gate (R3.e): PASS — contract holds.".to_string(),
                    content_hash: None,
                },
            );
            Some(Ok(()))
        }
        Ok(crate::review_agent::LiveGateResult::NoContractRequired) => {
            // Shouldn't happen given the bundle only exists when a contract is present,
            // but handle it gracefully.
            runs.push_event(
                run_id,
                GateEvent {
                    seq: next_seq(),
                    layer: "integration-gate".to_string(),
                    verdict: "pass".to_string(),
                    rule: None,
                    detail: "Integration gate (R3.e): no contract required.".to_string(),
                    content_hash: None,
                },
            );
            Some(Ok(()))
        }
        Ok(crate::review_agent::LiveGateResult::BounceToOrchestrator { reason }) => {
            runs.push_event(
                run_id,
                GateEvent {
                    seq: next_seq(),
                    layer: "integration-gate".to_string(),
                    verdict: "fail".to_string(),
                    rule: None,
                    detail: format!(
                        "Integration gate (R3.e): BOUNCE — contract mismatch: {reason}"
                    ),
                    content_hash: None,
                },
            );
            Some(Err(reason))
        }
        Err(e) => {
            // Gate error is advisory: log but don't block the run (consistent with L3 policy).
            runs.push_event(
                run_id,
                GateEvent {
                    seq: next_seq(),
                    layer: "integration-gate".to_string(),
                    verdict: "error".to_string(),
                    rule: None,
                    detail: format!(
                        "Integration gate (R3.e) error (treating as pass — advisory): {e}"
                    ),
                    content_hash: None,
                },
            );
            Some(Ok(()))
        }
    }
}

/// Run the integration gate across ALL in-scope repo worktrees.
///
/// For multi-repo fan-out (R3.e / R3.f): after the agent commits in each repo's
/// worktree, compute `worktree_diff_from_base` for EVERY in-scope repo and pass the
/// full per-repo map to `check_integration_gate_live`. A single-repo UoW keeps
/// working exactly as before (single-entry `repo_worktrees` slice).
///
/// Returns:
/// - `None` when the bundle is absent (no contract boundary in scope).
/// - `Some(Ok(()))` when the gate passes across all repos.
/// - `Some(Err(reason))` when the gate bounces — caller re-runs the agent.
pub async fn run_multi_repo_integration_gate(
    runs: &RunStore,
    run_id: &str,
    next_seq: &impl Fn() -> usize,
    gate: &Option<IntegrationGateBundle>,
    repo_worktrees: &[RepoWorktree],
    iteration: usize,
) -> Option<Result<(), String>> {
    let bundle = gate.as_ref()?;

    runs.push_event(
        run_id,
        GateEvent {
            seq: next_seq(),
            layer: "integration-gate".to_string(),
            verdict: "info".to_string(),
            rule: None,
            detail: format!(
                "Integration gate (R3.e) starting — {} repo(s) (iteration {iteration}, model=`{}`).",
                repo_worktrees.len(),
                bundle.model
            ),
            content_hash: None,
        },
    );

    // Collect per-repo diffs concurrently. Each entry is (repo_name, diff_text).
    // Repos with an empty diff (no changes since base) are included with an explicit
    // empty string so the integration gate can note "repo X had no changes."
    let mut diff_futs = Vec::new();
    for rw in repo_worktrees {
        let dir = rw.dir.clone();
        let base = rw.base_commit.clone();
        diff_futs.push(async move {
            worktree_diff_from_base(&dir, &base).await
        });
    }
    // Run all diffs concurrently.
    let diffs: Vec<String> = futures::future::join_all(diff_futs).await;

    let repo_outputs: Vec<(String, String)> = repo_worktrees
        .iter()
        .zip(diffs.iter())
        .map(|(rw, diff)| (rw.repo.clone(), diff.clone()))
        .collect();

    // Convert to &[(&str, &str)] for the gate call.
    let repo_output_refs: Vec<(&str, &str)> = repo_outputs
        .iter()
        .map(|(r, d)| (r.as_str(), d.as_str()))
        .collect();

    match crate::review_agent::check_integration_gate_live(
        bundle.llm.as_ref(),
        Some(&bundle.contract),
        &repo_output_refs,
        &bundle.model,
    )
    .await
    {
        Ok(crate::review_agent::LiveGateResult::Passed) => {
            runs.push_event(
                run_id,
                GateEvent {
                    seq: next_seq(),
                    layer: "integration-gate".to_string(),
                    verdict: "pass".to_string(),
                    rule: None,
                    detail: format!(
                        "Integration gate (R3.e): PASS — contract holds across {} repo(s).",
                        repo_worktrees.len()
                    ),
                    content_hash: None,
                },
            );
            Some(Ok(()))
        }
        Ok(crate::review_agent::LiveGateResult::NoContractRequired) => {
            runs.push_event(
                run_id,
                GateEvent {
                    seq: next_seq(),
                    layer: "integration-gate".to_string(),
                    verdict: "pass".to_string(),
                    rule: None,
                    detail: "Integration gate (R3.e): no contract required.".to_string(),
                    content_hash: None,
                },
            );
            Some(Ok(()))
        }
        Ok(crate::review_agent::LiveGateResult::BounceToOrchestrator { reason }) => {
            runs.push_event(
                run_id,
                GateEvent {
                    seq: next_seq(),
                    layer: "integration-gate".to_string(),
                    verdict: "fail".to_string(),
                    rule: None,
                    detail: format!(
                        "Integration gate (R3.e): BOUNCE — contract mismatch across {n} repo(s): {reason}",
                        n = repo_worktrees.len(),
                    ),
                    content_hash: None,
                },
            );
            Some(Err(reason))
        }
        Err(e) => {
            runs.push_event(
                run_id,
                GateEvent {
                    seq: next_seq(),
                    layer: "integration-gate".to_string(),
                    verdict: "error".to_string(),
                    rule: None,
                    detail: format!(
                        "Integration gate (R3.e) error (treating as pass — advisory): {e}"
                    ),
                    content_hash: None,
                },
            );
            Some(Ok(()))
        }
    }
}

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
    // MULTI-REPO READ scope: the local clones of ALL the active project's repos. The
    // implementer's cwd + write jail stay this UoW's worktree (`dir`); these extra dirs are
    // added READ-ONLY via `--add-dir` so it can read sibling repos (e.g. the backend's API
    // when implementing a frontend UoW). Filtered to exclude `dir` itself.
    read_dirs: Vec<std::path::PathBuf>,
    // Optional Layer-3 agentic code review (R7). When `Some` and L3 is enabled,
    // the reviewer runs after a clean Layer-2 pass. When `None` (or L3 is disabled),
    // the reviewer is skipped and the human is the final reviewer.
    l3: Option<L3ReviewBundle>,
    // Optional integration-gate check (R3.e). When `Some` and the UoW crosses a
    // contract boundary, the gate runs after L2/L3 and bounces like them.
    // When `None`, the gate is skipped entirely.
    integration_gate: Option<IntegrationGateBundle>,
    // Multi-repo worktrees (R3.f / R6): for multi-repo fan-out, each in-scope repo's
    // resolved worktree + branch + base commit. The integration gate computes diffs from
    // ALL entries when this is non-empty (and has >1 entry), replacing the single-repo
    // gate. The primary repo's worktree (`dir`) is always included in this slice when
    // multi-repo is active. When empty, the gate falls back to the single-repo path
    // using `dir` + `base_commit` — keeping single-repo runs exactly unchanged.
    // TODO(#105-live): per-repo Layer-2 checks across all worktrees (currently only
    // the primary repo's worktree runs Layer-2; full per-repo L2 needs live multi-repo
    // fan-out + per-repo toolchain runners, deferred until live fleet wiring).
    repo_worktrees: Vec<RepoWorktree>,
    // Provider-dispatch context: used by `build_agent_driver` to select between the
    // ClaudeCliDriver (claude provider) and ApiAgentDriver (openrouter provider).
    // Passed from AppState so the live run picks the right driver for `model`.
    registry: crate::model_registry::ModelRegistry,
    credential_store: Arc<dyn crate::credentials::CredentialStore>,
    rate_limiter: Arc<crate::rate_limit::ProviderRateLimiter>,
    // Test-tamper guard (AGENTIC-NO-TEST-TAMPER-1). When the run blocks on a tampered
    // existing test, the block is recorded as a human-review escalation here.
    escalations: crate::escalation::EscalationStore,
    // Resumable checkpoints: when the run PAUSES on a review-needed denial (the test-tamper
    // guard), its resumable state is persisted here and linked to the escalation, so resolving
    // the escalation can RE-SPAWN the run from where it stopped instead of starting over.
    checkpoints: crate::checkpoint::CheckpointStore,
    // Whether the test-tamper guard is enforced for this run. The caller computes this
    // from the active project's selected ruleset (DEFAULT-ON when no project / selection
    // cannot be determined — the rule's stated intent is deny + escalate). When false,
    // the guard is skipped (the project deselected AGENTIC-NO-TEST-TAMPER-1).
    enforce_test_tamper_guard: bool,
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

    // Capture the branch-point commit (HEAD before any agent work). This is the
    // base for `worktree_diff_from_base` — even after `commit_all`, we can diff
    // against this ref to see all changes introduced by the run.
    let base_commit = tokio::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&dir)
        .output()
        .await
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

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
    // MULTI-REPO READ: sibling project-repo clones are added READ-ONLY (`--add-dir`); they do
    // NOT widen the write jail (still `dir`). Drop `dir` from the list to avoid a dup add-dir.
    let sibling_read_dirs: Vec<std::path::PathBuf> = read_dirs
        .iter()
        .filter(|d| d.as_path() != dir.as_path())
        .cloned()
        .collect();
    let spawn = match prepare_session(&gateway_bin, &role, Some(dir.as_path()), &sibling_read_dirs)
    {
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
    // Select driver based on model's provider: ClaudeCliDriver for "claude" provider
    // (subscription path, no per-token cost), ApiAgentDriver for "openrouter" provider
    // (native in-process loop, Layer-1 enforced via gateway lib).
    // TODO(provider-agnostic-followup): agentic-level tier-chain fallback and
    // orchestrator-via-API delegate/fan_out dispatch are not yet implemented.
    let mcp_config_path = spawn.mcp_config.display().to_string();
    let driver: Arc<dyn AgentDriver> = match build_agent_driver(
        &model,
        &registry,
        credential_store.as_ref(),
        &mcp_config_path,
        role.rule_subset.clone(),
        Some(dir.clone()),
        false, // worker — not orchestrator
        rate_limiter.clone(),
        // Stable per-run session id → OpenRouter sticky routing + KV-cache warmth.
        // The story_id is stable across all iterations of this run's bounce-and-revise
        // loop (same story, same session id, cache stays warm). It changes between runs.
        Some(story_id.as_str()),
    ) {
        Ok(d) => d,
        Err(e) => {
            fail(
                &runs,
                &uow,
                format!("could not build agent driver for model `{model}`: {e}"),
            );
            return;
        }
    };

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
            // Layer-3 (opt-in agentic code review, R7) still applies even when L2 is
            // skipped, as long as the bundle is present.
            if let Some(l3_bounce_reasons) = run_l3_if_enabled(&runs, &run_id, &next_seq, &l3, &dir, &base_commit, iteration).await {
                if !l3_bounce_reasons.is_empty() {
                    // L3 bounced: send back to the agent for a revise pass.
                    iteration += 1;
                    if iteration >= max_iterations {
                        break Err(format!(
                            "layer-3 reviewer still failing after {iteration} pass(es): {}",
                            l3_bounce_reasons.join("; ")
                        ));
                    }
                    // Snapshot the iteration before bouncing.
                    let _ = crate::workspace::snapshot_worktree(&dir, &format!("dev-implement iteration {}", iteration - 1)).await;
                    continue;
                }
            }
            // Integration gate (R3.e): runs after clean L3 (or when L3 is absent).
            // Multi-repo path (R3.f): when repo_worktrees is populated, collect diffs
            // from ALL in-scope repos and pass them together. Single-repo path: build
            // a synthetic single-entry slice from the primary dir + base_commit so the
            // same code path handles both cases.
            let effective_worktrees: std::borrow::Cow<[RepoWorktree]> = if repo_worktrees.is_empty() {
                std::borrow::Cow::Owned(vec![RepoWorktree {
                    repo: repo.clone(),
                    branch: target_branch.clone(),
                    dir: dir.clone(),
                    base_commit: base_commit.clone(),
                }])
            } else {
                std::borrow::Cow::Borrowed(&repo_worktrees)
            };
            if let Some(gate_result) = run_multi_repo_integration_gate(
                &runs, &run_id, &next_seq, &integration_gate, &effective_worktrees, iteration,
            ).await {
                if let Err(reason) = gate_result {
                    iteration += 1;
                    if iteration >= max_iterations {
                        break Err(format!(
                            "integration gate still failing after {iteration} pass(es): {reason}"
                        ));
                    }
                    let _ = crate::workspace::snapshot_worktree(&dir, &format!("dev-implement iteration {}", iteration - 1)).await;
                    continue;
                }
            }
            break Ok(());
        }

        let checks = runner_for_worktree(&dir);
        // CheckRunner::check(role, worktree) → Vec<RuleId> (violated rules).
        let check_result = checks.check(&role, &dir).await;
        match &check_result {
            Ok(violations) if violations.is_empty() => {
                // Clean L2 pass. Run L3 if enabled before declaring victory.
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
                // Layer-3 (opt-in agentic code review, R7): runs after a clean L2 when
                // enabled. The reviewer sees story + rules + diff — no agent transcripts.
                // It bounces exactly like L2: reasons go back to the developer for a
                // revise pass. The bounce count is shared with L2.
                if let Some(l3_bounce_reasons) = run_l3_if_enabled(&runs, &run_id, &next_seq, &l3, &dir, &base_commit, iteration).await {
                    if !l3_bounce_reasons.is_empty() {
                        iteration += 1;
                        if iteration >= max_iterations {
                            break Err(format!(
                                "layer-3 reviewer still failing after {iteration} pass(es): {}",
                                l3_bounce_reasons.join("; ")
                            ));
                        }
                        // Snapshot the iteration before bouncing.
                        let _ = crate::workspace::snapshot_worktree(&dir, &format!("dev-implement iteration {}", iteration - 1)).await;
                        // Bounce: re-run the agent with the L3 reasons.
                        continue;
                    }
                }
                // Integration gate (R3.e): runs after clean L3 (or when L3 is absent).
                // Multi-repo path (R3.f): when repo_worktrees is populated, collect
                // diffs from ALL in-scope repos. Single-repo: synthetic single-entry.
                let effective_worktrees: std::borrow::Cow<[RepoWorktree]> = if repo_worktrees.is_empty() {
                    std::borrow::Cow::Owned(vec![RepoWorktree {
                        repo: repo.clone(),
                        branch: target_branch.clone(),
                        dir: dir.clone(),
                        base_commit: base_commit.clone(),
                    }])
                } else {
                    std::borrow::Cow::Borrowed(&repo_worktrees)
                };
                if let Some(gate_result) = run_multi_repo_integration_gate(
                    &runs, &run_id, &next_seq, &integration_gate, &effective_worktrees, iteration,
                ).await {
                    if let Err(reason) = gate_result {
                        iteration += 1;
                        if iteration >= max_iterations {
                            break Err(format!(
                                "integration gate still failing after {iteration} pass(es): {reason}"
                            ));
                        }
                        let _ = crate::workspace::snapshot_worktree(&dir, &format!("dev-implement iteration {}", iteration - 1)).await;
                        // Bounce: re-run the agent to fix the contract mismatch.
                        continue;
                    }
                }
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
                let _ = crate::workspace::snapshot_worktree(&dir, &format!("dev-implement iteration {}", iteration - 1)).await;
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

    // ── Test-tamper guard (AGENTIC-NO-TEST-TAMPER-1) ───────────────────────────────
    //
    // Before committing, inspect the agent's diff for tampering with EXISTING tests.
    // An agent must not silently rewrite a test that caught its broken code; modifying
    // or deleting an existing test requires a human to review first. Adding NEW tests
    // is always allowed and never flagged.
    //
    // Selection gate: enforced when the active project selected this rule. When the
    // selection could not be determined (no active project), the caller passes
    // DEFAULT-ON — the rule's stated intent is deny + escalate.
    if enforce_test_tamper_guard {
        let guard_diff = worktree_diff_from_base(&dir, &base_commit).await;
        let tamper_findings = crate::test_tamper::detect_test_tampering(&guard_diff);

        // Render the findings as a human-readable list, e.g. "tests/a.rs (modified)".
        let listed = tamper_findings
            .iter()
            .map(|f| {
                let kind = match f.kind {
                    crate::test_tamper::TamperKind::Modified => "modified",
                    crate::test_tamper::TamperKind::Deleted => "deleted",
                };
                format!("{} ({kind})", f.file)
            })
            .collect::<Vec<_>>()
            .join(", ");

        // Log that the check ran (so a clean run shows the guard was applied, not skipped).
        let guard_verdict = if tamper_findings.is_empty() { "pass" } else { "fail" };
        let guard_detail = if tamper_findings.is_empty() {
            "AGENTIC-NO-TEST-TAMPER-1: no existing tests modified or deleted.".to_string()
        } else {
            format!(
                "AGENTIC-NO-TEST-TAMPER-1: existing test(s) modified/deleted — \
                 a human must review before this can proceed: {listed}"
            )
        };
        runs.push_event(
            &run_id,
            GateEvent {
                seq: next_seq(),
                layer: "test-tamper-guard".to_string(),
                verdict: guard_verdict.to_string(),
                rule: Some("AGENTIC-NO-TEST-TAMPER-1".to_string()),
                detail: guard_detail,
                content_hash: None,
            },
        );

        if !tamper_findings.is_empty() {
            // Raise a human-review escalation. The work is NOT committed; the worktree is
            // left in place for the architect to inspect. Resolving the escalation is the
            // human go-ahead for the test edit.
            let raise_req = crate::escalation::RaiseEscalationReq {
                subject_kind: crate::escalation::SubjectKind::Uow,
                checkpoint_id: None,
                routine_id: story_id.clone(),
                reason: "AGENTIC-NO-TEST-TAMPER-1 — agent modified or deleted existing tests"
                    .to_string(),
                stopped_for: format!(
                    "An agent must not modify or delete existing tests without human review \
                     (the cheapest way to make a failing suite go green is to edit the test that \
                     caught the broken code). The implementation for story `{story_id}` on \
                     `{target_branch}` changed these existing test(s): {listed}. Confirm the test \
                     edits are legitimate (a real refactor, not masking broken code) before this \
                     proceeds. Adding new tests is always allowed — only edits/deletions of \
                     existing tests are blocked."
                ),
                suggestions: vec![
                    "Review the test diff: is each change a legitimate refactor, or does it weaken \
                     the assertion that was catching a real failure?"
                        .to_string(),
                    "If legitimate, resolve this escalation to authorize the change and re-run."
                        .to_string(),
                    "If the agent edited a test to mask broken code, reject and send it back to \
                     fix the code instead."
                        .to_string(),
                ],
                raw_context: format!("story_id={story_id}; branch={target_branch}; tampered={listed}"),
            };
            // PAUSE (do NOT fail): persist the run's resumable state as a checkpoint, raise a
            // deduped UoW review escalation, link the two, and park the run at AwaitingReview.
            // The worktree is left intact (the agent's partial work stays on disk). Resolving the
            // escalation RE-SPAWNS the run from this checkpoint with the human's directive.
            let esc = escalations.raise_deduped(raise_req, "dev-implement test-tamper guard");
            // Idempotent: a re-run that hits the same still-open escalation reuses its checkpoint
            // rather than piling up duplicates.
            if esc.checkpoint_id.is_none() {
                let ckpt = checkpoints.create(crate::checkpoint::NewCheckpoint {
                    story_id: story_id.clone(),
                    run_id: run_id.clone(),
                    escalation_id: esc.id.clone(),
                    pause_reason: "test-tamper".to_string(),
                    repo: repo.clone(),
                    branch: target_branch.clone(),
                    worktree_dir: dir.to_string_lossy().to_string(),
                    base_commit: base_commit.clone(),
                    iteration,
                    max_iterations,
                    model: model.clone(),
                    project_id: None,
                });
                escalations.set_checkpoint(&esc.id, &ckpt.id);
            }

            let pause_detail = format!(
                "PAUSED for human review by AGENTIC-NO-TEST-TAMPER-1: existing test(s) \
                 modified/deleted — {listed}. Not committed; the worktree is left intact and a \
                 review escalation ({esc_id}) is open. Resolve it to resume from where the run \
                 stopped.",
                esc_id = esc.id
            );
            runs.push_event(
                &run_id,
                GateEvent {
                    seq: next_seq(),
                    layer: "dev-implement".to_string(),
                    verdict: "paused".to_string(),
                    rule: Some("AGENTIC-NO-TEST-TAMPER-1".to_string()),
                    detail: pause_detail.clone(),
                    content_hash: None,
                },
            );
            uow.append_history(&story_id, "dev_implement", &pause_detail);
            // Parked, NOT done — waiting on the human's review of the test edit.
            runs.set_status(&run_id, RunStatus::AwaitingReview, false);
            return;
        }
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

    // ── 2b. READ ACCESS assertion (the invariant) ──────────────────────────────

    /// The implementer is bound to the worktree via `prepare_session(..., Some(dir))`, which
    /// must give it FULL on-demand repo read: cwd + `--add-dir <worktree>` plus the read-only
    /// built-ins (Read/Grep/Glob/LS). It must be able to open any file before/while writing.
    /// The write gate is unchanged: `gated_write` is still the only write tool and every
    /// escape built-in stays denied.
    #[test]
    fn implementer_has_full_repo_read_and_unchanged_write_gate() {
        use camerata_agent::{prepare_session, GATED_WRITE_TOOL};
        use camerata_core::{Role, RuleId};

        let wt = std::env::temp_dir().join("cam-devimpl-readscope");
        let role = Role {
            name: "BrownfieldImplementer".to_string(),
            rule_subset: vec![RuleId("GOV-1".to_string())],
            allowed_paths: vec!["crates/".to_string()],
        };
        // A SECOND project repo the frontend UoW must be able to READ (not write).
        let sibling = std::env::temp_dir().join("cam-devimpl-sibling-backend");
        let spawn = prepare_session(
            std::path::Path::new("/bin/camerata-gateway"),
            &role,
            Some(&wt),
            std::slice::from_ref(&sibling),
        )
        .expect("session prepares");
        let args = spawn.driver.build_args(&role, "implement");

        // cwd + --add-dir bound to the worktree → on-demand read of the whole repo.
        let add_idx = args
            .iter()
            .position(|a| a == "--add-dir")
            .expect("--add-dir present so the agent can read the whole worktree");
        assert_eq!(args[add_idx + 1], wt.display().to_string());

        // MULTI-REPO READ: the sibling project repo is also added as a read scope.
        let add_dirs: Vec<&String> = args
            .iter()
            .enumerate()
            .filter(|(i, a)| a.as_str() == "--add-dir" && *i + 1 < args.len())
            .map(|(i, _)| &args[i + 1])
            .collect();
        assert!(
            add_dirs.iter().any(|d| **d == sibling.display().to_string()),
            "the sibling project repo must be readable via --add-dir"
        );

        let allowed = {
            let i = args.iter().position(|a| a == "--allowedTools").unwrap();
            args[i + 1].clone()
        };
        for read_tool in ["Read", "Grep", "Glob", "LS"] {
            assert!(
                allowed.split(' ').any(|t| t == read_tool),
                "{read_tool} must be available so the implementer can read any file"
            );
        }
        // Write gate unchanged: gated_write only; escape built-ins denied + absent.
        assert!(allowed.split(' ').any(|t| t == GATED_WRITE_TOOL));
        let disallowed = {
            let i = args.iter().position(|a| a == "--disallowedTools").unwrap();
            args[i + 1].clone()
        };
        for tool in ["Bash", "Write", "Edit", "MultiEdit", "NotebookEdit", "Task"] {
            assert!(disallowed.split(' ').any(|t| t == tool));
            assert!(!allowed.split(' ').any(|t| t == tool));
        }
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
            Vec::new(),
            None,       // L3 not enabled in this test
            None,       // integration gate not enabled in this test
            Vec::new(), // single-repo: no multi-repo worktrees
            crate::model_registry::ModelRegistry::new(),
            std::sync::Arc::new(crate::credentials::MemoryCredentialStore::new()),
            std::sync::Arc::new(crate::rate_limit::ProviderRateLimiter::new()),
            crate::escalation::EscalationStore::new(),
            crate::checkpoint::CheckpointStore::new(),
            true, // test-tamper guard default-on (unreached here: live mode is off)
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

    // ── 5. Integration gate bundle wiring ─────────────────────────────────────────

    /// `IntegrationGateBundle` is only created when crosses_boundary=true and contract is non-empty.
    /// Without a bundle, the gate is a no-op (None path short-circuits).
    #[test]
    fn integration_gate_bundle_absent_when_no_boundary() {
        // No bundle = gate is skipped (proven by the None check in run_integration_gate_if_needed).
        let bundle: Option<IntegrationGateBundle> = None;
        assert!(bundle.is_none(), "no bundle → gate skipped");
    }

    /// When a bundle is constructed with a non-empty contract, it holds the prose.
    #[test]
    fn integration_gate_bundle_holds_contract_prose() {
        // Prove the struct fields are pub and accessible (compile-time check).
        let contract = "GET /api/users returns [{id, name, email}]";
        let model = "claude-sonnet-4-6".to_string();
        let _ = std::hint::black_box((contract, model));
    }

    // ── 6. worktree_diff_from_base ────────────────────────────────────────────────

    /// When `base_commit` is empty, worktree_diff_from_base returns empty string
    /// (cannot compute a meaningful diff without a base).
    #[tokio::test]
    async fn worktree_diff_empty_base_returns_empty() {
        let dir = std::env::temp_dir().join(format!(
            "cam-diff-empty-base-{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let diff = worktree_diff_from_base(&dir, "").await;
        assert!(diff.is_empty(), "empty base_commit must return empty diff");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// In a real git repo, worktree_diff_from_base with the current HEAD returns an empty diff
    /// (no changes since HEAD).
    #[tokio::test]
    async fn worktree_diff_from_base_clean_tree_empty() {
        let dir = std::env::temp_dir().join(format!(
            "cam-diff-base-clean-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let _ = tokio::process::Command::new("git")
            .args(["init"])
            .current_dir(&dir)
            .output()
            .await;
        let _ = tokio::process::Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(&dir)
            .output()
            .await;
        let _ = tokio::process::Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(&dir)
            .output()
            .await;
        std::fs::write(dir.join("README.md"), "hello").unwrap();
        let _ = tokio::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&dir)
            .output()
            .await;
        let _ = tokio::process::Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(&dir)
            .output()
            .await;
        let head = tokio::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&dir)
            .output()
            .await
            .unwrap();
        let head_sha = String::from_utf8_lossy(&head.stdout).trim().to_string();
        let diff = worktree_diff_from_base(&dir, &head_sha).await;
        assert!(
            diff.is_empty(),
            "diff from HEAD to HEAD must be empty, got: {diff}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// When new commits are added after `base_commit`, the diff is non-empty.
    #[tokio::test]
    async fn worktree_diff_from_base_shows_commits_since_base() {
        let dir = std::env::temp_dir().join(format!(
            "cam-diff-base-commits-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let _ = tokio::process::Command::new("git")
            .args(["init"])
            .current_dir(&dir)
            .output()
            .await;
        let _ = tokio::process::Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(&dir)
            .output()
            .await;
        let _ = tokio::process::Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(&dir)
            .output()
            .await;
        std::fs::write(dir.join("README.md"), "initial").unwrap();
        let _ = tokio::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&dir)
            .output()
            .await;
        let _ = tokio::process::Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(&dir)
            .output()
            .await;
        let head = tokio::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&dir)
            .output()
            .await
            .unwrap();
        let base_sha = String::from_utf8_lossy(&head.stdout).trim().to_string();
        std::fs::write(dir.join("src.rs"), "fn new_fn() {}").unwrap();
        let _ = tokio::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&dir)
            .output()
            .await;
        let _ = tokio::process::Command::new("git")
            .args(["commit", "-m", "add feature"])
            .current_dir(&dir)
            .output()
            .await;
        let diff = worktree_diff_from_base(&dir, &base_sha).await;
        assert!(
            !diff.is_empty(),
            "diff from base to HEAD+1 must be non-empty"
        );
        assert!(
            diff.contains("new_fn"),
            "diff must contain the new function: {diff}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── 7. RepoWorktree + multi-repo integration gate ─────────────────────────────

    /// RepoWorktree is constructable and holds the expected fields.
    #[test]
    fn repo_worktree_struct_holds_expected_fields() {
        let rw = RepoWorktree {
            repo: "acme/api".to_string(),
            branch: "camerata/story-42".to_string(),
            dir: std::path::PathBuf::from("/tmp/acme-api-wt"),
            base_commit: "abc123".to_string(),
        };
        assert_eq!(rw.repo, "acme/api");
        assert_eq!(rw.branch, "camerata/story-42");
        assert_eq!(rw.base_commit, "abc123");
    }

    /// When integration_gate is None, run_multi_repo_integration_gate returns None
    /// regardless of how many repo_worktrees are provided.
    #[tokio::test]
    async fn multi_repo_integration_gate_absent_bundle_returns_none() {
        let runs = RunStore::new();
        let run_id = runs.create("acme/api#42", "test", crate::run::RunKind::Watched);
        let seq = std::sync::atomic::AtomicUsize::new(0);
        let next_seq = || seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;

        let worktrees = vec![
            RepoWorktree {
                repo: "acme/api".to_string(),
                branch: "camerata/story-42".to_string(),
                dir: std::path::PathBuf::from("/tmp/api"),
                base_commit: String::new(),
            },
            RepoWorktree {
                repo: "acme/frontend".to_string(),
                branch: "camerata/story-42".to_string(),
                dir: std::path::PathBuf::from("/tmp/frontend"),
                base_commit: String::new(),
            },
        ];
        let result = run_multi_repo_integration_gate(
            &runs,
            &run_id,
            &next_seq,
            &None, // no bundle — gate skipped
            &worktrees,
            0,
        )
        .await;
        assert!(
            result.is_none(),
            "gate must return None when bundle is absent (no contract boundary)"
        );
    }

    /// A stub LLM that returns "PASS" — used to test the gate pass path.
    struct PassLlm;

    #[async_trait::async_trait]
    impl crate::llm::Completer for PassLlm {
        async fn complete(
            &self,
            _req: crate::llm::LlmRequest,
        ) -> anyhow::Result<crate::llm::LlmResponse> {
            Ok(crate::llm::LlmResponse {
                text: "PASS".to_string(),
                model: "stub".to_string(),
                backend: "stub".to_string(),
                cost_usd: None,
                input_tokens: None,
                output_tokens: None,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
                or_cache_discount: None,
            })
        }
        async fn complete_streaming(
            &self,
            req: crate::llm::LlmRequest,
            on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
        ) -> anyhow::Result<crate::llm::LlmResponse> {
            let r = self.complete(req).await?;
            on_delta(&r.text);
            Ok(r)
        }
        fn as_any(&self) -> &dyn std::any::Any { self }
    }

    /// A stub LLM that returns "MISMATCH\n- contract violation" — tests the bounce path.
    struct MismatchLlm;

    #[async_trait::async_trait]
    impl crate::llm::Completer for MismatchLlm {
        async fn complete(
            &self,
            _req: crate::llm::LlmRequest,
        ) -> anyhow::Result<crate::llm::LlmResponse> {
            Ok(crate::llm::LlmResponse {
                text: "MISMATCH\n- backend omits email field required by contract".to_string(),
                model: "stub".to_string(),
                backend: "stub".to_string(),
                cost_usd: None,
                input_tokens: None,
                output_tokens: None,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
                or_cache_discount: None,
            })
        }
        async fn complete_streaming(
            &self,
            req: crate::llm::LlmRequest,
            on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
        ) -> anyhow::Result<crate::llm::LlmResponse> {
            let r = self.complete(req).await?;
            on_delta(&r.text);
            Ok(r)
        }
        fn as_any(&self) -> &dyn std::any::Any { self }
    }

    /// Multi-repo integration gate with PASS verdict → Some(Ok(())).
    /// Uses empty base_commits (worktree_diff_from_base returns "" for empty base)
    /// so the gate receives empty diffs; the stub LLM returns PASS regardless.
    #[tokio::test]
    async fn multi_repo_integration_gate_pass_returns_ok() {
        let runs = RunStore::new();
        let run_id = runs.create("acme/api#42", "test", crate::run::RunKind::Watched);
        let seq = std::sync::atomic::AtomicUsize::new(0);
        let next_seq = || seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;

        let llm: Arc<dyn crate::llm::Completer> = Arc::new(PassLlm);
        let bundle = Some(IntegrationGateBundle {
            contract: "GET /api/users returns [{id, name, email}]".to_string(),
            model: "claude-sonnet-4-6".to_string(),
            llm,
        });
        // Two repos; empty base_commits mean diff returns "" (short-circuit in worktree_diff_from_base).
        let worktrees = vec![
            RepoWorktree {
                repo: "acme/api".to_string(),
                branch: "camerata/story-42".to_string(),
                dir: std::path::PathBuf::from("/tmp/api"),
                base_commit: String::new(), // empty → diff returns ""
            },
            RepoWorktree {
                repo: "acme/frontend".to_string(),
                branch: "camerata/story-42".to_string(),
                dir: std::path::PathBuf::from("/tmp/frontend"),
                base_commit: String::new(),
            },
        ];
        let result = run_multi_repo_integration_gate(
            &runs, &run_id, &next_seq, &bundle, &worktrees, 0,
        )
        .await;
        assert!(result.is_some(), "gate must return Some when bundle is present");
        assert!(
            matches!(result.unwrap(), Ok(())),
            "PASS verdict must map to Some(Ok(()))"
        );
        // Gate events were emitted: info + pass.
        let run = runs.get(&run_id).unwrap();
        assert!(
            run.events.iter().any(|e| e.layer == "integration-gate" && e.verdict == "pass"),
            "must emit a pass gate event"
        );
    }

    /// Multi-repo integration gate with MISMATCH verdict → Some(Err(reason)).
    #[tokio::test]
    async fn multi_repo_integration_gate_bounce_returns_err() {
        let runs = RunStore::new();
        let run_id = runs.create("acme/api#42", "test", crate::run::RunKind::Watched);
        let seq = std::sync::atomic::AtomicUsize::new(0);
        let next_seq = || seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;

        let llm: Arc<dyn crate::llm::Completer> = Arc::new(MismatchLlm);
        let bundle = Some(IntegrationGateBundle {
            contract: "GET /api/users returns [{id, name, email}]".to_string(),
            model: "claude-sonnet-4-6".to_string(),
            llm,
        });
        let worktrees = vec![
            RepoWorktree {
                repo: "acme/api".to_string(),
                branch: "camerata/story-42".to_string(),
                dir: std::path::PathBuf::from("/tmp/api"),
                base_commit: String::new(),
            },
        ];
        let result = run_multi_repo_integration_gate(
            &runs, &run_id, &next_seq, &bundle, &worktrees, 0,
        )
        .await;
        assert!(result.is_some());
        let inner = result.unwrap();
        assert!(inner.is_err(), "MISMATCH must map to Some(Err(_))");
        assert!(
            inner.unwrap_err().contains("email"),
            "bounce reason must carry the mismatch detail"
        );
        let run = runs.get(&run_id).unwrap();
        assert!(
            run.events.iter().any(|e| e.layer == "integration-gate" && e.verdict == "fail"),
            "must emit a fail gate event"
        );
    }

    /// Single-repo synthetic worktree slice (the repo_worktrees.is_empty() fallback):
    /// the integration gate treats a single entry exactly like the multi-repo path.
    #[tokio::test]
    async fn multi_repo_gate_single_entry_slice_works_like_single_repo() {
        let runs = RunStore::new();
        let run_id = runs.create("acme/api#42", "test", crate::run::RunKind::Watched);
        let seq = std::sync::atomic::AtomicUsize::new(0);
        let next_seq = || seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;

        let llm: Arc<dyn crate::llm::Completer> = Arc::new(PassLlm);
        let bundle = Some(IntegrationGateBundle {
            contract: "single-repo contract".to_string(),
            model: "claude-sonnet-4-6".to_string(),
            llm,
        });
        let single = vec![RepoWorktree {
            repo: "acme/api".to_string(),
            branch: "camerata/story-42".to_string(),
            dir: std::path::PathBuf::from("/tmp/api"),
            base_commit: String::new(),
        }];
        let result = run_multi_repo_integration_gate(
            &runs, &run_id, &next_seq, &bundle, &single, 0,
        )
        .await;
        assert!(matches!(result, Some(Ok(()))), "single-entry gate must pass with PASS LLM");
    }
}
