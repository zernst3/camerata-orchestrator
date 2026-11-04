//! Unit-of-work and work-item handlers: listing, authoring, publishing, attachments,
//! diagrams, mockups and design drafting.

use crate::*;

/// All known UoWs across every story.
pub(crate) async fn uow_list(State(state): State<AppState>) -> Json<Vec<crate::uow::UnitOfWork>> {
    let Some(p) = state.projects.active() else {
        return Json(vec![]);
    };
    Json(state.uow.list_for_project(&p.id, &p.repos))
}

/// The UoW for a story. ROUTES-8: a READ must not create — an unknown/typo'd id returns a
/// transient default UoW (story_id set) that is NOT persisted, instead of materializing a junk
/// record that then leaked into every list view. WRITE handlers still `get_or_create`.
pub(crate) async fn uow_get(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
) -> Json<crate::uow::UnitOfWork> {
    Json(state.uow.get_or_default(&story_id))
}

// ── Provider-agnostic WorkItem + UoW layer (governed-dev surface) ──────────────

/// `POST /api/workitems/pull` — pull ALL open issues across ALL the ACTIVE project's
/// repos via the GitHub adapter, normalized to [`WorkItem`] (each carrying its repo).
/// Manual (user-triggered), no cache. Returns `{ items: WorkItem[] }`.
///
/// Degrades gracefully: with no token, or no active project, or no repos, returns an
/// empty item list (never an error) so the UI can render a "Connect GitHub / add a
/// repo" hint. A per-repo fetch failure is skipped (the union of the repos that DID
/// resolve is returned) rather than failing the whole pull.
pub(crate) async fn workitems_pull(State(state): State<AppState>) -> Json<serde_json::Value> {
    let Some(token) = state.github_token() else {
        return Json(serde_json::json!({
            "items": [],
            "message": "Connect GitHub to pull work items.",
        }));
    };
    let Some(project) = state.projects.active() else {
        return Json(serde_json::json!({
            "items": [],
            "message": "No active project. Create or select one to pull work items.",
        }));
    };
    let mut items: Vec<crate::workitems::WorkItem> = Vec::new();
    for repo in &project.repos {
        // Prefer the GraphQL path: it carries the sub-issue `parent` linkage the REST
        // issues-list response omits (root cause of every issue showing "(no parent)").
        // FALL BACK to the parent-less REST list on any GraphQL failure (auth scope, API
        // error, network) so the pull still works — just without parent grouping.
        let fetched = match crate::github_issues::list_open_issues_with_parents(repo, &token).await
        {
            Ok(issues) => Ok(issues),
            Err(_) => crate::github_issues::list_open_issues(repo, &token).await,
        };
        match fetched {
            Ok(issues) => {
                for issue in issues {
                    // The list path returns IssueSummary (no state/labels); open issues
                    // are by definition "open" with no camerata labels needed for the
                    // pull view. Map straight onto a WorkItem with the repo set.
                    items.push(crate::workitems::WorkItem {
                        id: crate::workitems::WorkItem::github_id(repo, issue.number),
                        provider: "github".to_string(),
                        repo: repo.clone(),
                        number: issue.number,
                        title: issue.title,
                        body: issue.body,
                        state: "open".to_string(),
                        url: issue.url,
                        labels: Vec::new(),
                        parent_number: issue.parent_number,
                        // The bulk list path (IssueSummary) does not carry assignees or the
                        // updated-at timestamp; they populate on a single-issue Pull latest.
                        assignees: Vec::new(),
                        updated_at: String::new(),
                    });
                }
            }
            // Skip a repo that fails (bad name, 404, rate limit) — the union of the
            // repos that resolved is still useful; the architect sees what loaded.
            Err(_) => continue,
        }
    }
    Json(serde_json::json!({ "items": items }))
}

/// `POST /api/workitems/refresh` body `{ work_item_id }` — re-pull ONE work item from
/// its source (GitHub), returning `{ item: WorkItem }`. Needs the token.
#[derive(serde::Deserialize)]
pub(crate) struct WorkItemRefreshReq {
    pub(crate) work_item_id: String,
}

pub(crate) async fn workitems_refresh(
    State(state): State<AppState>,
    Json(req): Json<WorkItemRefreshReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    let token = state.github_token().ok_or_else(|| {
        AppError(anyhow::anyhow!(
            "no GitHub token — set CAMERATA_GITHUB_TOKEN"
        ))
    })?;
    let (repo, number) = parse_github_work_item_id(&req.work_item_id)?;
    let detail = crate::github_issues::get_issue_detail(&repo, number, &token)
        .await
        .map_err(AppError)?;
    let item = crate::workitems::WorkItem::from_github_issue(&repo, &detail);
    Ok(Json(serde_json::json!({ "item": item })))
}

/// `POST /api/workitems/comment` body `{ work_item_id, body }` — comment back onto the
/// source issue (GitHub) via the adapter / sync path. Returns `{ ok, url }`. Needs the
/// token. The `url` is the created comment's html_url.
#[derive(serde::Deserialize)]
pub(crate) struct WorkItemCommentReq {
    pub(crate) work_item_id: String,
    pub(crate) body: String,
}

pub(crate) async fn workitems_comment(
    State(state): State<AppState>,
    Json(req): Json<WorkItemCommentReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    let token = state.github_token().ok_or_else(|| {
        AppError(anyhow::anyhow!(
            "no GitHub token — set CAMERATA_GITHUB_TOKEN"
        ))
    })?;
    if req.body.trim().is_empty() {
        return Err(AppError::bad_request(anyhow::anyhow!(
            "comment body must not be empty"
        )));
    }
    let (repo, number) = parse_github_work_item_id(&req.work_item_id)?;
    let url = crate::github_issues::comment_on_issue(&repo, number, &req.body, &token)
        .await
        .map_err(AppError)?;
    Ok(Json(serde_json::json!({ "ok": true, "url": url })))
}

/// `POST /api/workitems/comments` body `{ work_item_id }` — read the COMMENTS on the
/// source issue (GitHub), returning `{ comments: IssueComment[] }` oldest-first.
///
/// Degrades gracefully (mirroring the pull path): with no token, or a malformed id, or
/// a fetch failure, returns an EMPTY comment list (never an error) so the UoW modal can
/// render "No comments." instead of breaking.
#[derive(serde::Deserialize)]
pub(crate) struct WorkItemCommentsReq {
    pub(crate) work_item_id: String,
}

pub(crate) async fn workitems_comments(
    State(state): State<AppState>,
    Json(req): Json<WorkItemCommentsReq>,
) -> Json<serde_json::Value> {
    let Some(token) = state.github_token() else {
        return Json(serde_json::json!({ "comments": [] }));
    };
    let Ok((repo, number)) = parse_github_work_item_id(&req.work_item_id) else {
        return Json(serde_json::json!({ "comments": [] }));
    };
    match crate::github_issues::get_issue_comments(&repo, number, &token).await {
        Ok(comments) => Json(serde_json::json!({ "comments": comments })),
        Err(_) => Json(serde_json::json!({ "comments": [] })),
    }
}

/// `POST /api/workitems/assignees` body `{ work_item_id }` — read the ASSIGNABLE users
/// for the work item's repo (the practical @-mention set), returning `{ users: [login] }`.
///
/// Degrades gracefully: with no token, or a malformed id, or a fetch failure, returns an
/// EMPTY user list (never an error) so the comment box's @-autocomplete simply shows no
/// suggestions instead of breaking.
#[derive(serde::Deserialize)]
pub(crate) struct WorkItemAssigneesReq {
    pub(crate) work_item_id: String,
}

pub(crate) async fn workitems_assignees(
    State(state): State<AppState>,
    Json(req): Json<WorkItemAssigneesReq>,
) -> Json<serde_json::Value> {
    let Some(token) = state.github_token() else {
        return Json(serde_json::json!({ "users": [] }));
    };
    let Ok((repo, _number)) = parse_github_work_item_id(&req.work_item_id) else {
        return Json(serde_json::json!({ "users": [] }));
    };
    match crate::github_issues::get_assignees(&repo, &token).await {
        Ok(users) => Json(serde_json::json!({ "users": users })),
        Err(_) => Json(serde_json::json!({ "users": [] })),
    }
}

