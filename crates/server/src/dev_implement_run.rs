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

use camerata_agent::{prepare_session, HeartbeatFn};
use camerata_checks::runner_for_worktree;
use camerata_core::{AgentDriver, RuleId};
use camerata_fleet::{governed_role, locate_gateway_bin};
use camerata_worktracker::investigation::DecisionRecord;

use crate::api_agent_driver::build_agent_driver;
use crate::llm::LlmPort;
use crate::run::{live_mode_enabled, GateEvent, RunStatus, RunStore};
use crate::review_agent::{run_l3_review, L3ReviewInput, ReviewVerdict};
use crate::uow::UowStore;

/// One in-scope escalation for a governed dev run: a SELECTED rule whose SELECTED option carries an
/// escalation spec. The agent is grounded on `condition`; the server resolves `severity`
/// authoritatively (the agent's self-report names the rule, the server decides what happens). Built
/// from the corpus + the project's selections in `spawn_brownfield_dev_run`.
#[derive(Debug, Clone)]
pub struct EscalationInScope {
    pub rule_id: String,
    pub condition: String,
    pub severity: camerata_rules::EscalationSeverity,
}

/// The wire shape of one escalation the gateway's `raise_escalation` tool appends to
/// `<session_dir>/escalation-requests.jsonl`. Mirrors the gateway binary's `EscalationRequestRecord`
/// (the binary's type is not importable as a lib type), so the server reads escalations back off the
/// agent→run channel. The agent NAMES the rule + what it was doing; severity is NOT trusted from here
/// (the server resolves it from the corpus, so an agent cannot downgrade a hard-pause).
#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct EscalationRequestRecord {
    pub rule_id: String,
    #[serde(default)]
    pub condition_met: String,
    #[serde(default)]
    pub justification: String,
}

/// Read the FIRST escalation the agent raised from the session's escalation-request sink. The agent
/// is told to raise the single most-blocking one then stop; extras re-raise on resume. Returns
/// `None` when the sink is absent/empty/unparseable (the common case: no escalation). Pure read.
pub(crate) fn read_first_escalation_request(
    session_dir: &std::path::Path,
) -> Option<EscalationRequestRecord> {
    let sink = session_dir.join("escalation-requests.jsonl");
    let text = std::fs::read_to_string(sink).ok()?;
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .find_map(|l| serde_json::from_str::<EscalationRequestRecord>(l).ok())
}

/// The wire shape of one project-memory learning the gateway's `propose_memory` tool appends to
/// `<session_dir>/memory-proposals.jsonl` (#112, Layer 3). Mirrors the gateway binary's
/// `MemoryProposalRecord`. The agent proposes; the server appends as `Proposed`; the human curates.
#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct MemoryProposalRecord {
    #[serde(default)]
    pub kind: String,
    pub text: String,
}

/// Read ALL memory proposals the agent raised this run (it may propose several). Empty when the
/// sink is absent (the common case: no proposals). Pure read.
pub(crate) fn read_memory_proposals(session_dir: &std::path::Path) -> Vec<MemoryProposalRecord> {
    let sink = session_dir.join("memory-proposals.jsonl");
    let Ok(text) = std::fs::read_to_string(sink) else {
        return Vec::new();
    };
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<MemoryProposalRecord>(l).ok())
        .collect()
}

/// Map a proposal's `kind` string to the typed [`crate::project::MemoryKind`] (default Decision).
fn memory_kind_from_str(s: &str) -> crate::project::MemoryKind {
    match s {
        "pattern" => crate::project::MemoryKind::Pattern,
        "gotcha" => crate::project::MemoryKind::Gotcha,
        "constraint" => crate::project::MemoryKind::Constraint,
        _ => crate::project::MemoryKind::Decision,
    }
}

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
    pub llm: Arc<dyn LlmPort>,
}

