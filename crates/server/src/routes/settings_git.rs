//! Settings, credential, checkout and git handlers.

use crate::*;

/// The `GET /api/settings` response: the persisted [`crate::settings::Settings`] fields, plus
/// whether an Anthropic API key is present (for the api-backend warning).
///
/// `chat_backend` (`docs/design/2026-09-22_per-project-backend.md`) is the GLOBAL backend
/// used ONLY by the project-less chat assistant — it has no bearing on any project's own
/// scan/dev-loop backend, which lives on `Project.backend` instead (see
/// `GET`/the per-project `backend` field on each project response).
#[derive(serde::Serialize)]
pub(crate) struct SettingsResp {
    pub(crate) workspace_root: Option<String>,
    pub(crate) repo_paths: std::collections::HashMap<String, String>,
    pub(crate) chat_model: Option<String>,
    /// The global chat assistant's backend (`"cli"` | `"api"`). This is the value the
    /// Settings UI reads and toggles; it affects ONLY the chatbox, never a project.
    pub(crate) chat_backend: crate::llm::ProjectBackend,
    /// Whether an Anthropic API key is available — in the keychain (the
    /// `anthropic_api_key` credential) or the `ANTHROPIC_API_KEY` env var. See
    /// [`anthropic_api_key_present`].
    pub(crate) api_key_present: bool,
    /// OpenRouter provider-safety policy (Pass 2 §3): the current `safe_mode` + pinned
    /// provider. Read by the credentials-panel toggle and the provider picker (both need
    /// to know the current posture to render correctly — e.g. greying unsafe rows).
    pub(crate) safe_mode: bool,
    pub(crate) pinned_provider: Option<String>,
}

/// The current app settings (incl. the workspace root), the global chat backend, and
/// whether an Anthropic API key is present.
pub(crate) async fn get_settings(State(state): State<AppState>) -> Json<SettingsResp> {
    let settings = state.settings.get();
    Json(SettingsResp {
        workspace_root: settings.workspace_root,
        repo_paths: settings.repo_paths,
        chat_model: settings.chat_model,
        chat_backend: settings.chat_backend,
        api_key_present: anthropic_api_key_present(state.credential_store.as_ref()),
        safe_mode: settings.provider_policy.safe_mode,
        pinned_provider: settings.provider_policy.pinned_provider,
    })
}

#[derive(serde::Deserialize)]
pub(crate) struct WorkspaceRootReq {
    pub(crate) path: Option<String>,
}

/// Set the workspace root (the visible folder where repos are cloned).
pub(crate) async fn set_workspace_root(
    State(state): State<AppState>,
    Json(req): Json<WorkspaceRootReq>,
) -> Json<crate::settings::Settings> {
    Json(state.settings.set_workspace_root(req.path))
}

#[derive(serde::Deserialize)]
pub(crate) struct ChatModelReq {
    pub(crate) model: Option<String>,
}

/// Set (or clear) the APP-LEVEL chat assistant model. The chat is a global, cross-project
/// assistant, so its model lives in app settings — not the per-project ResearchChat step.
pub(crate) async fn set_chat_model(
    State(state): State<AppState>,
    Json(req): Json<ChatModelReq>,
) -> Json<crate::settings::Settings> {
    Json(state.settings.set_chat_model(req.model))
}

#[derive(serde::Deserialize)]
pub(crate) struct ChatBackendReq {
    pub(crate) backend: String,
}