/// `GET /api/me` returns `{ login }`, the authenticated GitHub user for the configured token.
/// `login` is `null` when there is no token or the lookup fails (the UI hides/disables
/// "Assign to me" in that case). The login is resolved once and cached in `AppState`.
pub(crate) async fn me(State(state): State<AppState>) -> Json<serde_json::Value> {
    let login = state.github_login().await;
    Json(serde_json::json!({ "login": login }))
}

/// `POST /api/workitems/assign` body `{ work_item_id, assignee }`: add `assignee` (a
/// login; the UI passes the current user's login for "assign to me") to the work item's
/// source issue via GitHub, returning `{ ok, assignees, updated_at }` (the UPDATED
/// assignee logins, and the issue's `updated_at` from the same response so the UI can
/// re-baseline its change-poll last-seen value at assign time). Needs the token.
#[derive(serde::Deserialize)]
pub(crate) struct WorkItemAssignReq {
    pub(crate) work_item_id: String,
    pub(crate) assignee: String,
}

pub(crate) async fn workitems_assign(
    State(state): State<AppState>,
    Json(req): Json<WorkItemAssignReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    let token = state.github_token().ok_or_else(|| {
        AppError(anyhow::anyhow!(
            "no GitHub token — set CAMERATA_GITHUB_TOKEN"
        ))
    })?;
    if req.assignee.trim().is_empty() {
        return Err(AppError(anyhow::anyhow!("assignee must not be empty")));
    }
    let (repo, number) = parse_github_work_item_id(&req.work_item_id)?;
    let outcome =
        crate::github_issues::add_assignee_to_issue(&repo, number, req.assignee.trim(), &token)
            .await
            .map_err(AppError)?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "assignees": outcome.assignees,
        "updated_at": outcome.updated_at,
    })))
}

/// `POST /api/workitems/updated-check` body `{ items: [{ work_item_id, repo, number }] }`
/// A cheap background "has anything changed?" probe. Returns
/// `{ updates: [{ work_item_id, updated_at, state }] }`.
///
/// Efficiency: the requested items are grouped by repo and answered with ONE list call
/// per repo (`state=all&sort=updated&direction=desc&per_page=100`), NOT N single-issue
/// GETs, to mind GitHub's rate limits. An item whose number is not in its repo's
/// most-recent 100 is simply omitted (the poller keeps its prior last-seen). `repo` /
/// `number` are read from the body when present, else parsed from the `work_item_id`.
///
/// Degrades gracefully: with no token (or on any per-repo fetch failure) the affected
/// items are omitted; the endpoint never errors, so a failed poll is silent.
#[derive(serde::Deserialize)]
pub(crate) struct UpdatedCheckItem {
    pub(crate) work_item_id: String,
    #[serde(default)]
    pub(crate) repo: Option<String>,
    #[serde(default)]
    pub(crate) number: Option<u64>,
}

#[derive(serde::Deserialize)]
pub(crate) struct UpdatedCheckReq {
    #[serde(default)]
    pub(crate) items: Vec<UpdatedCheckItem>,
}

pub(crate) async fn workitems_updated_check(
    State(state): State<AppState>,
    Json(req): Json<UpdatedCheckReq>,
) -> Json<serde_json::Value> {
    let Some(token) = state.github_token() else {
        return Json(serde_json::json!({ "updates": [] }));
    };

    // Resolve each requested item to (work_item_id, repo, number). Prefer the body's
    // repo/number; fall back to parsing the work_item_id. Skip anything unresolvable.
    struct Resolved {
        work_item_id: String,
        repo: String,
        number: u64,
    }
    let mut resolved: Vec<Resolved> = Vec::new();
    for it in &req.items {
        let (repo, number) = match (it.repo.clone(), it.number) {
            (Some(r), Some(n)) if camerata_worktracker::RepoCoord::parse(&r).is_some() => (r, n),
            _ => match parse_github_work_item_id(&it.work_item_id) {
                Ok(pair) => pair,
                Err(_) => continue,
            },
        };
        resolved.push(Resolved {
            work_item_id: it.work_item_id.clone(),
            repo,
            number,
        });
    }

    // Group the requested numbers by repo so each repo is fetched exactly once.
    let mut by_repo: std::collections::HashMap<String, Vec<(String, u64)>> =
        std::collections::HashMap::new();
    for r in resolved {
        by_repo
            .entry(r.repo)
            .or_default()
            .push((r.work_item_id, r.number));
    }

    let mut updates: Vec<serde_json::Value> = Vec::new();
    for (repo, wanted) in by_repo {
        // ONE list call per repo. On failure, skip the whole repo (silent per contract).
        let rows = match crate::github_issues::list_issue_update_rows(&repo, None, &token).await {
            Ok(rows) => rows,
            Err(_) => continue,
        };
        let by_number: std::collections::HashMap<u64, &crate::github_issues::IssueUpdateRow> =
            rows.iter().map(|r| (r.number, r)).collect();
        for (work_item_id, number) in wanted {
            if let Some(row) = by_number.get(&number) {
                updates.push(serde_json::json!({
                    "work_item_id": work_item_id,
                    "updated_at": row.updated_at,
                    "state": row.state,
                }));
            }
        }
    }

    Json(serde_json::json!({ "updates": updates }))
}

/// `POST /api/workitems/set-parent` body `{ work_item_id, parent_number }` — make an
/// existing work item (the CHILD) a native GitHub sub-issue of `parent_number` (in the
/// child's own repo). This is the "Set parent" affordance from the work-item modal: it
/// fixes the orphaned-issue case where a pulled issue has no parent linkage yet.
///
/// `parent_number` accepts a JSON number or string (`42` or `"42"`/`"#42"`); it is
/// normalized to a bare number. Resolves the child's GitHub database id (the large `id`
/// the sub-issue API requires) from its per-repo number, then links it under the parent.
///
/// Returns `{ ok: true }` on success, `{ ok: false, message }` on any failure (no token,
/// malformed id, bad parent number, GitHub error). Never panics.
#[derive(serde::Deserialize)]
pub(crate) struct WorkItemSetParentReq {
    pub(crate) work_item_id: String,
    /// The parent issue number — a JSON number or a string (`"#42"` is accepted).
    pub(crate) parent_number: serde_json::Value,
}

pub(crate) async fn workitems_set_parent(
    State(state): State<AppState>,
    Json(req): Json<WorkItemSetParentReq>,
) -> Json<serde_json::Value> {
    // Normalize parent_number from either a JSON number or a string.
    let parent_raw = match &req.parent_number {
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => s.clone(),
        other => {
            return Json(serde_json::json!({
                "ok": false,
                "message": format!("parent_number must be a number or string, got `{other}`"),
            }));
        }
    };
    let Some(parent_str) = crate::github_issues::normalize_parent_number(&parent_raw) else {
        return Json(serde_json::json!({
            "ok": false,
            "message": format!("`{parent_raw}` is not a valid issue number"),
        }));
    };
    let parent_number: u64 = match parent_str.parse() {
        Ok(n) => n,
        Err(_) => {
            return Json(serde_json::json!({
                "ok": false,
                "message": format!("`{parent_str}` is not a valid issue number"),
            }));
        }
    };

    let Some(token) = state.github_token() else {
        return Json(serde_json::json!({
            "ok": false,
            "message": "Connect GitHub (set CAMERATA_GITHUB_TOKEN) to set a parent.",
        }));
    };

    let (repo, number) = match parse_github_work_item_id(&req.work_item_id) {
        Ok(pair) => pair,
        Err(e) => {
            return Json(serde_json::json!({
                "ok": false,
                "message": e.err.to_string(),
            }));
        }
    };
    let Some(coord) = camerata_worktracker::RepoCoord::parse(&repo) else {
        return Json(serde_json::json!({
            "ok": false,
            "message": format!("work item repo is not `owner/repo`: `{repo}`"),
        }));
    };

    // Resolve the CHILD issue's GitHub database id (distinct from its per-repo number),
    // which the sub-issue link endpoint requires as `sub_issue_id`.
    let child_db_id =
        match crate::github_issues::fetch_issue_db_id(&coord.owner, &coord.repo, number, &token)
            .await
        {
            Ok(id) => id,
            Err(e) => {
                return Json(serde_json::json!({
                    "ok": false,
                    "message": format!("could not resolve issue #{number}: {e}"),
                }));
            }
        };

    match crate::github_issues::link_sub_issue(
        &coord.owner,
        &coord.repo,
        parent_number,
        child_db_id,
        &token,
    )
    .await
    {
        Ok(()) => Json(serde_json::json!({ "ok": true })),
        Err(e) => Json(serde_json::json!({
            "ok": false,
            "message": format!("could not set parent #{parent_number}: {e}"),
        })),
    }
}