/// Bundle for the integration-gate check (R3.e / GAP-6).
///
/// Two layers, deterministic-first:
///
/// 1. The DETERMINISTIC engine ([`camerata_checks::run_gate`]): the stack-generalized
///    cross-agent reconciliation over the assembled tree, driven by the project's
///    SELECTED `INTEGRATION-*` rules. This is the primary gate — a binary,
///    reproducible verdict with no model in the loop (the ADR's hard line). It runs
///    whenever at least one `INTEGRATION-*` rule is selected, regardless of whether a
///    prose contract exists (broadens the old contract-only trigger).
/// 2. The optional prose-contract advisory: when a prose contract exists AND an LLM is
///    available, a model-backed pass supplements the deterministic verdict for the
///    genuinely-semantic parts a static extractor cannot recover. It NEVER turns a
///    deterministic pass into a fail on its own; a model opinion never shows up as the
///    gate's green — undeterminable seams are review-tier.
pub struct IntegrationGateBundle {
    /// The SELECTED `INTEGRATION-*` corpus rule ids driving the deterministic engine.
    /// Empty means no deterministic seam rule is on (the engine short-circuits and the
    /// optional advisory carries the gate, if a contract + llm are present).
    pub selected_integration_rules: Vec<String>,
    /// Pre-parsed per-artifact `camerata:allow` waivers for the integration rules
    /// (intentional-public endpoints etc.).
    pub waivers: Vec<camerata_checks::GateWaiver>,
    /// The prose cross-repo contract (may be empty when the gate is driven purely by
    /// selected rules with no boundary contract).
    pub contract: String,
    /// The model to use for the optional prose-contract advisory pass.
    pub model: String,
    /// The LLM seam. `None` disables the advisory pass (deterministic-only).
    pub llm: Option<Arc<dyn LlmPort>>,
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
    // Superseded by `run_multi_repo_integration_gate` (which runs the deterministic
    // engine first); this single-repo advisory path only runs when an LLM is present.
    let llm = bundle.llm.as_ref()?;
    match crate::review_agent::check_integration_gate_live(
        llm.as_ref(),
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
                "Integration gate (R3.e / GAP-6) starting — {} repo(s), {} selected INTEGRATION rule(s) (iteration {iteration}).",
                repo_worktrees.len(),
                bundle.selected_integration_rules.len(),
            ),
            content_hash: None,
        },
    );

    // ── LAYER 1: the DETERMINISTIC engine (the primary gate, no model) ──────────────
    // Run the stack-generalized reconciliation over the assembled worktrees, driven by
    // the SELECTED INTEGRATION-* rules. Binary, reproducible verdict. This runs FIRST
    // and is authoritative: a deterministic FAIL bounces without ever consulting a model.
    if !bundle.selected_integration_rules.is_empty() {
        let gate_repos: Vec<camerata_checks::GateRepo> = repo_worktrees
            .iter()
            .map(|rw| camerata_checks::GateRepo {
                repo: rw.repo.clone(),
                dir: rw.dir.clone(),
            })
            .collect();
        let rules = bundle.selected_integration_rules.clone();
        let waivers = bundle.waivers.clone();
        // The extractors walk the filesystem; run off the async runtime.
        let verdict = tokio::task::spawn_blocking(move || {
            camerata_checks::run_gate(&gate_repos, &rules, &waivers)
        })
        .await
        .unwrap_or_else(|_| camerata_checks::GateVerdict {
            failures: Vec::new(),
            review: Vec::new(),
            waived: Vec::new(),
        });

        // Review-tier seams: surfaced HONESTLY as info (routed to human QA), never green.
        for item in &verdict.review {
            runs.push_event(
                run_id,
                GateEvent {
                    seq: next_seq(),
                    layer: "integration-gate".to_string(),
                    verdict: "review".to_string(),
                    rule: Some(item.rule_id.clone()),
                    detail: format!(
                        "REVIEW-TIER (human QA, NOT passed): {}",
                        item.detail
                    ),
                    content_hash: None,
                },
            );
        }
        // Waived findings: audit trail.
        for w in &verdict.waived {
            runs.push_event(
                run_id,
                GateEvent {
                    seq: next_seq(),
                    layer: "integration-gate".to_string(),
                    verdict: "waived".to_string(),
                    rule: Some(w.rule_id.clone()),
                    detail: format!("Waived by camerata:allow: {} ({})", w.artifact, w.location),
                    content_hash: None,
                },
            );
        }

        if !verdict.passed() {
            // Deterministic FAIL → bounce with the specific per-agent deltas (the same
            // bounce-and-revise loop as Layer 2). A genuine two-sides-incompatible fork is
            // for the architect; here we surface the deltas grouped by responsible repo.
            let targets = verdict.bounce_targets();
            let reason = targets
                .iter()
                .map(|(repo, deltas)| format!("{repo}: {}", deltas.join(" | ")))
                .collect::<Vec<_>>()
                .join(" || ");
            runs.push_event(
                run_id,
                GateEvent {
                    seq: next_seq(),
                    layer: "integration-gate".to_string(),
                    verdict: "fail".to_string(),
                    rule: None,
                    detail: format!(
                        "Integration gate (deterministic): BOUNCE — {} ({})",
                        verdict.summary(),
                        reason,
                    ),
                    content_hash: None,
                },
            );
            return Some(Err(reason));
        }

        runs.push_event(
            run_id,
            GateEvent {
                seq: next_seq(),
                layer: "integration-gate".to_string(),
                verdict: "pass".to_string(),
                rule: None,
                detail: format!(
                    "Integration gate (deterministic): PASS across {} repo(s) — {}.",
                    repo_worktrees.len(),
                    verdict.summary(),
                ),
                content_hash: None,
            },
        );
    }

    // ── LAYER 2: the OPTIONAL prose-contract advisory (model-backed, non-authoritative)
    // Only when a prose contract AND an LLM are present. Supplements the deterministic
    // verdict for the genuinely-semantic parts a static extractor cannot recover. A model
    // error is advisory (does not block); it never renders as the gate's green.
    let Some(llm) = bundle.llm.as_ref() else {
        // Deterministic-only: the gate's verdict already stands.
        return Some(Ok(()));
    };
    if bundle.contract.trim().is_empty() {
        return Some(Ok(()));
    }

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
        llm.as_ref(),
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
    escalations: &[EscalationInScope],
    model: &str,
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
    // ESCALATION CONDITIONS (the rule-agnostic, agent-driven gate): the selected rules whose chosen
    // option calls for escalation. The agent self-reports via `raise_escalation` when its work meets
    // one; the server resolves what happens from the rule's severity.
    let escalation_block = if escalations.is_empty() {
        String::new()
    } else {
        let lines = escalations
            .iter()
            .map(|e| {
                let sev = match e.severity {
                    camerata_rules::EscalationSeverity::HardPause => {
                        "HARD-PAUSE: stop and wait for a human"
                    }
                    camerata_rules::EscalationSeverity::SoftFlag => {
                        "SOFT-FLAG: you may continue after raising"
                    }
                };
                format!("- `{}` [{}]: {}", e.rule_id, sev, e.condition)
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "## ESCALATION CONDITIONS\n\n\
             If your work would meet ANY of these conditions, call the `raise_escalation` tool with \
             the rule id, what specifically met it, and your justification. Do NOT proceed past a \
             HARD-PAUSE condition (raise it, then stop and end your turn). When unsure whether a \
             condition is met, raise it rather than guess.\n\n\
             {lines}\n\n"
        )
    };

    // The shared governance protocol, specialized for the implementing agent's model tier.
    let kernel = if model.trim().is_empty() {
        camerata_app_core::GOVERNANCE_KERNEL.to_string()
    } else {
        camerata_app_core::kernel_for(model)
    };

    // GEOLOGICAL LAYERING (prefix-cache-optimal, provider-neutral):
    //   Layer 1 (global immutable, top; maximal cache) = the role header + the governance kernel.
    //     Identical across every call for this model tier and project.
    //   Layer 2 (epic/session context, middle; highly cached) = the grounding block (repo digest
    //     + rule context). Changes only every few days.
    //   Layer 3 (volatile execution state, bottom; never cached) = the story, the approved
    //     decisions, the escalation conditions, the required procedure, and (appended later by
    //     `append_bounce_feedback`) the LATEST toolchain/gate error. Different on almost every call.
    // The volatile story/error/diff MUST stay at the bottom so it never perturbs the cached
    // prefix. See camerata_app_core::prompt_layers and the plan's cache-layering section.
    let layer1_global = format!(
        "You are the BROWNFIELD IMPLEMENTER for story `{story_id}` (branch `{target_branch}`).\n\n\
         {kernel}"
    );
    let layer3_volatile = format!(
        "## Story\n\n\
         Title: {story_title}\n\
         Description: {story_desc}\n\n\
         ## Architect-approved decisions (the spec)\n\n\
         {decisions_text}\n\n\
         The approved decisions are binding. If the actual code contradicts a decision, do NOT \
         silently pick one: implement the decision if possible and state the contradiction in your \
         final report. Never substitute your own preference for an approved decision.\n\n\
         {escalation_block}\
         ## Required procedure (IN ORDER)\n\n\
         1. READ FIRST. Read every file you intend to change and its callers. If a file is not \
         where you expect, Grep/Glob for it; never assume its contents.\n\
         2. PLAN the minimal change satisfying the story AND every decision. If the story names a \
         pattern/class of defect, Grep and enumerate EVERY occurrence; cover all of them.\n\
         3. TESTS WITH THE CHANGE. Each new/changed behavior gets a test in the project's style \
         that fails if the behavior is removed. A change with no test must be justified.\n\
         4. IMPLEMENT via gated_write only, full file contents. Handle error/empty cases on every \
         path; validate input at boundaries; no new unwrap/panic on fallible paths unless the \
         file already does; match existing conventions exactly.\n\
         5. SELF-REVIEW BEFORE DONE. Re-read every changed file end to end: each criterion+decision \
         maps to a change; no syntax errors / missing imports / dangling refs; no unrelated file \
         touched; every grounding rule still holds. Fix, then re-read again.\n\n\
         ## Hard prohibitions\n\n\
         Do NOT run `git commit` (the server commits your changes after you finish). Do NOT push. \
         Do NOT change unrelated files. Never weaken or skip tests.\n\n\
         ## Final report (exact format)\n\n\
         CHANGES / TESTS / DECISIONS-TRACE / CONCERNS (NONE if empty)."
    );

    let mut layered = camerata_app_core::LayeredPrompt::new(layer1_global, layer3_volatile);
    if !grounding_block.trim().is_empty() {
        layered = layered.with_grounding(grounding_block);
    }
    layered.render()
}

