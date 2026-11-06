//! Investigation, branch and pull-request handlers for units of work.

use crate::*;

/// Replace the full set of decision records on a story's UoW. The governed-dev gate
/// reads these to decide whether a run may start; the cockpit posts them when the
/// investigation surfaces (or the architect resolves) decisions. Body is the JSON
/// array of `DecisionRecord`s (the same shape `camerata-worktracker` serializes).
pub(crate) async fn uow_set_decisions(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    Json(decisions): Json<Vec<camerata_worktracker::investigation::DecisionRecord>>,
) -> Json<crate::uow::UnitOfWork> {
    Json(state.uow.set_decisions(&story_id, decisions))
}

/// Body for `POST /api/uow/:story_id/review-findings/:index/accept`.
#[derive(serde::Deserialize)]
pub(crate) struct AcceptReviewFindingReq {
    /// Why the finding is acceptable as is. Required and non-blank.
    #[serde(default)]
    pub(crate) reason: String,
}

/// A short lowercase slug of `text` for a decision artifact id (letters and digits, dashes
/// between, at most 40 characters).
pub(crate) fn decision_slug(text: &str) -> String {
    let mut slug = String::new();
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    slug.truncate(40);
    slug.trim_end_matches('-').to_string()
}

/// `POST /api/uow/:story_id/review-findings/:index/accept` body `{ "reason": "..." }`: the
/// architect accepts one Open Layer-3 reviewer finding as is. The reason is required. The
/// finding becomes `Accepted`, the unit's history gets a line, and an Approved decision record
/// is written through `set_decisions` so the acceptance is in the decision history the next run
/// reads. 400 for a blank reason, 404 for an unknown unit or index, 409 when the finding is
/// already fixed or accepted.
pub(crate) async fn uow_accept_review_finding(
    State(state): State<AppState>,
    Path((story_id, index)): Path<(String, usize)>,
    Json(req): Json<AcceptReviewFindingReq>,
) -> Response {
    use crate::uow::AcceptReviewFindingError;
    let reason = req.reason.trim();
    if reason.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "reason required",
                "reason": "Say why this finding is acceptable as is. A reason is required to accept a reviewer finding.",
            })),
        )
            .into_response();
    }
    let accepted = match state
        .uow
        .accept_review_finding(&story_id, index, "architect", reason)
    {
        Ok(f) => f,
        Err(AcceptReviewFindingError::NotFound) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "error": "no such finding",
                    "reason": "That reviewer finding does not exist on this unit.",
                })),
            )
                .into_response();
        }
        Err(AcceptReviewFindingError::NotOpen) => {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "finding not open",
                    "reason": "That reviewer finding is already fixed or accepted.",
                })),
            )
                .into_response();
        }
    };
    state.uow.append_history(
        &story_id,
        "review_finding_accepted",
        &format!(
            "Reviewer finding accepted: {}. Reason: {reason}",
            accepted.title
        ),
    );
    // The acceptance is also a decision, so the decision history the run reads carries it.
    let label = format!("Reviewer finding accepted: {}", accepted.title);
    let record = camerata_worktracker::investigation::DecisionRecord::ai_proposed(
        story_id.clone(),
        format!(
            "{story_id}/decision/reviewer-accepted-{index}-{}",
            decision_slug(&accepted.title)
        ),
        label,
        // A finding recorded without a scenario still gives the decision a question.
        if accepted.scenario.trim().is_empty() {
            accepted.title.clone()
        } else {
            accepted.scenario.clone()
        },
        reason.to_string(),
        Vec::new(),
        chrono::Utc::now(),
    )
    .approve(chrono::Utc::now());
    let mut decisions = state.uow.decisions_for(&story_id);
    decisions.push(record);
    state.uow.set_decisions(&story_id, decisions);
    Json(state.uow.get_or_create(&story_id)).into_response()
}

/// The decisions-review payload for a story (`GET /api/uow/:story_id/investigation`): the
/// investigation note (or `None` when none has been recorded) and the current decision
/// records. Powers the cockpit's decisions-review surface so the architect can read the
/// investigation output, mark it reviewed, and approve/reject each decision before the
/// Investigating → DecisionsApproved transition. `note_present` distinguishes "no note at
/// all" (e.g. live mode off produced only a placeholder, or the run errored) from an empty
/// note string, so the UI can surface an explicit "no output" state.
#[derive(serde::Serialize)]
pub(crate) struct InvestigationReviewResponse {
    pub(crate) story_id: String,
    pub(crate) note_present: bool,
    pub(crate) note: Option<camerata_worktracker::investigation::InvestigationArtifact>,
    pub(crate) decisions: Vec<camerata_worktracker::investigation::DecisionRecord>,
    /// BUG B: the reason the MOST RECENT investigation run failed, when it did. `None`
    /// covers both "no run yet" and "the last run did not fail" — the UI only needs this
    /// to distinguish a genuine failure from the generic "no note recorded yet" empty
    /// state, so it surfaces "Investigation failed: {reason}" instead of a silent gap.
    pub(crate) last_run_failed_reason: Option<String>,
    /// Whether live mode is currently enabled, so the empty-state UI can say precisely why
    /// there's no real note (live mode off vs. a genuine failure vs. just not run yet) even
    /// when no note has been recorded at all.
    pub(crate) live_mode_enabled: bool,
}