/// Parse a GitHub work-item id (`github:OWNER/REPO#NUMBER`) into `(repo, number)` through
/// the GitHub adapter ([`crate::workitems::parse_github_work_item_id`]). A 400 when the
/// provider is not `github` or the shape is malformed.
pub(crate) fn parse_github_work_item_id(work_item_id: &str) -> Result<(String, u64), AppError> {
    crate::workitems::parse_github_work_item_id(work_item_id)
        .map_err(|reason| AppError::bad_request(anyhow::anyhow!(reason)))
}

/// A UoW with the WorkItem it references and its lifecycle stage, for `GET /api/uows`.
#[derive(serde::Serialize)]
pub(crate) struct UowView {
    /// The UoW id: an opaque key (`uow-<token>` for a native unit, `OWNER/REPO#123` for a
    /// unit created from a GitHub issue, `draft-<token>` for an AI-authoring draft).
    pub(crate) id: String,
    /// The work item this UoW references, when it maps to one: a GitHub-sourced spine
    /// story, or a native unit's own spine story (provider `native`). `None` for legacy
    /// stories with no tracker and no native number AND for a blank/authoring DRAFT UoW
    /// that has not been published to the board yet.
    pub(crate) work_item: Option<crate::workitems::WorkItem>,
    /// Where the unit is tracked: `native` (Camerata itself, no board) or the tracker.
    pub(crate) provider: camerata_worktracker::Provider,
    /// The repository the unit works in ([`crate::uow::UnitOfWork::repo`]), if any.
    pub(crate) repo: Option<String>,
    /// The lifecycle stage as a snake_case wire string (`intake`, `development`, …).
    pub(crate) stage: String,
    /// `true` when this is a blank/authoring DRAFT UoW (it has an authoring state and no
    /// work item yet). The UI renders the authoring panel instead of the dev controls.
    pub(crate) authoring: bool,
    /// What the unit was created from (`"findings"` for a unit made from an inspection's
    /// findings), so the console list can badge it. `None` for every other unit.
    pub(crate) source_kind: Option<String>,
}

/// `GET /api/uows` — list all Units of Work, each with the WorkItem it references
/// (resolved from the story spine) and its lifecycle stage. A draft UoW's work item is
/// resolved by its explicit `work_item` link (set at publish), falling back to the key.
pub(crate) async fn uows_list(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    let Some(p) = state.projects.active() else {
        return Ok(Json(serde_json::json!({ "uows": [] })));
    };
    let stories = state.stories.list().await.map_err(AppError)?;
    let uows = state.uow.list_for_project(&p.id, &p.repos);
    let views: Vec<UowView> = uows
        .into_iter()
        .map(|u| {
            // A linked draft carries the real work-item story id in `work_item`; a
            // normal UoW's key IS the work-item story id. Resolve against the spine by
            // whichever applies.
            let lookup_id = u.work_item.clone().unwrap_or_else(|| u.story_id.clone());
            let work_item = stories.iter().find(|s| s.id == lookup_id).and_then(|s| {
                match (u.provider(), u.native_number) {
                    (camerata_worktracker::Provider::Native, Some(n)) => Some(
                        crate::workitems::WorkItem::from_native_story(s, u.repo(), n),
                    ),
                    _ => crate::workitems::WorkItem::from_canonical_story(s),
                }
            });
            // A draft is one with an authoring state and no published work item yet.
            let authoring = u.authoring.is_some() && work_item.is_none();
            UowView {
                provider: u.provider(),
                repo: u.repo().map(str::to_string),
                source_kind: u.source.as_ref().map(|s| s.kind_str().to_string()),
                id: u.story_id,
                work_item,
                stage: u.stage.wire_str().to_string(),
                authoring,
            }
        })
        .collect();
    Ok(Json(serde_json::json!({ "uows": views })))
}

/// `POST /api/uow/from-workitem` body `{ work_item_id }` — create a UoW referencing the
/// work item. DEDUP by external ref: if a UoW already exists for that work item, return
/// it with `created=false` (never a duplicate). Returns `{ uow_id, created }`.
///
/// The work item is also ensured on the canonical story spine (idempotent upsert) when
/// it is not already there, so `/api/uows` can resolve the WorkItem back and the
/// governed-dev endpoints find a story to run against. This REPLACES the adopt-issue
/// hack: the UI no longer names a repo + number directly; it pulls work items, then
/// projects one onto a UoW here.
#[derive(serde::Deserialize)]
pub(crate) struct UowFromWorkItemReq {
    pub(crate) work_item_id: String,
}

pub(crate) async fn uow_from_workitem(
    State(state): State<AppState>,
    Json(req): Json<UowFromWorkItemReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (repo, number) = parse_github_work_item_id(&req.work_item_id)?;
    // The UoW key is the spine story id (provider prefix stripped).
    let story_id = crate::workitems::story_id_for(&req.work_item_id);

    // DEDUP by external ref: a UoW already exists for this work item id WITHIN the
    // active project. Scoping to the active project's repos lets two projects adopt
    // the same issue id without colliding. No active project → no scope → no dedup.
    let already = match state.projects.active() {
        Some(p) => state
            .uow
            .list_for_project(&p.id, &p.repos)
            .iter()
            .any(|u| u.story_id == story_id),
        None => false,
    };
    if already {
        return Ok(Json(
            serde_json::json!({ "uow_id": story_id, "created": false }),
        ));
    }

    // Ensure the work item is on the canonical spine so /api/uows resolves it and the
    // governed-dev endpoints have a story to run against. Idempotent upsert: refresh
    // the row from GitHub when a token is configured, else seed a minimal row from the
    // id alone (so a token-free environment still creates a usable UoW).
    let story = match (
        state.stories.get(&story_id).await.map_err(AppError)?,
        state.github_token(),
    ) {
        (Some(existing), _) => existing,
        (None, Some(token)) => {
            match crate::github_issues::get_issue_detail(&repo, number, &token).await {
                Ok(detail) => {
                    crate::github_issues::issue_to_story(&repo, number, &detail.title, &detail.body)
                }
                // Token present but the fetch failed: still create a minimal spine row
                // so the UoW is usable; the architect can refresh it later.
                Err(_) => crate::github_issues::issue_to_story(&repo, number, "", ""),
            }
        }
        (None, None) => crate::github_issues::issue_to_story(&repo, number, "", ""),
    };
    state.stories.upsert(story).await.map_err(AppError)?;

    // Create the UoW (get_or_create materializes it at the default Intake stage).
    let uow = state.uow.get_or_create(&story_id);
    Ok(Json(
        serde_json::json!({ "uow_id": uow.story_id, "created": true }),
    ))
}

// ── AI story authoring from a blank UoW (2026-06-22) ─────────────────────────────

/// Optional body for `POST /api/uow/blank`. An absent body or an empty `{}` is
/// treated identically to `{ "parent_id": null }` (back-compat: callers that POST
/// no body, or the existing `serde_json::json!({})` call in the UI, all work unchanged).
#[derive(serde::Deserialize, Default)]
pub(crate) struct UowBlankReq {
    /// The parent issue identifier (accepts `"42"` or `"#42"`). Stored on the draft
    /// after normalization (strip `#`, keep digits only). `None` → no parent link at publish.
    #[serde(default)]
    pub(crate) parent_id: Option<String>,
}

/// `POST /api/uow/blank` — create a blank DRAFT UoW (no story yet, `work_item = None`,
/// an empty authoring state). Optionally accepts `{ "parent_id": "42" }` to mark the
/// story as a future sub-issue of an existing GitHub issue at publish time. It appears
/// in `/api/uows` as a draft (authoring=true) and is the start of the "author a story
/// with AI" flow. Returns `{ uow_id }`.
pub(crate) async fn uow_blank(
    State(state): State<AppState>,
    body: Option<Json<UowBlankReq>>,
) -> Json<serde_json::Value> {
    let req = body.map(|Json(r)| r).unwrap_or_default();
    // Normalize and validate the parent_id: strip "#", ensure digits only. An invalid
    // or empty string is silently treated as None (no parent) so a typo in the UI
    // doesn't block draft creation.
    let parent_id = req
        .parent_id
        .as_deref()
        .and_then(crate::github_issues::normalize_parent_number);
    // Scope the draft to the active project so it appears in that project's
    // `list_for_project` view (and ONLY that project's) even though it has no resolvable
    // repo yet. If there is no active project the draft is still created, just unscoped.
    let project_id = state.projects.active().map(|p| p.id);
    let uow = state.uow.create_blank_with_parent(parent_id, project_id);
    Json(serde_json::json!({ "uow_id": uow.story_id }))
}

