//! Unit-of-work state handlers: status, branch, intake, chat, contract and meta.

use crate::*;

#[derive(serde::Deserialize)]
pub(crate) struct UowStatusReq {
    /// Accepted values: `"new"`, `"in_progress"`, `"done"`.
    pub(crate) status: String,
}

/// Set the dev status for a story's UoW.
pub(crate) async fn uow_set_status(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    Json(req): Json<UowStatusReq>,
) -> Result<Json<crate::uow::UnitOfWork>, AppError> {
    let status = crate::uow::DevStatus::from_wire(&req.status).ok_or_else(|| {
        AppError(anyhow::anyhow!(
            "unknown status {:?}; expected new, in_progress, done",
            req.status
        ))
    })?;
    state.uow.set_status(&story_id, status);
    Ok(Json(state.uow.get_or_create(&story_id)))
}

#[derive(serde::Deserialize)]
pub(crate) struct UowBranchReq {
    /// The branch name, or `null` to clear it.
    pub(crate) branch: Option<String>,
}

/// A 400 for a branch name that fails [`camerata_api_types::uow::validate_branch_name`].
pub(crate) fn invalid_branch_response(reason: String) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": "invalid branch name", "reason": reason })),
    )
        .into_response()
}

/// Set (or clear) the branch for a story's UoW. A name is validated with the ref-format
/// rules first (it becomes a directory under the worktrees root), so `.`, `..` and the like
/// are refused with a 400 and nothing is recorded.
pub(crate) async fn uow_set_branch(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    Json(req): Json<UowBranchReq>,
) -> Response {
    if let Some(branch) = req.branch.as_deref() {
        if let Err(reason) = camerata_api_types::uow::validate_branch_name(branch) {
            return invalid_branch_response(reason);
        }
    }
    // The unit must belong to the active project: a story id from another project is refused
    // rather than given a branch in this project's repositories.
    let project = state.projects.active();
    if let Some(reason) = crate::run_target::project_membership_refusal(
        &story_id,
        state.uow.repo_for(&story_id).as_deref(),
        project.as_ref().map(|p| p.repos.as_slice()),
    ) {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "unit not in the active project",
                "reason": reason,
                "story_id": story_id,
            })),
        )
            .into_response();
    }
    state.uow.set_branch(&story_id, req.branch);
    Json(state.uow.get_or_create(&story_id)).into_response()
}

#[derive(serde::Deserialize)]
pub(crate) struct UowPublishReposReq {
    /// The `owner/repo` targets this node's issue should be published into. Empty means
    /// "not chosen yet" and publish falls back to the design's project repos.
    #[serde(default)]
    pub(crate) repos: Vec<String>,
}

/// `POST /api/uow/:story_id/publish-repos` body `{ repos: [...] }` — set the per-node
/// publish repo assignment used by design publish. Returns the updated UoW.
pub(crate) async fn uow_set_publish_repos(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    Json(req): Json<UowPublishReposReq>,
) -> Json<crate::uow::UnitOfWork> {
    Json(state.uow.set_publish_repos(&story_id, req.repos))
}

#[derive(serde::Deserialize)]
pub(crate) struct UowHistoryReq {
    pub(crate) kind: String,
    pub(crate) text: String,
}

/// Append an entry to the AI development history for a story's UoW.
pub(crate) async fn uow_append_history(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    Json(req): Json<UowHistoryReq>,
) -> Json<crate::uow::UnitOfWork> {
    state.uow.append_history(&story_id, &req.kind, &req.text);
    Json(state.uow.get_or_create(&story_id))
}

// ── 3-phase cockpit state (#104 / #105) ──────────────────────────────────────

#[derive(serde::Deserialize)]
pub(crate) struct UowIntakeContextReq {
    /// The free-text context for the investigation agent.
    pub(crate) context: String,
}

/// `POST /api/uow/:story_id/intake/context` body `{ context }` — set the Intake free-text
/// context for the investigation agent (3-phase doc §3). Returns the updated UoW.
pub(crate) async fn uow_set_intake_context(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    Json(req): Json<UowIntakeContextReq>,
) -> Json<crate::uow::UnitOfWork> {
    Json(state.uow.set_intake_context(&story_id, &req.context))
}

/// The active project's repositories (`None` with no active project), for the membership
/// check every worktree resolution goes through.
pub(crate) fn active_project_repos(state: &AppState) -> Option<Vec<String>> {
    state.projects.active().map(|p| p.repos)
}

#[derive(serde::Deserialize)]
pub(crate) struct UowIntakeReposReq {
    /// The full in-scope repo/branch set (replaces the prior scope). Out-of-scope repos
    /// are simply absent from this list (R6).
    pub(crate) repos: Vec<crate::uow::RepoScope>,
}