/// `GET /api/uow/:story_id/investigation` — the investigation note + decision records for
/// the decisions-review surface. Read-only.
pub(crate) async fn uow_get_investigation(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
) -> Json<InvestigationReviewResponse> {
    let note = state.uow.investigation_note_for(&story_id);
    let decisions = state.uow.decisions_for(&story_id);
    let last_run_failed_reason = state
        .runs
        .latest_run_for_story_and_mode(&story_id, "investigation")
        .and_then(|r| match r.status {
            crate::run::RunStatus::Failed { reason } => Some(reason),
            _ => None,
        });
    Json(InvestigationReviewResponse {
        story_id,
        note_present: note.is_some(),
        note,
        decisions,
        last_run_failed_reason,
        live_mode_enabled: crate::run::live_mode_enabled(),
    })
}

/// `POST /api/uow/:story_id/investigation/review` — mark the story's investigation note
/// reviewed by the architect (ROUTE-B). Marking is idempotent: reviewing an
/// already-reviewed note is still `ok:true` (with `already_reviewed:true`, no new
/// version). Returns 404 when there is no note to review at all.
pub(crate) async fn uow_mark_investigation_reviewed(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
) -> Response {
    use crate::uow::ReviewOutcome;
    match state.uow.mark_investigation_reviewed(&story_id) {
        ReviewOutcome::NewlyReviewed(version) => Json(serde_json::json!({
            "ok": true,
            "version": version,
            "already_reviewed": false,
        }))
        .into_response(),
        ReviewOutcome::AlreadyReviewed => Json(serde_json::json!({
            "ok": true,
            "already_reviewed": true,
        }))
        .into_response(),
        ReviewOutcome::NoNote => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "ok": false,
                "error": "no investigation note to review",
            })),
        )
            .into_response(),
    }
}

/// Helper: map a lifecycle [`crate::lifecycle::TransitionError`] to a 409 CONFLICT with
/// its human-readable message, so the cockpit surfaces exactly why a stage move was
/// blocked instead of a generic 500.
pub(crate) fn transition_response(
    result: Result<crate::uow::UnitOfWork, crate::lifecycle::TransitionError>,
) -> Response {
    match result {
        Ok(uow) => Json(uow).into_response(),
        Err(err) => {
            let body = Json(serde_json::json!({
                "error": "lifecycle transition blocked",
                "reason": err.message(),
                "detail": err,
            }));
            (StatusCode::CONFLICT, body).into_response()
        }
    }
}

/// Body for `POST /api/uow/:story_id/begin-investigation`. Optional `model` pins the
/// single investigation agent's model; `None`/blank defaults to the active project's
/// `tier_map.strongest`. Absent body (no JSON) is accepted (defaults applied).
#[derive(serde::Deserialize, Default)]
pub(crate) struct BeginInvestigationReq {
    #[serde(default)]
    pub(crate) model: Option<String>,
}

/// Drive the UoW Intake → Investigating (Pillar 2) AND kick a single, model-aware,
/// gated investigation agent that analyzes the story and records an investigation note
/// onto the UoW. Returns `{ "run_id", "story_id" }` so the UI can watch AgentActivity.
///
/// The investigation is a SINGLE agent (not the development fleet): it analyzes and
/// surfaces decisions; it does not scaffold or write code. The agent is gated identically
/// to every fleet agent (allowedTools = gated tools only; `Task` disallowed).
///
/// 409 (with the precise reason) if the stage transition is illegal (e.g. the UoW is
/// not at Intake) — surfaced before any run is started so the UI shows why.
/// `DELETE /api/uow/:story_id` — remove a UoW entirely. The UI gates this behind an
/// "are you sure?" confirmation. Returns `{ "ok": <removed> }`.
pub(crate) async fn uow_delete(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": state.uow.delete(&story_id) }))
}

