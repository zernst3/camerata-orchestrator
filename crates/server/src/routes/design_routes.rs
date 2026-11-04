//! Design materialize, publish, status and delete handlers.

use crate::*;

#[derive(serde::Deserialize)]
pub(crate) struct MaterializeChildReq {
    /// The schema work type for this node.
    pub(crate) node_type: String,
    /// The initial title.
    #[serde(default)]
    pub(crate) title: String,
    /// The initial body (markdown).
    #[serde(default)]
    pub(crate) body: String,
}

#[derive(serde::Deserialize)]
pub(crate) struct DesignMaterializeReq {
    /// The draft story_id of the PARENT node to hang the new children under.
    /// Must be in the same design tree (has the same root).
    pub(crate) parent_draft_id: String,
    /// The nodes to create.
    pub(crate) nodes: Vec<MaterializeChildReq>,
}

/// `POST /api/designs/:id/nodes` -- materialize child nodes from the proposed list
/// (or from an explicit `nodes` array). Each node's type is validated against the
/// project's `HierarchySchema`: the `parent_draft_id` node's type must have a
/// `TypeRelation` allowing each child type. Returns `{ "node_ids": [...] }`.
pub(crate) async fn design_materialize_nodes(
    State(state): State<AppState>,
    Path(_design_id): Path<String>,
    Json(req): Json<DesignMaterializeReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    // Resolve the parent node.
    let parent = state.uow.get_or_create(&req.parent_draft_id);
    let parent_type = parent.node_type.as_deref().unwrap_or("");

    // Get the effective schema for validation (falls back to the default ladder when the
    // active project has no/empty stored schema, matching design_author).
    let schema = state.effective_hierarchy_schema();

    let project_id = state.projects.active().map(|p| p.id);

    let mut node_ids = Vec::new();
    for node in &req.nodes {
        // Strict schema validation: reject invalid parent-child type pairs.
        if !parent_type.is_empty() && !node.node_type.is_empty() {
            validate_design_type_relation(&schema, parent_type, &node.node_type)
                .map_err(|e| AppError(anyhow::anyhow!("{e}")))?;
        }
        let child = state.uow.create_blank_design(
            Some(node.node_type.clone()),
            Some(req.parent_draft_id.clone()),
            project_id.clone(),
        );
        // Set the initial draft title/body if provided.
        if !node.title.is_empty() || !node.body.is_empty() {
            state.uow.append_authoring_turn(
                &child.story_id,
                "(initial draft)",
                "Draft created.",
                &node.title,
                &node.body,
            );
        }
        node_ids.push(child.story_id);
    }

    // Clear the proposed_children on the parent after materialization.
    state
        .uow
        .set_proposed_children(&req.parent_draft_id, vec![]);

    Ok(Json(serde_json::json!({ "node_ids": node_ids })))
}

/// `DELETE /api/designs/:id/nodes/:node_id` -- remove a draft node and all its
/// descendants. Idempotent. Returns `{ "ok": true, "removed": [...ids...] }`.
pub(crate) async fn design_delete_node(
    State(state): State<AppState>,
    Path((_design_id, node_id)): Path<(String, String)>,
) -> Json<serde_json::Value> {
    let removed = state.uow.remove_design_subtree(&node_id);
    Json(serde_json::json!({ "ok": true, "removed": removed }))
}

#[derive(serde::Deserialize, Default)]
pub(crate) struct DesignPublishReq {
    /// An OPTIONAL explicit fallback override: the `owner/repo` targets used for any node
    /// whose own `publish_repos` is empty. When this is also empty, publish falls back to
    /// the design's PROJECT repos. Per-node `publish_repos` always take precedence over
    /// this fallback.
    #[serde(default)]
    pub(crate) repos: Vec<String>,
}

/// One concrete publish step produced by [`plan_design_publish`]: create the node's
/// issue in `repo`, and (when `link_to_parent`) link it as a sub-issue of its parent's
/// already-published issue IN THAT SAME repo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PublishAction {
    /// The `owner/repo` target this action creates the issue in.
    pub(crate) repo: String,
    /// The design node's draft story_id this action publishes.
    pub(crate) node_story_id: String,
    /// `true` iff this node has a `draft_parent_id` AND that parent's effective repos also
    /// contain `repo` (so a sub-issue link is possible in this repo). `false` for a root, or
    /// for a node whose parent does not live in this repo (standalone publish, not a warning).
    pub(crate) link_to_parent: bool,
}