/// `POST /api/uow/:story_id/intake/repos` body `{ repos: [...] }` — replace the per-story
/// repo/branch scope (R6). Returns the updated UoW.
pub(crate) async fn uow_set_intake_repos(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    Json(req): Json<UowIntakeReposReq>,
) -> Response {
    // A branch name typed into the scope becomes a worktree directory name: validate every
    // explicit name before anything is recorded. (A blank name derives `camerata/<story>`,
    // which the run target validates when it is used.)
    for scope in &req.repos {
        if let Some(name) = scope.branch.explicit_name() {
            if let Err(reason) = camerata_api_types::uow::validate_branch_name(name) {
                return invalid_branch_response(format!("Repo `{}`: {reason}", scope.repo));
            }
        }
    }
    // A scoped repository becomes a directory under the workspace root and a push URL, so each
    // one must be an `OWNER/REPO` coordinate (the one shared validator) before anything is
    // recorded.
    for scope in &req.repos {
        if let Err(reason) = camerata_api_types::uow::validate_repo_name(&scope.repo) {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "invalid repository", "reason": reason })),
            )
                .into_response();
        }
    }
    // Every in-scope repository must belong to the active project (the same check as the run
    // and the branch record), so a scope cannot pull another project's clones into this unit.
    let project_repos = active_project_repos(&state);
    for scope in &req.repos {
        if let Some(reason) = crate::run_target::scope_membership_refusal(
            &story_id,
            &scope.repo,
            project_repos.as_deref(),
        ) {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "repository not in the active project",
                    "reason": reason,
                    "story_id": story_id,
                })),
            )
                .into_response();
        }
    }
    match state.uow.set_intake_repos(&story_id, req.repos) {
        Ok(uow) => Json(uow).into_response(),
        Err(reason) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "invalid repository", "reason": reason })),
        )
            .into_response(),
    }
}

/// Per-repo Ship result for one in-scope repo. Part of the [`UowIntakeShipResponse`].
#[derive(serde::Serialize, Debug)]
pub struct RepoShipResult {
    /// `owner/repo`.
    pub repo: String,
    /// The story branch that was pushed.
    pub branch: String,
    /// `true` when push + PR succeeded.
    pub ok: bool,
    /// The PR URL on success.
    pub pr_url: Option<String>,
    /// The PR number on success.
    pub pr_number: Option<u64>,
    /// Human-readable error when `ok = false`.
    pub error: Option<String>,
}

/// Response body for `POST /api/uow/:story_id/intake/ship`.
#[derive(serde::Serialize, Debug)]
pub struct UowIntakeShipResponse {
    /// Per-repo Ship results (one entry per in-scope repo in `intake.repos`).
    pub repos: Vec<RepoShipResult>,
    /// `true` when every repo shipped successfully.
    pub all_ok: bool,
}

/// Optional request body for `POST /api/uow/:story_id/intake/ship`.
#[derive(serde::Deserialize, Default)]
pub(crate) struct UowIntakeShipReq {
    /// The target/base branch to open each PR into. Empty/absent → each repo's default branch.
    #[serde(default)]
    pub(crate) base_branch: Option<String>,
}