/// `POST /api/settings/chat-backend` — set the GLOBAL CHAT backend (`"cli"` | `"api"`), used
/// ONLY by the project-less chatbox assistant (`docs/design/2026-09-22_per-project-backend.md`).
/// Persists the choice to the settings store, which is the single source of truth the chat
/// path reads at call time (via [`AppState::settings`]). Rejects any value other than
/// `"cli"`/`"api"` with 400. Returns the stored backend plus `api_key_present` so the UI can
/// immediately warn when `api` is selected with no Anthropic key.
///
/// Never touches process env: the effective backend is read straight from the persisted
/// settings store, so a runtime switch takes effect immediately without a restart AND
/// without the POSIX `set_var`/`getenv` race a prior env-hydrated design had.
pub(crate) async fn set_chat_backend_handler(
    State(state): State<AppState>,
    Json(req): Json<ChatBackendReq>,
) -> impl IntoResponse {
    let backend = match req.backend.trim().to_ascii_lowercase().as_str() {
        "cli" => crate::llm::ProjectBackend::Cli,
        "api" => crate::llm::ProjectBackend::Api,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "ok": false,
                    "message": format!("invalid backend: {:?} (expected \"cli\" or \"api\")", req.backend)
                })),
            );
        }
    };
    state.settings.set_chat_backend(backend);
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "backend": backend,
            "api_key_present": anthropic_api_key_present(state.credential_store.as_ref()),
        })),
    )
}

// ── Credential manager endpoints ──────────────────────────────────────────────

/// Request body for `POST /api/credentials/:name`.
#[derive(serde::Deserialize)]
pub(crate) struct SetCredentialReq {
    pub(crate) value: String,
}

/// One credential's status as returned by `GET /api/credentials`.
///
/// The `masked` field is the ONLY value ever placed in an HTTP response.
/// The full secret is never serialised or logged.
#[derive(serde::Serialize)]
pub(crate) struct CredentialStatus {
    pub(crate) name: String,
    pub(crate) is_set: bool,
    pub(crate) masked: Option<String>,
}

/// `POST /api/credentials/:name` — store a credential in the OS keychain.
///
/// Body: `{ "value": "…" }`
/// Response: `{ "ok": true, "masked": "sk-o••••" }`
///
/// The full value is written to the keychain and immediately discarded from
/// memory; the response contains only the masked form.
pub(crate) async fn set_credential(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(req): Json<SetCredentialReq>,
) -> impl IntoResponse {
    use crate::credentials::ALL_CREDENTIALS;
    // Reject unknown credential names to prevent arbitrary keychain writes.
    if !ALL_CREDENTIALS.contains(&name.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "ok": false,
                "message": format!("unknown credential name: {name}")
            })),
        );
    }
    // Whitespace-only counts as empty: the resolver treats a blank value as absent, so saving
    // one would show "set" in the UI while no model call could ever use it.
    if req.value.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "ok": false,
                "message": "value must not be empty"
            })),
        );
    }
    if let Err(e) = state.credential_store.set(&name, &req.value) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "ok": false,
                "message": format!("failed to store credential: {e}")
            })),
        );
    }
    // OpenRouter refresh trigger 2 of 3 (see the block above `serve()`): a freshly-saved
    // OpenRouter key should populate the model picker immediately, not after a restart or
    // a manual refresh-endpoint POST. No-op for every other credential name.
    trigger_openrouter_refresh_if_needed(
        &name,
        &state.model_registry,
        state.credential_store.clone(),
    );
    // Live Anthropic model-registry refresh (Feature A), trigger 2 of 3 (credential-save):
    // a freshly-saved Anthropic key should populate the LIVE `/v1/models` picker list
    // immediately, not after a restart or a manual refresh-endpoint POST. No-op for every
    // other credential name.
    trigger_anthropic_refresh_if_needed(
        &name,
        &state.model_registry,
        state.credential_store.clone(),
    );
    // The GitHub provider carries its token: re-select it so a token saved now is used now.
    reselect_provider_if_github(&name, &state);
    // ROUTES-9: this handler used to `std::env::set_var("ANTHROPIC_API_KEY", ...)` so a
    // freshly-saved key took effect for the `api` backend without a restart. That mutated
    // process-global env from a request-handler thread while worker threads read the same var
    // via `getenv` — undefined behaviour on POSIX, and the concurrent writer made the
    // credential env-fallback test flaky. The env write is gone, and nothing needs it:
    // EVERY model client (audit/rescan `Llm`, chat and fleet agent drivers, the live model
    // list, the gate) reads the key from this same store through
    // `credentials::anthropic_api_key` when it is built (env only as the fallback), so a
    // freshly-saved or replaced key takes effect on the next call without a restart AND
    // without touching process env.
    // Return the masked form so the UI can confirm what was stored.
    let masked = state
        .credential_store
        .masked(&name)
        .ok()
        .flatten()
        .unwrap_or_else(|| "••••".to_string());
    (
        StatusCode::OK,
        Json(serde_json::json!({ "ok": true, "masked": masked })),
    )
}