/// Pure, unit-testable publish planner. GitHub I/O stays OUT of this function.
///
/// For each repo `R` in the union of every node's effective repos, walk `tree` in its
/// given order (BFS: parents precede children) and emit a [`PublishAction`] for every
/// node whose effective repos contain `R`. `link_to_parent` is `true` iff the node has a
/// `draft_parent_id` AND that parent's effective repos ALSO contain `R` — so a child that
/// targets a repo its parent does not live in publishes STANDALONE in that repo (not a
/// warning). Within a repo, a parent's action always precedes its child's (guaranteed by
/// the BFS order of `tree`).
pub(crate) fn plan_design_publish(
    tree: &[crate::uow::UnitOfWork],
    effective: impl Fn(&crate::uow::UnitOfWork) -> Vec<String>,
) -> Vec<PublishAction> {
    // Precompute each node's effective repos + a story_id -> effective-repos map for the
    // parent-membership check.
    let eff: std::collections::HashMap<String, Vec<String>> = tree
        .iter()
        .map(|n| (n.story_id.clone(), effective(n)))
        .collect();

    // The union of every node's effective repos, in first-seen order (stable output).
    let mut repos_ordered: Vec<String> = Vec::new();
    for n in tree {
        for r in eff.get(&n.story_id).into_iter().flatten() {
            if !repos_ordered.contains(r) {
                repos_ordered.push(r.clone());
            }
        }
    }

    let mut actions: Vec<PublishAction> = Vec::new();
    for repo in &repos_ordered {
        for node in tree {
            let node_repos = eff.get(&node.story_id).cloned().unwrap_or_default();
            if !node_repos.iter().any(|r| r == repo) {
                continue;
            }
            let link_to_parent = node
                .draft_parent_id
                .as_deref()
                .and_then(|pid| eff.get(pid))
                .is_some_and(|prepos| prepos.iter().any(|r| r == repo));
            actions.push(PublishAction {
                repo: repo.clone(),
                node_story_id: node.story_id.clone(),
                link_to_parent,
            });
        }
    }
    actions
}