/// Shared machinery for kicking ONE single gated investigation-agent run: resolve the
/// model, pull the story context, ground the agent in the project + repo, and spawn
/// [`crate::investigation_run::execute_investigation_run`] in the background. Returns the
/// new run id (pollable via `GET /api/runs/:id`).
///
/// Used by BOTH:
/// - [`uow_begin_investigation`] — the FRESH start, which performs the Intake →
///   Investigating stage transition first (fails the whole request if that 409s).
/// - [`uow_rerun_investigation`] (BUG B) — a follow-up pass for a story ALREADY past
///   Intake, which must NOT repeat that transition (it would 409 — the stage already
///   moved on the first begin).
pub(crate) async fn spawn_investigation_agent_run(
    state: &AppState,
    story_id: &str,
    requested_model: Option<String>,
) -> String {
    // Resolve the model: the caller's choice, else the active project's strongest tier.
    let model = requested_model
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| {
            state
                .projects
                .active()
                .map(|p| p.tier_map.strongest)
                .unwrap_or_else(crate::model_tier::default_strongest_model)
        });

    // Pull the story context for the agent prompt (best-effort; fall back to the id).
    let (title, desc) = match state.stories.get(story_id).await {
        Ok(Some(s)) => (s.title, s.description),
        _ => (story_id.to_string(), String::new()),
    };

    // GROUNDING (the invariant): hand the investigation agent the project's rule context +
    // repo digest so it analyzes the ACTUAL project (it can also read the repo clone).
    let grounding = state.project_grounding().await;
    // ON-DEMAND REPO READ (the core invariant): resolve the active project's local clone so
    // the investigation agent runs WITH the repo as its cwd + `--add-dir` and can scan/read
    // any file it needs, not just the digest. Read-only — the write gate is unchanged. None
    // when the repo isn't local (the agent then has the digest only, as before).
    let repo_dir = state.active_repo_dir();
    // MULTI-REPO READ scope: ALL the active project's local repo clones — the union read
    // window for the investigation agent (cwd stays the primary `repo_dir`; each repo is
    // added read-only via `--add-dir`).
    let read_dirs = state.active_repo_dirs();

    // Create a run the UI can poll, then kick the single gated investigation agent.
    let run_id = state
        .runs
        .create(story_id, "investigation", crate::run::RunKind::Watched);
    // The client-engagement lock: this run has only the Claude Code CLI driver, so on a client
    // engagement it fails at once with the reason and no agent starts.
    if let Some(why) = cli_only_run_client_lock_refusal(state, "the investigation") {
        state.runs.fail_with_reason(&run_id, why);
        return run_id;
    }
    {
        let runs = state.runs.clone();
        let uow = state.uow.clone();
        let clarifications = state.clarifications.clone();
        let resume = state.clarify_resume.clone();
        let rid = run_id.clone();
        let sid = story_id.to_string();
        // Register the abort handle so a Stop (POST /api/runs/:id/cancel) can reap a run
        // blocked inside the live agent subprocess (kill_on_drop). Cleared when it finishes.
        let runs_for_clear = state.runs.clone();
        let rid_for_clear = run_id.clone();
        let handle = tokio::spawn(async move {
            crate::investigation_run::execute_investigation_run(
                runs,
                uow,
                clarifications,
                resume,
                rid,
                sid,
                title,
                desc,
                model,
                grounding,
                repo_dir,
                read_dirs,
            )
            .await;
            runs_for_clear.clear_abort(&rid_for_clear);
        });
        state.runs.register_abort(&run_id, handle.abort_handle());
    }

    run_id
}

pub(crate) async fn uow_begin_investigation(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    req: Option<Json<BeginInvestigationReq>>,
) -> Response {
    // Transition the stage first; if it is illegal, surface the reason and start nothing.
    if let Err(err) = state.uow.begin_investigation(&story_id) {
        let body = Json(serde_json::json!({
            "error": "lifecycle transition blocked",
            "reason": err.message(),
            "detail": err,
        }));
        return (StatusCode::CONFLICT, body).into_response();
    }

    let model = req.and_then(|Json(r)| r.model);
    let run_id = spawn_investigation_agent_run(&state, &story_id, model).await;
    Json(serde_json::json!({ "run_id": run_id, "story_id": story_id })).into_response()
}

/// `POST /api/uow/:story_id/rerun-investigation` (BUG B) — re-run the single investigation
/// agent for a story that is ALREADY past Intake (stage Investigating or
/// DecisionsApproved), WITHOUT repeating the Intake → Investigating transition.
///
/// `begin-investigation` 409s once the stage has already moved past Intake, so before this
/// endpoint existed there was NO way to re-run investigation after the first pass produced
/// no usable note (or errored) — the "Begin investigation" button simply disappeared. This
/// gives the architect an always-available re-run affordance for the whole Investigation &
/// Refinement phase.
///
/// Requires live mode (`CAMERATA_LIVE_BUILD=1`) — refuses honestly with a 409 + explicit
/// reason otherwise, exactly like the fresh run would (re-running with live mode off would
/// just record the same "live mode is off" placeholder note again, which is not useful).
///
/// 409s when the stage is not Investigating/DecisionsApproved (e.g. still at Intake — use
/// `begin-investigation` there instead — or already in/after Development, where
/// investigation is done). 404s when the story has no UoW at all.
pub(crate) async fn uow_rerun_investigation(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    req: Option<Json<BeginInvestigationReq>>,
) -> Response {
    if !crate::run::live_mode_enabled() {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "live mode required",
                "reason": "Re-running investigation requires the live agent; set \
                           CAMERATA_LIVE_BUILD=1 and retry.",
            })),
        )
            .into_response();
    }

    match state.uow.get(&story_id) {
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "no such UoW", "reason": "Unknown story." })),
            )
                .into_response();
        }
        Some(uow)
            if !matches!(
                uow.stage,
                crate::lifecycle::UowStage::Investigating
                    | crate::lifecycle::UowStage::DecisionsApproved
            ) =>
        {
            let reason = match uow.stage {
                crate::lifecycle::UowStage::Intake => {
                    "Investigation has not started yet — use \"Begin investigation\" instead."
                }
                _ => {
                    "Investigation can only be re-run while the UoW is in the Investigation & \
                      Refinement phase."
                }
            };
            let body = Json(serde_json::json!({
                "error": "lifecycle transition blocked",
                "reason": reason,
            }));
            return (StatusCode::CONFLICT, body).into_response();
        }
        Some(_) => {}
    }

    state.uow.append_history(
        &story_id,
        "note",
        "Investigation re-run requested by the architect (manual re-run).",
    );

    let model = req.and_then(|Json(r)| r.model);
    let run_id = spawn_investigation_agent_run(&state, &story_id, model).await;
    Json(serde_json::json!({ "run_id": run_id, "story_id": story_id })).into_response()
}