/// Body for `POST /api/uow/:story_id/set-draft-parent`. Optional `parent_id` (accepts
/// `"42"` or `"#42"`); absent / empty / null clears any previously-set parent.
#[derive(serde::Deserialize, Default)]
pub(crate) struct SetDraftParentReq {
    #[serde(default)]
    pub(crate) parent_id: Option<String>,
}

/// `POST /api/uow/:story_id/set-draft-parent` — set or clear the parent issue on a DRAFT
/// UoW. The parent is picked on the authoring screen (not up front in the nav); the value
/// is normalized and stored on the draft, and the publish step later creates the native
/// GitHub sub-issue link. Invalid / empty input clears the parent (a typo never blocks).
/// Returns the updated UoW.
pub(crate) async fn uow_set_draft_parent(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    body: Option<Json<SetDraftParentReq>>,
) -> Json<crate::uow::UnitOfWork> {
    let req = body.map(|Json(r)| r).unwrap_or_default();
    let parent_id = req
        .parent_id
        .as_deref()
        .and_then(crate::github_issues::normalize_parent_number);
    Json(state.uow.set_draft_parent(&story_id, parent_id))
}

#[derive(serde::Deserialize)]
pub(crate) struct UowAuthorReq {
    /// The next message in the clarification chat. The first message is the free-text
    /// requirements; subsequent ones answer the AI's clarifying questions.
    pub(crate) message: String,
    /// Optional per-turn model override (the UI's model selector beside Send). When
    /// present and non-empty it takes precedence over the project's StoryAuthoring step
    /// model; when absent we fall back to the project-configured model (back-compat).
    #[serde(default)]
    pub(crate) model: Option<String>,
}

/// The system prompt that turns the LLM into a story-authoring assistant. It produces a
/// JSON object `{ "title", "body", "reply" }` so the server can update the draft AND show
/// a conversational reply (which may be a clarifying question).
pub(crate) const STORY_AUTHOR_SYSTEM: &str =
    "You are a product-owner assistant that drafts a single \
GitHub-issue-style user story (a title and a markdown body) from a set of requirements and \
an ongoing clarification chat. Keep one cohesive story: a concise imperative title and a \
body with sections like Summary, Acceptance Criteria (a checklist), and Notes as warranted. \
When the requirements are ambiguous or missing key detail, ASK ONE concise clarifying \
question in your reply AND offer the answerer 2-4 concrete options to choose from, each with \
a short benefit/drawback so they can decide quickly (the AskUserQuestion style). Draft the \
best story you can so far. Respond ONLY with a minified JSON object with exactly these keys: \
\"title\" (string), \"body\" (string, markdown), \"reply\" (string: a short conversational \
message to the author, e.g. your clarifying question or a note on what you changed), and \
\"options\" (array, possibly empty: when you are asking a clarifying question, 2-4 objects \
each with \"label\" (a short selectable choice) and \"description\" (its benefit/drawback); \
leave it as an empty array when you are NOT asking a question). Do not wrap the JSON in code \
fences.";

/// Parse the LLM's story-authoring response into `(title, body, reply)`. The model is asked
/// for a JSON object; if it deviates (e.g. wraps in fences or returns prose), we degrade
/// gracefully: strip a fenced block if present, else treat the whole text as the reply and
/// leave the draft unchanged signals (empty strings).
pub(crate) fn parse_author_response(
    raw: &str,
) -> (String, String, String, Vec<crate::clarify::ClarifyOption>) {
    let trimmed = raw.trim();
    // Strip a leading/trailing ```json … ``` fence if the model added one.
    let inner = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .map(|s| s.trim_start())
        .and_then(|s| s.strip_suffix("```"))
        .map(|s| s.trim())
        .unwrap_or(trimmed);
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(inner) {
        let title = v
            .get("title")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let body = v
            .get("body")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let reply = v
            .get("reply")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        // Optional structured options for a clarifying question. Drop any entry missing
        // a label; an absent/empty array means "not asking a structured question" and
        // the loop falls back to the free-text reply.
        let options = v
            .get("options")
            .and_then(|x| x.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|o| {
                        let label = o.get("label").and_then(|x| x.as_str())?.trim().to_string();
                        if label.is_empty() {
                            return None;
                        }
                        let description = o
                            .get("description")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string();
                        Some(crate::clarify::ClarifyOption { label, description })
                    })
                    .collect()
            })
            .unwrap_or_default();
        return (title, body, reply, options);
    }
    // Not JSON: keep the raw text as the conversational reply; leave the draft untouched.
    (
        String::new(),
        String::new(),
        trimmed.to_string(),
        Vec::new(),
    )
}

/// Build the user prompt for the authoring LLM from the prior chat plus the new message.
pub(crate) fn build_author_prompt(
    chat: &[crate::uow::AuthorChatMessage],
    new_message: &str,
) -> String {
    let mut p = String::new();
    if chat.is_empty() {
        p.push_str("Requirements:\n");
        p.push_str(new_message);
    } else {
        p.push_str("Conversation so far:\n");
        for m in chat {
            let who = if m.role == "ai" {
                "Assistant"
            } else {
                "Author"
            };
            p.push_str(&format!("{who}: {}\n", m.text));
        }
        p.push_str("\nNew message from the author:\n");
        p.push_str(new_message);
    }
    p.push_str(
        "\n\nUpdate the story draft and reply. Respond ONLY with the JSON object described \
         in the system prompt.",
    );
    p
}