/// The refusal for a model step whose backend setting is `Api` with no key configured: a 409
/// that carries the reason, never a silent fallback to the CLI.
pub(crate) fn backend_refused(message: String) -> AppError {
    AppError::with_status(StatusCode::CONFLICT, anyhow::anyhow!(message))
}

/// Re-select the work-tracker provider when the `github_token` credential changed (saved or
/// removed), so the running app uses the new token with no restart. No-op for every other name.
pub(crate) fn reselect_provider_if_github(name: &str, state: &AppState) {
    if name == crate::credentials::GITHUB_TOKEN {
        state.provider.reselect(state.credential_store.as_ref());
    }
}

/// `GET /api/credentials` — list all known credentials with their set-status and
/// masked values.  The full value is never included.
///
/// Response: `[{ "name": "openrouter_api_key", "is_set": true, "masked": "sk-o••••" }, …]`
pub(crate) async fn list_credentials(State(state): State<AppState>) -> Json<Vec<CredentialStatus>> {
    use crate::credentials::ALL_CREDENTIALS;
    let statuses: Vec<CredentialStatus> = ALL_CREDENTIALS
        .iter()
        .map(|name| {
            let is_set = state.credential_store.is_set(name).unwrap_or(false);
            let masked = state.credential_store.masked(name).ok().flatten();
            CredentialStatus {
                name: name.to_string(),
                is_set,
                masked,
            }
        })
        .collect();
    Json(statuses)
}

/// Read-only checkout status for every repo in a project (no network).
pub(crate) async fn checkout_status(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<crate::client_source::resolution::RepoCheckoutView>>, AppError> {
    let project = state
        .projects
        .get(&id)
        .ok_or_else(|| AppError::not_found(anyhow::anyhow!("project not found: {id}")))?;
    // Override-aware (issue #33/#38): resolve each repo via its per-repo path override first,
    // falling back to `<workspace_root>/<owner>/<repo>`. A repo can resolve via an override even
    // when no workspace root is set, so we do NOT hard-fail on a missing workspace root — repos
    // that resolve via neither report not-cloned with a helpful reason instead. Each repo also
    // carries its source status (present / deleted / unresolved / mismatch).
    Ok(Json(
        crate::client_source::resolution::checkout_views(&state, &project).await,
    ))
}

/// The effective workspace root (fix for the "no workspace folder chosen yet" dead end): the
/// explicit `workspace_root` setting when one is chosen, otherwise the parent folder of the first
/// in-scope repo that already has an explicit local path override (`settings.repo_path`). This lets
/// a project whose repo was linked via "Link to this folder" (but never had a workspace folder
/// picked) still resolve somewhere for "Clone / update all repos" to put its OTHER repos. `None`
/// only when neither is available.
pub(crate) fn effective_workspace_root(
    state: &AppState,
    project: &crate::project::Project,
) -> Option<String> {
    if let Some(root) = state.settings.workspace_root() {
        return Some(root);
    }
    project.repos.iter().find_map(|repo| {
        let path = state.settings.repo_path(repo)?;
        std::path::Path::new(&path)
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.to_string_lossy().into_owned())
    })
}