/// Derive the story branch for one in-scope repo given its [`BranchMode`] and the
/// current story id. Used during per-repo worktree setup (R6 multi-repo path).
///
/// - `BranchMode::Existing { branch_name }` → use `branch_name` as-is (working off
///   an existing branch in this repo, as selected at Intake).
/// - `BranchMode::NewFromBase { new_name, .. }` → use `new_name` when non-empty;
///   fall back to `camerata/<story_id>` (the standard Camerata branch slug).
///
/// This is a pure helper — no I/O. It mirrors the fallback the primary-repo branch
/// derivation uses (`uow_branch.unwrap_or_else(|| format!("camerata/{story_id}"))`).
/// Parse per-artifact `camerata:allow INTEGRATION-* -- reason` waivers out of the
/// active project's in-scope repo worktrees, for the deterministic integration gate.
///
/// Reuses the same inline-waiver model as the per-agent tiers ([`crate::suppression`]):
/// a `// camerata:allow INTEGRATION-AUTH-SEAM-1 -- intentionally public` on the line
/// that CALLS the endpoint waives the relational finding for that endpoint. The waiver's
/// artifact identity is derived by re-extracting the endpoint on the waived line, so a
/// waiver is scoped to exactly the endpoint it annotates (never a blanket suppression).
///
/// Best-effort: unreadable files are skipped. A reason-less waiver is carried through
/// with `reason = None` so the gate treats it as invalid (does not suppress), mirroring
/// the per-agent invariant.
pub(crate) async fn collect_integration_waivers(
    state: &AppState,
) -> Vec<camerata_checks::GateWaiver> {
    let dirs = state.active_repo_dirs();
    tokio::task::spawn_blocking(move || {
        let mut out: Vec<camerata_checks::GateWaiver> = Vec::new();
        for dir in &dirs {
            for file in walk_source_files(dir) {
                let Ok(content) = std::fs::read_to_string(&file) else {
                    continue;
                };
                let rel = file
                    .strip_prefix(dir)
                    .unwrap_or(&file)
                    .to_string_lossy()
                    .to_string();
                for w in crate::suppression::parse_inline_waivers(&rel, &content) {
                    if !w.rule_id.starts_with("INTEGRATION-") {
                        continue;
                    }
                    // Scope the waiver to the endpoint on the annotated line, if we can
                    // recover one; otherwise it is a rule-wide (blanket) waiver.
                    let artifact = content
                        .lines()
                        .nth(w.line.saturating_sub(1))
                        .and_then(camerata_checks::integration::extractor_endpoint_identity)
                        .unwrap_or_default();
                    out.push(camerata_checks::GateWaiver {
                        rule_id: w.rule_id.clone(),
                        artifact,
                        reason: w.reason.clone(),
                    });
                }
            }
        }
        out
    })
    .await
    .unwrap_or_default()
}

/// Walk a repo dir for source files the integration extractors understand (pruning
/// vendored/build dirs). Small, dependency-free mirror used only for waiver parsing.
pub(crate) fn walk_source_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    const PRUNE: &[&str] = &[
        "node_modules",
        "target",
        ".git",
        ".camerata-venv",
        "vendor",
        "dist",
        "build",
        "__pycache__",
        ".next",
    ];
    const EXTS: &[&str] = &[
        "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "go", "rb", "java", "cs",
    ];
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                let name = entry.file_name();
                if PRUNE.contains(&name.to_string_lossy().as_ref()) {
                    continue;
                }
                stack.push(path);
            } else if ft.is_file() {
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    if EXTS.contains(&ext) {
                        out.push(path);
                    }
                }
            }
        }
    }
    out.sort();
    out
}