/// `POST /api/uow/:story_id/author` body `{ message }` — append a turn to a draft UoW's
/// clarification chat, ask the LLM to (re)draft the story, persist, and return the updated
/// `UnitOfWork`. Degrades gracefully with no LLM token (returns a clear note as the AI reply
/// and leaves the draft unchanged) — story authoring is an LLM text-generation assist (no
/// gate, same class as the chat assistant).
pub(crate) async fn uow_author(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    Json(req): Json<UowAuthorReq>,
) -> Result<Json<crate::uow::UnitOfWork>, AppError> {
    let message = req.message.trim().to_string();
    if message.is_empty() {
        return Err(AppError::bad_request(anyhow::anyhow!(
            "message must not be empty"
        )));
    }
    // Snapshot the prior chat + draft so we can preserve the draft if the LLM is off/fails.
    let before = state.uow.get_or_create(&story_id);
    let prior = before.authoring.unwrap_or_default();
    let prompt = build_author_prompt(&prior.chat, &message);

    let llm = state.step_llm().map_err(backend_refused)?;
    // Story authoring is a NON-FLEET step: its model comes from the active project's
    // per-step config (no env/const fallback once a project exists). The project-less edge
    // (no active project) is the only place the DEFAULT_MODEL floor applies. An explicit
    // per-turn override from the UI's model selector wins when supplied.
    let model = match req.model.as_deref().map(str::trim) {
        Some(m) if !m.is_empty() => m.to_string(),
        _ => step_model(&state, crate::project::StepKind::StoryAuthoring),
    };
    // GROUNDING (the invariant): the story-authoring agent is a bare-LLM call — the
    // grounding block is its ONLY window onto the real project. Without it, it behaves
    // like a context-less product owner (e.g. asking where to persist a preference
    // across "logged-in users" for an app that has no auth at all). Prepend the active
    // project's repo + rule digest to the system prompt so it drafts against the ACTUAL
    // codebase. See docs/decisions/2026-06-25_all-agents-grounded-in-repo-and-rules.md.
    let system = match state.project_grounding().await {
        Some(grounding) => format!("{STORY_AUTHOR_SYSTEM}\n\n{grounding}"),
        None => STORY_AUTHOR_SYSTEM.to_string(),
    };
    // ON-DEMAND REPO READ (the core invariant, beyond the digest): when the active project's
    // repo is local, let the story-authoring model SCAN the actual codebase (cwd + --add-dir
    // + read-only built-ins on the CLI backend) instead of relying only on the inlined
    // digest. This is what stops the context-less-product-owner failure (asking about auth
    // for an app that has none): it can read the real dependencies/code. READ-ONLY — no write
    // path is added. Degrades to digest-only on the API backend / when no repo is local.
    let mut request = crate::llm::LlmRequest::new(prompt)
        .with_model(model)
        .with_system(system);
    // MULTI-REPO READ: a project has several repos. Grant the story-author model read across
    // ALL the active project's repo clones (cwd = primary, the rest as read-only `--add-dir`).
    let repo_dirs = state.active_repo_dirs();
    if !repo_dirs.is_empty() {
        request = request.with_repo_read_dirs(repo_dirs);
    }
    let (title, body, reply, options) = match llm.complete(request).await {
        Ok(resp) => {
            let (t, b, r, opts) = parse_author_response(&resp.text);
            // Keep the existing draft when the model returned no usable title/body.
            let title = if t.is_empty() {
                prior.draft_title.clone()
            } else {
                t
            };
            let body = if b.is_empty() {
                prior.draft_body.clone()
            } else {
                b
            };
            let reply = if r.is_empty() {
                "Updated the draft.".to_string()
            } else {
                r
            };
            (title, body, reply, opts)
        }
        Err(e) => {
            // Token-less / LLM-off: don't crash; record a clear note and keep the draft.
            let note = format!(
                "AI drafting is unavailable right now ({}). Your message was saved; configure \
                 a model (the Claude Code CLI, or an Anthropic API key added in the app under \
                 Settings, Chat backend, API, or set as ANTHROPIC_API_KEY) and try again.",
                e
            );
            (
                prior.draft_title.clone(),
                prior.draft_body.clone(),
                note,
                Vec::new(),
            )
        }
    };

    // Structured-clarification upgrade: when the assistant asked a clarifying question
    // AND offered options, emit it as a STRUCTURED clarification keyed to this draft UoW.
    // It then surfaces in the NEEDS YOU queue as a resumable pause point, and is answered
    // via the same AskUserQuestion-style component; the answer feeds back as the next
    // author message. When the model returned no options we fall back to the free-text
    // chat reply only (back-compat), posting nothing structured. Story authoring is an
    // LLM text-generation assist (no gate involved).
    if !options.is_empty() {
        state
            .clarifications
            .post_structured(&story_id, &reply, "you", options, false, true);
    }

    let updated = state
        .uow
        .append_authoring_turn(&story_id, &message, &reply, &title, &body);
    Ok(Json(updated))
}

#[derive(serde::Deserialize)]
pub(crate) struct UowPublishReq {
    /// The target repo (`owner/repo`), one of the active project's repos.
    pub(crate) repo: String,
}

/// `POST /api/uow/:story_id/publish` body `{ repo }` — create a GitHub issue from the
/// drafted title/body, upsert the resulting work item onto the canonical spine, and LINK
/// the draft UoW to it (without re-keying the UoW). When the draft carries a `parent_id`,
/// also creates a native GitHub sub-issue link (child under parent) — FAIL SOFT: if the
/// sub-issue link cannot be created (bad parent number, GitHub error, etc.) the story is
/// still published and a `parent_link_warning` field is included in the response.
/// Returns `{ work_item, uow_id }` (plus optional `parent_link_warning`). Requires a
/// GitHub token; 4xx with a clear reason when the token is absent, the repo is
/// malformed, or the draft has no title.
pub(crate) async fn uow_publish(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    Json(req): Json<UowPublishReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    let coord = camerata_worktracker::RepoCoord::parse(&req.repo).ok_or_else(|| {
        AppError::bad_request(anyhow::anyhow!(
            "repo must be `owner/repo`, got `{}`",
            req.repo
        ))
    })?;
    let token = state.github_token().ok_or_else(|| {
        AppError::bad_request(anyhow::anyhow!(
            "Connect GitHub (set CAMERATA_GITHUB_TOKEN) to publish the story to the board."
        ))
    })?;
    let uow = state.uow.get_or_create(&story_id);
    let authoring = uow.authoring.clone().unwrap_or_default();
    if authoring.draft_title.trim().is_empty() {
        return Err(AppError::bad_request(anyhow::anyhow!(
            "The story has no drafted title yet. Author the story before publishing."
        )));
    }

    // Create the child issue; we need both the html_url (for the number parse below)
    // and the GitHub database id (for the sub-issue link). `create_issue_returning_id`
    // replaces the previous `onboard::create_issue` call — same endpoint, same payload,
    // additionally reads the `id` field from the response.
    // Embed any UoW attachments then the Mermaid diagram (if present) in the issue body.
    let issue_body = embed_attachments(&authoring.draft_body, &uow.attachments);
    let issue_body = embed_diagram(&issue_body, uow.diagram.as_deref());
    let (html_url, child_db_id) = crate::github_issues::create_issue_returning_id(
        &coord.owner,
        &coord.repo,
        &token,
        &authoring.draft_title,
        &issue_body,
    )
    .await
    .map_err(AppError)?;

    // The new issue number is the trailing path segment of the html_url
    // (`https://github.com/owner/repo/issues/<num>`).
    let number: u64 = html_url
        .rsplit('/')
        .next()
        .and_then(|s| s.trim().parse().ok())
        .ok_or_else(|| {
            AppError(anyhow::anyhow!(
                "could not parse the new issue number from `{html_url}`"
            ))
        })?;

    // Build the canonical story for the new issue and upsert it onto the spine so
    // /api/uows resolves the work item and dev runs have a story to run against.
    let story = crate::github_issues::issue_to_story(
        &req.repo,
        number,
        &authoring.draft_title,
        &issue_body,
    );
    let work_item_story_id = story.id.clone();
    state.stories.upsert(story).await.map_err(AppError)?;

    // Link the draft UoW to the work item WITHOUT re-keying it (the work-item ref carries
    // the real owner/repo#num).
    state.uow.link_work_item(&story_id, &work_item_story_id);

    // ── Optional: create a native GitHub sub-issue link (FAIL SOFT) ───────────────
    //
    // If the draft carried a parent_id, attempt to create the parent → child sub-issue
    // relationship in GitHub. Any failure (bad number, GitHub error, permissions) is
    // surfaced as a `parent_link_warning` in the response instead of failing the publish.
    let parent_link_warning: Option<String> = match uow.parent_id.as_deref() {
        None => None,
        Some(raw_parent) => match raw_parent.trim().parse::<u64>() {
            Err(_) => Some(format!(
                "published, but could not link to parent #{raw_parent}: not a valid issue number"
            )),
            Ok(parent_number) => {
                match crate::github_issues::link_sub_issue(
                    &coord.owner,
                    &coord.repo,
                    parent_number,
                    child_db_id,
                    &token,
                )
                .await
                {
                    Ok(()) => None,
                    Err(e) => Some(format!(
                        "published, but could not link to parent #{parent_number}: {e}"
                    )),
                }
            }
        },
    };

    // Resolve the linked work item for the response.
    let work_item = state
        .stories
        .get(&work_item_story_id)
        .await
        .map_err(AppError)?
        .as_ref()
        .and_then(crate::workitems::WorkItem::from_canonical_story);

    Ok(Json(serde_json::json!({
        "uow_id": story_id,
        "work_item": work_item,
        "parent_link_warning": parent_link_warning,
    })))
}

// ── UoW file attachments ──────────────────────────────────────────────────────
//
// A UoW can carry zero or more named file attachments (name + mime + content).
// Attachments are stored inline in the UoW JSON, travel with the project export,
// and are embedded in the published GitHub issue body as collapsed `<details>` blocks.
//
// Decided (2026-07-01): publish-embed uses inline `<details>` in the issue body: the
// content stays searchable and needs no extra GitHub scope. The gist/CDN and
// repo-commit paths were deferred.

/// `GET /api/uow/:story_id/attachments` -- list all attachments on a UoW
/// (name + mime; content is omitted from the list for bandwidth). Returns
/// `{ "attachments": [{ "name", "mime" }] }`.
pub(crate) async fn uow_list_attachments(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
) -> Json<serde_json::Value> {
    // ROUTES-8: READ — must not create. An unknown id yields an empty attachment list.
    let uow = state.uow.get_or_default(&story_id);
    let list: Vec<serde_json::Value> = uow
        .attachments
        .iter()
        .map(|a| serde_json::json!({ "name": a.name, "mime": a.mime }))
        .collect();
    Json(serde_json::json!({ "attachments": list }))
}