/// Clone (or fast-forward) every repo in a project into the workspace.
pub(crate) async fn checkout_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<crate::client_source::resolution::RepoCheckoutView>>, AppError> {
    let project = state
        .projects
        .get(&id)
        .ok_or_else(|| AppError::not_found(anyhow::anyhow!("project not found: {id}")))?;
    let Some(root) = effective_workspace_root(&state, &project) else {
        return Err(AppError(anyhow::anyhow!(
            "no workspace folder is set, and no repo has a linked local folder to derive one from. Pick a workspace folder first."
        )));
    };
    let token = state.github_token().unwrap_or_default();
    if token.trim().is_empty() {
        return Err(AppError(anyhow::anyhow!(
            "no GitHub token set. Set CAMERATA_GITHUB_TOKEN to clone."
        )));
    }
    let workspace_root = Some(root.clone());
    let root = std::path::PathBuf::from(&root);
    let mut out = Vec::with_capacity(project.repos.len());
    for repo in &project.repos {
        // A repo whose source the operator DELETED stays deleted: updating the others must not
        // quietly re-clone it as a side effect. "Re-fetch source" is the deliberate way back.
        if let Some(record) =
            crate::client_source::status::deleted_record(&state, &project.id, repo)
        {
            let status =
                crate::client_source::status::source_status(&state, &project.id, repo).await;
            out.push(crate::client_source::resolution::RepoCheckoutView::new(
                crate::workspace::RepoCheckout {
                    repo: repo.clone(),
                    cloned: false,
                    path: record.path.clone(),
                    branch: None,
                    dirty: false,
                    detail: String::new(),
                },
                status,
            ));
            continue;
        }
        // If the repo ALREADY resolves (override-aware git checkout with a matching origin),
        // REFRESH it (pull/fetch its current branch) instead of just re-reporting stale status —
        // "Clone / update all repos" must actually update repos that are already linked/cloned, not
        // just repos it clones fresh. Only repos that do not already resolve get cloned into
        // `<root>/<owner>/<repo>` (issue #38).
        let override_path = state.settings.repo_path(repo);
        let resolution = crate::workspace::repo_resolution(
            override_path.as_deref(),
            workspace_root.as_deref(),
            repo,
        )
        .await;
        let checkout = if resolution.resolved {
            let dir = resolution
                .path
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| crate::workspace::repo_dir(&root, repo));
            crate::workspace::refresh_resolved(&dir, repo, &token).await
        } else {
            crate::workspace::clone_or_pull(&root, repo, &token).await
        };
        let source = crate::client_source::status::source_status(&state, &project.id, repo).await;
        out.push(crate::client_source::resolution::RepoCheckoutView::new(
            checkout, source,
        ));
    }
    Ok(Json(out))
}

#[derive(serde::Deserialize)]
pub(crate) struct BranchReq {
    pub(crate) repo: String,
    pub(crate) branch: String,
}

/// Create (or switch to) a working branch in a project's local clone.
pub(crate) async fn checkout_branch(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<BranchReq>,
) -> Result<Json<crate::workspace::RepoCheckout>, AppError> {
    let project = state
        .projects
        .get(&id)
        .ok_or_else(|| AppError::not_found(anyhow::anyhow!("project not found: {id}")))?;
    // GAP-2 / PROCESS-BRANCH-NAMING-1 chokepoint. HARD-BLOCK a branch name that violates the
    // project's branch-naming rule. Branch-naming is opt-in (disabled by default), so for
    // projects that have not enabled it this gate is a no-op and any name passes.
    if let Err(e) = crate::vcs_choke::gated_branch(&project.process_rule_config, &req.branch) {
        return Err(AppError::bad_request(anyhow::anyhow!(
            "VCS-action gate blocked the branch name `{}`: {e}",
            req.branch
        )));
    }
    let Some(root) = state.settings.workspace_root() else {
        return Err(AppError(anyhow::anyhow!("no workspace folder is set")));
    };
    let root = std::path::PathBuf::from(root);
    crate::workspace::create_branch(&root, &req.repo, &req.branch).await?;
    Ok(Json(
        crate::workspace::checkout_status(&root, &req.repo).await,
    ))
}

#[derive(serde::Deserialize)]
pub(crate) struct ShipReq {
    pub(crate) repo: String,
    pub(crate) branch: String,
    pub(crate) title: String,
    #[serde(default)]
    pub(crate) body: String,
    /// An optional governed-run id (issue #21). When set, that run's provenance
    /// summary is appended to the PR body so the PR carries the honest accounting of
    /// what the gate enforced and bounced. Opening the PR stays an EXPLICIT action.
    #[serde(default)]
    pub(crate) run_id: Option<String>,
}