/// The branch an in-scope repo works on for `story_id`; delegates to the shared
/// [`crate::uow::BranchMode::branch_for_story`] so the console's run start agrees.
pub(crate) fn derive_scope_branch(mode: &crate::uow::BranchMode, story_id: &str) -> String {
    mode.branch_for_story(story_id)
}

/// The 4xx reason when a repository action is asked of a unit that names no repository.
pub(crate) const NO_UNIT_REPO: &str =
    "this unit of work names no repository: set it under Intake, Repos \
     in scope (a unit created from a GitHub issue works in the issue's repository)";

/// `POST /api/uow/:story_id/branches` → `{ "local": [...], "origin": [...] }`.
///
/// Lists the branches this UoW can merge FROM, populating the "Update branch" picker.
/// Resolves the unit's repository ([`crate::uow::UnitOfWork::repo`]) and its local clone
/// dir; `local` are the working copy's branches, `origin` are the `origin/*`
/// remote-tracking branches (prefix stripped). Token-less / no-clone / unresolvable repo → empty lists (graceful, never an
/// error) so the UI renders an empty picker rather than breaking.
pub(crate) async fn uow_list_branches(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
) -> Json<crate::workspace::MergeSourceBranches> {
    let Some(repo) = state.uow.repo_for(&story_id) else {
        return Json(crate::workspace::MergeSourceBranches::default());
    };
    let override_path = state.settings.repo_path(&repo);
    let workspace_root = state.settings.workspace_root();
    let Some(dir) = crate::workspace::resolve_repo_dir(
        override_path.as_deref(),
        workspace_root.as_deref(),
        &repo,
    ) else {
        return Json(crate::workspace::MergeSourceBranches::default());
    };
    Json(crate::workspace::list_merge_sources(&dir).await)
}

/// Request body for `POST /api/uow/:story_id/update-branch`.
#[derive(serde::Deserialize)]
pub(crate) struct UpdateBranchReq {
    /// The branch to merge INTO the UoW branch.
    pub(crate) source_branch: String,
    /// Where the source lives: `"local"` or `"origin"`.
    pub(crate) source: String,
    /// Optional model id for the conflict-resolution agent; defaults to the active
    /// project's strongest tier.
    #[serde(default)]
    pub(crate) model: Option<String>,
}

/// `POST /api/uow/:story_id/update-branch` body `{ source_branch, source }` → `{ run_id }`.
///
/// Merges `source_branch` (local/origin) INTO the UoW's working branch in its local
/// clone — the GitHub "Update branch" pattern, AI-assisted. A clean merge commits; a
/// conflict spawns ONE gated agent to resolve it (see [`crate::update_branch_run`]). The
/// run is pollable via `GET /api/runs/:id`.
///
/// 4xx (no run created) when: the source is malformed; the UoW has no branch yet
/// (nothing to update); the repo can't be derived from the story id; or the repo isn't
/// resolved to a local clone.
pub(crate) async fn uow_update_branch(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    Json(req): Json<UpdateBranchReq>,
) -> Response {
    let bad = |msg: String| {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": msg })),
        )
            .into_response()
    };
    // The client-engagement lock: this run has only the Claude Code CLI driver.
    if let Some(why) = cli_only_run_client_lock_refusal(&state, "the branch update") {
        return bad(why);
    }

    let Some(source_kind) = crate::update_branch_run::MergeSourceKind::from_wire(&req.source)
    else {
        return bad(format!(
            "`source` must be `local` or `origin`, got `{}`",
            req.source
        ));
    };
    if req.source_branch.trim().is_empty() {
        return bad("`source_branch` must not be empty".to_string());
    }

    // The UoW must have a working branch to update.
    let uow = state.uow.get_or_create(&story_id);
    let Some(target_branch) = uow.branch.clone().filter(|b| !b.trim().is_empty()) else {
        return bad(
            "this UoW has no branch yet — start development first so there is a branch to update"
                .to_string(),
        );
    };

    let Some(repo) = uow.repo().map(str::to_string) else {
        return bad(NO_UNIT_REPO.to_string());
    };
    let override_path = state.settings.repo_path(&repo);
    let workspace_root = state.settings.workspace_root();
    // Per-UoW worktree (Decision 1): the UoW's branch is checked out in its OWN worktree off
    // the shared clone, not in the shared clone itself — so two same-repo UoWs never collide on
    // a checkout. This is the canonical dir where this UoW's code lives; ship/push (Phase 2)
    // will reuse it. `None` when the repo isn't a local clone (no override + no workspace root,
    // or the clone doesn't exist yet) — same condition `resolve_repo_dir` would have failed on.
    let Some(dir) = crate::workspace::resolve_uow_worktree(
        override_path.as_deref(),
        workspace_root.as_deref(),
        &repo,
        &target_branch,
        active_project_repos(&state).as_deref(),
    )
    .await
    else {
        return bad(
            "repo not resolved locally — set its path in the Rules view (and start development \
             so the repo is cloned) before updating the branch"
                .to_string(),
        );
    };

    // Resolve the conflict-resolution agent's model: the caller's choice, else the active
    // project's strongest tier.
    let model = req
        .model
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| {
            state
                .projects
                .active()
                .map(|p| p.tier_map.strongest)
                .unwrap_or_else(crate::model_tier::default_strongest_model)
        });

    let run_id = state
        .runs
        .create(&story_id, "update-branch", crate::run::RunKind::Watched);
    {
        let runs = state.runs.clone();
        let uow_store = state.uow.clone();
        let rid = run_id.clone();
        let sid = story_id.clone();
        let token = state.github_token();
        let src = req.source_branch.clone();
        // GROUNDING (the invariant): rule + repo digest for the conflict resolver.
        let grounding = state.project_grounding().await;
        // MULTI-REPO READ scope: ALL the active project's local repo clones (read-only).
        let read_dirs = state.active_repo_dirs();
        // GAP-2: the active project's VCS-action process rules for the server-side merge-commit gate.
        let vcs_config = state
            .projects
            .active()
            .map(|p| p.process_rule_config)
            .unwrap_or_default();
        // LIFECYCLE-1: register the abort handle so a Stop can reap a run blocked inside the
        // live agent subprocess (kill_on_drop). Cleared when the run finishes.
        let runs_for_clear = state.runs.clone();
        let rid_for_clear = run_id.clone();
        let handle = tokio::spawn(async move {
            crate::update_branch_run::execute_update_branch_run(
                runs,
                uow_store,
                rid,
                sid,
                repo,
                dir,
                target_branch,
                src,
                source_kind,
                token,
                model,
                grounding,
                read_dirs,
                vcs_config,
            )
            .await;
            runs_for_clear.clear_abort(&rid_for_clear);
        });
        state.runs.register_abort(&run_id, handle.abort_handle());
    }

    Json(serde_json::json!({ "run_id": run_id, "story_id": story_id })).into_response()
}