#[derive(serde::Deserialize)]
pub(crate) struct AddAttachmentReq {
    /// The filename (e.g. `"mockup.html"`).
    pub(crate) name: String,
    /// MIME type (e.g. `"text/html"`). Defaults to `"text/plain"`.
    #[serde(default = "default_attachment_mime_handler")]
    pub(crate) mime: String,
    /// The file content as a UTF-8 string. Caller must base64-encode binary content.
    pub(crate) content: String,
}

pub(crate) fn default_attachment_mime_handler() -> String {
    "text/plain".to_string()
}

/// `POST /api/uow/:story_id/attachments` -- add or replace a named attachment.
/// Replacing an existing name is idempotent (last-writer-wins). Returns the full UoW.
pub(crate) async fn uow_add_attachment(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    Json(req): Json<AddAttachmentReq>,
) -> Result<Json<crate::uow::UnitOfWork>, AppError> {
    if req.name.trim().is_empty() {
        return Err(AppError::bad_request(anyhow::anyhow!(
            "attachment name must not be empty"
        )));
    }
    let attachment = crate::uow::UowAttachment {
        name: req.name.trim().to_string(),
        mime: req.mime,
        content: req.content,
    };
    Ok(Json(state.uow.add_attachment(&story_id, attachment)))
}

/// `GET /api/uow/:story_id/attachments/:name` -- fetch a single attachment
/// (name + mime + full content). Returns `{ "attachment": { name, mime, content } }`
/// or `{ "ok": false }` when the attachment is not found.
pub(crate) async fn uow_get_attachment(
    State(state): State<AppState>,
    Path((story_id, name)): Path<(String, String)>,
) -> Json<serde_json::Value> {
    // ROUTES-8: READ — must not create. An unknown UoW id has no attachments, so the
    // not-found branch fires (no junk UoW persisted).
    let uow = state.uow.get_or_default(&story_id);
    match uow.attachments.iter().find(|a| a.name == name) {
        Some(a) => Json(serde_json::json!({
            "attachment": { "name": a.name, "mime": a.mime, "content": a.content }
        })),
        None => Json(
            serde_json::json!({ "ok": false, "error": format!("attachment `{name}` not found") }),
        ),
    }
}

/// `DELETE /api/uow/:story_id/attachments/:name` -- remove a named attachment.
/// Idempotent. Returns `{ "ok": true }`.
pub(crate) async fn uow_remove_attachment(
    State(state): State<AppState>,
    Path((story_id, name)): Path<(String, String)>,
) -> Json<serde_json::Value> {
    state.uow.remove_attachment(&story_id, &name);
    Json(serde_json::json!({ "ok": true }))
}

/// Embed any attachments on a UoW into its issue body as collapsed `<details>` blocks.
/// HTML attachments use an HTML code-fence; all others use a code-fence keyed to the
/// file extension. This is the decided low-risk approach (no CDN, no gist dependency):
/// the content lives in the issue body itself, stays searchable, and requires no extra
/// GitHub permissions. (Gist/CDN paths deferred for Zach to confirm.)
pub(crate) fn embed_attachments(body: &str, attachments: &[crate::uow::UowAttachment]) -> String {
    if attachments.is_empty() {
        return body.to_string();
    }
    let mut result = body.to_string();
    result.push_str("\n\n---\n");
    for att in attachments {
        let lang = if att.mime.contains("html") {
            "html"
        } else if att.mime.contains("mermaid") || att.name.ends_with(".mmd") {
            "mermaid"
        } else if att.mime.contains("json") || att.name.ends_with(".json") {
            "json"
        } else {
            "text"
        };
        result.push_str(&format!(
            "\n<details>\n<summary>Attachment: {}</summary>\n\n```{}\n{}\n```\n\n</details>\n",
            att.name, lang, att.content
        ));
    }
    result
}

/// Embed an AI-generated Mermaid diagram into the issue body as a native fenced block.
/// GitHub renders ```mermaid fences natively (since May 2022). Returns body unchanged
/// when diagram is None or empty.
pub(crate) fn embed_diagram(body: &str, diagram: Option<&str>) -> String {
    match diagram {
        Some(d) if !d.trim().is_empty() => {
            format!("{body}\n\n```mermaid\n{}\n```", d.trim())
        }
        _ => body.to_string(),
    }
}

/// `GET /api/uow/:story_id/diagram` -- fetch the stored Mermaid diagram text.
/// Returns `{ "diagram": "<text>" }` or `{ "diagram": null }` when absent.
pub(crate) async fn uow_get_diagram(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
) -> Json<serde_json::Value> {
    // ROUTES-8: READ — must not create. An unknown id yields `{ "diagram": null }`.
    let uow = state.uow.get_or_default(&story_id);
    Json(serde_json::json!({ "diagram": uow.diagram }))
}

pub(crate) const DIAGRAM_AUTHOR_SYSTEM: &str =
    "You are a technical diagram assistant. Given a work \
item's title, body, and any conversation context, generate a concise Mermaid diagram that \
visually represents the work: its components, data flow, state machine, sequence, or class \
relationships — whichever type best fits the content. Respond with ONLY the raw Mermaid \
diagram source text. No fences, no prose, no explanations — just the diagram.";

#[derive(serde::Deserialize)]
pub(crate) struct GenerateDiagramReq {
    /// Optional model override; falls back to the project's StoryAuthoring model.
    #[serde(default)]
    pub(crate) model: Option<String>,
    /// Optional extra instruction from the user (e.g. "focus on the data model").
    #[serde(default)]
    pub(crate) instruction: Option<String>,
}

/// `POST /api/uow/:story_id/diagram` -- generate a Mermaid diagram via AI from the UoW's
/// draft title + body + optional instruction. Stores the result on the UoW and returns
/// the updated `UnitOfWork`. Fail-soft: if the AI call fails, returns an error but does
/// not mutate the stored diagram.
pub(crate) async fn uow_generate_diagram(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    Json(req): Json<GenerateDiagramReq>,
) -> Result<Json<crate::uow::UnitOfWork>, AppError> {
    let uow = state.uow.get_or_create(&story_id);
    let authoring = uow.authoring.clone().unwrap_or_default();

    let title = authoring.draft_title.trim().to_string();
    let body = authoring.draft_body.trim().to_string();
    let instruction = req
        .instruction
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .to_string();

    let prompt = {
        let mut p = String::new();
        if !title.is_empty() {
            p.push_str(&format!("Title: {title}\n\n"));
        }
        if !body.is_empty() {
            p.push_str(&format!("Body:\n{body}\n\n"));
        }
        if !instruction.is_empty() {
            p.push_str(&format!("Extra instruction: {instruction}\n\n"));
        }
        if p.is_empty() {
            p.push_str("Generate a simple placeholder Mermaid diagram.");
        }
        p.push_str("Generate the Mermaid diagram now.");
        p
    };

    let model = match req.model.as_deref().map(str::trim) {
        Some(m) if !m.is_empty() => m.to_string(),
        _ => step_model(&state, crate::project::StepKind::StoryAuthoring),
    };
    let system = match state.project_grounding().await {
        Some(grounding) => format!("{DIAGRAM_AUTHOR_SYSTEM}\n\n{grounding}"),
        None => DIAGRAM_AUTHOR_SYSTEM.to_string(),
    };
    let request = crate::llm::LlmRequest::new(prompt)
        .with_model(model)
        .with_system(system);

    let diagram_text = state
        .step_llm()
        .map_err(backend_refused)?
        .complete(request)
        .await
        .map_err(AppError)?
        .text;

    // Strip any accidental fences the model may have added despite instructions.
    let stripped = strip_mermaid_fence(&diagram_text);
    let updated = state.uow.set_diagram(&story_id, stripped);
    Ok(Json(updated))
}

/// Strip leading/trailing ```mermaid ... ``` fences in case the model disobeys.
pub(crate) fn strip_mermaid_fence(raw: &str) -> String {
    let s = raw.trim();
    if let Some(inner) = s
        .strip_prefix("```mermaid")
        .or_else(|| s.strip_prefix("```"))
    {
        if let Some(stripped) = inner.strip_suffix("```") {
            return stripped.trim().to_string();
        }
    }
    s.to_string()
}