/// `POST /api/uow/:story_id/intake/ship` body `{ base_branch? }` — per-repo Ship (R3.f / R6).
///
/// Iterates the UoW's `intake.repos`; for each in-scope repo: derives the story branch
/// (via `derive_scope_branch`), resolves its worktree, pushes the branch, and opens a PR.
/// Returns per-repo results in [`UowIntakeShipResponse`] — partial success is surfaced
/// (some repos may succeed while others fail).
///
/// Falls back to the single-repo PR-open path (`uow_pr_open` behaviour) when
/// `intake.repos` is empty (no multi-repo scope selected at Intake).
///
/// 4xx when: no GitHub token; or when single-repo fallback finds no branch / no clone.
pub(crate) async fn uow_intake_ship(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    req: Option<Json<UowIntakeShipReq>>,
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

    let base_branch = req
        .and_then(|Json(r)| r.base_branch)
        .filter(|b| !b.trim().is_empty());
    let uow = state.uow.get_or_create(&story_id);
    let workspace_root = state.settings.workspace_root();

    // GAP-2: Camerata authors these Ship PRs, so make their title/body COMPLIANT with the
    // project's active process rules and HARD-BLOCK gate them at each PR-open site.
    let vcs_config = crate::vcs_choke::config_for_unit(
        &process_rule_config_for(&state, &active_project_id(&state)),
        &uow,
    );
    let numeric_id = uow.commit_reference();

    // ── Multi-repo path: intake.repos has explicit selections ─────────────────
    if !uow.intake.repos.is_empty() {
        let mut results: Vec<RepoShipResult> = Vec::new();

        for scope in &uow.intake.repos {
            // The same branch the run works on (the unit's recorded branch for its own
            // repository; the scope's branch for the others).
            let scope_branch =
                crate::run_target::unit_branch_for_repo(&uow, &story_id, &scope.repo)
                    .unwrap_or_else(|| derive_scope_branch(&scope.branch, &story_id));
            let override_path = state.settings.repo_path(&scope.repo);

            // Resolve the worktree for this repo + branch.
            let dir = match crate::workspace::resolve_uow_worktree(
                override_path.as_deref(),
                workspace_root.as_deref(),
                &scope.repo,
                &scope_branch,
                active_project_repos(&state).as_deref(),
            )
            .await
            {
                Some(d) => d,
                None => {
                    results.push(RepoShipResult {
                        repo: scope.repo.clone(),
                        branch: scope_branch.clone(),
                        ok: false,
                        pr_url: None,
                        pr_number: None,
                        error: Some(format!(
                            "repo `{}` is not resolved locally — set its path in the Rules view \
                             and start development so the repo is cloned",
                            scope.repo
                        )),
                    });
                    continue;
                }
            };

            // Push the branch from its worktree.
            if let Err(e) =
                crate::workspace::push_branch(&dir, &scope.repo, &scope_branch, &token).await
            {
                results.push(RepoShipResult {
                    repo: scope.repo.clone(),
                    branch: scope_branch.clone(),
                    ok: false,
                    pr_url: None,
                    pr_number: None,
                    error: Some(format!("could not push `{scope_branch}`: {e}")),
                });
                continue;
            }

            // Open (or discover) the PR. Compliant machine title/body + HARD-BLOCK gate.
            let (title, body) = crate::vcs_choke::compliant_machine_pr(
                &vcs_config,
                &format!("Camerata: {story_id} ({})", scope.repo),
                &format!(
                    "Opened by Camerata for story `{story_id}` in repo `{}`.",
                    scope.repo
                ),
                &numeric_id,
            );
            if let Err(e) = crate::vcs_choke::gated_pr(&vcs_config, &title, &body) {
                results.push(RepoShipResult {
                    repo: scope.repo.clone(),
                    branch: scope_branch.clone(),
                    ok: false,
                    pr_url: None,
                    pr_number: None,
                    error: Some(format!("VCS-action gate blocked the machine PR: {e}")),
                });
                continue;
            }
            match crate::workspace::open_pr_with_base(
                &scope.repo,
                &scope_branch,
                base_branch.as_deref(),
                &title,
                &body,
                &token,
            )
            .await
            {
                Ok(opened) => {
                    state.uow.append_history(
                        &story_id,
                        "intake_ship",
                        &format!(
                            "Opened PR #{} for `{}` on `{}`: {}",
                            opened.number, scope.repo, scope_branch, opened.url
                        ),
                    );
                    results.push(RepoShipResult {
                        repo: scope.repo.clone(),
                        branch: scope_branch.clone(),
                        ok: true,
                        pr_url: Some(opened.url),
                        pr_number: Some(opened.number),
                        error: None,
                    });
                }
                Err(e) => {
                    results.push(RepoShipResult {
                        repo: scope.repo.clone(),
                        branch: scope_branch.clone(),
                        ok: false,
                        pr_url: None,
                        pr_number: None,
                        error: Some(format!("could not open PR: {e}")),
                    });
                }
            }
        }

        let all_ok = results.iter().all(|r| r.ok);
        return Json(UowIntakeShipResponse {
            repos: results,
            all_ok,
        })
        .into_response();
    }

    // ── Single-repo fallback: no intake.repos — use the UoW's primary branch ──
    // Mirrors the `uow_pr_open` flow so existing single-repo users see no change.
    let Some(branch) = uow.branch.clone().filter(|b| !b.trim().is_empty()) else {
        return bad(
            "this UoW has no branch yet — start development first so there is a branch to \
             open a PR for, or set the in-scope repos in the Intake view for multi-repo Ship"
                .to_string(),
        );
    };
    let Some(repo) = uow.repo().map(str::to_string) else {
        return bad(NO_UNIT_REPO.to_string());
    };
    let override_path = state.settings.repo_path(&repo);
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
            "repo not resolved locally — set its path in the Rules view (and start \
             development so the repo is cloned) before opening a PR"
                .to_string(),
        );
    };
    if let Err(e) = crate::workspace::push_branch(&dir, &repo, &branch, &token).await {
        return bad(format!("could not push `{branch}`: {e}"));
    }
    // Compliant machine title/body + HARD-BLOCK gate (GAP-2).
    let (title, body) = crate::vcs_choke::compliant_machine_pr(
        &vcs_config,
        &format!("Camerata: {story_id}"),
        &format!("Opened by Camerata for story `{story_id}`."),
        &numeric_id,
    );
    if let Err(e) = crate::vcs_choke::gated_pr(&vcs_config, &title, &body) {
        return bad(format!("VCS-action gate blocked the machine PR: {e}"));
    }
    match crate::workspace::open_pr_with_base(
        &repo,
        &branch,
        base_branch.as_deref(),
        &title,
        &body,
        &token,
    )
    .await
    {
        Ok(opened) => {
            state
                .uow
                .set_pr(&story_id, Some(opened.number), Some(opened.url.clone()));
            state.uow.append_history(
                &story_id,
                "intake_ship",
                &format!(
                    "Opened PR #{} for `{branch}`: {}",
                    opened.number, opened.url
                ),
            );
            let results = vec![RepoShipResult {
                repo: repo.clone(),
                branch,
                ok: true,
                pr_url: Some(opened.url),
                pr_number: Some(opened.number),
                error: None,
            }];
            Json(UowIntakeShipResponse {
                repos: results,
                all_ok: true,
            })
            .into_response()
        }
        Err(e) => bad(format!("could not open the PR: {e}")),
    }
}