// ── Per-UoW PR lifecycle (Decision 2) ──────────────────────────────────────────

/// Request body for `POST /api/uow/:story_id/pr/open`.
#[derive(serde::Deserialize)]
pub(crate) struct PrOpenReq {
    /// The target/base branch to open the PR INTO (the console's base-branch picker).
    /// Empty / omitted falls back to the repo's default branch.
    #[serde(default)]
    pub(crate) base_branch: Option<String>,
}

/// `POST /api/uow/:story_id/pr/open` body `{ base_branch }` → push the UoW branch and
/// open a PR into the chosen base, STORING the PR number + url on the UoW.
///
/// Resolves the UoW WORKTREE (Phase 1 seam), pushes the branch, then opens the PR. If a
/// PR for this head already exists (incl. one opened directly in GitHub), `open_pr_with_base`
/// discovers + returns it and we store that. 4xx when: no token; the UoW has no branch; the
/// repo can't be derived; or the repo isn't a local clone.
pub(crate) async fn uow_pr_open(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    Json(req): Json<PrOpenReq>,
) -> Response {
    let bad = |msg: String| {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": msg })),
        )
            .into_response()
    };
    let Some(token) = state.github_token() else {
        return bad("no GitHub token — set CAMERATA_GITHUB_TOKEN to open a PR".to_string());
    };
    let uow = state.uow.get_or_create(&story_id);
    let Some(branch) = uow.branch.clone().filter(|b| !b.trim().is_empty()) else {
        return bad(
            "this UoW has no branch yet — start development first so there is a branch to open a PR for"
                .to_string(),
        );
    };
    let Some(repo) = uow.repo().map(str::to_string) else {
        return bad(NO_UNIT_REPO.to_string());
    };
    let override_path = state.settings.repo_path(&repo);
    let workspace_root = state.settings.workspace_root();
    let Some(dir) = crate::workspace::resolve_uow_worktree(
        override_path.as_deref(),
        workspace_root.as_deref(),
        &repo,
        &branch,
        active_project_repos(&state).as_deref(),
    )
    .await
    else {
        return bad(
            "repo not resolved locally — set its path in the Rules view (and start development \
             so the repo is cloned) before opening a PR"
                .to_string(),
        );
    };

    // Push the UoW branch from its worktree.
    if let Err(e) = crate::workspace::push_branch(&dir, &repo, &branch, &token).await {
        return bad(format!("could not push `{branch}`: {e}"));
    }
    // Open (or discover) the PR into the chosen base.
    //
    // GAP-2 chokepoint. Camerata authors this PR's title + body, so it makes them COMPLIANT
    // with the project's active process rules and HARD-BLOCK gates them. A non-compliant
    // machine PR surfaces as an error here rather than opening an ungated PR.
    let vcs_config = crate::vcs_choke::config_for_unit(
        &process_rule_config_for(&state, &active_project_id(&state)),
        &uow,
    );
    let numeric_id = uow.commit_reference();
    let summary = uow
        .work_item
        .as_deref()
        .map(|w| format!("{w}: {story_id}"))
        .unwrap_or_else(|| format!("Camerata: {story_id}"));
    let (title, body) = crate::vcs_choke::compliant_machine_pr(
        &vcs_config,
        &summary,
        &format!("Opened by Camerata for story `{story_id}`."),
        &numeric_id,
    );
    if let Err(e) = crate::vcs_choke::gated_pr(&vcs_config, &title, &body) {
        return bad(format!("VCS-action gate blocked the machine PR: {e}"));
    }
    let base = req.base_branch.as_deref();
    match crate::workspace::open_pr_with_base(&repo, &branch, base, &title, &body, &token).await {
        Ok(opened) => {
            state
                .uow
                .set_pr(&story_id, Some(opened.number), Some(opened.url.clone()));
            state.uow.append_history(
                &story_id,
                "pr_open",
                &format!(
                    "Opened PR #{} for `{branch}`: {}",
                    opened.number, opened.url
                ),
            );
            Json(serde_json::json!({
                "ok": true,
                "pr_number": opened.number,
                "pr_url": opened.url,
            }))
            .into_response()
        }
        Err(e) => bad(format!("could not open the PR: {e}")),
    }
}