/// `DELETE /api/uow/:story_id/diagram` -- remove the stored diagram. Idempotent.
/// Returns `{ "ok": true }`.
pub(crate) async fn uow_clear_diagram(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
) -> Json<serde_json::Value> {
    state.uow.clear_diagram(&story_id);
    Json(serde_json::json!({ "ok": true }))
}

// ── Mockup window endpoint ────────────────────────────────────────────────────

pub(crate) const MOCKUP_AUTHOR_SYSTEM: &str = "You are a UI mockup assistant. Given a work item's \
context and user instructions, generate a self-contained HTML prototype or wireframe that \
visually illustrates the feature. Respond with ONLY the raw HTML — no markdown, no prose, \
no backticks, no fences. The HTML MUST be fully self-contained: inline CSS (no external \
stylesheets), no external scripts, no network requests. Use modern clean design with a \
dark card-based UI if no specific style is requested.";

#[derive(serde::Deserialize)]
pub(crate) struct MockupReq {
    /// The user's instruction (e.g. "make a login form with email + password").
    pub(crate) message: String,
    /// Optional model override.
    #[serde(default)]
    pub(crate) model: Option<String>,
}

/// Build the grounding prompt for the mockup generator. Pure (no I/O) so it is unit-testable.
///
/// Grounding order: PARENT epic/story context first (so the model reads the broader story),
/// then this node's type, title, body, and original requirements prompt, then the instruction.
/// When `message` is empty a synthesized "represent this work item" instruction is used, so a
/// mockup still generates from the story alone.
pub(crate) fn build_mockup_prompt(
    authoring: &camerata_app_core::uow::AuthoringState,
    node_type: Option<&str>,
    parent: Option<&crate::uow::UnitOfWork>,
    message: &str,
) -> String {
    let mut p = String::new();

    // Parent epic/story context first, so the model reads the broader story before
    // this node's specifics.
    if let Some(parent) = parent {
        if let Some(pa) = parent.authoring.as_ref() {
            let pt = pa.draft_title.trim();
            let pb = pa.draft_body.trim();
            if !pt.is_empty() || !pb.is_empty() {
                let ptype = parent.node_type.as_deref().unwrap_or("Parent").trim();
                let ptype = if ptype.is_empty() { "Parent" } else { ptype };
                p.push_str(&format!("Parent {ptype}: {pt}\n"));
                if !pb.is_empty() {
                    p.push_str(&format!("{pb}\n"));
                }
                p.push('\n');
            }
        }
    }

    if let Some(nt) = node_type.map(str::trim) {
        if !nt.is_empty() {
            p.push_str(&format!("Work item type: {nt}\n\n"));
        }
    }
    if !authoring.draft_title.trim().is_empty() {
        p.push_str(&format!(
            "Work item title: {}\n\n",
            authoring.draft_title.trim()
        ));
    }
    if !authoring.draft_body.trim().is_empty() {
        p.push_str(&format!(
            "Work item body:\n{}\n\n",
            authoring.draft_body.trim()
        ));
    }
    if !authoring.requirements_prompt.trim().is_empty() {
        p.push_str(&format!(
            "Original requirements prompt:\n{}\n\n",
            authoring.requirements_prompt.trim()
        ));
    }
    if message.trim().is_empty() {
        p.push_str(
            "Instruction: Generate an HTML mockup that best represents this work item \
based on its title, body, and parent context.\n\nGenerate the HTML mockup now.",
        );
    } else {
        p.push_str(&format!(
            "Instruction: {}\n\nGenerate the HTML mockup now.",
            message.trim()
        ));
    }
    p
}

/// `POST /api/uow/:story_id/mockup` -- generate a self-contained HTML mockup via AI,
/// save it as the `mockup.html` attachment on the UoW, and return `{ html, uow }`.
/// Grounds the mockup in this node's draft title + body + original requirements prompt
/// AND its parent epic/story context (resolved via `draft_parent_id`). The user's
/// `message` is an optional instruction: when empty, a synthesized "represent this work
/// item" instruction is used, so a mockup still generates from the story alone. Only
/// fails the request outright when the message is empty AND there is no node context at
/// all to ground on. Fail-hard: returns AppError if the AI call fails (caller decides
/// whether to show a toast or retry).
pub(crate) async fn uow_generate_mockup(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    Json(req): Json<MockupReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    let message = req.message.trim().to_string();
    let uow = state.uow.get_or_create(&story_id);
    let authoring = uow.authoring.clone().unwrap_or_default();

    // The new contract: an empty message is allowed AS LONG AS there is usable node
    // context to ground the mockup on (this node's title/body/requirements, or a parent).
    // Only fail when the message is empty AND there is nothing at all to ground on.
    let has_node_context = !authoring.draft_title.trim().is_empty()
        || !authoring.draft_body.trim().is_empty()
        || !authoring.requirements_prompt.trim().is_empty();
    if message.is_empty() && !has_node_context {
        return Err(AppError::bad_request(anyhow::anyhow!(
            "message must not be empty when there is no node context to ground on"
        )));
    }

    // Resolve the PARENT (epic/story) so the mockup is grounded in its context, not just
    // this node's title/body. ROUTES-8: this is a READ for grounding — use the non-creating
    // getter so a stale `draft_parent_id` does not persist a junk parent UoW. Absent parent =
    // no extra grounding (the node's own context still drives the mockup).
    let parent = uow
        .draft_parent_id
        .as_ref()
        .and_then(|pid| state.uow.get(pid));

    let prompt = build_mockup_prompt(
        &authoring,
        uow.node_type.as_deref(),
        parent.as_ref(),
        &message,
    );
    let model = match req.model.as_deref().map(str::trim) {
        Some(m) if !m.is_empty() => m.to_string(),
        _ => step_model(&state, crate::project::StepKind::StoryAuthoring),
    };
    let system = match state.project_grounding().await {
        Some(grounding) => format!("{MOCKUP_AUTHOR_SYSTEM}\n\n{grounding}"),
        None => MOCKUP_AUTHOR_SYSTEM.to_string(),
    };
    let request = crate::llm::LlmRequest::new(prompt)
        .with_model(model)
        .with_system(system);

    let html = state
        .step_llm()
        .map_err(backend_refused)?
        .complete(request)
        .await
        .map_err(AppError)?
        .text;

    // Save as the `mockup.html` attachment (last-writer-wins).
    let updated = state.uow.add_attachment(
        &story_id,
        crate::uow::UowAttachment {
            name: "mockup.html".to_string(),
            mime: "text/html".to_string(),
            content: html.clone(),
        },
    );
    Ok(Json(serde_json::json!({ "html": html, "uow": updated })))
}

// ── Design draft-tree endpoints (Design Page Increment 2 backend) ─────────────
//
// A "design" is a hierarchical draft tree: a root node plus child nodes linked by
// `draft_parent_id`. All nodes are UoWs with `node_type` set. The tree is assembled
// in-memory before a batch top-down publish pushes it to GitHub as a tree of issues
// linked via native sub-issues.
//
// Node types are validated against the active project's `HierarchySchema` (the
// work-type DAG built by the drag-and-drop schema builder). Strict enforcement:
// a child may only be created under a parent when the schema's `relations` list
// contains a `TypeRelation { parent, child }` matching the pair.

/// The system prompt for design-mode authoring. Distinct from `STORY_AUTHOR_SYSTEM`:
/// it asks the AI to draft THIS node's title/body AND propose immediate children
/// whose types are constrained by the allowed-child list injected at call time.
pub(crate) const DESIGN_AUTHOR_SYSTEM: &str =
    "You are a product planning assistant. You help a team \
co-design a hierarchy of work. Given a work item and a conversation, you: (1) draft or refine \
this item's title and body, and (2) propose immediate child items that logically belong under \
it, using ONLY the child types listed under ALLOWED_CHILD_TYPES in the user message. \
Respond ONLY with a minified JSON object with exactly these keys: \
\"title\" (string: this item's title), \
\"body\" (string: markdown body with Summary, Acceptance Criteria, and Notes as appropriate), \
\"reply\" (string: a short conversational message -- either a clarifying question with 2-4 \
options, or a brief note on what you proposed), \
\"proposed_children\" (array: each element has exactly \"node_type\" (string, must be one of \
ALLOWED_CHILD_TYPES), \"title\" (string), and \"body\" (string, markdown)). \
Propose 2-5 children unless the conversation makes a different count obvious. \
Do not wrap the JSON in code fences.";