/// `POST /api/designs/:id/publish` -- per-node, multi-repo publish of the design tree.
///
/// Each node carries its own `publish_repos` assignment (the project's connected repos it
/// targets). A node with an empty assignment falls back to the request's `repos` override
/// or, failing that, the design's PROJECT repos. [`plan_design_publish`] turns this into a
/// flat list of [`PublishAction`]s (parents before children within each repo); the handler
/// executes them, creating each node's issue in each of its repos and linking sub-issues
/// only where BOTH the node and its parent were published in the SAME repo. A child that
/// targets a repo its parent does not live in publishes STANDALONE there (not a warning).
/// Returns `{ "nodes": ["owner/repo#N", ...], "warnings": [...] }`.
pub(crate) async fn design_publish(
    State(state): State<AppState>,
    Path(design_id): Path<String>,
    Json(req): Json<DesignPublishReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    let token = state.github_token().ok_or_else(|| {
        AppError(anyhow::anyhow!(
            "Connect GitHub (set CAMERATA_GITHUB_TOKEN) to publish the design to the board."
        ))
    })?;

    // Walk the tree in BFS order (parents always processed before children per repo).
    let tree = state.uow.list_design_tree(&design_id);
    if tree.is_empty() {
        return Err(AppError(anyhow::anyhow!(
            "design `{design_id}` not found or has no nodes"
        )));
    }

    // The fallback repo set for any node with an empty `publish_repos`: the request's
    // explicit override when present, else the design's PROJECT repos (resolved from the
    // root's `project_id`).
    let project_repos: Vec<String> = if !req.repos.is_empty() {
        req.repos.clone()
    } else {
        tree.first()
            .and_then(|root| root.project_id.clone())
            .and_then(|pid| state.projects.get(&pid))
            .map(|p| p.repos)
            .unwrap_or_default()
    };
    if project_repos.is_empty() && tree.iter().all(|n| n.publish_repos.is_empty()) {
        return Err(AppError(anyhow::anyhow!(
            "no repos to publish into: assign repos per node, pass `repos`, or connect repos to the project"
        )));
    }

    // Effective repos for a node: its own assignment, or the project fallback.
    let fallback = project_repos.clone();
    let plan = plan_design_publish(&tree, |node| {
        if node.publish_repos.is_empty() {
            fallback.clone()
        } else {
            node.publish_repos.clone()
        }
    });

    // A quick lookup from draft story_id -> node for authoring/attachments/labels.
    let by_id: std::collections::HashMap<&str, &crate::uow::UnitOfWork> =
        tree.iter().map(|n| (n.story_id.as_str(), n)).collect();

    // (repo, node_story_id) -> published issue number, for in-repo sub-issue linking.
    let mut published: std::collections::HashMap<(String, String), u64> =
        std::collections::HashMap::new();
    let mut node_refs: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    // Nodes we linked a work item for already (link once per node, at first success).
    let mut linked: std::collections::HashSet<String> = std::collections::HashSet::new();

    for action in &plan {
        let Some(node) = by_id.get(action.node_story_id.as_str()) else {
            continue;
        };
        let coord = match camerata_worktracker::RepoCoord::parse(&action.repo) {
            Some(c) => c,
            None => {
                warnings.push(format!(
                    "node `{}` skipped in `{}`: repo must be `owner/repo`",
                    node.story_id, action.repo
                ));
                continue;
            }
        };
        let authoring = node.authoring.clone().unwrap_or_default();
        if authoring.draft_title.trim().is_empty() {
            warnings.push(format!(
                "node `{}` skipped in `{}`: no drafted title",
                node.story_id, action.repo
            ));
            continue;
        }

        // Create the issue. Embed any node attachments then the diagram in the body.
        // Fail-soft: a per-node create error must NOT abort the whole tree (that would
        // discard already-created issues and duplicate them on retry). Warn and continue,
        // always returning `{nodes, warnings}` so the caller can retry the remainder.
        let node_issue_body = embed_attachments(&authoring.draft_body, &node.attachments);
        let node_issue_body = embed_diagram(&node_issue_body, node.diagram.as_deref());
        let (html_url, child_db_id) = match crate::github_issues::create_issue_returning_id(
            &coord.owner,
            &coord.repo,
            &token,
            &authoring.draft_title,
            &node_issue_body,
        )
        .await
        {
            Ok(v) => v,
            Err(e) => {
                warnings.push(format!(
                    "node `{}` failed to create in `{}`: {e}",
                    node.story_id, action.repo
                ));
                continue;
            }
        };

        let Some(number) = html_url
            .rsplit('/')
            .next()
            .and_then(|s| s.trim().parse::<u64>().ok())
        else {
            warnings.push(format!(
                "node `{}` created in `{}` but its issue number could not be parsed from `{html_url}`",
                node.story_id, action.repo
            ));
            continue;
        };

        // Apply the type label (fail-soft: label failure does not abort publish).
        if let Some(ref node_type) = node.node_type {
            let label = format!("type:{}", node_type.to_lowercase());
            if let Err(e) = crate::github_issues::add_labels_to_issue(
                &coord.owner,
                &coord.repo,
                number,
                &[label.as_str()],
                &token,
            )
            .await
            {
                warnings.push(format!(
                    "{}#{number}: label `{label}` could not be applied: {e}",
                    action.repo
                ));
            }
        }

        // Sub-issue link to parent, only when the plan says the parent lives in THIS repo.
        // `link_sub_issue` needs the parent's issue NUMBER and the child's DATABASE id.
        if action.link_to_parent {
            if let Some(parent_draft_id) = node.draft_parent_id.as_deref() {
                match published.get(&(action.repo.clone(), parent_draft_id.to_string())) {
                    Some(&parent_number) => {
                        if let Err(e) = crate::github_issues::link_sub_issue(
                            &coord.owner,
                            &coord.repo,
                            parent_number,
                            child_db_id,
                            &token,
                        )
                        .await
                        {
                            warnings.push(format!(
                                "{}#{number}: sub-issue link to #{parent_number} failed: {e}",
                                action.repo
                            ));
                        }
                    }
                    // The plan asked to link but the parent was never published in THIS
                    // repo, so there is no parent issue number to link to. Surface it
                    // instead of silently dropping the promised link.
                    None => warnings.push(format!(
                        "{}#{number}: sub-issue link skipped — parent node `{parent_draft_id}` was not published in `{}`",
                        action.repo, action.repo
                    )),
                }
            }
        }

        // Upsert the story onto the canonical spine (per repo it was created in). The issue
        // already exists on GitHub, so a local upsert failure is fail-soft: warn but still
        // record the node so the response reflects reality (and children can still link).
        let story = crate::github_issues::issue_to_story(
            &action.repo,
            number,
            &authoring.draft_title,
            &node_issue_body,
        );
        let work_item_story_id = story.id.clone();
        if let Err(e) = state.stories.upsert(story).await {
            warnings.push(format!(
                "{}#{number}: created but could not upsert onto the spine: {e}",
                action.repo
            ));
        } else if linked.insert(node.story_id.clone()) {
            // Link the work item onto the UoW once (first successful action for this node).
            state
                .uow
                .link_work_item(&node.story_id, &work_item_story_id);
        }

        published.insert((action.repo.clone(), node.story_id.clone()), number);
        node_refs.push(format!("{}#{number}", action.repo));
    }

    // Mark the design root as published when at least one action succeeded. Best-effort:
    // no-op if `design_id` is not a design root (e.g. a legacy tree whose root predates
    // the marker), so a successful publish is never turned into an error.
    if !node_refs.is_empty() {
        state.uow.set_design_status(&design_id, "published");
    }

    Ok(Json(serde_json::json!({
        "nodes": node_refs,
        "warnings": warnings,
    })))
}