#[derive(serde::Deserialize)]
pub(crate) struct UowChatReq {
    /// `"user"` or `"agent"`.
    pub(crate) role: String,
    /// The message body.
    pub(crate) text: String,
}

/// `POST /api/uow/:story_id/investigation/chat` body `{ role, text }` — append one turn to
/// the investigation/refinement agent chat transcript (3-phase doc §4). Returns the
/// updated UoW.
pub(crate) async fn uow_append_investigation_chat(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    Json(req): Json<UowChatReq>,
) -> Json<crate::uow::UnitOfWork> {
    Json(
        state
            .uow
            .append_investigation_chat(&story_id, &req.role, &req.text),
    )
}

/// `POST /api/uow/:story_id/development/chat` body `{ role, text }` — append one turn to the
/// development agent chat transcript (3-phase doc §5). Returns the updated UoW.
pub(crate) async fn uow_append_development_chat(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    Json(req): Json<UowChatReq>,
) -> Json<crate::uow::UnitOfWork> {
    Json(
        state
            .uow
            .append_development_chat(&story_id, &req.role, &req.text),
    )
}

#[derive(serde::Deserialize)]
pub(crate) struct UowContractReq {
    /// The prose interface contract (free-form).
    pub(crate) contract: String,
    /// Whether the work crosses a contract boundary (a contract is then required before
    /// development per R3.g). Defaults to `false` when absent.
    #[serde(default)]
    pub(crate) crosses_boundary: bool,
}

/// `POST /api/uow/:story_id/contract` body `{ contract, crosses_boundary }` — set the prose
/// interface contract + the boundary flag (R3.g / §4.6). Returns the updated UoW.
pub(crate) async fn uow_set_contract(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    Json(req): Json<UowContractReq>,
) -> Json<crate::uow::UnitOfWork> {
    Json(
        state
            .uow
            .set_contract(&story_id, &req.contract, req.crosses_boundary),
    )
}

#[derive(serde::Deserialize)]
pub(crate) struct UowMetaReq {
    /// `"intake"` / `"investigation"` / `"development"`. Absent → unchanged.
    #[serde(default)]
    pub(crate) viewed_phase: Option<String>,
    #[serde(default)]
    pub(crate) intake_finished: Option<bool>,
    #[serde(default)]
    pub(crate) investigation_finished: Option<bool>,
    #[serde(default)]
    pub(crate) development_finished: Option<bool>,
    #[serde(default)]
    pub(crate) done: Option<bool>,
}

/// `POST /api/uow/:story_id/meta` body `{ viewed_phase?, intake_finished?, … }` — patch the
/// 3-phase cockpit meta (viewed phase + per-phase finished flags + done/archived). Each
/// field is optional; absent fields are left unchanged (3-phase doc §2 / §7). An unknown
/// `viewed_phase` string is ignored (treated as absent) so a typo never wedges the meta.
/// Returns the updated UoW.
pub(crate) async fn uow_set_meta(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    Json(req): Json<UowMetaReq>,
) -> Json<crate::uow::UnitOfWork> {
    let viewed_phase = req
        .viewed_phase
        .as_deref()
        .and_then(crate::uow::PhaseTab::from_wire);
    Json(state.uow.set_meta(
        &story_id,
        viewed_phase,
        req.intake_finished,
        req.investigation_finished,
        req.development_finished,
        req.done,
    ))
}

// ── Project-aware chat grounding (#54) ───────────────────────────────────────
