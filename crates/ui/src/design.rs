//! Design canvas: AI-assisted hierarchical work decomposition.
//!
//! Two-pane surface: left = draft tree table (all nodes), right = per-node authoring panel.
//! Backend APIs are under `/api/designs/*`; all nodes are `UnitOfWork`s linked by
//! `draft_parent_id` (draft story_id). The root node has no `draft_parent_id`.

use camerata_ui_core::designs::{short_updated, DesignSummary};
use dioxus::prelude::*;

// ── Data models ───────────────────────────────────────────────────────────────

#[derive(Clone, PartialEq, serde::Deserialize, Default, Debug)]
struct DesignNode {
    story_id: String,
    #[serde(default)]
    node_type: Option<String>,
    #[serde(default)]
    draft_parent_id: Option<String>,
    #[serde(default)]
    proposed_children: Vec<ProposedChild>,
    /// Children the AI proposed that were DROPPED as schema-invalid under this node's type.
    /// Rendered as a visible warning so a drop is never silent (mirrors the server field).
    #[serde(default)]
    dropped_children: Vec<ProposedChild>,
    /// The per-node publish repo assignment (`owner/repo`). Empty means "not chosen yet";
    /// publish falls back to the project repos.
    #[serde(default)]
    publish_repos: Vec<String>,
    #[serde(default)]
    authoring: Option<Authoring>,
}

#[derive(Clone, PartialEq, serde::Deserialize, Default, Debug)]
struct Authoring {
    #[serde(default)]
    pub draft_title: String,
    #[serde(default)]
    pub draft_body: String,
    #[serde(default)]
    pub chat: Vec<ChatMsg>,
}

#[derive(Clone, PartialEq, serde::Deserialize, Default, Debug)]
struct ChatMsg {
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub text: String,
}

#[derive(Clone, PartialEq, serde::Deserialize, Default, Debug)]
struct ProposedChild {
    #[serde(default)]
    pub node_type: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: String,
}

/// The project's work-hierarchy schema (mirrors the server `HierarchySchema` shape): the
/// allowed parent→child nestings. Used to drive the "+ Add child node" type off the schema
/// instead of hardcoding one, so the child is always valid under the selected node's type.
#[derive(Clone, PartialEq, serde::Deserialize, Default, Debug)]
struct HierarchySchema {
    #[serde(default)]
    relations: Vec<TypeRelation>,
}

#[derive(Clone, PartialEq, serde::Deserialize, Default, Debug)]
struct TypeRelation {
    #[serde(default)]
    parent: String,
    #[serde(default)]
    child: String,
}

impl HierarchySchema {
    /// The child types allowed directly under `parent_type`, in schema order.
    fn allowed_children(&self, parent_type: &str) -> Vec<String> {
        self.relations
            .iter()
            .filter(|r| r.parent == parent_type)
            .map(|r| r.child.clone())
            .collect()
    }
}

// ── API helpers ───────────────────────────────────────────────────────────────

/// Fetch the active project's effective hierarchy schema. Resolves the active project id via
/// `GET /api/projects/active`, then `GET /api/projects/:id/hierarchy` (the server resolves an
/// empty/absent schema to the seeded default ladder). Returns `None` when there is no active
/// project or on any network error.
async fn api_fetch_active_hierarchy() -> Option<HierarchySchema> {
    let active: serde_json::Value = reqwest::get(format!(
        "{}/api/projects/active",
        crate::bff_base(),
    ))
    .await
    .ok()?
    .json()
    .await
    .ok()?;
    let project_id = active.get("id").and_then(|v| v.as_str())?.to_string();
    let v: serde_json::Value = reqwest::get(format!(
        "{}/api/projects/{}/hierarchy",
        crate::bff_base(),
        project_id,
    ))
    .await
    .ok()?
    .json()
    .await
    .ok()?;
    v.get("hierarchy")
        .and_then(|h| serde_json::from_value(h.clone()).ok())
}

async fn api_design_blank(
    root_type: &str,
    draft_parent_id: Option<&str>,
    project_id: Option<&str>,
) -> Option<String> {
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/api/designs/blank", crate::bff_base()))
        .json(&serde_json::json!({
            "root_type": root_type,
            "draft_parent_id": draft_parent_id,
            "project_id": project_id,
        }))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    v.get("design_id")
        .and_then(|id| id.as_str())
        .map(String::from)
}

async fn api_fetch_design_nodes(root_id: &str) -> Vec<DesignNode> {
    let v: serde_json::Value = match reqwest::get(format!(
        "{}/api/designs/{}/nodes",
        crate::bff_base(),
        root_id,
    ))
    .await
    .ok()
    {
        Some(r) => r.json().await.unwrap_or_default(),
        None => return Vec::new(),
    };
    v.get("nodes")
        .and_then(|n| serde_json::from_value(n.clone()).ok())
        .unwrap_or_default()
}

async fn api_design_author(
    node_id: &str,
    message: &str,
) -> Option<DesignNode> {
    reqwest::Client::new()
        .post(format!("{}/api/designs/{}/author", crate::bff_base(), node_id))
        .json(&serde_json::json!({ "message": message }))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()
}

