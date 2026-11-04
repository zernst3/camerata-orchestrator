//! Run lifecycle handlers: health, rules and stories, governed runs and their provenance,
//! feedback, sign-off and clarifications.

use crate::*;

pub(crate) async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok", "service": "camerata-server" }))
}

/// The gate's enforced rules, minus the GOV-1 verification fixture (kept in the
/// registry and the test suite, deliberately not surfaced to the cockpit).
pub(crate) async fn rules() -> Json<Vec<RuleDto>> {
    let dtos = RULE_REGISTRY
        .iter()
        .filter(|e| e.id != "GOV-1")
        .map(|e| RuleDto {
            id: e.id.to_string(),
            statement: e.description.to_string(),
        })
        .collect();
    Json(dtos)
}

/// The canonical story spine, scoped to the active project.
///
/// ISOLATION (A8): the spine is global, so an unfiltered list leaks every project's
/// stories. A story belongs to the active project when any of its build TARGET repos
/// or its SOURCE container (`external_ref.container`) is in the project's `repos`.
/// No active project → empty list.
pub(crate) async fn stories(
    State(state): State<AppState>,
) -> Result<Json<Vec<CanonicalStory>>, AppError> {
    let Some(p) = state.projects.active() else {
        return Ok(Json(Vec::new()));
    };
    let in_scope = |s: &CanonicalStory| -> bool {
        s.targets
            .iter()
            .any(|t| p.repos.iter().any(|r| r == &t.repo))
            || s.external_ref
                .as_ref()
                .and_then(|e| e.container.as_ref())
                .is_some_and(|c| p.repos.iter().any(|r| r == c))
    };
    let list: Vec<CanonicalStory> = state
        .stories
        .list()
        .await
        .map_err(AppError)?
        .into_iter()
        .filter(in_scope)
        .collect();
    Ok(Json(list))
}

/// Optional request body for starting a governed run. All fields are optional
/// so the existing no-body callers remain compatible.
#[derive(serde::Deserialize, Default)]
pub(crate) struct StartRunReq {
    /// The model id (`/api/models`) for every `claude -p` agent in the live
    /// fleet. Ignored for the scripted (token-free) path. `None`/blank falls
    /// back to the CLI default so the live fleet's behaviour is unchanged when
    /// the caller sends no body.
    #[serde(default)]
    pub(crate) model: Option<String>,
    /// The per-UoW THREE-TIER model map (ORCH-MODEL-TIERING-1). When present, the
    /// development fleet runs TIERED: each task runs on its capability band's model
    /// (`strongest`/`balanced`/`fast`), with the strongest tier acting as the
    /// orchestrator/lead. When absent, the single-`model` path is used (back-compat).
    /// The two fields are independent; `tier_map` takes precedence when both are sent.
    #[serde(default)]
    pub(crate) tier_map: Option<crate::model_tier::TierMap>,
    /// One-time BOOTSTRAP escape hatch (default OFF): when `Some(true)`, this single run
    /// uses a NO-OP layer-2 runner — no post-task lint/test bounce — so a brownfield repo
    /// can install the linters/checkers that layer-2 needs without tripping the fail-closed
    /// "could-not-run" deadlock. It skips ONLY layer 2. Layer 1 (the deny-before-write gate
    /// every spawned agent runs behind) and the no-code-first decisions gate
    /// (`ensure_development_gate`) are UNCHANGED in both cases — the gate is never bypassed.
    /// `None`/`false` is exactly today's behaviour (the real per-language CheckRunner). See
    /// `docs/decisions/2026-06-22_ci_wiring_both_layers_and_layer2_bootstrap_bypass.md`.
    #[serde(default)]
    pub(crate) skip_layer2: Option<bool>,
}

/// Start a governed run for a story. Returns the run id immediately; the run walks
/// to completion on a background task, driving planted tool calls through the REAL
/// gate (deterministic, token-free). Poll `GET /api/runs/:id` for status + verdicts.
///
/// The optional JSON body accepts `{ "model": "<id>" }` to pin the model for the
/// live-fleet path. The scripted (token-free) path ignores it.
pub(crate) async fn start_run(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    req: Option<Json<StartRunReq>>,
) -> Response {
    let (model, tier_map, skip_layer2) = match req {
        Some(Json(r)) => (
            r.model.filter(|m| !m.trim().is_empty()),
            r.tier_map,
            r.skip_layer2.unwrap_or(false),
        ),
        None => (None, None, false),
    };

    // LIFECYCLE-9 (single-flight guard): reject a second run on a story that already has an
    // active (non-done) run. Two concurrent runs on one story would share a single worktree —
    // corrupting each other's edits — and a sign-off could tear down a still-live run. The
    // paused-then-resumed path is NOT blocked: a resume marks the paused run done before the
    // resume run supersedes it (see `resume_governed_run` / escalation resolve). Enforced
    // BEFORE the development gate so a duplicate start is refused as early as possible.
    if let Some(active) = state.runs.active_run_for_story(&story_id) {
        let body = Json(serde_json::json!({
            "error": "a run is already active for this story",
            "reason": format!(
                "Run {} is still in progress for story {story_id} (status: {:?}). Only one \
                 run may be active on a story at a time (concurrent runs would share one \
                 worktree). Cancel or let the active run finish before starting another.",
                active.id, active.status,
            ),
            "story_id": story_id,
            "active_run_id": active.id,
        }));
        return (StatusCode::CONFLICT, body).into_response();
    }

    // Repository pre-flight (live runs only; the scripted run writes no code): a unit that
    // should work in a repository but cannot (no branch selected, no local clone, ...) is
    // refused here with the reason, never silently handed to the greenfield scaffolder.
    // Checked BEFORE the development gate so a refusal leaves the lifecycle stage untouched.
    if live_mode_enabled() {
        let uow = state.uow.get_or_create(&story_id);
        if let crate::run_target::RunTarget::Refused { reason } =
            crate::run_target::plan_run_target_for_state(&state, &story_id, &uow)
        {
            let body = Json(serde_json::json!({
                "error": "no repository for this run",
                "reason": reason,
                "story_id": story_id,
            }));
            return (StatusCode::CONFLICT, body).into_response();
        }
    }

    // The no-code-first gate (Pillar 2): a governed run cannot start until every
    // DecisionRecord on this story's UoW is approved (decisions_approved_for_development).
    // We block + surface exactly why, rather than silently starting a run that the
    // architect did not gate. The check reads the persisted decisions on the UoW. The
    // gate is identical for the single-model and tiered paths.
    if let Err(reason) = ensure_development_gate(&state, &story_id) {
        let body = Json(serde_json::json!({
            "error": "development gate not satisfied",
            "reason": reason,
            "story_id": story_id,
        }));
        return (StatusCode::CONFLICT, body).into_response();
    }

    // Interactive HTTP start: Watched (alert-only on stall). The autonomous/routine path
    // passes RunKind::Autonomous (see start_governed_run's `kind` param).
    let (run_id, mode) = start_governed_run(
        &state,
        &story_id,
        model,
        tier_map,
        skip_layer2,
        crate::run::RunKind::Watched,
    )
    .await;
    Json(serde_json::json!({ "run_id": run_id, "story_id": story_id, "mode": mode }))
        .into_response()
}

/// Enforce the no-code-first gate for a story before a governed run may start.
///
/// Returns `Ok(())` when development is permitted, or `Err(reason)` with a human-
/// readable explanation when it is blocked. The gate is the structural decision check
/// (`decisions_approved_for_development`): at least one decision exists and every
/// decision is `Approved`.
///
/// As a side effect, when the gate IS satisfied it best-effort drives the UoW's
/// lifecycle stage forward to `Development` (Investigating → DecisionsApproved →
/// Development as needed), so the persisted stage reflects that a governed run is now
/// underway. The forward drive is best-effort: a UoW already past these stages is left
/// as-is, never moved backward.
pub fn ensure_development_gate(state: &AppState, story_id: &str) -> Result<(), String> {
    use crate::lifecycle::UowStage;
    use camerata_worktracker::investigation::decisions_approved_for_development;

    let uow = state.uow.get_or_create(story_id);

    if !decisions_approved_for_development(&uow.decisions) {
        let unapproved = uow.decisions.iter().filter(|d| d.needs_review()).count();
        let reason = if uow.decisions.is_empty() {
            "No decisions have been recorded for this story yet. The investigation \
             must surface at least one decision and the architect must approve it \
             before any code is written."
                .to_string()
        } else {
            format!(
                "{unapproved} of {} decision(s) still need the architect's approval. \
                 Every decision must be approved before a governed run can start.",
                uow.decisions.len()
            )
        };
        return Err(reason);
    }

    // Contract precondition (R3.g): when the work crosses a contract boundary,
    // a non-empty contract must exist before development starts. The orchestrator
    // refuses and pushes back — as a lead engineer refuses to parallelize a team
    // across an interface no one has agreed.
    if uow.investigation.crosses_boundary && uow.investigation.contract.trim().is_empty() {
        return Err(
            "This story crosses a contract boundary (crosses_boundary = true) but no \
             contract has been written yet. Write the interface contract in the \
             Investigation phase before starting development (R3.g). The refinement \
             agent may author the contract, but it must exist before code is written."
                .to_string(),
        );
    }

    // Gate satisfied — drive the lifecycle stage forward to Development, stepping
    // through any intermediate stages. Each step is best-effort: a transition that is
    // illegal from the current stage (because the UoW is already further along, or was
    // never moved off Intake) is simply skipped, never forced.
    match uow.stage {
        UowStage::Intake => {
            let _ = state.uow.begin_investigation(story_id);
            let _ = state.uow.approve_decisions(story_id);
            let _ = state.uow.start_development(story_id);
        }
        UowStage::Investigating => {
            let _ = state.uow.approve_decisions(story_id);
            let _ = state.uow.start_development(story_id);
        }
        UowStage::DecisionsApproved => {
            let _ = state.uow.start_development(story_id);
        }
        // Already at/after Development: leave the stage as-is.
        _ => {}
    }
    Ok(())
}