/// `GET /api/uow/:story_id/pr` → the console "Pull PR info": resolve the PR for the UoW
/// (stored number → else head-branch search + STORE), then return its state + comments +
/// CI checks.
///
/// Graceful: with no token, no derivable repo, or no PR for the UoW, returns a clear empty
/// payload `{ ok: false, pr: null, ... }` — never an error — so the console renders "No PR
/// yet" instead of breaking. (Asking for PR data is a read, not a gated write.)
pub(crate) async fn uow_pr_get(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
) -> Json<serde_json::Value> {
    let empty = |msg: &str| serde_json::json!({ "ok": false, "pr": null, "comments": [], "checks": null, "message": msg });
    let Some(token) = state.github_token() else {
        return Json(empty(
            "Connect GitHub (set CAMERATA_GITHUB_TOKEN) to pull PR info.",
        ));
    };
    // ROUTES-8: READ — must not create. An unknown id resolves to a default UoW (no PR ref),
    // so the "No PR" path fires without persisting a junk UoW.
    let uow = state.uow.get_or_default(&story_id);
    let Some(repo) = uow.repo().map(str::to_string) else {
        return Json(empty(NO_UNIT_REPO));
    };
    let Some(info) =
        crate::pr::resolve_pr_for_uow(&state.uow, &story_id, &uow, &repo, &token).await
    else {
        return Json(empty("No PR for this UoW yet."));
    };
    let comments = crate::pr::list_pr_comments(&repo, info.number, &token)
        .await
        .unwrap_or_default();
    let checks = crate::pr::pr_checks(&repo, &info.head_sha, &token)
        .await
        .unwrap_or_default();
    Json(serde_json::json!({
        "ok": true,
        "pr": info,
        "comments": comments,
        "checks": checks,
    }))
}

/// Request body for `POST /api/uow/:story_id/pr/comment`.
#[derive(serde::Deserialize)]
pub(crate) struct PrCommentReq {
    pub(crate) body: String,
}

/// `POST /api/uow/:story_id/pr/comment` body `{ body }` → post a comment on the UoW's PR.
/// Resolves the PR (stored → else head-search + store), then posts. 4xx on no token /
/// empty body / no PR / no repo.
pub(crate) async fn uow_pr_comment(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    Json(req): Json<PrCommentReq>,
) -> Response {
    let bad = |msg: String| {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": msg })),
        )
            .into_response()
    };
    let Some(token) = state.github_token() else {
        return bad("no GitHub token — set CAMERATA_GITHUB_TOKEN to comment".to_string());
    };
    if req.body.trim().is_empty() {
        return bad("comment body must not be empty".to_string());
    }
    let uow = state.uow.get_or_create(&story_id);
    let Some(repo) = uow.repo().map(str::to_string) else {
        return bad(NO_UNIT_REPO.to_string());
    };
    let Some(info) =
        crate::pr::resolve_pr_for_uow(&state.uow, &story_id, &uow, &repo, &token).await
    else {
        return bad("no PR for this UoW yet — open one first".to_string());
    };
    match crate::pr::post_pr_comment(&repo, info.number, &req.body, &token).await {
        Ok(url) => Json(serde_json::json!({ "ok": true, "url": url })).into_response(),
        Err(e) => bad(format!("could not post the comment: {e}")),
    }
}