async fn api_design_materialize(root_id: &str, parent_draft_id: &str, nodes: Vec<serde_json::Value>) -> bool {
    reqwest::Client::new()
        .post(format!("{}/api/designs/{}/nodes", crate::bff_base(), root_id))
        .json(&serde_json::json!({
            "parent_draft_id": parent_draft_id,
            "nodes": nodes,
        }))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

async fn api_design_delete_node(root_id: &str, node_id: &str) -> bool {
    reqwest::Client::new()
        .delete(format!(
            "{}/api/designs/{}/nodes/{}",
            crate::bff_base(),
            root_id,
            node_id,
        ))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Publish the whole design tree. Sends an EMPTY body: the server reads each node's own
/// `publish_repos` assignment (falling back to the project repos), so no repo is passed
/// from the UI. Returns `(ok, human-readable message)`.
async fn api_design_publish(root_id: &str) -> (bool, String) {
    let v: serde_json::Value = match reqwest::Client::new()
        .post(format!("{}/api/designs/{}/publish", crate::bff_base(), root_id))
        .json(&serde_json::json!({}))
        .send()
        .await
        .ok()
    {
        Some(r) => r.json().await.unwrap_or_default(),
        None => return (false, "network error".to_string()),
    };
    if v.get("nodes").is_some() {
        let count = v["nodes"].as_array().map(|a| a.len()).unwrap_or(0);
        let warnings: Vec<String> = v
            .get("warnings")
            .and_then(|w| w.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|w| w.as_str())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();
        let mut msg = format!("Published {count} node(s) to GitHub.");
        if !warnings.is_empty() {
            msg.push_str(&format!(" Warnings: {}", warnings.join("; ")));
        }
        (true, msg)
    } else {
        let msg = v
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("unknown error")
            .to_string();
        (false, msg)
    }
}

// ── Saved-designs list API (designs persistence) ─────────────────────────────

/// Resolve the active project id via `GET /api/projects/active`. Returns `None` when there is
/// no active project or on any network error. Shared by the designs-list fetch.
async fn api_active_project_id() -> Option<String> {
    let active: serde_json::Value =
        reqwest::get(format!("{}/api/projects/active", crate::bff_base()))
            .await
            .ok()?
            .json()
            .await
            .ok()?;
    active
        .get("id")
        .and_then(|v| v.as_str())
        .map(String::from)
}

/// Fetch the active project's connected repos. Resolves the active project via
/// `GET /api/projects/active` (which returns the full project including `repos`) and reads
/// its `repos` array. Returns an empty vec when there is no active project or on any error.
/// `_project_id` is accepted so callers can pass the resolved id; the active-project
/// endpoint is authoritative for the repo list.
async fn fetch_project_repos(_project_id: &str) -> Vec<String> {
    let active: serde_json::Value =
        match reqwest::get(format!("{}/api/projects/active", crate::bff_base()))
            .await
            .ok()
        {
            Some(r) => r.json().await.unwrap_or_default(),
            None => return Vec::new(),
        };
    active
        .get("repos")
        .and_then(|r| serde_json::from_value(r.clone()).ok())
        .unwrap_or_default()
}

/// `POST /api/uow/:id/publish-repos` `{ repos: [...] }` → set a node's per-node publish
/// repo assignment. Returns `true` on a 2xx response, `false` otherwise.
async fn api_set_publish_repos(node_id: &str, repos: Vec<String>) -> bool {
    reqwest::Client::new()
        .post(format!("{}/api/uow/{}/publish-repos", crate::bff_base(), node_id))
        .json(&serde_json::json!({ "repos": repos }))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// `GET /api/projects/:id/designs` → the project's saved designs (server sorts newest-first,
/// falls back to "Untitled design" for a missing title). Returns an empty vec on any error.
async fn fetch_designs(project_id: &str) -> Vec<DesignSummary> {
    let v: serde_json::Value = match reqwest::get(format!(
        "{}/api/projects/{}/designs",
        crate::bff_base(),
        project_id,
    ))
    .await
    .ok()
    {
        Some(r) => r.json().await.unwrap_or_default(),
        None => return Vec::new(),
    };
    v.get("designs")
        .and_then(|d| serde_json::from_value(d.clone()).ok())
        .unwrap_or_default()
}

/// `DELETE /api/designs/:id` → deletes the whole design tree. Returns `true` on a 2xx
/// `{ "ok": true }`, `false` otherwise (404 / network error).
async fn delete_design(id: &str) -> bool {
    let v: serde_json::Value = match reqwest::Client::new()
        .delete(format!("{}/api/designs/{}", crate::bff_base(), id))
        .send()
        .await
        .ok()
    {
        Some(r) => r.json().await.unwrap_or_default(),
        None => return false,
    };
    v.get("ok").and_then(|o| o.as_bool()).unwrap_or(false)
}

/// `POST /api/designs/:id/status` `{ "status": ... }` → set a design's lifecycle status
/// (draft | published | archived). Returns `true` on a 2xx response, `false` on 400/404/network.
async fn set_design_status(id: &str, status: &str) -> bool {
    reqwest::Client::new()
        .post(format!("{}/api/designs/{}/status", crate::bff_base(), id))
        .json(&serde_json::json!({ "status": status }))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

// ── SavedDesignRow ────────────────────────────────────────────────────────────

/// One saved-design row in the tree pane's empty state: title + status badge, a
/// "Type · N nodes" meta line + trimmed updated date, and a two-step inline delete.
/// Purely presentational — all mutations are raised to the parent via event handlers.
#[component]
fn SavedDesignRow(
    design: DesignSummary,
    pending: bool,
    on_open: EventHandler<()>,
    on_request_delete: EventHandler<()>,
    on_confirm_delete: EventHandler<()>,
) -> Element {
    let title = design.title.clone();
    let meta = design.meta_label();
    let updated = short_updated(&design.updated);
    let badge_cls = design.status_badge_class();
    let badge_label = design.status_label();

    rsx! {
        div {
            class: "design-saved-row",
            onclick: move |_| on_open.call(()),
            div { class: "design-saved-main",
                div { class: "design-saved-titlerow",
                    span { class: "design-saved-title", "{title}" }
                    span { class: "{badge_cls}", "{badge_label}" }
                }
                div { class: "design-saved-meta",
                    span { class: "design-saved-metatext", "{meta}" }
                    if !updated.is_empty() {
                        span { class: "design-saved-updated", "{updated}" }
                    }
                }
            }
            if pending {
                button {
                    class: "design-saved-confirm",
                    title: "Confirm delete of this design and its whole tree",
                    onclick: move |e| {
                        e.stop_propagation();
                        on_confirm_delete.call(());
                    },
                    "Confirm?"
                }
            } else {
                button {
                    class: "design-saved-del",
                    title: "Delete this design and its whole tree",
                    onclick: move |e| {
                        e.stop_propagation();
                        on_request_delete.call(());
                    },
                    "🗑"
                }
            }
        }
    }
}

// ── DesignCanvasView ──────────────────────────────────────────────────────────

/// The Design canvas: create / navigate a draft work-hierarchy tree on the left,
/// author each node with AI on the right.
#[component]
pub fn DesignCanvasView() -> Element {
    let mut root_id: Signal<Option<String>> = use_signal(|| None);
    let mut selected_node_id: Signal<Option<String>> = use_signal(|| None);
    let mut refresh = use_signal(|| 0u32);

    // New-root creation inputs.
    let mut new_root_type = use_signal(|| "Epic".to_string());
    let mut creating = use_signal(|| false);
    let mut load_input = use_signal(String::new);

    // Which saved-design row is awaiting delete confirmation (two-step inline confirm).
    let mut confirm_delete: Signal<Option<String>> = use_signal(|| None);
    // Bumped after create/delete/status change to re-fetch the saved-designs list.
    let mut designs_refresh = use_signal(|| 0u32);

    // The active project id — resolved once; drives the saved-designs fetch and new-design create.
    let active_pid_res = use_resource(move || async move { api_active_project_id().await });
    let active_pid = active_pid_res.read().clone().flatten();

    // The project's saved designs, shown in the empty state so they're discoverable + manageable.
    // Re-fetched whenever the active project resolves or `designs_refresh` bumps.
    let designs_res = use_resource(move || {
        let _dep = designs_refresh();
        let pid = active_pid_res.read().clone().flatten();
        async move {
            match pid {
                Some(id) => fetch_designs(&id).await,
                None => Vec::new(),
            }
        }
    });
    let designs = designs_res.read().clone().unwrap_or_default();

    // Tree nodes: fetched whenever root_id or refresh changes.
    let nodes_res = use_resource(move || {
        let _dep = refresh();
        let rid = root_id();
        async move {
            match rid {
                Some(id) => api_fetch_design_nodes(&id).await,
                None => Vec::new(),
            }
        }
    });
    let nodes = nodes_res.read().clone().unwrap_or_default();

    // The active project's hierarchy schema — drives the "+ Add child node" type so the child
    // is always valid under the selected node's type (no hardcoded, schema-invalid child).
    let schema_res = use_resource(move || async move { api_fetch_active_hierarchy().await });
    let schema = schema_res.read().clone().flatten().unwrap_or_default();

    // Selected node (pulled from tree for display).
    let selected = selected_node_id().as_ref().and_then(|id| {
        nodes.iter().find(|n| &n.story_id == id).cloned()
    });

    // The saved-design summary for the currently-open tree (matched by root id), used to show
    // its status badge + drive the Archive toggle in the header. Falls back to "draft" when the
    // design isn't in the fetched list yet (e.g. just created before the list re-fetches).
    let open_summary = root_id()
        .as_ref()
        .and_then(|rid| designs.iter().find(|d| &d.id == rid).cloned());
    let mut archiving = use_signal(|| false);

    rsx! {
        div { class: "design-canvas",
            // ── Left: tree pane ──────────────────────────────────────────────
            div { class: "design-tree-pane",
                div { class: "design-tree-head",
                    p { class: "section-label", "Design Tree" }
                    if root_id().is_some() {
                        div { class: "design-head-status",
                            {
                                let status = open_summary
                                    .as_ref()
                                    .map(|s| s.status.clone())
                                    .filter(|s| !s.is_empty())
                                    .unwrap_or_else(|| "draft".to_string());
                                let badge_cls = camerata_ui_core::designs::status_badge_class(&status);
                                let badge_label = camerata_ui_core::designs::status_label(&status);
                                let is_archived = status == "archived";
                                let rid = root_id().unwrap_or_default();
                                rsx! {
                                    span { class: "{badge_cls}", "{badge_label}" }
                                    // Auto-save reassurance: the design persists server-side on every
                                    // author / materialize turn — this indicator just confirms it.
                                    span { class: "design-autosave", title: "This design is saved automatically on every change", "✓ Saved" }
                                    button {
                                        class: "design-archive-btn",
                                        disabled: archiving() || open_summary.is_none(),
                                        title: if is_archived { "Restore this design to draft" } else { "Archive this design" },
                                        onclick: move |_| {
                                            let id = rid.clone();
                                            let next = if is_archived { "draft" } else { "archived" };
                                            archiving.set(true);
                                            spawn(async move {
                                                set_design_status(&id, next).await;
                                                archiving.set(false);
                                                designs_refresh += 1;
                                            });
                                        },
                                        if is_archived { "Unarchive" } else { "Archive" }
                                    }
                                }
                            }
                        }
                        p { class: "design-root-id",
                            "Root: {root_id().unwrap_or_default()}"
                        }
                    }
                }

                if root_id().is_none() {
                    div { class: "design-tree-empty",
                        // New tree creation — the primary "New design" action, kept prominent.
                        div { class: "design-new-root",
                            p { class: "design-input-label", "New design — root node type" }
                            input {
                                class: "design-input",
                                placeholder: "Epic",
                                value: "{new_root_type}",
                                oninput: move |e| new_root_type.set(e.value()),
                            }
                            button {
                                class: "btn-run",
                                disabled: creating(),
                                onclick: move |_| {
                                    let nt = new_root_type().trim().to_string();
                                    if nt.is_empty() { return; }
                                    let pid = active_pid.clone();
                                    creating.set(true);
                                    spawn(async move {
                                        if let Some(id) = api_design_blank(&nt, None, pid.as_deref()).await {
                                            root_id.set(Some(id.clone()));
                                            selected_node_id.set(Some(id));
                                            refresh += 1;
                                            designs_refresh += 1;
                                        }
                                        creating.set(false);
                                    });
                                },
                                if creating() { "Creating…" } else { "New design" }
                            }
                        }

                        // ── Saved designs for the active project ─────────────────
                        div { class: "design-saved",
                            p { class: "section-label", "Saved designs" }
                            if active_pid.is_none() {
                                p { class: "ws-hint", "Select a project to see its designs." }
                            } else if designs.is_empty() {
                                p { class: "ws-hint", "No saved designs yet. Create one above." }
                            } else {
                                for d in designs.iter() {
                                    {
                                        let d = d.clone();
                                        let did_open = d.id.clone();
                                        let did_del = d.id.clone();
                                        let did_conf = d.id.clone();
                                        let pending = confirm_delete().as_deref() == Some(d.id.as_str());
                                        rsx! {
                                            SavedDesignRow {
                                                key: "{d.id}",
                                                design: d,
                                                pending,
                                                on_open: move |_| {
                                                    root_id.set(Some(did_open.clone()));
                                                    selected_node_id.set(None);
                                                    refresh += 1;
                                                },
                                                on_request_delete: move |_| {
                                                    confirm_delete.set(Some(did_conf.clone()));
                                                },
                                                on_confirm_delete: move |_| {
                                                    let id = did_del.clone();
                                                    confirm_delete.set(None);
                                                    spawn(async move {
                                                        delete_design(&id).await;
                                                        designs_refresh += 1;
                                                    });
                                                },
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        // Load existing by root ID — secondary/advanced fallback (the list above is
                        // the primary way to reopen a design; this stays for pasted/known IDs).
                        div { class: "design-new-root design-load-advanced",
                            p { class: "design-input-label", "Advanced — load by root ID" }
                            input {
                                class: "design-input",
                                placeholder: "draft-xxxxxxxx",
                                value: "{load_input}",
                                oninput: move |e| load_input.set(e.value()),
                            }
                            button {
                                class: "btn-edit-sm",
                                onclick: move |_| {
                                    let id = load_input().trim().to_string();
                                    if !id.is_empty() {
                                        root_id.set(Some(id));
                                        refresh += 1;
                                    }
                                },
                                "Load"
                            }
                        }
                    }
                } else {
                    div { class: "design-node-list",
                        for node in nodes.iter() {
                            {
                                let nid = node.story_id.clone();
                                let rid = root_id().unwrap_or_default();
                                let is_selected = selected_node_id().as_deref() == Some(&node.story_id);
                                let is_root = node.draft_parent_id.is_none();
                                let type_label = node.node_type.clone().unwrap_or_else(|| "?".to_string());
                                let title = node.authoring.as_ref()
                                    .map(|a| a.draft_title.clone())
                                    .filter(|t| !t.is_empty())
                                    .unwrap_or_else(|| "(untitled)".to_string());
                                let indent = if is_root { 0usize } else { 1 };
                                let row_cls = if is_selected {
                                    "design-node-row selected"
                                } else {
                                    "design-node-row"
                                };
                                let nid_click = nid.clone();
                                let nid_del = nid.clone();
                                rsx! {
                                    div {
                                        class: "{row_cls}",
                                        style: "padding-left: {12 + indent * 16}px",
                                        onclick: move |_| selected_node_id.set(Some(nid_click.clone())),
                                        span { class: "design-node-type", "{type_label}" }
                                        span { class: "design-node-title", "{title}" }
                                        button {
                                            class: "design-node-del",
                                            title: "Delete this node and its subtree",
                                            onclick: move |e| {
                                                e.stop_propagation();
                                                let n = nid_del.clone();
                                                let r = rid.clone();
                                                spawn(async move {
                                                    api_design_delete_node(&r, &n).await;
                                                    selected_node_id.set(None);
                                                    refresh += 1;
                                                });
                                            },
                                            "×"
                                        }
                                    }
                                }
                            }
                        }
                        // "Add child" button at the bottom of a selected node. The child type is
                        // derived from the selected node's type via the project schema (the FIRST
                        // allowed child), NOT hardcoded — a hardcoded child that the schema forbids
                        // is rejected by materialize validation, giving a dead-end button.
                        if let Some(sel_id) = selected_node_id() {
                            {
                                let sel_type = nodes
                                    .iter()
                                    .find(|n| n.story_id == sel_id)
                                    .and_then(|n| n.node_type.clone())
                                    .unwrap_or_default();
                                let child_type = schema.allowed_children(&sel_type).into_iter().next();
                                match child_type {
                                    Some(ct) => rsx! {
                                        div { class: "design-add-child",
                                            button {
                                                class: "btn-edit-sm",
                                                onclick: move |_| {
                                                    let p = sel_id.clone();
                                                    let ct = ct.clone();
                                                    spawn(async move {
                                                        // `api_design_blank` with a parent already creates AND
                                                        // parents the child node; a second materialize call here
                                                        // created a duplicate sibling, so it is intentionally gone.
                                                        if let Some(new_id) = api_design_blank(&ct, Some(&p), None).await {
                                                            selected_node_id.set(Some(new_id));
                                                            refresh += 1;
                                                        }
                                                    });
                                                },
                                                "+ Add {ct}"
                                            }
                                        }
                                    },
                                    // Leaf type in the schema (no allowed children): no add button.
                                    None => rsx! {},
                                }
                            }
                        }
                    }
                }

                // "Reset / new tree" button when a tree is loaded.
                if root_id().is_some() {
                    button {
                        class: "design-reset-btn",
                        onclick: move |_| {
                            root_id.set(None);
                            selected_node_id.set(None);
                            confirm_delete.set(None);
                            designs_refresh += 1;
                        },
                        "← Designs"
                    }
                }
            }

            // ── Right: authoring pane ─────────────────────────────────────────
            div { class: "design-author-pane",
                match selected.clone() {
                    None => rsx! {
                        div { class: "design-author-empty",
                            p { class: "ws-hint",
                                if root_id().is_none() {
                                    "Create or load a design tree to get started."
                                } else {
                                    "Select a node from the tree to author it with AI."
                                }
                            }
                        }
                    },
                    Some(node) => rsx! {
                        DesignNodeAuthorPanel {
                            key: "{node.story_id}",
                            node,
                            root_id: root_id().unwrap_or_default(),
                            on_refresh: move |_| { refresh += 1; },
                        }
                    },
                }
            }
        }
    }
}

// ── Per-node authoring panel ──────────────────────────────────────────────────

#[component]
fn DesignNodeAuthorPanel(
    node: DesignNode,
    root_id: String,
    on_refresh: EventHandler<()>,
) -> Element {
    let node_id = node.story_id.clone();
    let node_type = node.node_type.clone().unwrap_or_else(|| "Node".to_string());
    let authoring = node.authoring.clone().unwrap_or_default();
    let chat = authoring.chat.clone();
    let draft_title = authoring.draft_title.clone();
    let draft_body = authoring.draft_body.clone();
    let proposed = node.proposed_children.clone();
    let dropped = node.dropped_children.clone();
    let node_publish_repos = node.publish_repos.clone();

    let mut message = use_signal(String::new);
    let mut sending = use_signal(|| false);
    let mut publishing = use_signal(|| false);
    let mut pub_msg = use_signal(String::new);

    // The active project's connected repos + its hierarchy schema — drive the per-node
    // repo assignment and the no-children leaf/atomic distinction respectively.
    let project_repos_res = use_resource(move || async move {
        match api_active_project_id().await {
            Some(pid) => fetch_project_repos(&pid).await,
            None => Vec::new(),
        }
    });
    let project_repos = project_repos_res.read().clone().unwrap_or_default();

    let schema_res = use_resource(move || async move { api_fetch_active_hierarchy().await });
    let schema = schema_res.read().clone().flatten().unwrap_or_default();

    // The whole design tree — used only to derive the read-only publish summary
    // ("Publishes N nodes across: repoA (x nodes), ..."). Re-fetched when the root changes.
    let tree_root = root_id.clone();
    let tree_res = use_resource(move || {
        let rid = tree_root.clone();
        async move { api_fetch_design_nodes(&rid).await }
    });
    let tree_nodes = tree_res.read().clone().unwrap_or_default();

    // The child types the schema allows under this node's type (drives the no-children
    // messaging: a leaf type has none, a non-leaf type has at least one).
    let allowed_here = schema.allowed_children(&node_type);

    // A node whose per-node list is empty defaults to "all project repos" (matching the
    // publish fallback). Bumped after each toggle-save to re-read the persisted selection.
    let selected_repos: Vec<String> = if node_publish_repos.is_empty() {
        project_repos.clone()
    } else {
        node_publish_repos.clone()
    };

    // Clones needed because node_id is moved into two separate closures.
    let node_id_kd = node_id.clone();
    let node_id_click = node_id.clone();

    rsx! {
        div { class: "design-node-panel",
            // Header
            div { class: "design-node-panel-head",
                span { class: "design-node-type lg", "{node_type}" }
                if !draft_title.is_empty() {
                    h2 { class: "design-node-panel-title", "{draft_title}" }
                } else {
                    p { class: "ws-hint", "(no title yet — ask the AI to draft one)" }
                }
                p { class: "design-node-id", "ID: {node_id}" }
            }

            // Draft body (when present)
            if !draft_body.is_empty() {
                div { class: "design-draft-body",
                    div {
                        dangerous_inner_html: "{crate::md::md_to_html(&draft_body)}"
                    }
                }
            }

            // Chat history
            div { class: "authoring-chat design-chat",
                if chat.is_empty() {
                    p { class: "section-hint",
                        "Describe what this {node_type} should accomplish and the AI will draft it."
                    }
                }
                for m in chat.iter() {
                    {
                        let who = if m.role == "ai" { "Assistant" } else { "You" };
                        let cls = if m.role == "ai" { "authoring-msg ai" } else { "authoring-msg user" };
                        rsx! {
                            div { class: "{cls}",
                                span { class: "authoring-msg-role", "{who}" }
                                p { class: "authoring-msg-text", "{m.text}" }
                            }
                        }
                    }
                }
            }

            // Message input
            div { class: "design-send-row",
                textarea {
                    class: "design-input-area",
                    placeholder: "Describe requirements or ask a question…",
                    disabled: sending(),
                    value: "{message}",
                    oninput: move |e| message.set(e.value()),
                    onkeydown: move |e: KeyboardEvent| {
                        if e.key() == Key::Enter && e.modifiers().contains(Modifiers::META) {
                            let msg = message().trim().to_string();
                            if msg.is_empty() || sending() { return; }
                            let nid = node_id_kd.clone();
                            sending.set(true);
                            message.set(String::new());
                            spawn(async move {
                                let _guard = crate::loading::LoadingGuard::new();
                                api_design_author(&nid, &msg).await;
                                sending.set(false);
                                on_refresh.call(());
                            });
                        }
                    },
                }
                button {
                    class: "btn-run",
                    disabled: sending(),
                    onclick: move |_| {
                        let msg = message().trim().to_string();
                        if msg.is_empty() { return; }
                        let nid = node_id_click.clone();
                        sending.set(true);
                        message.set(String::new());
                        spawn(async move {
                            let _guard = crate::loading::LoadingGuard::new();
                            api_design_author(&nid, &msg).await;
                            sending.set(false);
                            on_refresh.call(());
                        });
                    },
                    if sending() { "Sending…" } else { "Send (Cmd+↵)" }
                }
            }

            // Proposed children outcome — ALWAYS visible. One of three states:
            //   1. children proposed  → the click-through list + Materialize button
            //   2. children dropped   → a warning naming each dropped type + the allowed set
            //   3. none of either     → a leaf-vs-atomic explanation off the schema
            if !proposed.is_empty() {
                div { class: "design-proposed-section",
                    p { class: "section-label", "Proposed children" }
                    div { class: "design-proposed",
                        for child in proposed.iter() {
                            {
                                let label = if child.title.is_empty() {
                                    child.node_type.clone()
                                } else {
                                    format!("{}: {}", child.node_type, child.title)
                                };
                                let body_html = if child.body.is_empty() {
                                    String::new()
                                } else {
                                    crate::md::md_to_html(&child.body)
                                };
                                let has_body = !child.body.is_empty();
                                rsx! {
                                    div { class: "design-proposed-item",
                                        p { class: "design-proposed-title", "{label}" }
                                        if has_body {
                                            div {
                                                class: "design-proposed-body",
                                                dangerous_inner_html: "{body_html}"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    {
                        let root = root_id.clone();
                        let nid_for_mat = node.story_id.clone();
                        let proposed_for_mat = proposed.clone();
                        rsx! {
                            button {
                                class: "btn-edit-sm",
                                onclick: move |_| {
                                    let r = root.clone();
                                    let p = nid_for_mat.clone();
                                    let children: Vec<serde_json::Value> = proposed_for_mat
                                        .iter()
                                        .map(|c| serde_json::json!({
                                            "node_type": c.node_type,
                                            "title": c.title,
                                            "body": c.body,
                                        }))
                                        .collect();
                                    spawn(async move {
                                        api_design_materialize(&r, &p, children).await;
                                        on_refresh.call(());
                                    });
                                },
                                "Materialize {proposed.len()} child(ren)"
                            }
                        }
                    }
                }
            } else if !dropped.is_empty() {
                {
                    // State 2: the AI proposed children, but they were invalid under this
                    // node's type and were dropped. Name each so the drop is never silent.
                    let dropped_list = dropped
                        .iter()
                        .map(|c| {
                            let title = if c.title.is_empty() { "(untitled)" } else { c.title.as_str() };
                            format!("{}: {}", c.node_type, title)
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    let allowed_str = if allowed_here.is_empty() {
                        "(none)".to_string()
                    } else {
                        allowed_here.join(", ")
                    };
                    let count = dropped.len();
                    rsx! {
                        div { class: "design-proposed-section design-proposed-dropped",
                            p { class: "section-label", "Proposed children" }
                            p { class: "ws-hint",
                                "The AI proposed {count} child story(ies), but they were not valid "
                                "under this {node_type} and were dropped: {dropped_list}. "
                                "Allowed child types here: [{allowed_str}]."
                            }
                        }
                    }
                }
            } else {
                {
                    // State 3: nothing proposed and nothing dropped. Distinguish a leaf type
                    // (schema defines no children) from an atomic judgement on a non-leaf.
                    let is_leaf = allowed_here.is_empty();
                    let allowed_str = allowed_here.join(", ");
                    rsx! {
                        div { class: "design-proposed-section design-proposed-none",
                            p { class: "section-label", "Proposed children" }
                            if is_leaf {
                                p { class: "ws-hint",
                                    "This is a leaf-level {node_type}; your hierarchy defines no "
                                    "child types under it, so no children are expected."
                                }
                            } else {
                                p { class: "ws-hint",
                                    "The AI proposed no child stories for this node (it judged the "
                                    "work atomic). Use + Add child to add one manually if you want "
                                    "to break it down. Allowed child types: [{allowed_str}]."
                                }
                            }
                        }
                    }
                }
            }

            // Assign-to-repos section: one checkbox per project repo. Initialized from the
            // node's `publish_repos` (empty = all project repos, matching the publish
            // fallback). Toggling auto-saves via POST /api/uow/:id/publish-repos + refreshes.
            if !project_repos.is_empty() {
                div { class: "design-assign-repos-section",
                    p { class: "section-label", "Assign to repos" }
                    p { class: "ws-hint",
                        "This node's issue is created in each checked repo. Leave all checked "
                        "to publish everywhere this project is connected."
                    }
                    div { class: "design-assign-repos",
                        for repo in project_repos.iter() {
                            {
                                let repo = repo.clone();
                                let checked = selected_repos.iter().any(|r| r == &repo);
                                let current = selected_repos.clone();
                                let nid = node.story_id.clone();
                                rsx! {
                                    label { class: "design-assign-repo",
                                        input {
                                            r#type: "checkbox",
                                            checked,
                                            onchange: move |_| {
                                                // Toggle this repo in the node's selection, then persist.
                                                let mut next: Vec<String> = current.clone();
                                                if let Some(pos) = next.iter().position(|r| r == &repo) {
                                                    next.remove(pos);
                                                } else {
                                                    next.push(repo.clone());
                                                }
                                                let nid = nid.clone();
                                                spawn(async move {
                                                    api_set_publish_repos(&nid, next).await;
                                                    on_refresh.call(());
                                                });
                                            },
                                        }
                                        span { "{repo}" }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Publish section
            {
                // Read-only summary of where the whole tree will publish, derived from every
                // node's effective repo selection (its own list, or all project repos when
                // empty — the same fallback the server applies).
                let mut counts: Vec<(String, usize)> = Vec::new();
                for n in tree_nodes.iter() {
                    let eff = if n.publish_repos.is_empty() {
                        project_repos.clone()
                    } else {
                        n.publish_repos.clone()
                    };
                    for r in eff {
                        match counts.iter_mut().find(|(name, _)| name == &r) {
                            Some((_, c)) => *c += 1,
                            None => counts.push((r, 1)),
                        }
                    }
                }
                let node_count = tree_nodes.len();
                let summary = if counts.is_empty() {
                    "No repos assigned yet. Check a repo above (or connect repos to the project) to publish.".to_string()
                } else {
                    let parts = counts
                        .iter()
                        .map(|(r, c)| format!("{r} ({c} node(s))"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("Publishes {node_count} node(s) across: {parts}")
                };
                let can_publish = !counts.is_empty();
                let root = root_id.clone();
                rsx! {
                    div { class: "design-publish-section",
                        p { class: "section-label", "Publish tree to GitHub" }
                        p { class: "ws-hint",
                            "Creates GitHub issues for every node in its assigned repos, links "
                            "sub-issues within a repo, and applies type labels."
                        }
                        p { class: "ws-hint", "{summary}" }
                        div { class: "design-publish-row",
                            button {
                                class: "btn-run",
                                disabled: publishing() || !can_publish,
                                onclick: move |_| {
                                    let root = root.clone();
                                    publishing.set(true);
                                    pub_msg.set(String::new());
                                    spawn(async move {
                                        let (ok, msg) = api_design_publish(&root).await;
                                        publishing.set(false);
                                        pub_msg.set(if ok {
                                            msg
                                        } else {
                                            format!("Error: {msg}")
                                        });
                                    });
                                },
                                if publishing() { "Publishing…" } else { "Publish all" }
                            }
                        }
                        if !pub_msg().is_empty() {
                            p { class: "ws-hint", "{pub_msg}" }
                        }
                    }
                }
            }

            // Mockup window
            MockupPanel { uow_id: node.story_id.clone() }
        }
    }
}

// ── MockupPanel ───────────────────────────────────────────────────────────────

/// `POST /api/uow/:id/mockup` — generate HTML + save as mockup.html attachment.
/// Returns `{ html, uow }`.
async fn api_generate_mockup(uow_id: &str, message: &str) -> Option<String> {
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/api/uow/{}/mockup", crate::bff_base(), uow_id))
        .json(&serde_json::json!({ "message": message }))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    v.get("html").and_then(|h| h.as_str()).map(String::from)
}

/// A collapsible mockup window: chat with AI to generate an HTML wireframe,
/// preview it live in an `<iframe srcdoc>`. The generated HTML is auto-saved
/// as a `mockup.html` attachment on the UoW.
#[component]
fn MockupPanel(uow_id: String) -> Element {
    let mut expanded = use_signal(|| false);
    let mut mockup_html: Signal<Option<String>> = use_signal(|| None);
    let mut message = use_signal(String::new);
    let mut generating = use_signal(|| false);
    let mut error_msg = use_signal(String::new);

    rsx! {
        div { class: "mockup-panel",
            button {
                class: "mockup-toggle",
                onclick: move |_| expanded.set(!expanded()),
                if expanded() { "▼ Mockup window" } else { "▶ Mockup window" }
            }

            if expanded() {
                div { class: "mockup-body",
                    div { class: "mockup-left",
                        p { class: "section-label", "Generate HTML mockup" }
                        p { class: "ws-hint",
                            "Describe the UI you want — or leave blank to generate from this \
                             item's story + parent context. The AI generates self-contained HTML \
                             and saves it as a mockup.html attachment on this node."
                        }
                        textarea {
                            class: "design-input-area",
                            placeholder: "Describe the screen, layout, or component… (or leave blank to use this item's story + parent context)",
                            disabled: generating(),
                            value: "{message}",
                            oninput: move |e| message.set(e.value()),
                        }
                        div { class: "mockup-actions",
                            button {
                                class: "btn-run",
                                disabled: generating(),
                                onclick: move |_| {
                                    // Empty is allowed: the server grounds the mockup in this
                                    // node's story + parent-epic context when no instruction is
                                    // typed. No early return — the button fires regardless.
                                    let msg = message().trim().to_string();
                                    let uid = uow_id.clone();
                                    generating.set(true);
                                    error_msg.set(String::new());
                                    spawn(async move {
                                        let _guard = crate::loading::LoadingGuard::new();
                                        match api_generate_mockup(&uid, &msg).await {
                                            Some(html) => mockup_html.set(Some(html)),
                                            None => error_msg.set("AI generation failed or no token configured.".to_string()),
                                        }
                                        generating.set(false);
                                    });
                                },
                                if generating() { "Generating…" } else { "Generate" }
                            }
                            if mockup_html().is_some() {
                                button {
                                    class: "btn-edit-sm",
                                    onclick: move |_| mockup_html.set(None),
                                    "Clear"
                                }
                            }
                        }
                        if !error_msg().is_empty() {
                            p { class: "ws-hint", style: "color: var(--bad)", "{error_msg}" }
                        }
                    }
                    div { class: "mockup-right",
                        if let Some(html) = mockup_html() {
                            iframe {
                                class: "mockup-iframe",
                                srcdoc: "{html}",
                                title: "Mockup preview",
                            }
                        } else {
                            div { class: "mockup-placeholder",
                                p { class: "ws-hint", "Preview will appear here after generation." }
                            }
                        }
                    }
                }
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowed_children_drives_add_child_type_from_schema() {
        // The "+ Add child node" button reads the child type off the schema instead of
        // hardcoding "Story" (which the default ladder forbids under Epic → dead-end button).
        let schema = HierarchySchema {
            relations: vec![
                TypeRelation { parent: "Epic".into(), child: "Feature".into() },
                TypeRelation { parent: "Feature".into(), child: "Story".into() },
                TypeRelation { parent: "Feature".into(), child: "Defect".into() },
            ],
        };
        assert_eq!(schema.allowed_children("Epic"), vec!["Feature".to_string()]);
        assert_eq!(
            schema.allowed_children("Feature"),
            vec!["Story".to_string(), "Defect".to_string()],
        );
        // Leaf type (no allowed children) → no add button is rendered.
        assert!(schema.allowed_children("Story").is_empty());
        assert!(schema.allowed_children("Unknown").is_empty());
    }

    #[test]
    fn mockup_panel_renders_collapsed() {
        fn harness() -> Element {
            rsx! { MockupPanel { uow_id: "draft-test".to_string() } }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("Mockup window"), "toggle label renders");
        assert!(!html.contains("Generate HTML mockup"), "body hidden when collapsed");
    }

    #[test]
    fn design_canvas_view_renders_empty_state() {
        fn harness() -> Element {
            rsx! { DesignCanvasView {} }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("Design Tree"), "tree pane label renders");
        assert!(html.contains("New design"), "new design button renders");
        assert!(html.contains("Saved designs"), "saved-designs section renders");
        assert!(
            html.contains("Advanced — load by root ID"),
            "load-by-root-ID fallback still renders as advanced"
        );
    }

    #[test]
    fn design_node_author_panel_renders_empty_chat() {
        fn harness() -> Element {
            rsx! {
                DesignNodeAuthorPanel {
                    node: DesignNode {
                        story_id: "draft-abc".to_string(),
                        node_type: Some("Epic".to_string()),
                        draft_parent_id: None,
                        proposed_children: vec![],
                        authoring: Some(Authoring {
                            draft_title: "Checkout Revamp".to_string(),
                            draft_body: String::new(),
                            chat: vec![],
                        }),
                        ..Default::default()
                    },
                    root_id: "draft-abc".to_string(),
                    on_refresh: |_| {},
                }
            }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("Epic"), "node type renders");
        assert!(html.contains("Checkout Revamp"), "draft title renders");
        assert!(html.contains("Publish tree to GitHub"), "publish section renders");
        // The proposed-children outcome is now ALWAYS visible: with neither proposed nor
        // dropped children the panel renders the no-children explanation (never silent).
        assert!(
            html.contains("Proposed children"),
            "proposed-children outcome section always renders"
        );
        assert!(
            html.contains("no children are expected") || html.contains("judged the work atomic"),
            "no-children state explains leaf-vs-atomic instead of showing nothing"
        );
    }

    // ── Tier 2: contract regression tests (wiremock) ─────────────────────────────
    // These pin the parse paths that were silently broken before the fix. Any
    // future server-response shape change will fail these before it reaches prod.

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn api_design_blank_parses_design_id() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/designs/blank"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "design_id": "draft-x" })),
            )
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let result = super::api_design_blank("Epic", None, None).await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert_eq!(result.as_deref(), Some("draft-x"), "must return the design_id string");
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn api_design_publish_parses_nodes_as_success() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/designs/root-1/publish"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    serde_json::json!({ "nodes": ["owner/repo#1", "owner/repo#2"], "warnings": [] }),
                ),
            )
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let (ok, msg) = super::api_design_publish("root-1").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(ok, "nodes present => success");
        assert!(msg.contains("2"), "message must mention the count of published nodes");
    }

    #[test]
    fn design_node_author_panel_renders_proposed_children() {
        fn harness() -> Element {
            rsx! {
                DesignNodeAuthorPanel {
                    node: DesignNode {
                        story_id: "draft-root".to_string(),
                        node_type: Some("Epic".to_string()),
                        draft_parent_id: None,
                        proposed_children: vec![
                            ProposedChild {
                                node_type: "Feature".to_string(),
                                title: "Auth UI".to_string(),
                                body: "## Summary\nAllow users to log in via OAuth.".to_string(),
                            },
                        ],
                        authoring: None,
                        ..Default::default()
                    },
                    root_id: "draft-root".to_string(),
                    on_refresh: |_| {},
                }
            }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("Feature: Auth UI"), "proposed child title renders");
        assert!(
            html.contains("Allow users to log in via OAuth"),
            "proposed child body is displayed in rendered HTML"
        );
        assert!(html.contains("Materialize 1 child"), "materialize button renders");
    }

    #[test]
    fn design_node_author_panel_renders_dropped_children_warning() {
        // No proposed children, but some were dropped as schema-invalid: the panel must
        // render a visible warning naming each dropped type rather than showing nothing.
        fn harness() -> Element {
            rsx! {
                DesignNodeAuthorPanel {
                    node: DesignNode {
                        story_id: "draft-root".to_string(),
                        node_type: Some("Epic".to_string()),
                        draft_parent_id: None,
                        proposed_children: vec![],
                        dropped_children: vec![
                            ProposedChild {
                                node_type: "Story".to_string(),
                                title: "Login form".to_string(),
                                body: String::new(),
                            },
                        ],
                        authoring: None,
                        ..Default::default()
                    },
                    root_id: "draft-root".to_string(),
                    on_refresh: |_| {},
                }
            }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("were dropped"), "dropped-children warning renders");
        assert!(html.contains("Story: Login form"), "each dropped child is named");
    }

    // ── Designs-persistence: saved-designs list ──────────────────────────────────

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_designs_parses_summaries_array() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/projects/proj-7/designs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "designs": [
                    {
                        "id": "draft-a",
                        "title": "Checkout Revamp",
                        "node_type": "Epic",
                        "status": "draft",
                        "node_count": 3,
                        "updated": "2026-07-02T14:31:07Z",
                    },
                    {
                        "id": "draft-b",
                        "title": "Untitled design",
                        "node_type": serde_json::Value::Null,
                        "status": "published",
                        "node_count": 1,
                        "updated": "2026-07-01T09:00:00Z",
                    },
                ],
            })))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let out = super::fetch_designs("proj-7").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert_eq!(out.len(), 2, "both designs parse");
        assert_eq!(out[0].id, "draft-a");
        assert_eq!(out[0].title, "Checkout Revamp");
        assert_eq!(out[0].node_type.as_deref(), Some("Epic"));
        assert_eq!(out[0].status, "draft");
        assert_eq!(out[0].node_count, 3);
        assert_eq!(out[0].meta_label(), "Epic · 3 nodes");
        // A null node_type deserializes to None and the meta label drops the type prefix.
        assert_eq!(out[1].node_type, None);
        assert_eq!(out[1].meta_label(), "1 node");
        assert_eq!(out[1].status, "published");
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_designs_empty_array_yields_empty_vec() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/projects/proj-7/designs"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "designs": [] })),
            )
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let out = super::fetch_designs("proj-7").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(out.is_empty(), "empty designs array => empty vec");
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn delete_design_returns_true_on_ok_body() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/api/designs/draft-a"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": true })),
            )
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let ok = super::delete_design("draft-a").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(ok, "{{ ok: true }} => delete succeeded");
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn delete_design_returns_false_on_404() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/api/designs/missing"))
            .respond_with(
                ResponseTemplate::new(404).set_body_json(serde_json::json!({ "error": "not found" })),
            )
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let ok = super::delete_design("missing").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(!ok, "404 (no ok:true) => false");
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn set_design_status_posts_status_and_reads_success() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/designs/draft-a/status"))
            .and(body_json(serde_json::json!({ "status": "archived" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "draft-a",
                "status": "archived",
            })))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let ok = super::set_design_status("draft-a", "archived").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(ok, "2xx => status update succeeded");
    }

    #[test]
    fn saved_design_row_renders_title_badge_and_meta() {
        fn harness() -> Element {
            rsx! {
                SavedDesignRow {
                    design: DesignSummary {
                        id: "draft-a".to_string(),
                        title: "Checkout Revamp".to_string(),
                        node_type: Some("Epic".to_string()),
                        status: "published".to_string(),
                        node_count: 3,
                        updated: "2026-07-02T14:31:07Z".to_string(),
                    },
                    pending: false,
                    on_open: |_| {},
                    on_request_delete: |_| {},
                    on_confirm_delete: |_| {},
                }
            }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("Checkout Revamp"), "design title renders");
        assert!(html.contains("Published"), "status badge label renders");
        assert!(
            html.contains("design-status-badge published"),
            "published status badge class renders"
        );
        assert!(html.contains("Epic · 3 nodes"), "node_count meta label renders");
        assert!(html.contains("2026-07-02"), "trimmed updated date renders");
        // Not in confirm state → shows the trash affordance, not the confirm button.
        assert!(!html.contains("Confirm?"), "delete-confirm hidden until requested");
    }

    #[test]
    fn saved_design_row_pending_shows_confirm() {
        fn harness() -> Element {
            rsx! {
                SavedDesignRow {
                    design: DesignSummary {
                        id: "draft-a".to_string(),
                        title: "Checkout Revamp".to_string(),
                        node_type: None,
                        status: "draft".to_string(),
                        node_count: 1,
                        updated: String::new(),
                    },
                    pending: true,
                    on_open: |_| {},
                    on_request_delete: |_| {},
                    on_confirm_delete: |_| {},
                }
            }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("Confirm?"), "confirm button shows in pending state");
        assert!(html.contains("Draft"), "draft badge renders");
        // No node_type → meta drops the type prefix and pluralizes correctly.
        assert!(html.contains("1 node"), "singular node label renders");
    }
}