/// The valid Design-Canvas statuses. `None`/absent reads as `"draft"`.
pub(crate) const DESIGN_STATUSES: [&str; 3] = ["draft", "published", "archived"];

/// The current design status, defaulting to `"draft"` when unset.
pub(crate) fn design_status_of(uow: &crate::uow::UnitOfWork) -> String {
    uow.design_status
        .clone()
        .unwrap_or_else(|| "draft".to_string())
}

/// Build the compact design summary the list + status endpoints return.
/// `node_count` is the full-tree size (root + descendants).
pub(crate) fn design_summary(state: &AppState, root: &crate::uow::UnitOfWork) -> serde_json::Value {
    let node_count = state.uow.list_design_tree(&root.story_id).len();
    let title = root
        .authoring
        .as_ref()
        .map(|a| a.draft_title.trim().to_string())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "Untitled design".to_string());
    serde_json::json!({
        "id": root.story_id,
        "title": title,
        "node_type": root.node_type,
        "status": design_status_of(root),
        "node_count": node_count,
        "updated": root.updated,
    })
}

/// `GET /api/projects/:id/designs` -- list every DESIGN ROOT owned by the project,
/// newest-first by `updated`. Returns
/// `{ "designs": [ { id, title, node_type, status, node_count, updated }, ... ] }`.
pub(crate) async fn list_project_designs(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> Json<serde_json::Value> {
    let mut roots = state.uow.list_design_roots_for_project(&project_id);
    // Newest-first by the root's `updated` timestamp (RFC 3339 strings sort chronologically).
    roots.sort_by(|a, b| b.updated.cmp(&a.updated));
    let designs: Vec<serde_json::Value> = roots.iter().map(|r| design_summary(&state, r)).collect();
    Json(serde_json::json!({ "designs": designs }))
}

#[derive(serde::Deserialize)]
pub(crate) struct DesignStatusReq {
    /// One of `"draft"`, `"published"`, `"archived"`.
    pub(crate) status: String,
}

/// `POST /api/designs/:id/status` -- set a design root's status (archive/unarchive/etc).
/// Validates the status (400 on unknown), 404s when `:id` is not a design root, and
/// returns the updated design summary on success.
pub(crate) async fn design_set_status(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<DesignStatusReq>,
) -> Response {
    let status = req.status.trim();
    if !DESIGN_STATUSES.contains(&status) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": format!(
                    "unknown design status {:?}; expected one of: {}",
                    req.status,
                    DESIGN_STATUSES.join(", ")
                )
            })),
        )
            .into_response();
    }
    match state.uow.set_design_status(&id, status) {
        Some(root) => (StatusCode::OK, Json(design_summary(&state, &root))).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": format!("design `{id}` not found (not a design root)")
            })),
        )
            .into_response(),
    }
}

/// `DELETE /api/designs/:id` -- delete an ENTIRE design (root + all descendants).
/// 404 when `:id` is not a design root (a child node is deleted via the
/// `DELETE /api/designs/:id/nodes/:node_id` route instead). Returns `{ "ok": true }`.
pub(crate) async fn design_delete(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    if state.uow.get_design_root(&id).is_none() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": format!("design `{id}` not found (not a design root)")
            })),
        )
            .into_response();
    }
    // `remove_design_subtree` removes the node itself AND every descendant.
    state.uow.remove_design_subtree(&id);
    (StatusCode::OK, Json(serde_json::json!({ "ok": true }))).into_response()
}