/// Ship a repo: push its working branch and open a PR. Returns the PR URL. This is the
/// EXPLICIT open-PR action — Camerata never auto-opens PRs. When a `run_id` is supplied,
/// that run's provenance is folded into the PR body (issue #21).
pub(crate) async fn ship_repo(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<ShipReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    let _project = state
        .projects
        .get(&id)
        .ok_or_else(|| AppError::not_found(anyhow::anyhow!("project not found: {id}")))?;
    let Some(root) = state.settings.workspace_root() else {
        return Err(AppError(anyhow::anyhow!("no workspace folder is set")));
    };
    let token = state.github_token().unwrap_or_default();
    if token.trim().is_empty() {
        return Err(AppError(anyhow::anyhow!(
            "no GitHub token — set CAMERATA_GITHUB_TOKEN to push"
        )));
    }
    let body = body_with_provenance(&state, &req.body, req.run_id.as_deref());
    let root = std::path::PathBuf::from(root);
    let url =
        crate::workspace::ship(&req.repo, &req.branch, &req.title, &body, &root, &token).await?;
    Ok(Json(serde_json::json!({ "pr_url": url })))
}

/// Append a run's provenance markdown to a PR body when a run id is supplied and the
/// run exists. Returns the original body unchanged otherwise. Keeps the provenance the
/// architect reviewed visible in the PR itself (issue #21).
pub(crate) fn body_with_provenance(state: &AppState, body: &str, run_id: Option<&str>) -> String {
    let Some(rid) = run_id.filter(|r| !r.trim().is_empty()) else {
        return body.to_string();
    };
    let Some(run) = state.runs.get(rid) else {
        return body.to_string();
    };
    let rules = camerata_gateway::enforced_gate_rules();
    let prov = run_provenance(&run, &rules);
    let block = crate::run::provenance_markdown(&prov);
    if body.trim().is_empty() {
        block
    } else {
        format!("{body}\n\n{block}")
    }
}

// ── Local git controls (issue #37) ───────────────────────────────────────────

/// Query parameters shared by git read endpoints.
#[derive(serde::Deserialize)]
pub(crate) struct GitRepoQuery {
    pub(crate) repo: String,
}

/// Query parameters for the commit log.
#[derive(serde::Deserialize)]
pub(crate) struct GitLogQuery {
    pub(crate) repo: String,
    #[serde(default = "default_log_limit")]
    pub(crate) limit: usize,
}

pub(crate) fn default_log_limit() -> usize {
    50
}

/// Resolve a repo's local dir from project settings, or return an error response.
pub(crate) fn resolve_git_dir(
    state: &AppState,
    repo: &str,
) -> Result<std::path::PathBuf, Json<serde_json::Value>> {
    let override_path = state.settings.repo_path(repo);
    let workspace_root = state.settings.workspace_root();
    crate::workspace::resolve_repo_dir(override_path.as_deref(), workspace_root.as_deref(), repo)
        .ok_or_else(|| {
            Json(serde_json::json!({
                "ok": false,
                "message": "repo not resolved locally — set its path in the Rules view"
            }))
        })
}