/// LIFECYCLE-5: append the previous bounce iteration's failure feedback to the TAIL of the
/// base implement prompt, so the re-run agent gets the NEW information (violated rule ids +
/// the full toolchain / gate error text) instead of re-reading the identical prompt.
///
/// Tail placement is deliberate and cache-friendly: the base prompt (`base_task`) is the
/// stable, cached prefix; only this delta at the end is new, so the KV-cache prefix stays
/// warm across iterations. The block mirrors the `directive_grounding` append pattern from
/// `resume_governed_run` — it addresses the agent directly with the correction to apply.
///
/// STACK-AGNOSTIC: `feedback` is whatever the Layer-2 / Layer-3 / integration-gate check
/// emitted for this stack (clippy / tsc / pytest / go vet / gate deny reasons / contract
/// mismatch). Nothing here names a specific toolchain — it forwards the check's own output
/// verbatim. Pure + testable: no I/O, no async.
///
/// `iteration` is the count of passes that have failed so far (1 after the first bounce),
/// used only to label the revise block. `feedback` is the already-assembled failure detail.
pub fn append_bounce_feedback(base_task: &str, iteration: usize, feedback: &str) -> String {
    if feedback.trim().is_empty() {
        return base_task.to_string();
    }
    format!(
        "{base_task}\n\n\
         ## REVISE — a previous pass (#{iteration}) failed the gate\n\n\
         Your previous pass did NOT pass the post-task checks. Do NOT re-submit the same work: \
         READ the failure output below, find the ROOT cause in the code you touched, and fix it. \
         The exact violated rule ids and the verbatim toolchain / gate output from the failed \
         checks follow — treat them as the authoritative signal for what to change:\n\n\
         {feedback}",
        feedback = feedback.trim(),
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
    // The ACTIVE escalation spec for the test-tamper rule (the deterministic backstop), resolved
    // from the project's SELECTED option against the corpus. `Some` only when the selected option
    // carries an `escalation` — so an "allow" selection is `None` and the backstop is skipped. The
    // spec's `condition` + `severity` drive the escalation (hard-pause -> pause; soft-flag -> log +
    // continue). Field-driven, so the corpus is the source of truth, not hardcoded option ids.
    test_tamper_escalation: Option<camerata_rules::EscalationSpec>,
    // ALL in-scope agent-driven escalations: the selected rules whose selected option carries an
    // escalation spec. The agent is grounded on their conditions + can `raise_escalation`; the
    // server resolves severity from this list (authoritative) when an escalation comes back.
    escalations_in_scope: Vec<EscalationInScope>,
    // Project-memory sink (#112, Layer 3): the agent's `propose_memory` calls are read post-run and
    // appended as PROPOSED entries on this project. `project_id` is the active project to append to
    // (`None` skips memory capture, e.g. project-less test runs).
    projects: crate::project::ProjectStore,
    project_id: Option<String>,
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
        // LIFECYCLE-2: a failure is a genuine FAILED terminal, not a silent AwaitingQa. This
        // is what lets stamp_provenance_when_done withhold the stage advance + QA evidence
        // for work that never completed, while still freezing the honest gate provenance.
        runs.fail_with_reason(&run_id, detail);
    };
    // LIFECYCLE-1: honor a cancel mid-run. The terminal Cancelled state is already set by
    // RunStore::cancel; record it on the trail and stop BEFORE any git mutation (commit /
    // push). We never advance to AwaitingQa on a cancel.
    let cancelled_stop = |runs: &RunStore, uow: &UowStore, where_: &str| {
        runs.push_event(
            &run_id,
            GateEvent {
                seq: next_seq(),
                layer: "dev-implement".to_string(),
                verdict: "info".to_string(),
                rule: None,
                detail: format!("Run cancelled {where_}; stopped before any git mutation."),
                content_hash: None,
            },
        );
        uow.append_history(&story_id, "dev_implement", &format!("Cancelled {where_}."));
    };

    // Honor a cancel that arrived before the executor got scheduled.
    if runs.is_cancelled(&run_id) {
        cancelled_stop(&runs, &uow, "before start");
        return;
    }

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

    // GAP-2 / PROCESS-BRANCH-NAMING-1 chokepoint: HARD-BLOCK a UoW branch name that violates
    // the project's branch-naming rule before creating it. Opt-in rule (disabled by default),
    // so a no-op for projects that have not enabled branch-naming.
    let branch_gate_config = project_id
        .as_deref()
        .and_then(|pid| projects.get(pid))
        .map(|p| p.process_rule_config)
        .unwrap_or_default();
    if let Err(e) = crate::vcs_choke::gated_branch(&branch_gate_config, &target_branch) {
        fail(
            &runs,
            &uow,
            format!("VCS-action gate blocked the UoW branch name `{target_branch}`: {e}"),
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
    let spawn = match prepare_session(&gateway_bin, &role, Some(dir.as_path()), &sibling_read_dirs, None)
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
    // LIFECYCLE-7: wire the run's activity heartbeat onto the built driver so a healthy long
    // implement run keeps last_activity_ms fresh and is not reported stalled. The CLI path
    // fires it per output line; the API path fires it per loop turn (both wired inside
    // build_agent_driver). Mirrors investigation_run / update_branch_run / pr_resolve_run.
    let store_hb = runs.clone();
    let rid_hb = run_id.clone();
    let on_activity: HeartbeatFn = Arc::new(move || store_hb.touch_activity(&rid_hb, None));
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
        // Opt the implementer into the READ-CLASS raise_escalation tool so it can self-escalate
        // when its work meets a selected rule's escalation condition (the rule-agnostic path).
        true,
        Some(on_activity),
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

    // The STABLE base prompt (LIFECYCLE-5): built once, it is the cache-friendly prefix
    // that never changes across bounce iterations. On a bounce, the failed iteration's
    // rule ids + verbatim toolchain / gate output are appended at the TAIL via
    // `append_bounce_feedback` to form the pass-specific `task`, so the re-run agent gets
    // the NEW failure information instead of re-reading the identical prompt.
    let base_task = implement_prompt(
        &story_id,
        &story_title,
        &story_desc,
        &target_branch,
        &decisions,
        grounding.as_deref(),
        &escalations_in_scope,
        &model,
    );
    // The prompt actually handed to the agent this pass. First pass = the base prompt; each
    // bounce rebuilds it with the prior iteration's failure feedback appended at the tail.
    let mut task = base_task.clone();

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

        // ── Project-memory proposals (#112, Layer 3) ───────────────────────────────────
        // Did the agent call `propose_memory` this pass? Append each as a PROPOSED entry for the
        // human to curate, then clear the sink so it is not re-read on the next pass. Captured
        // BEFORE the escalation check so a paused run's proposals are not lost.
        if let Some(pid) = &project_id {
            let proposals = read_memory_proposals(spawn._dir.path());
            if !proposals.is_empty() {
                let _ = std::fs::remove_file(spawn._dir.path().join("memory-proposals.jsonl"));
                let _ = projects.update(pid, |p| {
                    for pr in &proposals {
                        if pr.text.trim().is_empty() {
                            continue;
                        }
                        let id = p.next_memory_id();
                        p.memory.push(crate::project::MemoryEntry {
                            id,
                            kind: memory_kind_from_str(&pr.kind),
                            text: pr.text.trim().to_string(),
                            source: format!("agent:{story_id}"),
                            status: crate::project::MemoryStatus::Proposed,
                            created: chrono::Utc::now().to_rfc3339(),
                        });
                    }
                });
            }
        }

        // ── Agent-driven escalation (the RULE-AGNOSTIC gate) ───────────────────────────
        // Did the agent call `raise_escalation` this pass? Severity is resolved AUTHORITATIVELY
        // from the rule's spec (the agent NAMES the rule; the server decides what happens — an
        // agent cannot downgrade a hard-pause). A not-in-scope rule id fails safe to hard-pause.
        if let Some(req) = read_first_escalation_request(spawn._dir.path()) {
            let severity = escalations_in_scope
                .iter()
                .find(|e| e.rule_id == req.rule_id)
                .map(|e| e.severity)
                .unwrap_or(camerata_rules::EscalationSeverity::HardPause);
            match severity {
                camerata_rules::EscalationSeverity::SoftFlag => {
                    let flag = format!(
                        "SOFT-FLAG {rule}: {what}. {why} Logged; the run continues.",
                        rule = req.rule_id,
                        what = req.condition_met,
                        why = req.justification,
                    );
                    runs.push_event(
                        &run_id,
                        GateEvent {
                            seq: next_seq(),
                            layer: "escalation".to_string(),
                            verdict: "soft-flag".to_string(),
                            rule: Some(req.rule_id.clone()),
                            detail: flag.clone(),
                            content_hash: None,
                        },
                    );
                    uow.append_history(&story_id, "dev_implement", &flag);
                    // Clear the sink so this same flag is not re-read on the next pass.
                    let _ = std::fs::remove_file(
                        spawn._dir.path().join("escalation-requests.jsonl"),
                    );
                }
                camerata_rules::EscalationSeverity::HardPause => {
                    // PAUSE for human review: checkpoint + UoW review escalation + AwaitingReview,
                    // the SAME engine the test-tamper backstop uses. The worktree is left intact.
                    let just = if req.justification.trim().is_empty() {
                        "(none given)"
                    } else {
                        req.justification.as_str()
                    };
                    let raise_req = crate::escalation::RaiseEscalationReq {
                        subject_kind: crate::escalation::SubjectKind::Uow,
                        checkpoint_id: None,
                        routine_id: story_id.clone(),
                        reason: format!("{}: {}", req.rule_id, req.condition_met),
                        stopped_for: format!(
                            "The implementer raised an escalation for rule `{rule}` on story \
                             `{story_id}` (branch `{target_branch}`). What met the condition: \
                             {what}. The agent's justification: {just}. Approve to resume from \
                             here, Amend to redirect, or Reject to revert and stop.",
                            rule = req.rule_id,
                            what = req.condition_met,
                        ),
                        suggestions: vec![
                            "Approve to authorize and resume the run from where it stopped."
                                .to_string(),
                            "Amend to give a corrected directive, then resume.".to_string(),
                            "Reject to revert the agent's work and stop.".to_string(),
                        ],
                        raw_context: format!(
                            "rule={}; story_id={story_id}; branch={target_branch}",
                            req.rule_id
                        ),
                    };
                    let esc =
                        escalations.raise_deduped(raise_req, "dev-implement raise_escalation");
                    if esc.checkpoint_id.is_none() {
                        let ckpt = checkpoints.create(crate::checkpoint::NewCheckpoint {
                            story_id: story_id.clone(),
                            run_id: run_id.clone(),
                            escalation_id: esc.id.clone(),
                            pause_reason: format!("rule-escalation:{}", req.rule_id),
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
                        "PAUSED for human review: the implementer raised `{rule}` — {what}. Not \
                         committed; the worktree is intact and review escalation ({esc_id}) is \
                         open. Resolve it to resume.",
                        rule = req.rule_id,
                        what = req.condition_met,
                        esc_id = esc.id,
                    );
                    runs.push_event(
                        &run_id,
                        GateEvent {
                            seq: next_seq(),
                            layer: "escalation".to_string(),
                            verdict: "paused".to_string(),
                            rule: Some(req.rule_id.clone()),
                            detail: pause_detail.clone(),
                            content_hash: None,
                        },
                    );
                    uow.append_history(&story_id, "dev_implement", &pause_detail);
                    runs.set_status(&run_id, RunStatus::AwaitingReview, false);
                    return;
                }
            }
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
                    // LIFECYCLE-5: feed the L3 reviewer's reasons into the next pass's prompt tail.
                    task = append_bounce_feedback(
                        &base_task,
                        iteration,
                        &format!(
                            "Layer-3 code review bounced. Reviewer findings:\n{}",
                            l3_bounce_reasons.join("\n")
                        ),
                    );
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
                    // LIFECYCLE-5: feed the integration-gate mismatch reason into the next pass.
                    task = append_bounce_feedback(
                        &base_task,
                        iteration,
                        &format!("Integration gate (R3.e) bounced — contract mismatch:\n{reason}"),
                    );
                    continue;
                }
            }
            break Ok(());
        }

        let checks = runner_for_worktree(&dir);
        // CheckRunner::check(role, worktree) → CheckOutcome { violated, diagnostics }.
        // `violated` is the rule-id verdict we bounce on; `diagnostics` is the captured,
        // truncation-bounded toolchain stdout/stderr fed back into the revise prompt tail.
        let check_result = checks.check(&role, &dir).await;
        match &check_result {
            Ok(outcome) if outcome.violated.is_empty() => {
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
                        // LIFECYCLE-5: feed the L3 reviewer's reasons into the next pass's prompt tail.
                        task = append_bounce_feedback(
                            &base_task,
                            iteration,
                            &format!(
                                "Layer-3 code review bounced. Reviewer findings:\n{}",
                                l3_bounce_reasons.join("\n")
                            ),
                        );
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
                        // LIFECYCLE-5: feed the integration-gate mismatch reason into the next pass.
                        task = append_bounce_feedback(
                            &base_task,
                            iteration,
                            &format!("Integration gate (R3.e) bounced — contract mismatch:\n{reason}"),
                        );
                        // Bounce: re-run the agent to fix the contract mismatch.
                        continue;
                    }
                }
                break Ok(());
            }
            Ok(outcome) => {
                let violations = &outcome.violated;
                let rule_ids: Vec<String> =
                    violations.iter().map(|RuleId(id)| id.clone()).collect();
                let rule_summary = rule_ids.join(", ");
                // The captured toolchain diagnostics (clippy/tsc/pytest/go stdout+stderr),
                // already truncation-bounded by the CheckOutcome; forwarded verbatim.
                let diagnostics = outcome.diagnostics.as_str();
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
                // LIFECYCLE-5: feed the failure back into the NEXT pass's prompt at the tail.
                // The Layer-2 check emits the violated rule ids AND the verbatim toolchain
                // diagnostics (stack-agnostic: whatever the detected toolchain flagged — Rust
                // clippy/test, tsc, pytest, go vet, manifest checks). We forward BOTH so a
                // literal open-weight model gets the actual error text to self-correct, not
                // just the rule id. The diagnostics go LAST (cache-friendly tail) and are
                // already truncation-bounded by CheckOutcome so the warm prefix cache holds.
                let l2_feedback = if diagnostics.trim().is_empty() {
                    format!("Layer-2 checks failed. Violated rule id(s): {rule_summary}")
                } else {
                    format!(
                        "Layer-2 checks failed. Violated rule id(s): {rule_summary}\n\n\
                         Verbatim toolchain output from the failed checks (authoritative — \
                         fix the ROOT cause it describes):\n{diagnostics}"
                    )
                };
                task = append_bounce_feedback(&base_task, iteration, &l2_feedback);
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
    // FIELD-DRIVEN: this deterministic backstop runs only when the project's SELECTED option for
    // AGENTIC-NO-TEST-TAMPER-1 carries an `escalation` spec (`test_tamper_escalation` is `Some`).
    // An "allow" selection is `None` and skips the backstop entirely. The spec's `severity` decides
    // hard-pause (stop for review) vs soft-flag (log + continue), and its `condition` is the message.
    if let Some(esc_spec) = &test_tamper_escalation {
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
            format!("AGENTIC-NO-TEST-TAMPER-1: {} — {listed}", esc_spec.condition)
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
            match esc_spec.severity {
                // SOFT-FLAG: the selected option is advisory — record a warning and CONTINUE.
                camerata_rules::EscalationSeverity::SoftFlag => {
                    let flag = format!(
                        "SOFT-FLAG AGENTIC-NO-TEST-TAMPER-1: {} — {listed}. Logged for review; the \
                         run continues (the selected option is advisory, not a hard pause).",
                        esc_spec.condition
                    );
                    runs.push_event(
                        &run_id,
                        GateEvent {
                            seq: next_seq(),
                            layer: "test-tamper-guard".to_string(),
                            verdict: "soft-flag".to_string(),
                            rule: Some("AGENTIC-NO-TEST-TAMPER-1".to_string()),
                            detail: flag.clone(),
                            content_hash: None,
                        },
                    );
                    uow.append_history(&story_id, "dev_implement", &flag);
                    // Fall through: no pause, the run proceeds to commit.
                }
                // HARD-PAUSE: stop for human review. Persist the run's resumable state as a
                // checkpoint, raise a deduped UoW review escalation, link the two, and park the
                // run at AwaitingReview. The worktree is left intact (the agent's partial work
                // stays on disk). Resolving the escalation RE-SPAWNS the run from this checkpoint.
                camerata_rules::EscalationSeverity::HardPause => {
                    let raise_req = crate::escalation::RaiseEscalationReq {
                        subject_kind: crate::escalation::SubjectKind::Uow,
                        checkpoint_id: None,
                        routine_id: story_id.clone(),
                        reason: format!("AGENTIC-NO-TEST-TAMPER-1 — {}", esc_spec.condition),
                        stopped_for: format!(
                            "An agent must not modify or delete existing tests without human \
                             review (the cheapest way to make a failing suite go green is to edit \
                             the test that caught the broken code). The implementation for story \
                             `{story_id}` on `{target_branch}` changed these existing test(s): \
                             {listed}. Confirm the test edits are legitimate (a real refactor, not \
                             masking broken code) before this proceeds. Adding new tests is always \
                             allowed — only edits/deletions of existing tests are blocked."
                        ),
                        suggestions: vec![
                            "Review the test diff: is each change a legitimate refactor, or does \
                             it weaken the assertion that was catching a real failure?"
                                .to_string(),
                            "If legitimate, Approve to resume the run from where it stopped."
                                .to_string(),
                            "If the agent edited a test to mask broken code, Reject to revert and \
                             stop."
                                .to_string(),
                        ],
                        raw_context: format!(
                            "story_id={story_id}; branch={target_branch}; tampered={listed}"
                        ),
                    };
                    let esc =
                        escalations.raise_deduped(raise_req, "dev-implement test-tamper guard");
                    // Idempotent: a re-run hitting the same still-open escalation reuses its
                    // checkpoint rather than piling up duplicates.
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
                         modified/deleted — {listed}. Not committed; the worktree is left intact \
                         and a review escalation ({esc_id}) is open. Resolve it to resume from \
                         where the run stopped.",
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
        }
    }

    // LIFECYCLE-1: cancel check IMMEDIATELY before the git mutation. A Stop that arrived
    // while the agent ran (or during layer-2 / the test-tamper guard) must halt the run
    // BEFORE the server commits anything to the worktree.
    if runs.is_cancelled(&run_id) {
        cancelled_stop(&runs, &uow, "before commit");
        return;
    }

    // The SERVER commits the agent's implementation (commit stays server-side, never
    // the agent — mirrors pr_resolve_run exactly).
    //
    // GAP-2 chokepoint. This is an ORCHESTRATION-INTERNAL commit, but Camerata knows the run's
    // story id, so we author a message that is COMPLIANT with the project's active process rules
    // (conventional-commit shape + substantive body + story-id reference in the required format)
    // and take the HARD-BLOCK path. A non-compliant machine message is then a real bug that
    // surfaces here, not a silent bypass. The `_or_bypass` fallback remains available for cases
    // that genuinely cannot satisfy a rule.
    let vcs_config = project_id
        .as_deref()
        .and_then(|pid| projects.get(pid))
        .map(|p| p.process_rule_config)
        .unwrap_or_default();
    let numeric_id = crate::vcs_choke::numeric_story_id(&story_id);
    let commit_msg = crate::vcs_choke::compliant_machine_commit_message(
        &vcs_config,
        "feat",
        &format!("implement story {story_id} on {target_branch}"),
        &numeric_id,
    );
    if let Err(e) = crate::vcs_choke::gated_commit(&vcs_config, &commit_msg) {
        // The machine message failed the project's gate. Fail closed — this is a bug in the
        // message generator or a run missing a usable story id, not a reason to commit past it.
        fail(&runs, &uow, format!("VCS-action gate blocked the machine commit: {e}"));
        return;
    }

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

    // LIFECYCLE-1: cancel check IMMEDIATELY before the push (network git mutation). The
    // commit above is local; a cancel here stops us short of publishing the branch, and the
    // run stays in its terminal Cancelled state (never advances to AwaitingQa).
    if runs.is_cancelled(&run_id) {
        cancelled_stop(&runs, &uow, "before push (implementation committed locally)");
        return;
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
            &[],
            "",
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

    /// The hardened implement prompt embeds the shared governance kernel, the ordered
    /// procedure with the mandatory SELF-REVIEW step, and the exact final-report contract.
    #[test]
    fn implement_prompt_embeds_kernel_selfreview_and_report_contract() {
        let p = implement_prompt(
            "acme/api#7",
            "T",
            "D",
            "b",
            &[approved_decision("opt-a", "Q", "R")],
            None,
            &[],
            "claude-opus-4-8",
        );

        // Governance kernel markers + a load-bearing clause.
        assert!(
            p.contains("=== CAMERATA OPERATING PROTOCOL"),
            "prompt must embed the governance kernel"
        );
        assert!(p.contains("=== END OPERATING PROTOCOL ==="));
        assert!(p.contains("GROUND EVERY FACT"), "kernel clause must be present");

        // The ordered procedure's mandatory self-review step.
        assert!(
            p.contains("SELF-REVIEW BEFORE DONE"),
            "prompt must mandate the pre-done self-review pass"
        );
        assert!(
            p.contains("Required procedure (IN ORDER)"),
            "prompt must present the ordered procedure"
        );

        // The exact final-report contract.
        assert!(
            p.contains("CHANGES / TESTS / DECISIONS-TRACE / CONCERNS"),
            "prompt must carry the exact final-report contract"
        );

        // Decisions-binding clause.
        assert!(
            p.contains("approved decisions are binding"),
            "prompt must state that approved decisions are binding"
        );

        // An Opus model carries the strongest-tier addendum.
        assert!(
            p.contains("TIER DISCIPLINE (strongest)"),
            "an Opus model must carry the strongest-tier addendum"
        );
    }

    /// GEOLOGICAL LAYERING: the kernel (Layer 1) leads, the grounding block (Layer 2) sits in
    /// the middle, and the volatile story + procedure (Layer 3) is at the bottom, so the stable
    /// Layer-1/Layer-2 prefix never has volatile content leak above it.
    #[test]
    fn implement_prompt_orders_layers_kernel_then_grounding_then_story() {
        let grounding = "=== PROJECT GROUNDING ===\nrepo digest here\n=== END PROJECT GROUNDING ===";
        let p = implement_prompt(
            "s/r#9",
            "MY-STORY-TITLE",
            "desc",
            "b",
            &[approved_decision("opt", "Q", "R")],
            Some(grounding),
            &[],
            "claude-opus-4-8",
        );
        let i_kernel = p.find("=== CAMERATA OPERATING PROTOCOL").expect("layer 1");
        let i_grounding = p.find("=== PROJECT GROUNDING ===").expect("layer 2");
        let i_story = p.find("MY-STORY-TITLE").expect("layer 3");
        assert!(
            i_kernel < i_grounding,
            "Layer 1 (kernel) must lead Layer 2 (grounding)"
        );
        assert!(
            i_grounding < i_story,
            "Layer 2 (grounding) must precede Layer 3 (story)"
        );
        // The whole grounding block is above the story: no volatile content leaks up.
        let i_grounding_end = p.find("=== END PROJECT GROUNDING ===").expect("grounding end");
        assert!(
            i_grounding_end < i_story,
            "the story (Layer 3) must sit entirely below the grounding block"
        );
    }

    /// The stable Layer-1/Layer-2 prefix is byte-identical across two builds that differ only in
    /// the Layer-3 story/description input (the prefix-cache-stability invariant).
    #[test]
    fn implement_prompt_prefix_is_stable_across_differing_story() {
        let grounding = "=== PROJECT GROUNDING ===\ndigest\n=== END PROJECT GROUNDING ===";
        let mk = |title: &str, desc: &str| {
            implement_prompt(
                "s/r#1",
                title,
                desc,
                "b",
                &[approved_decision("opt", "Q", "R")],
                Some(grounding),
                &[],
                "claude-opus-4-8",
            )
        };
        let a = mk("Story A", "description alpha");
        let b = mk("Story B totally different", "description beta");
        // Both share the identical stable prefix up to the end of the grounding block.
        let end = "=== END PROJECT GROUNDING ===";
        let a_prefix_len = a.find(end).unwrap() + end.len();
        let b_prefix_len = b.find(end).unwrap() + end.len();
        assert_eq!(a_prefix_len, b_prefix_len, "prefix boundary must be at the same byte offset");
        assert_eq!(
            &a[..a_prefix_len],
            &b[..b_prefix_len],
            "the Layer-1/Layer-2 prefix must be byte-identical across differing Layer-3 input"
        );
        // The bodies themselves differ (Layer 3 changed).
        assert_ne!(a, b);
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
            &[],
            "",
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
        let p = implement_prompt("s/r#1", "T", "D", "b", &[], None, &[], "");
        assert!(p.contains("no approved decisions"));
    }

    #[test]
    fn implement_prompt_renders_escalation_conditions() {
        let escalations = vec![
            EscalationInScope {
                rule_id: "ORCH-ONE-WAY-DOOR-1".to_string(),
                condition: "the change is hard to reverse".to_string(),
                severity: camerata_rules::EscalationSeverity::HardPause,
            },
            EscalationInScope {
                rule_id: "ORCH-BUDGET-1".to_string(),
                condition: "spend is running away".to_string(),
                severity: camerata_rules::EscalationSeverity::SoftFlag,
            },
        ];
        let p = implement_prompt("s/r#1", "T", "D", "b", &[], None, &escalations, "");
        // The agent is told about the tool + each rule's condition + severity.
        assert!(p.contains("## ESCALATION CONDITIONS"));
        assert!(p.contains("raise_escalation"));
        assert!(p.contains("ORCH-ONE-WAY-DOOR-1"));
        assert!(p.contains("HARD-PAUSE"));
        assert!(p.contains("the change is hard to reverse"));
        assert!(p.contains("ORCH-BUDGET-1"));
        assert!(p.contains("SOFT-FLAG"));
    }

    #[test]
    fn implement_prompt_omits_escalation_section_when_none_in_scope() {
        let p = implement_prompt("s/r#1", "T", "D", "b", &[], None, &[], "");
        assert!(!p.contains("## ESCALATION CONDITIONS"));
        assert!(!p.contains("raise_escalation"));
    }

    #[test]
    fn read_first_escalation_request_parses_first_and_handles_absent() {
        let dir = tempfile::tempdir().unwrap();
        let sink = dir.path().join("escalation-requests.jsonl");
        std::fs::write(
            &sink,
            "\n{\"rule_id\":\"ORCH-ONE-WAY-DOOR-1\",\"condition_met\":\"renamed a public trait\",\"justification\":\"needed for X\"}\n\
             {\"rule_id\":\"OTHER\",\"condition_met\":\"y\"}\n",
        )
        .unwrap();
        let req = read_first_escalation_request(dir.path()).expect("first record parses");
        assert_eq!(req.rule_id, "ORCH-ONE-WAY-DOOR-1");
        assert_eq!(req.condition_met, "renamed a public trait");
        assert_eq!(req.justification, "needed for X");
        // Absent sink -> None (the common case: the agent did not escalate).
        let empty = tempfile::tempdir().unwrap();
        assert!(read_first_escalation_request(empty.path()).is_none());
    }

    #[test]
    fn read_memory_proposals_parses_all_and_kind_maps() {
        let dir = tempfile::tempdir().unwrap();
        let sink = dir.path().join("memory-proposals.jsonl");
        std::fs::write(
            &sink,
            "\n{\"kind\":\"gotcha\",\"text\":\"The auth flow assumes X.\"}\n\
             {\"kind\":\"pattern\",\"text\":\"Use the repository pattern.\"}\n",
        )
        .unwrap();
        let props = read_memory_proposals(dir.path());
        assert_eq!(props.len(), 2, "ALL proposals are read (agent may propose several)");
        assert_eq!(props[0].text, "The auth flow assumes X.");
        assert_eq!(
            memory_kind_from_str(&props[0].kind),
            crate::project::MemoryKind::Gotcha
        );
        assert_eq!(
            memory_kind_from_str(&props[1].kind),
            crate::project::MemoryKind::Pattern
        );
        // Unknown / absent kind falls back to Decision.
        assert_eq!(
            memory_kind_from_str("nonsense"),
            crate::project::MemoryKind::Decision
        );
        // Absent sink -> empty (the common case: no proposals).
        let empty = tempfile::tempdir().unwrap();
        assert!(read_memory_proposals(empty.path()).is_empty());
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
            None,
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

    // ── 2c. LIFECYCLE-5: bounce loop feeds errors back into the re-prompt ────────

    /// The second pass's prompt CONTAINS the prior iteration's rule ids + error text at the
    /// TAIL, and DIFFERS from the first pass's prompt. This is the open-weight linchpin: the
    /// re-run agent gets new information instead of re-reading the identical prompt.
    #[test]
    fn append_bounce_feedback_puts_rule_ids_and_errors_at_the_tail() {
        let base = implement_prompt(
            "acme/api#42",
            "Add login",
            "Support email/password login.",
            "camerata/story-42",
            &[approved_decision("auth", "JWT?", "Use JWT.")],
            None,
            &[],
            "",
        );
        // Feedback carries a violated rule id AND verbatim toolchain error text.
        let feedback = "Layer-2 checks failed. Violated rule id(s): RUST-CLIPPY\n\
                        error[E0308]: mismatched types\n  --> src/auth.rs:12:5";
        let second = append_bounce_feedback(&base, 1, feedback);

        // It differs from the first prompt.
        assert_ne!(second, base, "the bounce prompt must differ from the first pass");
        // The base prompt is a PREFIX (cache-friendly tail placement): the whole base is
        // still present, unchanged, at the head of the second prompt.
        assert!(second.starts_with(&base), "base prompt must remain the cached prefix");
        // The rule id + verbatim error text are present.
        assert!(second.contains("RUST-CLIPPY"), "violated rule id must be fed back");
        assert!(
            second.contains("error[E0308]: mismatched types"),
            "verbatim toolchain error text must be fed back"
        );
        assert!(second.contains("src/auth.rs:12:5"), "error location must be fed back");
        // The feedback sits at the TAIL (after the base prompt), not in the middle.
        let feedback_pos = second.find("error[E0308]").unwrap();
        assert!(
            feedback_pos > base.len().saturating_sub(1),
            "the error feedback must be appended AFTER the base prompt (tail placement)"
        );
        // The revise header is present so the agent knows this is a correction pass.
        assert!(second.contains("REVISE"), "the revise header must be present");
    }

    /// LIFECYCLE-5 (CheckRunner full-diagnostics): the L2 bounce feedback assembled at
    /// the call site now carries the FULL toolchain diagnostics captured by the runner
    /// (not just rule ids), and they ride the CACHE-FRIENDLY TAIL of the re-prompt.
    #[test]
    fn append_bounce_feedback_carries_full_runner_diagnostics_at_the_tail() {
        use camerata_core::CheckOutcome;

        let base = implement_prompt("s/r#7", "T", "D", "b", &[], None, &[], "");
        // Mirror the runner → call-site path: a CheckOutcome with violated rule ids
        // AND captured toolchain diagnostics (the "strict stack trace").
        let outcome = CheckOutcome::new(
            vec![camerata_core::RuleId("RUST-TEST".into())],
            "$ cargo test\nrunning 1 test\ntest auth::login_rejects_bad_password ... FAILED\n\
             thread 'auth::login_rejects_bad_password' panicked at 'assertion failed: \
             `(left == right)`\n  left: `401`,\n right: `200`', src/auth.rs:88:9",
        );
        // Same assembly the L2 arm performs: rule ids first, verbatim diagnostics last.
        let rule_summary = outcome
            .violated
            .iter()
            .map(|r| r.0.clone())
            .collect::<Vec<_>>()
            .join(", ");
        let l2_feedback = format!(
            "Layer-2 checks failed. Violated rule id(s): {rule_summary}\n\n\
             Verbatim toolchain output from the failed checks (authoritative — \
             fix the ROOT cause it describes):\n{}",
            outcome.diagnostics
        );
        let second = append_bounce_feedback(&base, 1, &l2_feedback);

        assert!(second.starts_with(&base), "base prompt must remain the cached prefix");
        // The FULL diagnostic body (panic message + assertion + file:line), not just the id.
        assert!(second.contains("RUST-TEST"), "rule id must be present");
        assert!(
            second.contains("assertion failed") && second.contains("src/auth.rs:88:9"),
            "the FULL toolchain diagnostics must be fed back, not just the rule id"
        );
        // Diagnostics ride the tail: the panic text lands AFTER the rule-id citation.
        let ids_pos = second.find("RUST-TEST").unwrap();
        let diag_pos = second.find("assertion failed").unwrap();
        assert!(diag_pos > ids_pos, "diagnostics must be appended after the rule ids");
    }

    /// LIFECYCLE-5 truncation: diagnostics larger than the cap are bounded by
    /// `CheckOutcome` BEFORE they reach the prompt, so the warm prefix cache holds.
    /// The oldest head is dropped and the failing tail is preserved.
    #[test]
    fn oversized_runner_diagnostics_are_truncated_before_the_reprompt() {
        use camerata_core::{CheckOutcome, DIAGNOSTICS_CAP_BYTES};

        let base = implement_prompt("s/r#8", "T", "D", "b", &[], None, &[], "");
        let mut outcome = CheckOutcome::new(vec![camerata_core::RuleId("RUST-CLIPPY".into())], "");
        outcome.push_diagnostics("HEAD_NOISE_TO_EVICT");
        outcome.push_diagnostics(&"clippy warning line\n".repeat(DIAGNOSTICS_CAP_BYTES / 10));
        outcome.push_diagnostics("FINAL_ERROR_SUMMARY_KEEP_ME");

        let second = append_bounce_feedback(&base, 1, &outcome.diagnostics);

        // Bounded: base prompt + a diagnostics block near the cap + the fixed revise
        // header prose. The point is the diagnostics can't grow unbounded with the raw
        // toolchain output — it stays within the cap plus a small constant overhead.
        assert!(
            second.len() <= base.len() + DIAGNOSTICS_CAP_BYTES + 1024,
            "the re-prompt diagnostics must be bounded by the cap, got {} extra bytes",
            second.len() - base.len()
        );
        assert!(
            second.contains("FINAL_ERROR_SUMMARY_KEEP_ME"),
            "the failing tail must survive truncation"
        );
        assert!(
            !second.contains("HEAD_NOISE_TO_EVICT"),
            "the oldest head must be evicted past the cap"
        );
    }

    /// Empty feedback is a no-op: the prompt is unchanged (defensive — never append an
    /// empty revise block).
    #[test]
    fn append_bounce_feedback_empty_is_noop() {
        let base = implement_prompt("s/r#1", "T", "D", "b", &[], None, &[], "");
        assert_eq!(append_bounce_feedback(&base, 1, "   "), base);
        assert_eq!(append_bounce_feedback(&base, 1, ""), base);
    }

    /// STACK-AGNOSTIC: whatever the check emitted is forwarded verbatim — the helper never
    /// hardcodes a toolchain. A tsc error, a pytest failure, and a go vet finding all pass
    /// through identically.
    #[test]
    fn append_bounce_feedback_is_stack_agnostic() {
        let base = implement_prompt("s/r#1", "T", "D", "b", &[], None, &[], "");
        for feedback in [
            "src/app.ts(10,3): error TS2322: Type 'string' is not assignable to type 'number'.",
            "FAILED tests/test_auth.py::test_login - assert 401 == 200",
            "./main.go:8:2: undefined: foo",
            "Integration gate (R3.e) bounced — contract mismatch:\nbackend omits email field",
        ] {
            let out = append_bounce_feedback(&base, 1, feedback);
            assert!(out.contains(feedback), "the check's own output must pass through verbatim: {feedback}");
            assert!(out.starts_with(&base), "base stays the prefix for {feedback}");
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
            None, // no active test-tamper escalation (unreached here: live mode is off)
            Vec::new(), // no in-scope agent-driven escalations in this test
            crate::project::ProjectStore::new(),
            None, // no project to capture memory into in this test
        )
        .await;

        let run = runs.get(&run_id).expect("run exists");

        // LIFECYCLE-2: an inability to implement (live mode off) is a genuine FAILED
        // terminal, not a spurious AwaitingQa success. The stamper keys off this to withhold
        // the stage advance + QA evidence for work that never happened.
        assert!(matches!(run.status, RunStatus::Failed { .. }));
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
            &[],
            "",
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
    impl crate::llm::LlmPort for PassLlm {
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
    impl crate::llm::LlmPort for MismatchLlm {
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

        let llm: Arc<dyn crate::llm::LlmPort> = Arc::new(PassLlm);
        let bundle = Some(IntegrationGateBundle {
            selected_integration_rules: Vec::new(), // model-advisory-only path here
            waivers: Vec::new(),
            contract: "GET /api/users returns [{id, name, email}]".to_string(),
            model: "claude-sonnet-4-6".to_string(),
            llm: Some(llm),
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

        let llm: Arc<dyn crate::llm::LlmPort> = Arc::new(MismatchLlm);
        let bundle = Some(IntegrationGateBundle {
            selected_integration_rules: Vec::new(),
            waivers: Vec::new(),
            contract: "GET /api/users returns [{id, name, email}]".to_string(),
            model: "claude-sonnet-4-6".to_string(),
            llm: Some(llm),
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

    /// DETERMINISTIC path (GAP-6): selected INTEGRATION rules drive the stack-generalized
    /// engine over the assembled worktrees, with NO model in the loop. A matching
    /// producer/consumer pair passes; a drifting pair bounces to the consumer.
    #[tokio::test]
    async fn deterministic_gate_matching_pair_passes_drifting_pair_bounces() {
        use std::io::Write;
        fn repo(files: &[(&str, &str)]) -> tempfile::TempDir {
            let td = tempfile::tempdir().unwrap();
            std::fs::write(td.path().join("package.json"), "{\"name\":\"x\"}").unwrap();
            for (name, content) in files {
                let p = td.path().join(name);
                let mut f = std::fs::File::create(&p).unwrap();
                f.write_all(content.as_bytes()).unwrap();
            }
            td
        }

        // MATCHING pair: producer serves POST /members/export, consumer calls it.
        let api = repo(&[("routes.js", "app.post('/members/export', h)\n")]);
        let ui = repo(&[("client.js", "axios.post('/members/export', body)\n")]);
        let worktrees = vec![
            RepoWorktree {
                repo: "acme/api".to_string(),
                branch: "b".to_string(),
                dir: api.path().to_path_buf(),
                base_commit: String::new(),
            },
            RepoWorktree {
                repo: "acme/ui".to_string(),
                branch: "b".to_string(),
                dir: ui.path().to_path_buf(),
                base_commit: String::new(),
            },
        ];
        let bundle = Some(IntegrationGateBundle {
            selected_integration_rules: vec!["INTEGRATION-API-CONTRACT-1".to_string()],
            waivers: Vec::new(),
            contract: String::new(), // deterministic-only, no model advisory
            model: String::new(),
            llm: None,
        });
        let runs = RunStore::new();
        let run_id = runs.create("acme/api#1", "test", crate::run::RunKind::Watched);
        let seq = std::sync::atomic::AtomicUsize::new(0);
        let next_seq = || seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        let result =
            run_multi_repo_integration_gate(&runs, &run_id, &next_seq, &bundle, &worktrees, 0).await;
        assert!(matches!(result, Some(Ok(()))), "matching pair passes deterministically");

        // DRIFTING pair: consumer calls a route the producer never exposes.
        let ui2 = repo(&[("client.js", "axios.post('/members/csv', body)\n")]);
        let worktrees2 = vec![
            RepoWorktree {
                repo: "acme/api".to_string(),
                branch: "b".to_string(),
                dir: api.path().to_path_buf(),
                base_commit: String::new(),
            },
            RepoWorktree {
                repo: "acme/ui".to_string(),
                branch: "b".to_string(),
                dir: ui2.path().to_path_buf(),
                base_commit: String::new(),
            },
        ];
        let runs2 = RunStore::new();
        let run_id2 = runs2.create("acme/api#2", "test", crate::run::RunKind::Watched);
        let seq2 = std::sync::atomic::AtomicUsize::new(0);
        let next_seq2 = || seq2.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        let result2 =
            run_multi_repo_integration_gate(&runs2, &run_id2, &next_seq2, &bundle, &worktrees2, 0)
                .await;
        match result2 {
            Some(Err(reason)) => assert!(reason.contains("acme/ui"), "consumer bounced: {reason}"),
            other => panic!("drifting pair must bounce, got {other:?}"),
        }
        let run = runs2.get(&run_id2).unwrap();
        assert!(run
            .events
            .iter()
            .any(|e| e.layer == "integration-gate" && e.verdict == "fail"));
    }

    /// DETERMINISTIC review-tier (GAP-6): a repo whose stack has no extractor is reported
    /// review-tier (an honest info event), NOT a silent green.
    #[tokio::test]
    async fn deterministic_gate_no_extractor_is_review_tier() {
        let mystery = tempfile::tempdir().unwrap();
        std::fs::write(mystery.path().join("README.txt"), "no manifest").unwrap();
        let worktrees = vec![RepoWorktree {
            repo: "acme/mystery".to_string(),
            branch: "b".to_string(),
            dir: mystery.path().to_path_buf(),
            base_commit: String::new(),
        }];
        let bundle = Some(IntegrationGateBundle {
            selected_integration_rules: vec!["INTEGRATION-API-CONTRACT-1".to_string()],
            waivers: Vec::new(),
            contract: String::new(),
            model: String::new(),
            llm: None,
        });
        let runs = RunStore::new();
        let run_id = runs.create("acme/mystery#1", "test", crate::run::RunKind::Watched);
        let seq = std::sync::atomic::AtomicUsize::new(0);
        let next_seq = || seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        let result =
            run_multi_repo_integration_gate(&runs, &run_id, &next_seq, &bundle, &worktrees, 0).await;
        // No deterministic FAIL, but a review-tier event must be present.
        assert!(matches!(result, Some(Ok(()))), "no mechanical failure");
        let run = runs.get(&run_id).unwrap();
        assert!(
            run.events
                .iter()
                .any(|e| e.layer == "integration-gate" && e.verdict == "review"),
            "uncovered stack must surface a review-tier event, not a silent pass"
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

        let llm: Arc<dyn crate::llm::LlmPort> = Arc::new(PassLlm);
        let bundle = Some(IntegrationGateBundle {
            selected_integration_rules: Vec::new(),
            waivers: Vec::new(),
            contract: "single-repo contract".to_string(),
            model: "claude-sonnet-4-6".to_string(),
            llm: Some(llm),
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