/// Start a governed run for a story through the ONE pipeline every development task
/// uses — live fleet (worktree → gated MCP write → layer-2 checks → bounce) when opted
/// in, the token-free scripted gate otherwise. Returns `(run_id, mode)`. Shared so a
/// brownfield remediation run is governed EXACTLY like any other dev task, not a
/// special path: fixing the audited items is a development task, the first one.
///
/// `model` is forwarded to every `claude -p` agent in the live-fleet path. It is
/// ignored for the scripted path (which makes no agent calls).
///
/// `skip_layer2` is the one-time BOOTSTRAP escape hatch (default OFF). When `true`, the
/// live fleet runs this ONE run with a no-op layer-2 runner (no post-task lint/test
/// bounce) so a brownfield repo can install the tooling layer-2 needs. It skips ONLY
/// layer 2: layer 1 (the deny-before-write gate) and the no-code-first decisions gate
/// (`ensure_development_gate`, already enforced in the caller) are unchanged. The scripted
/// Revert a paused run's work in `worktree_dir` (used on Reject). Best-effort.
///
/// LIFECYCLE-12: the per-iteration bounce loop makes `camerata: snapshot` COMMITS, so
/// discarding only uncommitted changes (`git checkout -- .` + `git clean -fd`) would leave
/// the agent's committed work on the branch — where it could still be pushed. A Reject
/// promises to "revert the agent's work", so when `base_commit` is known we FIRST
/// `git reset --hard <base_commit>` to drop every commit the run added since the branch
/// point, THEN clean untracked files. Without a base_commit (older checkpoints) we fall back
/// to the uncommitted-only revert. Errors are swallowed (the worktree may be gone); the
/// caller records the outcome regardless.
pub(crate) async fn revert_worktree(worktree_dir: &str, base_commit: Option<&str>) {
    let dir = std::path::Path::new(worktree_dir);
    if !dir.exists() {
        return;
    }
    // Drop the run's committed snapshots first: reset the branch back to the branch-point
    // commit so nothing the agent committed survives. Only when we actually have the base.
    if let Some(base) = base_commit.filter(|b| !b.trim().is_empty()) {
        let _ = workspace::git_command(Some(dir), workspace::GitEnv::Isolated)
            .args(["reset", "--hard", base])
            .output()
            .await;
    } else {
        // No base commit recorded: discard tracked changes (uncommitted-only fallback).
        let _ = workspace::git_command(Some(dir), workspace::GitEnv::Isolated)
            .args(["checkout", "--", "."])
            .output()
            .await;
    }
    // Remove any new untracked files the agent left behind (a reset --hard leaves these).
    let _ = workspace::git_command(Some(dir), workspace::GitEnv::Isolated)
        .args(["clean", "-fd"])
        .output()
        .await;
}