/// List local branches + the current HEAD branch for a repo.
pub(crate) async fn git_branches(
    State(state): State<AppState>,
    Path(_id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<GitRepoQuery>,
) -> Json<serde_json::Value> {
    let dir = match resolve_git_dir(&state, &q.repo) {
        Ok(d) => d,
        Err(e) => return e,
    };
    match crate::workspace::list_branches(&dir).await {
        Ok(bl) => {
            Json(serde_json::json!({ "ok": true, "current": bl.current, "branches": bl.branches }))
        }
        Err(e) => Json(serde_json::json!({ "ok": false, "message": format!("{e}") })),
    }
}

/// Full git status for a repo: branch, dirty flag, ahead/behind counts, and
/// a human-readable detail string. Used by the cockpit's per-repo status bar.
/// No network: ahead/behind reflects what was fetched locally.
pub(crate) async fn git_status_endpoint(
    State(state): State<AppState>,
    Path(_id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<GitRepoQuery>,
) -> Json<serde_json::Value> {
    let dir = match resolve_git_dir(&state, &q.repo) {
        Ok(d) => d,
        Err(e) => return e,
    };
    match crate::workspace::git_status(&dir).await {
        Ok(st) => Json(serde_json::json!({
            "ok":     true,
            "branch": st.branch,
            "dirty":  st.dirty,
            "ahead":  st.sync.ahead,
            "behind": st.sync.behind,
            "detail": st.detail,
        })),
        Err(e) => Json(serde_json::json!({ "ok": false, "message": format!("{e}") })),
    }
}

/// Recent commit log for a repo.
pub(crate) async fn git_log(
    State(state): State<AppState>,
    Path(_id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<GitLogQuery>,
) -> Json<serde_json::Value> {
    let dir = match resolve_git_dir(&state, &q.repo) {
        Ok(d) => d,
        Err(e) => return e,
    };
    match crate::workspace::git_log(&dir, q.limit).await {
        Ok(commits) => Json(serde_json::json!({ "ok": true, "commits": commits })),
        Err(e) => Json(serde_json::json!({ "ok": false, "message": format!("{e}") })),
    }
}

#[derive(serde::Deserialize)]
pub(crate) struct GitCheckoutReq {
    pub(crate) repo: String,
    pub(crate) branch: String,
    #[serde(default)]
    pub(crate) create: bool,
}

/// Switch to (or create) a local branch.
pub(crate) async fn git_checkout(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<GitCheckoutReq>,
) -> Json<serde_json::Value> {
    let dir = match resolve_git_dir(&state, &req.repo) {
        Ok(d) => d,
        Err(e) => return e,
    };
    // GAP-2 / PROCESS-BRANCH-NAMING-1 chokepoint: only branch CREATION is gated (a plain switch
    // to an existing branch does not author a name). Opt-in rule, so a no-op by default.
    if req.create {
        let vcs_config = process_rule_config_for(&state, &id);
        if let Err(e) = crate::vcs_choke::gated_branch(&vcs_config, &req.branch) {
            return Json(serde_json::json!({
                "ok": false,
                "message": format!("VCS-action gate blocked the branch name `{}`: {e}", req.branch),
            }));
        }
    }
    let result = if req.create {
        crate::workspace::create_branch_at(&dir, &req.branch).await
    } else {
        crate::workspace::switch_branch(&dir, &req.branch).await
    };
    match result {
        Ok(()) => Json(serde_json::json!({ "ok": true, "branch": req.branch })),
        Err(e) => Json(serde_json::json!({ "ok": false, "message": format!("{e}") })),
    }
}

#[derive(serde::Deserialize)]
pub(crate) struct GitCommitReq {
    pub(crate) repo: String,
    pub(crate) message: String,
}

/// Stage all changes and commit them.
pub(crate) async fn git_commit(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<GitCommitReq>,
) -> Json<serde_json::Value> {
    if req.message.trim().is_empty() {
        return Json(serde_json::json!({ "ok": false, "message": "commit message is required" }));
    }
    // GAP-2 chokepoint: HARD-BLOCK a commit whose message violates the project's process
    // rules before staging anything. No commit is created on a violation.
    let config = process_rule_config_for(&state, &id);
    if let Err(e) = crate::vcs_choke::gated_commit(&config, &req.message) {
        return Json(serde_json::json!({
            "ok": false,
            "blocked": true,
            "message": format!("{e}"),
        }));
    }
    let dir = match resolve_git_dir(&state, &req.repo) {
        Ok(d) => d,
        Err(e) => return e,
    };
    match crate::workspace::commit_all(&dir, &req.message).await {
        Ok(out) => Json(serde_json::json!({ "ok": true, "output": out })),
        Err(e) => Json(serde_json::json!({ "ok": false, "message": format!("{e}") })),
    }
}

#[derive(serde::Deserialize)]
pub(crate) struct GitPushReq {
    pub(crate) repo: String,
    pub(crate) branch: String,
}

/// Push the branch to origin (user-triggered; token required).
pub(crate) async fn git_push(
    State(state): State<AppState>,
    Path(_id): Path<String>,
    Json(req): Json<GitPushReq>,
) -> Json<serde_json::Value> {
    let token = state.github_token().unwrap_or_default();
    if token.trim().is_empty() {
        return Json(
            serde_json::json!({ "ok": false, "message": "no GitHub token — set CAMERATA_GITHUB_TOKEN to push" }),
        );
    }
    let dir = match resolve_git_dir(&state, &req.repo) {
        Ok(d) => d,
        Err(e) => return e,
    };
    match crate::workspace::push_branch(&dir, &req.repo, &req.branch, &token).await {
        Ok(()) => Json(serde_json::json!({ "ok": true })),
        Err(e) => Json(serde_json::json!({ "ok": false, "message": format!("{e}") })),
    }
}

#[derive(serde::Deserialize)]
pub(crate) struct GitPullReq {
    pub(crate) repo: String,
    pub(crate) branch: String,
}

/// Fast-forward pull from origin.
pub(crate) async fn git_pull(
    State(state): State<AppState>,
    Path(_id): Path<String>,
    Json(req): Json<GitPullReq>,
) -> Json<serde_json::Value> {
    let token = state.github_token().unwrap_or_default();
    if token.trim().is_empty() {
        return Json(
            serde_json::json!({ "ok": false, "message": "no GitHub token — set CAMERATA_GITHUB_TOKEN to pull" }),
        );
    }
    let dir = match resolve_git_dir(&state, &req.repo) {
        Ok(d) => d,
        Err(e) => return e,
    };
    match crate::workspace::pull_branch(&dir, &req.repo, &req.branch, &token).await {
        Ok(out) => Json(serde_json::json!({ "ok": true, "output": out })),
        Err(e) => Json(serde_json::json!({ "ok": false, "message": format!("{e}") })),
    }
}

#[derive(serde::Deserialize)]
pub(crate) struct GitFetchReq {
    pub(crate) repo: String,
}

/// Fetch every branch from origin (all remote-tracking refs), no merge/checkout involved.
pub(crate) async fn git_fetch(
    State(state): State<AppState>,
    Path(_id): Path<String>,
    Json(req): Json<GitFetchReq>,
) -> Json<serde_json::Value> {
    let token = state.github_token().unwrap_or_default();
    if token.trim().is_empty() {
        return Json(
            serde_json::json!({ "ok": false, "message": "no GitHub token set. Set CAMERATA_GITHUB_TOKEN to fetch." }),
        );
    }
    let dir = match resolve_git_dir(&state, &req.repo) {
        Ok(d) => d,
        Err(e) => return e,
    };
    match crate::workspace::fetch_all(&dir, &req.repo, &token).await {
        Ok(out) => Json(serde_json::json!({ "ok": true, "output": out })),
        Err(e) => Json(serde_json::json!({ "ok": false, "message": format!("{e}") })),
    }
}

#[derive(serde::Deserialize)]
pub(crate) struct GitCherryPickReq {
    pub(crate) repo: String,
    pub(crate) sha: String,
}

/// Cherry-pick a commit onto the current HEAD branch. On conflict, returns the error
/// message so the UI can display it (the repo stays in conflict state for the user to
/// resolve).
pub(crate) async fn git_cherry_pick(
    State(state): State<AppState>,
    Path(_id): Path<String>,
    Json(req): Json<GitCherryPickReq>,
) -> Json<serde_json::Value> {
    let dir = match resolve_git_dir(&state, &req.repo) {
        Ok(d) => d,
        Err(e) => return e,
    };
    match crate::workspace::cherry_pick(&dir, &req.sha).await {
        Ok(out) => Json(serde_json::json!({ "ok": true, "output": out })),
        Err(e) => Json(serde_json::json!({ "ok": false, "message": format!("{e}") })),
    }
}

// ── Unit of Work handlers (issue #39) ────────────────────────────────────────