/// Parse the LLM's design-author response. Extracts `title`, `body`, `reply`, and
/// `proposed_children`. Falls back gracefully when the model deviates from the JSON
/// contract: strips code fences, then treats unparse-able text as the reply.
pub(crate) fn parse_design_author_response(
    raw: &str,
) -> (String, String, String, Vec<crate::uow::ProposedChild>) {
    let trimmed = raw.trim();
    let inner = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .map(|s| s.trim_start())
        .and_then(|s| s.strip_suffix("```"))
        .map(|s| s.trim())
        .unwrap_or(trimmed);
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(inner) {
        let title = v
            .get("title")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let body = v
            .get("body")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let reply = v
            .get("reply")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let children = v
            .get("proposed_children")
            .and_then(|x| x.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|o| {
                        let node_type = o
                            .get("node_type")
                            .and_then(|x| x.as_str())?
                            .trim()
                            .to_string();
                        if node_type.is_empty() {
                            return None;
                        }
                        Some(crate::uow::ProposedChild {
                            node_type,
                            title: o
                                .get("title")
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_string(),
                            body: o
                                .get("body")
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        return (title, body, reply, children);
    }
    (
        String::new(),
        String::new(),
        trimmed.to_string(),
        Vec::new(),
    )
}

/// Validate that `child_type` is an allowed child of `parent_type` in `schema`.
/// Returns `Ok(())` when valid, `Err(reason)` when not.
pub(crate) fn validate_design_type_relation(
    schema: &crate::project::HierarchySchema,
    parent_type: &str,
    child_type: &str,
) -> Result<(), String> {
    let allowed = schema
        .relations
        .iter()
        .any(|r| r.parent == parent_type && r.child == child_type);
    if allowed {
        Ok(())
    } else {
        Err(format!(
            "type `{child_type}` is not an allowed child of `{parent_type}` in this project's \
             hierarchy schema; allowed children of `{parent_type}`: [{}]",
            schema
                .relations
                .iter()
                .filter(|r| r.parent == parent_type)
                .map(|r| r.child.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }
}

#[derive(serde::Deserialize)]
pub(crate) struct DesignBlankReq {
    /// The schema type for the root node. Must be marked `is_design_root: true`
    /// in the project's hierarchy schema. If absent or not a design-root type,
    /// the node is still created (the schema check is a soft guard for this endpoint:
    /// the strict validation lives at `materialize_nodes`).
    #[serde(default)]
    pub(crate) root_type: Option<String>,
    /// When set, the new node is created as a child of this draft node ID.
    /// Allows the "Add child node" flow to properly link the new node to its parent.
    #[serde(default)]
    pub(crate) draft_parent_id: Option<String>,
}

/// `POST /api/designs/blank` -- create a blank root design node (the "design" itself).
/// Returns `{ "design_id": "<draft-token>" }`. The root node is a UoW with
/// `node_type` set to `root_type`, no `draft_parent_id`. Scoped to the active project.
pub(crate) async fn design_blank(
    State(state): State<AppState>,
    body: Option<Json<DesignBlankReq>>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (root_type, draft_parent_id) = body
        .map(|Json(r)| (r.root_type, r.draft_parent_id))
        .unwrap_or((None, None));
    let project_id = state.projects.active().map(|p| p.id);
    let root = state
        .uow
        .create_blank_design(root_type, draft_parent_id, project_id);
    Ok(Json(serde_json::json!({ "design_id": root.story_id })))
}

/// `GET /api/designs/:id/nodes` -- return all nodes in the design tree rooted at
/// `:id` in BFS order (root first). Returns `{ "nodes": [...UnitOfWork...] }`.
pub(crate) async fn design_list_nodes(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Json<serde_json::Value> {
    let nodes = state.uow.list_design_tree(&id);
    Json(serde_json::json!({ "nodes": nodes }))
}

#[derive(serde::Deserialize)]
pub(crate) struct DesignAuthorReq {
    /// The next message in the design clarification chat.
    pub(crate) message: String,
    /// Optional per-turn model override.
    #[serde(default)]
    pub(crate) model: Option<String>,
}

/// `POST /api/designs/:id/author` -- design-mode author turn. Analogous to
/// `uow_author` but uses a design-specific system prompt that asks the AI to
/// propose child nodes (types constrained to what the schema allows under this
/// node's type). Returns the full updated `UnitOfWork` including `proposed_children`.
pub(crate) async fn design_author(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<DesignAuthorReq>,
) -> Result<Json<crate::uow::UnitOfWork>, AppError> {
    let message = req.message.trim().to_string();
    if message.is_empty() {
        return Err(AppError::bad_request(anyhow::anyhow!(
            "message must not be empty"
        )));
    }
    let before = state.uow.get_or_create(&id);
    let prior = before.authoring.clone().unwrap_or_default();

    // Inject schema context: which child types are allowed under this node's type.
    let node_type = before.node_type.clone().unwrap_or_default();
    let (schema_context, allowed_child_types) = {
        let schema = state.effective_hierarchy_schema();
        let allowed: Vec<String> = schema
            .relations
            .iter()
            .filter(|r| r.parent == node_type)
            .map(|r| r.child.clone())
            .collect();
        let ctx = if !node_type.is_empty() {
            format!(
                "\nCurrent node type: {node_type}\nALLOWED_CHILD_TYPES: [{}]",
                allowed.join(", ")
            )
        } else {
            String::new()
        };
        (ctx, allowed)
    };

    let prompt = {
        let mut p = build_author_prompt(&prior.chat, &message);
        p.push_str(&schema_context);
        p
    };

    let model = match req.model.as_deref().map(str::trim) {
        Some(m) if !m.is_empty() => m.to_string(),
        _ => step_model(&state, crate::project::StepKind::StoryAuthoring),
    };
    let system = match state.project_grounding().await {
        Some(grounding) => format!("{DESIGN_AUTHOR_SYSTEM}\n\n{grounding}"),
        None => DESIGN_AUTHOR_SYSTEM.to_string(),
    };
    let mut request = crate::llm::LlmRequest::new(prompt)
        .with_model(model)
        .with_system(system);
    let repo_dirs = state.active_repo_dirs();
    if !repo_dirs.is_empty() {
        request = request.with_repo_read_dirs(repo_dirs);
    }
    let design_llm = state.step_llm().map_err(backend_refused)?;
    let (title, body, reply, mut proposed) = match design_llm.complete(request).await {
        Ok(resp) => {
            let (t, b, r, children) = parse_design_author_response(&resp.text);
            let title = if t.is_empty() {
                prior.draft_title.clone()
            } else {
                t
            };
            let body = if b.is_empty() {
                prior.draft_body.clone()
            } else {
                b
            };
            let reply = if r.is_empty() {
                "Updated the draft.".to_string()
            } else {
                r
            };
            (title, body, reply, children)
        }
        Err(e) => {
            let note = format!(
                "AI drafting is unavailable right now ({}). Your message was saved; \
                 configure a model (the Claude Code CLI, or an Anthropic API key added in the app \
                 under Settings, Chat backend, API, or set as ANTHROPIC_API_KEY) and try again.",
                e
            );
            (
                prior.draft_title.clone(),
                prior.draft_body.clone(),
                note,
                Vec::new(),
            )
        }
    };

    // Partition the proposed children into KEPT (node_type allowed under this node's
    // type) and DROPPED (schema-invalid). Both are persisted so the Design Canvas can
    // render the drop as a visible outcome instead of silently showing nothing. When no
    // child types are allowed at all (leaf node type, or unknown type) nothing is
    // dropped as invalid here; the empty-proposed leaf/atomic case is surfaced in the UI.
    let dropped: Vec<crate::uow::ProposedChild> = if allowed_child_types.is_empty() {
        Vec::new()
    } else {
        let (kept, dropped): (Vec<_>, Vec<_>) = proposed
            .drain(..)
            .partition(|c| allowed_child_types.contains(&c.node_type));
        proposed = kept;
        dropped
    };

    state
        .uow
        .append_authoring_turn(&id, &message, &reply, &title, &body);
    let updated = state
        .uow
        .set_proposed_and_dropped_children(&id, proposed, dropped);
    Ok(Json(updated))
}