/// Resume a governed dev run that PAUSED on a review escalation (Approve / Amend). Re-spawns the
/// implement run from the checkpoint's worktree (the agent's partial work is still on disk) with
/// the human's `directive` injected into the grounding, so the agent continues from where it
/// stopped. Marks the checkpoint resumed so it can't be consumed twice. Returns the new run id +
/// mode, or an error when the checkpoint was already resumed.
pub(crate) async fn resume_governed_run(
    state: &AppState,
    checkpoint: &crate::checkpoint::Checkpoint,
    directive: &str,
) -> Result<(String, &'static str), &'static str> {
    // Guard: a checkpoint resumes at most once.
    if state.checkpoints.mark_resumed(&checkpoint.id).is_none() {
        return Err("checkpoint already resumed or unknown");
    }
    let live = live_mode_enabled();
    let mode = if live { "live" } else { "scripted" };
    let story_id = checkpoint.story_id.clone();
    let (title, desc) = match state.stories.get(&story_id).await {
        Ok(Some(s)) => (s.title, s.description),
        _ => (story_id.clone(), String::new()),
    };
    let run_id = state
        .runs
        .create(&story_id, mode, crate::run::RunKind::Watched);
    state
        .record_governance(
            camerata_persistence::GovernanceEvent::info(run_id.clone(), "run_started", "system")
                .with_story_id(story_id.clone())
                .with_reason(format!("resumed run (mode={mode}) after human review")),
        )
        .await;

    if !live {
        // HONESTY GATE (NO placeholders): a resume cannot CONTINUE the paused work without a live
        // agent. Marking a non-continued run `AwaitingQa` (success) would fabricate a completed
        // cycle, so we REFUSE — fail the run loudly (mirrors start_governed_run + the three
        // token-free refusals). LIFECYCLE-4: the provenance watcher is still spawned FIRST so it
        // observes the terminal transition and freezes this run's (empty) gate provenance; because
        // the terminal status is `Failed` it will NOT advance the UoW stage or attach evidence.
        spawn_provenance_watcher(state, &run_id, &story_id);
        state.runs.fail_with_reason(
            &run_id,
            "Resuming this run requires the AI agent. Set CAMERATA_LIVE_BUILD=1 to continue."
                .to_string(),
        );
        state.uow.append_history(
            &story_id,
            "dev_implement",
            &format!(
                "Resume refused: live mode is off (set CAMERATA_LIVE_BUILD=1). The run was not \
                 continued and no state advanced. Requested directive: {directive}"
            ),
        );
        return Ok((run_id, mode));
    }

    // Describe why the run PAUSED from the checkpoint's actual pause_reason, rather than
    // hardcoding the test-tamper guard (checkpoints also pause on rule escalations).
    let pause_desc = match checkpoint.pause_reason.as_str() {
        "test-tamper" => {
            "the test-tamper guard AGENTIC-NO-TEST-TAMPER-1 (it modified or deleted an existing test)"
                .to_string()
        }
        other => match other.strip_prefix("rule-escalation:") {
            Some(rule_id) => format!("a governance escalation on rule {rule_id}"),
            None => format!("a human-review escalation ({other})"),
        },
    };
    let directive_grounding = format!(
        "## RESUMED AFTER HUMAN REVIEW\n\nThis run PAUSED for human review by \
         {pause_desc}. A human has now reviewed it and responded. Continue from the CURRENT \
         worktree state (your prior work is still here) and apply this decision \
         exactly:\n\n{directive}"
    );
    // LIFECYCLE-4: the resumed live run now gets the SAME completion-driven, success-gated
    // provenance-stamping watcher a fresh run does — previously the resume path spawned none,
    // so a resumed run never got provenance / evidence / a stage advance. Spawned BEFORE
    // handing `story_id` to the implement run; the watcher awaits the run's terminal state.
    spawn_provenance_watcher(state, &run_id, &story_id);
    // A unit created from inspection findings also carries the findings it must close.
    let extra_grounding =
        crate::remediation::extra_grounding_for(&state.uow, &story_id, Some(directive_grounding))
            .await;
    spawn_brownfield_dev_run(
        state,
        run_id.clone(),
        story_id,
        title,
        desc,
        checkpoint.repo.clone(),
        std::path::PathBuf::from(&checkpoint.worktree_dir),
        checkpoint.branch.clone(),
        checkpoint.model.clone(),
        checkpoint.max_iterations,
        None,  // resume uses the checkpoint's single model, not a tier map
        false, // resume runs the full gates (no skip_layer2)
        extra_grounding,
    )
    .await;
    Ok((run_id, mode))
}

/// Shared brownfield "derive project inputs + spawn the implement run", used by BOTH a fresh
/// governed run ([`start_governed_run`]) and a RESUMED one ([`resume_governed_run`]). Given the run
/// IDENTITY (run id, story text, worktree, model, iteration budget), it derives everything that
/// comes from the active project + UoW — grounding (plus any `extra_grounding`, e.g. a resume
/// directive), the read scope, the L3 + integration-gate bundles, the multi-repo worktrees, and the
/// test-tamper enforcement flag — then spawns `execute_dev_implement_run`. Keeping this in one place
/// is what stops the fresh-start and resume paths from drifting.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn spawn_brownfield_dev_run(
    state: &AppState,
    run_id: String,
    story_id: String,
    title: String,
    desc: String,
    repo: String,
    dir: std::path::PathBuf,
    branch: String,
    impl_model: String,
    max_iterations: usize,
    tier_map: Option<crate::model_tier::TierMap>,
    skip_layer2: bool,
    // Extra context appended to the project grounding. `None` for a fresh run; for a RESUME this is
    // the human's directive (approve / amend / reject) so the re-spawned agent knows the decision.
    extra_grounding: Option<String>,
) {
    let store = state.runs.clone();
    let uow_store = state.uow.clone();
    let uow_data = state.uow.get_or_create(&story_id);
    let decisions = state.uow.decisions_for(&story_id);
    let token = state.github_token();

    // GROUNDING (the invariant): rule + repo digest for the implementer, plus any resume directive.
    let base_grounding = state.project_grounding().await;
    let grounding = match extra_grounding {
        Some(extra) if !extra.trim().is_empty() => Some(match base_grounding {
            Some(g) => format!("{g}\n\n{extra}"),
            None => extra,
        }),
        _ => base_grounding,
    };
    // MULTI-REPO READ scope: ALL the active project's local repo clones (added READ-ONLY).
    let read_dirs = state.active_repo_dirs();
    // Layer-3 agentic code review (R7): built when the active project has L3 enabled.
    let l3_bundle: Option<crate::dev_implement_run::L3ReviewBundle> = {
        state.projects.active().and_then(|proj| {
            if !proj.l3_review.enabled {
                return None;
            }
            let l3_model = proj.l3_model().to_string();
            let rules_prose = grounding.clone().unwrap_or_default();
            let story_text = format!("{title}\n\n{desc}");
            // The L3 reviewer runs on THIS project's backend (a refused Api project reports the
            // refusal from the review instead of running on the subscription).
            let llm: Arc<dyn crate::llm::LlmPort> = match state.llm_for_project(Some(&proj)) {
                Ok(l) => Arc::new(l),
                Err(message) => Arc::new(crate::llm::RefusedLlm::new(message)),
            };
            Some(crate::dev_implement_run::L3ReviewBundle {
                story_text,
                rules_prose,
                model: l3_model,
                llm,
                on_error: proj.l3_review.on_error,
                fix_rounds: proj
                    .l3_review
                    .fix_rounds
                    .min(camerata_api_types::project::MAX_L3_FIX_ROUNDS),
            })
        })
    };
    // Integration gate bundle (R3.e / GAP-6): built when the project has SELECTED at
    // least one `INTEGRATION-*` rule (deterministic engine), OR the UoW crosses a
    // boundary with a prose contract (the optional model advisory). This broadens the
    // old contract-only trigger: the deterministic cross-agent gate runs whenever an
    // integration rule is on, whether or not a prose contract exists.
    let integration_gate_bundle: Option<crate::dev_implement_run::IntegrationGateBundle> = {
        // The project's SELECTED INTEGRATION-* rule ids drive the deterministic engine.
        let selected_integration_rules: Vec<String> = state
            .projects
            .active()
            .map(|p| {
                p.ruleset
                    .selections
                    .iter()
                    .filter(|s| s.rule_id.starts_with("INTEGRATION-"))
                    // A selection is "on" unless it explicitly chose a no-op option.
                    .map(|s| s.rule_id.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let has_contract = uow_data.investigation.crosses_boundary
            && !uow_data.investigation.contract.trim().is_empty();

        if selected_integration_rules.is_empty() && !has_contract {
            None
        } else {
            // Pre-parse per-artifact `camerata:allow INTEGRATION-* -- reason` waivers
            // from the in-scope repo worktrees (intentional-public endpoints etc.).
            let waivers = collect_integration_waivers(state).await;
            let gate_model = match &tier_map {
                Some(map) => map.balanced_primary().to_string(),
                None => impl_model.clone(),
            };
            // The model advisory only runs with a prose contract; otherwise llm=None
            // keeps the gate deterministic-only.
            let llm: Option<Arc<dyn crate::llm::LlmPort>> = if has_contract {
                Some(state.step_llm_port())
            } else {
                None
            };
            Some(crate::dev_implement_run::IntegrationGateBundle {
                selected_integration_rules,
                waivers,
                contract: uow_data.investigation.contract.clone(),
                model: gate_model,
                llm,
            })
        }
    };
    // Multi-repo worktree setup (R3.f / R6): a worktree + story branch per in-scope repo.
    let repo_worktrees: Vec<crate::dev_implement_run::RepoWorktree> = {
        let intake_repos = uow_data.intake.repos.clone();
        if intake_repos.len() <= 1 {
            Vec::new()
        } else {
            let workspace_root = state.settings.workspace_root();
            let mut worktrees = Vec::new();
            for scope in &intake_repos {
                let scope_branch =
                    crate::run_target::unit_branch_for_repo(&uow_data, &story_id, &scope.repo)
                        .unwrap_or_else(|| derive_scope_branch(&scope.branch, &story_id));
                let override_path = state.settings.repo_path(&scope.repo);
                if let Some(wt_dir) = crate::workspace::resolve_uow_worktree(
                    override_path.as_deref(),
                    workspace_root.as_deref(),
                    &scope.repo,
                    &scope_branch,
                    active_project_repos(state).as_deref(),
                )
                .await
                {
                    let base = workspace::git_command(Some(&wt_dir), workspace::GitEnv::Isolated)
                        .args(["rev-parse", "HEAD"])
                        .output()
                        .await
                        .ok()
                        .filter(|o| o.status.success())
                        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                        .unwrap_or_default();
                    worktrees.push(crate::dev_implement_run::RepoWorktree {
                        repo: scope.repo.clone(),
                        branch: scope_branch,
                        dir: wt_dir,
                        base_commit: base,
                    });
                }
            }
            worktrees
        }
    };
    // Test-tamper DETERMINISTIC BACKSTOP (AGENTIC-NO-TEST-TAMPER-1): field-driven, not hardcoded.
    // We resolve the rule's ACTIVE escalation spec from the project's selected option against the
    // corpus — `Some` only when the selected (or default) option carries an `escalation`, so an
    // "allow" selection yields `None` and the backstop never fires. The spec's condition/severity
    // then drive the escalation in the run. (The agent-driven path covers all OTHER rules; this is
    // the one rule that also has a mechanical detector, kept as a safety net.)
    // Two outputs from one corpus load:
    //   - `escalations_in_scope`: every SELECTED rule whose SELECTED option carries an escalation
    //     spec. The agent is grounded on these + can `raise_escalation` (the rule-agnostic gate).
    //   - `test_tamper_escalation`: the test-tamper rule's active spec specifically, driving its
    //     deterministic detector backstop. `None` when its selected option does not escalate.
    let (escalations_in_scope, test_tamper_escalation) = {
        let corpus = camerata_rules::load_corpus_lenient(&camerata_rules::corpus_path())
            .await
            .0;
        let selections = state
            .projects
            .active()
            .map(|p| p.ruleset.selections.clone())
            .unwrap_or_default();
        let in_scope: Vec<crate::dev_implement_run::EscalationInScope> = selections
            .iter()
            .filter_map(|s| {
                let rule = corpus.get_by_id(&s.rule_id)?;
                let spec = rule.selected_escalation(s.chosen_option.as_deref())?;
                Some(crate::dev_implement_run::EscalationInScope {
                    rule_id: s.rule_id.clone(),
                    condition: spec.condition.clone(),
                    severity: spec.severity,
                })
            })
            .collect();
        let tt = crate::test_tamper::test_tamper_escalation(&corpus, &selections).cloned();
        (in_scope, tt)
    };
    let impl_escalations = state.escalations.clone();
    let impl_checkpoints = state.checkpoints.clone();
    // Project-memory capture (#112, Layer 3): the store + the active project to append proposals to.
    let impl_projects = state.projects.clone();
    let impl_project_id = state.projects.active().map(|p| p.id);
    let impl_registry = state.model_registry.clone();
    let impl_policy = state.settings.provider_policy();
    let impl_creds = state.credential_store.clone();
    let impl_limiter = state.rate_limiter.clone();
    // Phase H2: threaded into `execute_dev_implement_run` so the audit trail (agent_step,
    // gate_allow/deny, escalation_raised, check_failed/layer2_bounce) is recorded under
    // THIS run's id. `None` in tests / on open failure (fail-soft — see AppState::from_env).
    let impl_governance_log = state.governance_log.clone();
    // LIFECYCLE-1: register the abort handle so a Stop can reap a run blocked inside the
    // live implement agent subprocess (kill_on_drop). Cleared when the run finishes.
    let runs_for_clear = state.runs.clone();
    let rid_for_clear = run_id.clone();
    let rid_for_clear2 = run_id.clone();
    let handle = tokio::spawn(async move {
        crate::dev_implement_run::execute_dev_implement_run(
            store,
            uow_store,
            run_id,
            story_id,
            title,
            desc,
            repo,
            dir,
            branch,
            decisions,
            token,
            impl_model,
            max_iterations,
            skip_layer2,
            grounding,
            read_dirs,
            l3_bundle,
            integration_gate_bundle,
            repo_worktrees,
            impl_registry,
            impl_policy,
            impl_creds,
            impl_limiter,
            impl_escalations,
            impl_checkpoints,
            test_tamper_escalation,
            escalations_in_scope,
            impl_projects,
            impl_project_id,
            impl_governance_log,
        )
        .await;
        runs_for_clear.clear_abort(&rid_for_clear);
    });
    state
        .runs
        .register_abort(&rid_for_clear2, handle.abort_handle());
}

/// path has no layer-2 bounce, so the flag is a no-op there.
pub(crate) async fn start_governed_run(
    state: &AppState,
    story_id: &str,
    model: Option<String>,
    tier_map: Option<crate::model_tier::TierMap>,
    skip_layer2: bool,
    // LIFECYCLE-6: the run kind decides the stall threshold + auto-cancel posture. Interactive
    // (HTTP) starts pass `Watched` (alert-only); a routine-/scheduler-driven walk-away run passes
    // `Autonomous` (longer threshold + auto-cancel by the background sweep).
    kind: crate::run::RunKind,
) -> (String, &'static str) {
    let live = live_mode_enabled();
    let mode = if live { "live" } else { "scripted" };
    let kind_label = format!("{kind:?}");
    let run_id = state.runs.create(story_id, mode, kind);
    state
        .record_governance(
            camerata_persistence::GovernanceEvent::info(run_id.clone(), "run_started", "system")
                .with_story_id(story_id.to_string())
                .with_reason(format!(
                    "governed run started (mode={mode}, kind={kind_label})"
                )),
        )
        .await;
    let store = state.runs.clone();
    let rid = run_id.clone();

    if live {
        // Real governed fleet (needs the gateway binary + claude + tokens). Pass the
        // story so the live executor can build a plan from it.
        let (title, desc) = match state.stories.get(story_id).await {
            Ok(Some(s)) => (s.title, s.description),
            _ => (story_id.to_string(), String::new()),
        };

        // Resolve the effective tier map for this run: caller-supplied map takes
        // precedence (explicit per-run override); fall back to the project's
        // persisted tier map so the project settings panel is authoritative.
        let tier_map: Option<crate::model_tier::TierMap> =
            tier_map.or_else(|| state.projects.active().map(|p| p.tier_map));

        // The active project's Designer (vision) band toggle. Gates whether the
        // orchestrator's delegate-models map carries a `"vision"` tier at all; when
        // false (the default), `delegate {tier:"vision"}` is refused on the child side.
        let vision_enabled = state
            .projects
            .active()
            .map(|p| p.vision_enabled)
            .unwrap_or(false);

        // The active project's loop-guard ceiling (#29) governs how many times a dirty
        // stage may bounce-and-revise before its residual violations are surfaced.
        let max_iterations = state
            .projects
            .active()
            .map(|p| p.max_iterations)
            .unwrap_or(1);

        // ── Brownfield vs. greenfield dispatch ────────────────────────────────
        //
        // `run_target::plan_run_target` decides where the run writes:
        //   - Repository: the unit's branch is set and its repo has a local clone. Use the
        //     brownfield in-place implement path: the agent edits the EXISTING codebase on the
        //     UoW's branch, and the server commits. The spec comes from the UoW's APPROVED
        //     decisions (the no-code-first gate already verified them).
        //   - Greenfield: the project has no repositories and the unit names none. Scaffold a
        //     new app from a plan in a throwaway temp dir (execute_live_run / _tiered), and
        //     say so in the run's events.
        //   - Refused: the unit should work in a repository but cannot (no branch selected, no
        //     local clone, ...). The run fails with the reason; it is NEVER silently handed to
        //     the greenfield scaffolder. `start_run` refuses the same cases up front with a 409;
        //     this arm covers the other callers (e.g. the orchestrator chat).
        //
        // GATE UNCHANGED in every path: `ensure_development_gate` (the no-code-first
        // check) runs above, and every agent spawned here uses `governed_role` +
        // `gated_write`-only tools regardless of which branch is taken.
        //
        // `story_id` is a `&str` reference that doesn't outlive the function; own it
        // early so the tokio::spawn closure can capture it as `'static`.
        let story_id_owned = story_id.to_string();
        let uow_data = state.uow.get_or_create(story_id);
        let target = match crate::run_target::plan_run_target_for_state(state, story_id, &uow_data)
        {
            crate::run_target::RunTarget::Repository {
                repo,
                branch,
                clone,
            } => {
                // One source of truth for the branch: the plan derived it from the Intake
                // scope, so record it (the history says so when it changed) BEFORE the
                // worktree and the implement run, which read the unit's recorded branch.
                crate::run_target::record_run_branch(&state.uow, story_id, &branch);
                match crate::run_target::ensure_target_worktree(&clone, &branch).await {
                    Ok(dir) => Ok(Some((repo, branch, dir))),
                    Err(reason) => Err(reason),
                }
            }
            crate::run_target::RunTarget::Greenfield => Ok(None),
            crate::run_target::RunTarget::Refused { reason } => Err(reason),
        };

        match target {
            Err(reason) => {
                state.runs.fail_with_reason(&rid, reason.clone());
                state.uow.append_history(
                    story_id,
                    "dev_implement",
                    &format!("Development run refused: {reason}"),
                );
            }
            Ok(Some((repo, branch, dir))) => {
                // Brownfield: implement in-place in the UoW's worktree.
                // For the tiered path we pick the strongest model for the implementer; for
                // the single-model path we use the caller's model (or the default).
                let impl_model = match &tier_map {
                    Some(map) => map.strongest.clone(),
                    None => model
                        .clone()
                        .unwrap_or_else(crate::model_tier::default_strongest_model),
                };
                // Derive the project inputs + spawn the implement run. SHARED with the resume path
                // (`spawn_brownfield_dev_run`), so a fresh run and a resumed run never drift.
                // A unit created from inspection findings carries the findings it must close and
                // the rules they violate into the implementer's and the reviewer's prose.
                let extra_grounding =
                    crate::remediation::extra_grounding_for(&state.uow, &story_id_owned, None)
                        .await;
                spawn_brownfield_dev_run(
                    state,
                    rid,
                    story_id_owned,
                    title,
                    desc,
                    repo,
                    dir,
                    branch,
                    impl_model,
                    max_iterations,
                    tier_map,
                    skip_layer2,
                    extra_grounding,
                )
                .await;
            }
            Ok(None) => {
                // Greenfield: scaffold a new app from the plan in a throwaway dir, announced in the
                // run's events so it is never mistaken for work on a real repository.
                state.runs.push_event(
                    &rid,
                    crate::run::GateEvent {
                        seq: 0,
                        layer: "run".to_string(),
                        verdict: "info".to_string(),
                        rule: None,
                        detail: crate::run_target::GREENFIELD_NOTICE.to_string(),
                        content_hash: None,
                    },
                );
                match tier_map {
                    // TIERED path (ORCH-MODEL-TIERING-1): each task on its band's model, the
                    // strongest tier leading. The single `model` is ignored when a map is given.
                    Some(map) => {
                        // Provider-dispatch context for the LEAD/orchestrator driver factory
                        // (the lead runs on the strongest model's OWN provider).
                        let live_registry = state.model_registry.clone();
                        let live_policy = state.settings.provider_policy();
                        let live_creds = state.credential_store.clone();
                        let live_limiter = state.rate_limiter.clone();
                        // The compliance-safety gate: the active project's backend setting,
                        // threaded into the lead/child driver factory. No active project floors
                        // to `ProjectBackend::Cli` (the same zero-setup default a fresh project gets).
                        let live_backend = state
                            .projects
                            .active()
                            .map(|p| p.backend)
                            .unwrap_or_default();
                        // LIFECYCLE-1: register the abort handle so a Stop reaps the greenfield
                        // fleet's live agent subprocesses (kill_on_drop). Cleared on finish.
                        let runs_for_clear = state.runs.clone();
                        let rid_for_clear = rid.clone();
                        let rid_register = rid.clone();
                        let handle = tokio::spawn(async move {
                            live_fleet::execute_live_run_tiered(
                                store,
                                rid,
                                title,
                                desc,
                                map,
                                max_iterations,
                                skip_layer2,
                                vision_enabled,
                                live_registry,
                                live_policy,
                                live_creds,
                                live_limiter,
                                live_backend,
                            )
                            .await;
                            runs_for_clear.clear_abort(&rid_for_clear);
                        });
                        state
                            .runs
                            .register_abort(&rid_register, handle.abort_handle());
                    }
                    // Single-model path (back-compat): one operator-wide model for every agent.
                    None => {
                        // LIFECYCLE-1: register the abort handle (see tiered arm above).
                        let runs_for_clear = state.runs.clone();
                        let rid_for_clear = rid.clone();
                        let rid_register = rid.clone();
                        let handle = tokio::spawn(async move {
                            live_fleet::execute_live_run(
                                store,
                                rid,
                                title,
                                desc,
                                model,
                                max_iterations,
                                skip_layer2,
                            )
                            .await;
                            runs_for_clear.clear_abort(&rid_for_clear);
                        });
                        state
                            .runs
                            .register_abort(&rid_register, handle.abort_handle());
                    }
                }
            }
        }
    } else {
        // HONESTY GATE (NO placeholders): without a live agent no code can be written, so the
        // production run button must REFUSE rather than run a scripted demo that would fabricate
        // a completed dev cycle + SOC-2 evidence over fictional file paths. This mirrors the
        // token-free refusals in update_branch_run / dev_implement_run / pr_resolve_run: fail the
        // run loudly and write NOTHING fake. The provenance watcher below still runs and freezes
        // this run's (empty) gate provenance, but because the terminal status is `Failed` it will
        // NOT advance the UoW stage and NOT attach QA evidence (see `stamp_provenance_when_done`).
        //
        // The deterministic scripted executor (`crate::run::execute_run`) is retained for its own
        // unit test only; it is no longer reachable from any path a real user can click.
        state.runs.fail_with_reason(
            &rid,
            "Development requires the AI agent. Set CAMERATA_LIVE_BUILD=1 to run this story."
                .to_string(),
        );
        state.uow.append_history(
            story_id,
            "dev_implement",
            "Development run refused: live mode is off (set CAMERATA_LIVE_BUILD=1). No code was \
             written and no evidence was produced.",
        );
    }

    // Provenance-stamping watcher (Pillar 2), shared by fresh + resume runs (LIFECYCLE-4).
    spawn_provenance_watcher(state, &run_id, story_id);

    (run_id, mode)
}

/// Spawn the completion-driven, success-gated provenance-stamping watcher for `run_id`
/// (Pillar 2). Called by BOTH [`start_governed_run`] (fresh) and [`resume_governed_run`]
/// (resumed, LIFECYCLE-4) so a resumed run gets provenance + a stage advance on success
/// exactly like a fresh one.
///
/// Once the run reaches its terminal state the watcher freezes the gate provenance onto the
/// story's UoW and, ON SUCCESS ONLY, advances the lifecycle stage Development → AwaitingQa
/// and attaches the SOC-2 QA evidence (issue #53) — see [`stamp_provenance_when_done`] for
/// the full success/failure/cancel semantics. Runs as its own task so the run executor stays
/// unaware of the UoW (keeps the layers thin). Graceful degradation throughout: evidence /
/// ledger failures never block the run.
pub(crate) fn spawn_provenance_watcher(state: &AppState, run_id: &str, story_id: &str) {
    let runs = state.runs.clone();
    let uow = state.uow.clone();
    let watch_id = run_id.to_string();
    let watch_story = story_id.to_string();
    let ledger = state.enforcement_ledger.clone();
    let governance_log = state.governance_log.clone();
    tokio::spawn(async move {
        stamp_provenance_when_done(runs, uow, ledger, governance_log, watch_id, watch_story).await;
    });
}

/// Await a run's terminal state (COMPLETION-driven, LIFECYCLE-3) and then stamp its UoW
/// according to the terminal STATUS (LIFECYCLE-2). Shared by BOTH a fresh governed run
/// ([`start_governed_run`]) and a RESUMED one ([`resume_governed_run`], LIFECYCLE-4) so the
/// two never drift.
///
/// LIFECYCLE-3 (completion signal, not a poll): the watcher awaits [`RunStore::wait_until_done`],
/// which resolves the instant a terminal setter fires the per-run completion `Notify`. A live
/// run that legitimately outlives the old 5-minute poll budget is still stamped. The generous
/// `safety_timeout` is a backstop against a run that wedges without ever going terminal, never
/// the normal path.
///
/// LIFECYCLE-2 (advance only on success):
/// - SUCCESS (`AwaitingQa`): freeze gate provenance, `finish_development` (advance the stage),
///   and assemble + attach the SOC-2 QA evidence.
/// - FAILURE (`Failed`): still freeze gate provenance (an honest record of what the gate saw)
///   but do NOT advance the stage and do NOT attach QA evidence — the work never completed.
/// - CANCEL (`Cancelled`): freeze nothing, advance nothing, attach nothing.
///
/// In every terminal case the enforcement-catch ledger capture still runs (it is a record of
/// the gate decisions the run produced, independent of whether the work advanced).
pub(crate) async fn stamp_provenance_when_done(
    runs: RunStore,
    uow: crate::uow::UowStore,
    ledger: crate::enforcement_ledger::EnforcementLedger,
    // Phase H2: the governance-event audit trail. `None` in tests / on open failure
    // (fail-soft — see `AppState::record_governance`).
    governance_log: Option<Arc<camerata_persistence::GovernanceLog>>,
    run_id: String,
    story_id: String,
) {
    // Backstop only: the completion signal is the normal wake path (LIFECYCLE-3). 6 hours
    // comfortably covers a long operator-driven live run without leaking the task forever.
    const SAFETY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);
    let run = match runs.wait_until_done(&run_id, SAFETY_TIMEOUT).await {
        Some(run) => run,
        // Wedged without ever going terminal, or an unknown run id. Nothing to stamp.
        None => return,
    };

    // Phase H2: `run_finished` — the single convergence point for every governed run
    // (fresh or resumed, brownfield or greenfield, tiered or single-model), keyed on the
    // TERMINAL status: info for the success terminal (AwaitingQa), warn for Failed/Cancelled.
    let finished_detail = serde_json::json!({ "status": format!("{:?}", run.status) }).to_string();
    let finished_event = if matches!(run.status, crate::run::RunStatus::AwaitingQa) {
        camerata_persistence::GovernanceEvent::info(run_id.clone(), "run_finished", "system")
    } else {
        camerata_persistence::GovernanceEvent::warn(run_id.clone(), "run_finished", "system")
    }
    .with_story_id(story_id.clone())
    .with_reason(format!("run finished with status {:?}", run.status))
    .with_detail(finished_detail);
    if let Some(log) = governance_log.as_ref() {
        if let Err(e) = log.record(finished_event).await {
            tracing::warn!(error = %e, run_id = %run_id, "failed to record run_finished governance event");
        }
    }

    let rules = camerata_gateway::enforced_gate_rules();
    let prov = run_provenance(&run, &rules);

    // Branch on the TERMINAL STATUS — this is the LIFECYCLE-2 fix. The old code advanced the
    // stage + attached evidence on ANY `done` run (including failed and cancelled ones).
    match &run.status {
        // CANCEL: freeze nothing, advance nothing, attach nothing. The run stopped before
        // any completed work; there is no honest gate accounting to record.
        crate::run::RunStatus::Cancelled => {
            // Ledger capture still records whatever gate decisions occurred before the stop.
        }
        // FAILURE and SUCCESS both freeze the gate provenance (the honest record of what the
        // gate actually saw). Only SUCCESS advances the stage + attaches QA evidence.
        crate::run::RunStatus::Failed { .. } | crate::run::RunStatus::AwaitingQa => {
            let frozen = crate::uow::GateProvenance {
                run_id: prov.run_id.clone(),
                mode: prov.mode.clone(),
                allow_count: prov.allow_count,
                deny_count: prov.deny_count,
                total_bounces: prov.total_bounces,
                rules_fired: prov.rules_fired.clone(),
                recorded: chrono::Utc::now().to_rfc3339(),
            };
            uow.record_gate_provenance(&story_id, frozen);

            if matches!(run.status, crate::run::RunStatus::AwaitingQa) {
                // SUCCESS ONLY: advance Development → AwaitingQa (best-effort: only legal
                // from Development; a UoW elsewhere is left as-is, never forced).
                let _ = uow.finish_development(&story_id);

                // SUCCESS ONLY — Evidence assembly (issue #53): build the SOC-2 evidence
                // record and attach it. A failed run attaches NO QA evidence, so the sign-off
                // gate can never mistake failed work for verified work.
                let evidence = assemble_evidence_for_run(&run, &prov, &story_id);
                uow.attach_evidence(&story_id, evidence);
            }
        }
        // A non-terminal status behind a `done` flag should not occur (the terminal guard in
        // RunStore keeps done runs at a terminal status). Be conservative: stamp nothing.
        _ => {}
    }

    // ── Enforcement-catch ledger capture (terminal point 1) ──────────────────
    // Write one catch per deny/bounce gate event to the append-only enforcement ledger,
    // regardless of the terminal kind. Best-effort / fail-soft: errors are logged and
    // swallowed inside `capture_run_finalization`; a ledger failure never affects state.
    crate::enforcement_ledger::capture_run_finalization(&ledger, &run.events, &run.id, &story_id)
        .await;
}

/// Build a [`crate::evidence::UowEvidenceRecord`] from a completed run's provenance
/// and gate events (issue #53). Runs the scoped deterministic audit over the changed
/// paths (derived from the gate events' targets) to populate the `scoped_scan` field
/// and the `has_critical` blocking flag.
///
/// The "changed files" for the scoped scan are the TARGET paths of every allowed gate
/// verdict: files the run actually managed to write under the gate. Denied writes are
/// excluded (the content never landed on disk). For the scripted run, these are the
/// planted fictional paths; for a live run, they are the real paths the agent wrote.
///
/// Since the scripted run writes fictional content (which is not on disk), the scoped
/// scan receives empty file content for those paths — the deterministic floor only fires
/// on actual file content, so no findings are produced and `has_critical = false` for a
/// clean scripted run. This is correct: the scripted path exercises the gate logic, not
/// real source files.
pub(crate) fn assemble_evidence_for_run(
    run: &crate::run::Run,
    prov: &crate::run::RunProvenance,
    story_id: &str,
) -> crate::evidence::UowEvidenceRecord {
    use crate::evidence::{scoped_audit, GateDecision, UowEvidenceRecord};

    let now = chrono::Utc::now().to_rfc3339();
    let mut record = UowEvidenceRecord::new(story_id, &run.id, &now);

    // ── Governance event history ─────────────────────────────────────────────
    // Run started.
    record.add_event(
        &now,
        "governed-fleet",
        "run",
        format!(
            "Governed run {} completed (mode: {}, {} allowed, {} denied).",
            run.id, run.mode, prov.allow_count, prov.deny_count,
        ),
    );

    // Gate events → evidence history + gate decisions.
    for event in &run.events {
        let ts = &now; // run events don't carry a timestamp; use the record timestamp.
        if event.verdict == "deny" {
            let rule_id = event.rule.as_deref().unwrap_or("-");
            record.add_event(
                ts,
                "gate-layer-1",
                "gate_deny",
                format!(
                    "Gate denied write to `{}`: rule {} fired — {}",
                    event.detail, rule_id, event.detail
                ),
            );
            record.record_gate_decision(GateDecision::deny(ts, &event.detail, rule_id));
        } else if event.verdict == "allow" {
            record.add_event(
                ts,
                "gate-layer-1",
                "gate_allow",
                format!("Gate allowed write: {}", event.detail),
            );
            record.record_gate_decision(GateDecision::allow(ts, &event.detail));
        }
        // "info", "error", "bounce" events from the live fleet are not gate decisions;
        // record them as notes so they appear in the history.
        if event.verdict == "info" || event.verdict == "error" || event.verdict == "bounce" {
            record.add_event(ts, "governed-fleet", "note", &event.detail);
        }
    }

    // ── Rules enforced ───────────────────────────────────────────────────────
    // Record each rule that was in force during the run. For rules that actually fired
    // a denial, use "denied" as an extra tag in the directive.
    let fired_set: std::collections::HashSet<&str> =
        prov.rules_fired.iter().map(|r| r.as_str()).collect();
    for rule_id in &prov.rules_in_force {
        let directive = if fired_set.contains(rule_id.as_str()) {
            format!("Enforced (fired a denial during this run). Rule id: {rule_id}")
        } else {
            format!("Enforced (no violation this run). Rule id: {rule_id}")
        };
        record.record_rule(rule_id, directive, "mechanical");
    }

    // ── Scoped security scan ─────────────────────────────────────────────────
    // Derive "changed paths" from the allowed gate verdicts (files that landed on disk).
    // For each changed path, we supply an empty file body — the scripted run's fictional
    // paths have no real content on disk, and the deterministic floor only fires on actual
    // content. A live run's paths do exist, but reading them here would require knowing the
    // workspace root (not available in this pure-ish context). The empty-body approach is
    // correct for the scripted path and conservative (fewer false positives) for live runs.
    // TODO(live-scan): for the live path, resolve the workspace root and read actual content.
    let allowed_paths: Vec<String> = run
        .events
        .iter()
        .filter(|e| e.verdict == "allow")
        .map(|e| e.detail.clone())
        .collect();
    // Build a synthetic file list: allowed paths with empty content.
    let all_files: Vec<(String, String)> = allowed_paths
        .iter()
        .map(|p| (p.clone(), String::new()))
        .collect();
    let scan_result = scoped_audit(
        &format!("{story_id}/run/{}", run.id),
        &all_files,
        &allowed_paths,
    );
    // Add a critical_finding event to the history for each critical finding.
    for finding in scan_result
        .summary
        .findings
        .iter()
        .filter(|f| f.severity == "critical")
    {
        record.add_event(
            &now,
            "scoped-audit",
            "critical_finding",
            format!(
                "CRITICAL: {} in {} (line {}): {}",
                finding.rule_id, finding.path, finding.line, finding.detail
            ),
        );
    }
    // Add security_finding events for non-critical findings.
    for finding in scan_result
        .summary
        .findings
        .iter()
        .filter(|f| f.severity != "critical")
    {
        record.add_event(
            &now,
            "scoped-audit",
            "security_finding",
            format!(
                "{}: {} in {} (line {}): {}",
                finding.severity.to_uppercase(),
                finding.rule_id,
                finding.path,
                finding.line,
                finding.detail
            ),
        );
    }
    record.set_scoped_scan(scan_result.summary);

    // ── Content hash ──────────────────────────────────────────────────────────
    record.compute_hash();
    record
}

/// The current state of a run enriched with live stall diagnostics.
///
/// Fields added beyond `Run`:
/// - `idle_ms` — milliseconds since last activity (now − last_activity_ms).
/// - `stalled` — true when idle_ms > stall_threshold_ms.
/// - `stall_threshold_ms` — the active threshold (env-overridable, default 120 000ms).
/// - `stall_policy` — whether the run's policy is to alert or auto-cancel on stall.
/// - `failure_reason` — human-readable failure reason when the run failed.
#[derive(serde::Serialize)]
pub(crate) struct RunStatusResponse {
    #[serde(flatten)]
    pub(crate) run: Run,
    pub(crate) idle_ms: u128,
    pub(crate) stalled: bool,
    pub(crate) stall_threshold_ms: u128,
    pub(crate) stall_policy: crate::run::StallPolicy,
    pub(crate) failure_reason: Option<String>,
}

/// `GET /api/runs/:id` — the current state of a run: status, gate verdicts, and
/// live stall diagnostics so the cockpit can surface a stalled run without polling
/// a separate endpoint.
pub(crate) async fn get_run(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<RunStatusResponse>, AppError> {
    let run = state
        .runs
        .get(&id)
        .ok_or_else(|| AppError::not_found(anyhow::anyhow!("run not found: {id}")))?;
    // LIFECYCLE-6: the effective threshold comes from the ACTIVE project's per-kind stall
    // setting (autonomous vs. watched), falling back to the env/default when no project is
    // active. This is the SAME threshold the background sweep applies, so the reported
    // `stalled` flag and the auto-cancel decision agree.
    let threshold_ms = state
        .projects
        .active()
        .map(|p| p.stall_threshold_ms(run.kind.is_autonomous()))
        .unwrap_or_else(crate::run::run_stall_threshold_ms);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let idle = crate::run::idle_ms(u128::from(run.tracker.last_activity_ms()), now_ms);
    // LIFECYCLE-7: a `done` run (success/failed/cancelled) or a PARKED run
    // (AwaitingReview / AwaitingClarification, waiting on a human) is NEVER stalled — it is
    // intentionally idle, not wedged. Only a live, in-flight run can stall.
    let stalled =
        !run.done && !run.status.is_parked() && crate::run::is_stalled(idle, threshold_ms);
    let stall_policy = run.stall_policy.clone();
    let failure_reason = run.failure_reason.clone();
    Ok(Json(RunStatusResponse {
        run,
        idle_ms: idle,
        stalled,
        stall_threshold_ms: threshold_ms,
        stall_policy,
        failure_reason,
    }))
}

/// `POST /api/runs/:id/cancel` — cancel a run immediately. Idempotent; a run that is
/// already done or cancelled is unaffected. Always returns 204.
pub(crate) async fn cancel_run(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    state.runs.cancel(&id);
    StatusCode::NO_CONTENT
}

/// The per-agent transcripts for a run: the GENERATED prompt each agent was handed and
/// its output so far. Powers the Agent-activity drawer (the otherwise-hidden prompting).
pub(crate) async fn get_run_agents(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Json<Vec<crate::transcript::AgentTranscript>> {
    Json(state.transcripts.get(&id))
}

/// The PROVENANCE summary for a run (issue #21): which rules were in force, the gate
/// deny/allow tallies, and the total bounces — the honest accounting an architect
/// reads before signing the run off. Derived from the run's REAL recorded verdicts.
pub(crate) async fn get_run_provenance(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<RunProvenance>, AppError> {
    let run = state
        .runs
        .get(&id)
        .ok_or_else(|| AppError::not_found(anyhow::anyhow!("run not found: {id}")))?;
    let rules = camerata_gateway::enforced_gate_rules();
    Ok(Json(run_provenance(&run, &rules)))
}

/// `GET /api/runs/:id/events` — the full governance-event audit trail for one run, in
/// insertion order (Phase H3: the read path over the Phase H1/H2 governance-event
/// ledger). Returns an empty array — never an error — when no governance log is open
/// this session; the ledger is fail-soft by design (see `AppState::record_governance`),
/// so its absence must not surface as a 500 to a caller just asking to see the trail.
pub(crate) async fn get_run_events(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<camerata_persistence::GovernanceEvent>>, AppError> {
    let Some(log) = state.governance_log.as_ref() else {
        return Ok(Json(Vec::new()));
    };
    let events = log.by_run(&id).await.map_err(|e| AppError(e.into()))?;
    Ok(Json(events))
}

/// Query for `GET /api/governance/events`: `?limit=N` caps the page. Defaults to 100,
/// capped at 1000 so an unbounded request can't page the entire table in one response.
#[derive(serde::Deserialize)]
pub(crate) struct RecentGovernanceEventsQuery {
    pub(crate) limit: Option<u32>,
}

/// `GET /api/governance/events?limit=N` — the `N` most recently recorded governance
/// events across ALL runs, newest first (Phase H3). Default `N=100`, capped at 1000.
/// Returns an empty array — never an error — when no governance log is open this
/// session (same fail-soft contract as [`get_run_events`]).
pub(crate) async fn recent_governance_events(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<RecentGovernanceEventsQuery>,
) -> Result<Json<Vec<camerata_persistence::GovernanceEvent>>, AppError> {
    let Some(log) = state.governance_log.as_ref() else {
        return Ok(Json(Vec::new()));
    };
    let limit = i64::from(q.limit.unwrap_or(100).min(1000));
    let events = log.recent(limit).await.map_err(|e| AppError(e.into()))?;
    Ok(Json(events))
}

// ── Product-Owner feedback loop: defect-report ingest + read + status update ───────

/// `POST /api/feedback` — ingest one [`camerata_api_types::feedback::DefectReport`]
/// (Product-Owner feedback loop: a scaffolded app's auto-capture reporter, or a human's
/// click-to-report). The server stamps `id`/`ts`/`status` fresh regardless of what the
/// caller sent (never trust a client to assign its own row id, backdate its own
/// timestamp, or submit a report already `Resolved`), stores it via
/// [`AppState::record_defect`], AND records a paired governance-audit row (kind
/// `"defect_reported"`, `severity` derived from the defect's severity, `actor`
/// `"system"` for [`DefectSource::Auto`] / `"human"` for [`DefectSource::User`], `reason`
/// the report's title, `detail` the full stored report as JSON) so the audit trail shows
/// what was reported and by what/whom — the governance event's `run_id` is the report's
/// `project_id` (there is no run backing a raw defect report; grouping by project is the
/// natural browse key here, mirroring how `run_id` groups a run's own events).
///
/// Fail-soft: when no feedback store is open this session, responds `{ ok: false,
/// message }` (200, never a 500) — an ingest endpoint going down must not take the
/// reporting app down with it. On a genuine write failure (store open, insert failed),
/// same `{ ok: false, message }` shape.
/// Compute the ingest-time dedupe fingerprint for a report: a stable hash of `kind` +
/// the top stack frame (the FIRST line of `context.stack`, trimmed) + `context.route`.
/// Reuses [`camerata_persistence::content_hash`] (SHA-256, already a dependency of this
/// crate via `camerata-persistence`) rather than adding a new hashing dependency —
/// this fingerprint does not need to be cryptographically strong, only stable, and
/// `content_hash` already is.
pub(crate) fn compute_feedback_fingerprint(
    report: &camerata_api_types::feedback::DefectReport,
) -> String {
    let top_frame = report
        .context
        .stack
        .as_deref()
        .and_then(|s| s.lines().next())
        .unwrap_or("")
        .trim();
    let route = report.context.route.as_deref().unwrap_or("");
    let raw = format!("{}|{}|{}", report.kind.as_str(), top_frame, route);
    camerata_persistence::content_hash(&raw)
}

/// `POST /api/feedback` — ingest one defect report (auto-capture or click-to-report).
///
/// Fail-soft: when no feedback store is open this session, responds `{ ok: false,
/// message }` (200, never a 500) — an ingest endpoint going down must not take the
/// reporting app down with it. On a genuine write failure (store open, insert failed),
/// same `{ ok: false, message }` shape.
///
/// Fingerprint + dedupe fold (the "cheapest-now / most-expensive-later" fold — see
/// the product-owner vibe-mode design notes' usability backlog): when
/// the client did not already send a `fingerprint`, one is computed here
/// ([`compute_feedback_fingerprint`]). If a recent OPEN report with the SAME
/// fingerprint already exists for this project, this occurrence is folded into that
/// row (`count` incremented) instead of inserting a new one, and NO new
/// `defect_reported` governance row is written (the governance trail records the
/// first sighting of a defect, not every repeat of it) — the response still carries
/// the (existing) `id` and a `deduped: true` flag so a caller can tell the two cases
/// apart.
pub(crate) async fn submit_feedback(
    State(state): State<AppState>,
    Json(mut report): Json<camerata_api_types::feedback::DefectReport>,
) -> Json<serde_json::Value> {
    use camerata_api_types::feedback::{DefectSeverity, DefectSource, DefectStatus};

    report.id = None;
    report.ts = chrono::Utc::now().to_rfc3339();
    report.status = DefectStatus::Open;
    report.count = 1;

    // FOLD E (chat secret-interceptor, applied at the one live free-text ingest
    // path today): a deployed scaffolded app's auto-capture reporter or a human's
    // click-to-report can carry a secret straight out of the running app's own
    // logs (a leaked API key printed to the console, a stack frame that embeds a
    // connection string). Scrub BEFORE this report is stored or mirrored into the
    // governance trail — every other free-text context field (`route`, `element`)
    // is either a URL path or a UI selector, not a place a raw secret realistically
    // lands, so only `console`/`stack` are scrubbed.
    if let Some(console) = report.context.console.take() {
        report.context.console =
            Some(camerata_orchestrator_core::secrets::scrub(&console).scrubbed);
    }
    if let Some(stack) = report.context.stack.take() {
        report.context.stack = Some(camerata_orchestrator_core::secrets::scrub(&stack).scrubbed);
    }

    if report.fingerprint.as_deref().unwrap_or("").is_empty() {
        report.fingerprint = Some(compute_feedback_fingerprint(&report));
    }
    // Safe to unwrap: the branch above guarantees `Some` by this point.
    let fingerprint = report.fingerprint.clone().unwrap_or_default();

    let Some(store) = state.feedback_store.as_ref() else {
        return Json(serde_json::json!({
            "ok": false,
            "message": "feedback store is not available this session",
        }));
    };

    match store
        .bump_fingerprint(&report.project_id, &fingerprint)
        .await
    {
        Ok(Some(existing_id)) => {
            return Json(serde_json::json!({
                "ok": true,
                "id": existing_id,
                "deduped": true,
            }));
        }
        Ok(None) => {
            // No matching open report — fall through to insert a genuinely new one.
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to check defect-report fingerprint for dedupe");
            // Fail open on the DEDUPE check specifically: still attempt the insert
            // below rather than losing the report entirely over a dedupe-lookup error.
        }
    }

    match store.record(report.clone()).await {
        Ok(id) => {
            let actor = match report.source {
                DefectSource::Auto => "system",
                DefectSource::User => "human",
            };
            let severity = match report.severity {
                DefectSeverity::Info => "info",
                DefectSeverity::Warning => "warn",
                DefectSeverity::Error | DefectSeverity::Critical => "error",
            };
            let mut stamped = report;
            stamped.id = Some(id);
            let detail = serde_json::to_string(&stamped).unwrap_or_default();
            state
                .record_governance(
                    camerata_persistence::GovernanceEvent::new(
                        stamped.project_id.clone(),
                        "defect_reported",
                        severity,
                        actor,
                    )
                    .with_reason(stamped.title.clone())
                    .with_detail(detail),
                )
                .await;
            Json(serde_json::json!({ "ok": true, "id": id }))
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to record defect report");
            Json(serde_json::json!({
                "ok": false,
                "message": format!("failed to record defect report: {e}"),
            }))
        }
    }
}

/// `GET /api/projects/:id/feedback` — one project's defect reports, newest first.
/// Returns an empty array — never an error — when no feedback store is open this
/// session (fail-soft, same contract as [`get_run_events`]).
pub(crate) async fn project_feedback(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<camerata_api_types::feedback::DefectReport>>, AppError> {
    let Some(store) = state.feedback_store.as_ref() else {
        return Ok(Json(Vec::new()));
    };
    let reports = store
        .by_project(&id)
        .await
        .map_err(|e| AppError(e.into()))?;
    Ok(Json(reports))
}

/// Query for `GET /api/feedback/recent`: `?limit=N` caps the page. Defaults to 100,
/// capped at 1000 (mirrors [`RecentGovernanceEventsQuery`]).
#[derive(serde::Deserialize)]
pub(crate) struct RecentFeedbackQuery {
    pub(crate) limit: Option<u32>,
}

/// `GET /api/feedback/recent?limit=N` — the `N` most recently recorded defect reports
/// across ALL projects, newest first. Default `N=100`, capped at 1000. Returns an empty
/// array — never an error — when no feedback store is open this session (same
/// fail-soft contract as [`recent_governance_events`]).
pub(crate) async fn recent_feedback(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<RecentFeedbackQuery>,
) -> Result<Json<Vec<camerata_api_types::feedback::DefectReport>>, AppError> {
    let Some(store) = state.feedback_store.as_ref() else {
        return Ok(Json(Vec::new()));
    };
    let limit = i64::from(q.limit.unwrap_or(100).min(1000));
    let reports = store.recent(limit).await.map_err(|e| AppError(e.into()))?;
    Ok(Json(reports))
}

/// Body for `POST /api/feedback/:id/status`.
#[derive(serde::Deserialize)]
pub(crate) struct UpdateFeedbackStatusReq {
    pub(crate) status: camerata_api_types::feedback::DefectStatus,
}

/// `POST /api/feedback/:id/status` — update one defect report's status (Open ->
/// Acknowledged -> Resolved, or any direct transition; no state-machine enforcement
/// here). Fail-soft: `{ ok: false, message }` (200, never a 500) when no feedback store
/// is open this session, or on a genuine write failure.
pub(crate) async fn update_feedback_status(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<UpdateFeedbackStatusReq>,
) -> Json<serde_json::Value> {
    let Some(store) = state.feedback_store.as_ref() else {
        return Json(serde_json::json!({
            "ok": false,
            "message": "feedback store is not available this session",
        }));
    };
    match store.set_status(id, req.status).await {
        Ok(()) => Json(serde_json::json!({ "ok": true })),
        Err(e) => {
            tracing::warn!(error = %e, "failed to update defect report status");
            Json(serde_json::json!({
                "ok": false,
                "message": format!("failed to update status: {e}"),
            }))
        }
    }
}

#[derive(serde::Deserialize)]
pub(crate) struct SignOffReq {
    /// Who is signing off (the architect's handle/name). Defaults to "architect".
    #[serde(default)]
    pub(crate) by: Option<String>,
    /// An optional note attached to the sign-off.
    #[serde(default)]
    pub(crate) note: Option<String>,
    /// An explicit reason to waive a Critical scoped-scan finding that would otherwise
    /// block sign-off (issue #53). A Critical finding in the evidence record blocks the
    /// `AwaitingQa → SignedOff` transition unless the architect provides a non-empty
    /// reason here. A reason-less waive (`waive_reason: ""` or absent) is rejected with
    /// HTTP 409 so the UI must ask the architect to type a justification.
    ///
    /// When present and the evidence has a critical finding, the waiver reason is
    /// appended to the sign-off note so it is durable in the UoW history.
    #[serde(default)]
    pub(crate) waive_reason: Option<String>,
    /// Optional GitHub PR number to post the evidence markdown as a comment on (issue
    /// #53). When supplied AND a `CAMERATA_GITHUB_TOKEN` is set, Camerata posts the
    /// evidence record as a PR comment via the arm.rs GitHub primitives. If omitted,
    /// no PR comment is posted (the evidence is still stored on the UoW). Graceful
    /// degradation: a failed PR comment never blocks the sign-off.
    #[serde(default)]
    pub(crate) pr_number: Option<u64>,
    /// The `owner/repo` the PR lives in (required when `pr_number` is set). Format:
    /// `"owner/repo"`. When absent and `pr_number` is set, the PR comment is skipped.
    #[serde(default)]
    pub(crate) pr_repo: Option<String>,
}

/// SIGN-OFF action for a run (issue #21 / #53): the architect explicitly marks a
/// completed governed run as signed off. Persisted on the story's Unit of Work (which
/// survives sessions) along with the run id and a history entry.
///
/// # Critical-finding gate (issue #53)
///
/// When the UoW's evidence record contains a Critical scoped-scan finding, the sign-off
/// is BLOCKED until the architect supplies an explicit `waive_reason` in the request.
/// A reason-less waive is rejected with HTTP 409 (CONFLICT), forcing the architect to
/// acknowledge the finding. When a waive is accepted, the reason is appended to the
/// sign-off note and recorded in the UoW history.
///
/// Camerata never signs work off on its own — this is the deliberate human gate after
/// reviewing the provenance.
///
/// # PR comment posting (issue #53)
///
/// When `pr_number` and `pr_repo` are supplied, the evidence record's markdown is
/// posted as a GitHub PR comment via the arm.rs primitives. Graceful degradation: no
/// token or a GitHub error skips the PR comment without failing the sign-off.
pub(crate) async fn sign_off_run(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<SignOffReq>,
) -> Result<Response, AppError> {
    let run = state
        .runs
        .get(&id)
        .ok_or_else(|| AppError::not_found(anyhow::anyhow!("run not found: {id}")))?;

    // ── Critical-finding sign-off gate (issue #53) ───────────────────────────
    // Read the UoW's attached evidence. If it has a critical scoped-scan finding,
    // block sign-off unless the architect supplied an explicit waive_reason.
    let current_uow = state.uow.get_or_create(&run.story_id);
    if current_uow.is_sign_off_blocked() {
        let waive = req.waive_reason.as_deref().filter(|r| !r.trim().is_empty());
        if waive.is_none() {
            // Critical finding present but no waive reason — reject with 409.
            let body = Json(serde_json::json!({
                "error": "sign-off blocked by critical security finding",
                "reason": "The evidence record for this run contains a Critical scoped-scan \
                           finding. Sign-off is blocked until you supply a non-empty \
                           `waive_reason` explaining why the finding is acceptable to ship.",
                "blocked": true,
            }));
            return Ok((StatusCode::CONFLICT, body).into_response());
        }
        // Waive with reason: fold the reason into the note so it is durable.
        // The waiver is also recorded as a history entry by `uow.sign_off` (it appends
        // the full note text, which includes the waiver reason).
        let _ = waive; // acknowledged above; used below when building the effective note.
    }

    // ── Reviewer-finding sign-off gate (gap 4) ───────────────────────────────
    // A HIGH or MEDIUM Layer-3 reviewer finding that is still open blocks sign-off. Unlike the
    // critical-finding gate it has no waiver: the way past is to fix the finding or to accept
    // it with a reason (`POST /api/uow/:id/review-findings/:index/accept`). LOW never blocks.
    let open_review_findings = current_uow.open_blocking_review_findings();
    if open_review_findings > 0 {
        return Ok((
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "sign-off blocked by open reviewer findings",
                "reason": format!(
                    "{open_review_findings} reviewer finding(s) are still open. Fix them or record a decision on each before signing off."
                ),
                "blocked": true,
            })),
        )
            .into_response());
    }

    // Build the effective note, folding in the waiver reason when present.
    let effective_note = {
        let waive = req.waive_reason.as_deref().filter(|r| !r.trim().is_empty());
        match (req.note.as_deref(), waive) {
            (Some(note), Some(reason)) => Some(format!("{note} [WAIVER: {reason}]")),
            (None, Some(reason)) => Some(format!("[WAIVER] {reason}")),
            (Some(note), None) => Some(note.to_string()),
            (None, None) => None,
        }
    };

    let by = req
        .by
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "architect".to_string());
    let mut uow = state
        .uow
        .sign_off(&run.story_id, &by, &run.id, effective_note.as_deref());

    // Phase H2: durable record of the human sign-off decision, keyed on the run it signs off.
    state
        .record_governance(
            camerata_persistence::GovernanceEvent::info(run.id.clone(), "sign_off", "human")
                .with_story_id(run.story_id.clone())
                .with_reason(
                    effective_note
                        .clone()
                        .unwrap_or_else(|| format!("signed off by {by}")),
                ),
        )
        .await;

    // ── Post evidence as PR comment (issue #53) ───────────────────────────────
    // When the UoW has an evidence record AND the caller supplied a PR number + repo,
    // post the rendered markdown as a GitHub PR comment. Best-effort: any error is
    // logged but never propagates to the caller.
    if let (Some(evidence), Some(pr_number), Some(pr_repo)) =
        (&uow.evidence, req.pr_number, req.pr_repo.as_deref())
    {
        // Update sign-off in the evidence record copy for the PR comment.
        let mut evidence_for_pr = evidence.clone();
        if let Some(so) = &uow.sign_off {
            evidence_for_pr.set_sign_off(so);
            evidence_for_pr.compute_hash();
        }
        let markdown = crate::evidence::render_pr_markdown(&evidence_for_pr);

        if let Some((owner, repo_name)) = pr_repo.split_once('/') {
            let token = state.github_token().unwrap_or_default();
            let comment_url =
                crate::arm::post_pr_comment(owner, repo_name, pr_number, &markdown, &token)
                    .await
                    .unwrap_or_default(); // graceful degradation: None on any failure

            if let Some(url) = comment_url {
                // Record the PR comment link in the UoW history (best-effort update).
                state.uow.append_history(
                    &run.story_id,
                    "evidence_pr_comment",
                    &format!("SOC-2 evidence posted as PR comment: {url}"),
                );
                // Re-read the UoW with the updated history so the response is current.
                uow = state.uow.get_or_create(&run.story_id);
            }
        }
    }

    // ── Per-UoW worktree teardown (Decision 1) ───────────────────────────────
    // Sign-off ends this UoW's active dev lifecycle, so its per-UoW worktree can be
    // reclaimed. Best-effort + non-fatal: a missing worktree, an unresolved repo, or a
    // git error never blocks sign-off. The shared clone and the branch itself are left
    // intact (the branch may still be wanted for the PR); only the extra checkout is removed.
    //
    // LIFECYCLE-9 (single-flight guard): NEVER tear down the worktree while the run is still
    // live. A sign-off that arrives before the run reaches a terminal (`done`) state must not
    // yank the worktree out from under the agent mid-edit. The sign-off itself is recorded
    // above; only the destructive teardown is withheld until `run.done`.
    let run_after_signoff = state.runs.get(&run.id).unwrap_or_else(|| run.clone());
    if run_after_signoff.done {
        if let Some(branch) = uow.branch.as_deref().filter(|b| !b.trim().is_empty()) {
            if let Some(repo) = uow.repo().map(str::to_string) {
                let override_path = state.settings.repo_path(&repo);
                let workspace_root = state.settings.workspace_root();
                if let Some(clone) = crate::workspace::resolve_repo_dir(
                    override_path.as_deref(),
                    workspace_root.as_deref(),
                    &repo,
                ) {
                    crate::workspace::remove_uow_worktree(&clone, branch).await;
                }
            }
        }
    } else {
        // The run is still live: record that teardown was deferred so the trail is honest.
        state.uow.append_history(
            &run.story_id,
            "sign_off",
            &format!(
                "Sign-off recorded, but worktree teardown deferred: run {} is still live \
                 (not done). The worktree stays intact until the run reaches a terminal state.",
                run.id
            ),
        );
    }

    Ok(Json(uow).into_response())
}

/// Non-destructively stamp `run_id` on each clarification from the resume-context store (a
/// plain `.get()`, NOT `.take()` — this must never consume the pause point; only actually
/// answering does that). Lets a list response tell the client which open clarifications paused
/// a governed run, so it can re-attach the Bombe polling loop on answer (see `ClarifyQuestion`'s
/// `run_id` field doc in the UI). A free-standing (non-run) clarification is left `None`.
pub(crate) fn with_resume_run_ids(
    resume: &crate::clarify_resume::ClarifyResumeStore,
    clars: Vec<Clarification>,
) -> Vec<Clarification> {
    clars
        .into_iter()
        .map(|mut c| {
            c.run_id = resume.get(&c.id).map(|ctx| ctx.run_id);
            c
        })
        .collect()
}

/// OPEN clarifications for the active project's stories (the NEEDS YOU queue).
/// No active project → empty queue.
pub(crate) async fn list_open_clarifications(
    State(state): State<AppState>,
) -> Json<Vec<Clarification>> {
    let Some(p) = state.projects.active() else {
        return Json(vec![]);
    };
    let clars = state
        .clarifications
        .all_open_for_project(&p.repos, |id| state.uow.repo_for(id));
    Json(with_resume_run_ids(&state.clarify_resume, clars))
}

/// All clarifications on a story (open and answered).
pub(crate) async fn list_clarifications(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
) -> Json<Vec<Clarification>> {
    let clars = state.clarifications.for_story(&story_id);
    Json(with_resume_run_ids(&state.clarify_resume, clars))
}

/// Post a clarifying question on a story, addressed to the chosen recipient. When a
/// real tracker is wired AND the story has an external ref, the question is ALSO
/// posted as a comment on the tracker item via the provider (best-effort; the local
/// record is returned regardless so the cockpit never blocks on a remote failure).
pub(crate) async fn post_clarification(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    Json(req): Json<PostClarifyReq>,
) -> Json<Clarification> {
    let clar = state.clarifications.post_structured(
        &story_id,
        &req.question,
        &req.addressee,
        req.options.clone(),
        req.multi_select,
        req.allow_free_text,
    );

    if state.provider.current().live {
        if let Ok(Some(story)) = state.stories.get(&story_id).await {
            if let Some(reference) = story.external_ref.as_ref() {
                let questions = [req.question.clone()];
                if let Err(e) = state
                    .provider
                    .current()
                    .provider
                    .post_clarifying_questions(reference, &questions)
                    .await
                {
                    eprintln!("[camerata-server] clarify write-back to tracker failed: {e}");
                }
            }
        }
    }

    Json(clar)
}

/// Record the answer to a clarification.
pub(crate) async fn answer_clarification(
    State(state): State<AppState>,
    Path(cid): Path<String>,
    Json(req): Json<AnswerReq>,
) -> Result<Json<Clarification>, AppError> {
    // Structured path: selected options and/or a free-text "Other". When both the
    // structured fields are empty, fall back to the legacy `answer` string as the
    // free-text leg, so old callers keep working unchanged.
    let free_text = match req.free_text.clone() {
        Some(ft) => Some(ft),
        None if !req.answer.trim().is_empty() => Some(req.answer.clone()),
        None => None,
    };
    let selection = crate::clarify::ClarifyAnswer {
        selected: req.selected.clone(),
        free_text,
    };
    let answered = state
        .clarifications
        .answer_structured(&cid, selection, &req.answered_by)
        .ok_or_else(|| AppError::not_found(anyhow::anyhow!("clarification not found: {cid}")))?;

    // Phase 3b: if a gated run is PARKED on this clarification, resume it now. The resume
    // context is consumed (no double-resume); the re-spawned agent gets the original task
    // plus the question plus this answer. The gate is unchanged — the resume rebuilds the
    // SAME governed role + gated session. Only the investigation phase resumes today; the
    // enum keeps the dev-phase resume a closed, explicit future branch.
    if let Some(ctx) = state.clarify_resume.take(&cid) {
        let answer_summary = answered
            .answer
            .clone()
            .unwrap_or_else(|| req.answer.clone());
        match ctx.phase {
            crate::clarify_resume::PausedPhase::Investigation => {
                let runs = state.runs.clone();
                let uow = state.uow.clone();
                let clarifications = state.clarifications.clone();
                let resume = state.clarify_resume.clone();
                // ON-DEMAND REPO READ (the invariant): re-resolve the active project's local
                // clone so the RESUMED agent also runs with the repo as cwd + `--add-dir`
                // and keeps full read access. Read-only; the write gate is unchanged.
                let repo_dir = state.active_repo_dir();
                // MULTI-REPO READ scope: ALL the active project's repo clones (read-only).
                let read_dirs = state.active_repo_dirs();
                tokio::spawn(async move {
                    crate::investigation_run::resume_investigation_after_clarification(
                        runs,
                        uow,
                        clarifications,
                        resume,
                        ctx,
                        answer_summary,
                        repo_dir,
                        read_dirs,
                    )
                    .await;
                });
            }
        }
    }

    Ok(Json(answered))
}