/// `POST /api/uow/:story_id/pr/resolve` → a GATED run (mirrors `update-branch`) that feeds
/// the PR feedback (open review comments + failing check names) to ONE governed agent to
/// fix, commit, and optionally push, IN THE UoW WORKTREE. Returns `{ run_id }`.
///
/// The gate is universal + unchanged: same governed role, `Task` disallowed, gated writes
/// only, layer-2 bounce. Reading the PR feedback here is a read; FIXING it goes through the
/// gate. 4xx when: no token; no branch; no derivable repo; the repo isn't a local clone; or
/// no PR exists for the UoW.
#[derive(serde::Deserialize)]
pub(crate) struct PrResolveReq {
    #[serde(default)]
    pub(crate) model: Option<String>,
}

pub(crate) async fn uow_pr_resolve(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    Json(req): Json<PrResolveReq>,
) -> Response {
    let bad = |msg: String| {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": msg })),
        )
            .into_response()
    };
    // The client-engagement lock: this run has only the Claude Code CLI driver.
    if let Some(why) = cli_only_run_client_lock_refusal(&state, "PR feedback resolution") {
        return bad(why);
    }
    let Some(token) = state.github_token() else {
        return bad(
            "no GitHub token — set CAMERATA_GITHUB_TOKEN to resolve PR feedback".to_string(),
        );
    };
    let uow = state.uow.get_or_create(&story_id);
    let Some(target_branch) = uow.branch.clone().filter(|b| !b.trim().is_empty()) else {
        return bad("this UoW has no branch yet — nothing to resolve".to_string());
    };
    let Some(repo) = uow.repo().map(str::to_string) else {
        return bad(NO_UNIT_REPO.to_string());
    };
    // Resolve the PR (stored → else head-search + store) so we have the feedback to feed.
    let Some(info) =
        crate::pr::resolve_pr_for_uow(&state.uow, &story_id, &uow, &repo, &token).await
    else {
        return bad("no PR for this UoW yet — open one first".to_string());
    };
    // Gather the feedback: open REVIEW comments (actionable code feedback) + failing checks.
    let review_comments: Vec<String> = crate::pr::list_pr_comments(&repo, info.number, &token)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|c| c.review && !c.body.trim().is_empty())
        .map(|c| c.body)
        .collect();
    let failing_checks = crate::pr::pr_checks(&repo, &info.head_sha, &token)
        .await
        .map(|c| c.failing)
        .unwrap_or_default();

    let override_path = state.settings.repo_path(&repo);
    let workspace_root = state.settings.workspace_root();
    let Some(dir) = crate::workspace::resolve_uow_worktree(
        override_path.as_deref(),
        workspace_root.as_deref(),
        &repo,
        &target_branch,
        active_project_repos(&state).as_deref(),
    )
    .await
    else {
        return bad(
            "repo not resolved locally — set its path in the Rules view (and start development \
             so the repo is cloned) before resolving PR feedback"
                .to_string(),
        );
    };

    let model = req
        .model
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| {
            state
                .projects
                .active()
                .map(|p| p.tier_map.strongest)
                .unwrap_or_else(crate::model_tier::default_strongest_model)
        });

    let run_id = state
        .runs
        .create(&story_id, "pr-resolve", crate::run::RunKind::Watched);
    {
        let runs = state.runs.clone();
        let uow_store = state.uow.clone();
        let rid = run_id.clone();
        let sid = story_id.clone();
        let pr_number = info.number;
        // GROUNDING (the invariant): rule + repo digest for the PR-feedback resolver.
        let grounding = state.project_grounding().await;
        // MULTI-REPO READ scope: ALL the active project's local repo clones (read-only).
        let read_dirs = state.active_repo_dirs();
        // GAP-2: the active project's VCS-action process rules for the server-side commit gate.
        let vcs_config = state
            .projects
            .active()
            .map(|p| p.process_rule_config)
            .unwrap_or_default();
        // LIFECYCLE-1: register the abort handle so a Stop can reap a run blocked inside the
        // live agent subprocess (kill_on_drop). Cleared when the run finishes.
        let runs_for_clear = state.runs.clone();
        let rid_for_clear = run_id.clone();
        let handle = tokio::spawn(async move {
            crate::pr_resolve_run::execute_pr_resolve_run(
                runs,
                uow_store,
                rid,
                sid,
                repo,
                dir,
                target_branch,
                pr_number,
                review_comments,
                failing_checks,
                Some(token),
                model,
                grounding,
                read_dirs,
                vcs_config,
            )
            .await;
            runs_for_clear.clear_abort(&rid_for_clear);
        });
        state.runs.register_abort(&run_id, handle.abort_handle());
    }
    Json(serde_json::json!({ "run_id": run_id, "story_id": story_id })).into_response()
}

/// Drive the UoW Investigating → DecisionsApproved (Pillar 2), gated by the story's
/// decision records. 409 (with the precise reason) if the gate is not satisfied or the
/// UoW is at the wrong stage.
pub(crate) async fn uow_approve_decisions(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
) -> Response {
    transition_response(state.uow.approve_decisions(&story_id))
}

// ── Feature flags ────────────────────────────────────────────────────────────
