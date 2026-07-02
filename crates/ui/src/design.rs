//! Design canvas: AI-assisted hierarchical work decomposition.
//!
//! Two-pane surface: left = draft tree table (all nodes), right = per-node authoring panel.
//! Backend APIs are under `/api/designs/*`; all nodes are `UnitOfWork`s linked by
//! `draft_parent_id` (draft story_id). The root node has no `draft_parent_id`.

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

async fn api_design_publish(root_id: &str, repo: &str) -> (bool, String) {
    let v: serde_json::Value = match reqwest::Client::new()
        .post(format!("{}/api/designs/{}/publish", crate::bff_base(), root_id))
        .json(&serde_json::json!({ "repo": repo }))
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

    rsx! {
        div { class: "design-canvas",
            // ── Left: tree pane ──────────────────────────────────────────────
            div { class: "design-tree-pane",
                div { class: "design-tree-head",
                    p { class: "section-label", "Design Tree" }
                    if root_id().is_some() {
                        p { class: "design-root-id",
                            "Root: {root_id().unwrap_or_default()}"
                        }
                    }
                }

                if root_id().is_none() {
                    div { class: "design-tree-empty",
                        p { class: "ws-hint",
                            "Start a new design tree or enter an existing root node ID."
                        }
                        // New tree creation.
                        div { class: "design-new-root",
                            p { class: "design-input-label", "Root node type" }
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
                                    creating.set(true);
                                    spawn(async move {
                                        if let Some(id) = api_design_blank(&nt, None, None).await {
                                            root_id.set(Some(id.clone()));
                                            selected_node_id.set(Some(id));
                                            refresh += 1;
                                        }
                                        creating.set(false);
                                    });
                                },
                                if creating() { "Creating…" } else { "New tree" }
                            }
                        }
                        // Load existing.
                        div { class: "design-new-root",
                            p { class: "design-input-label", "Or load by root ID" }
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
                                let rid = root_id().unwrap_or_default();
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
                                                    let r = rid.clone();
                                                    let p = sel_id.clone();
                                                    let ct = ct.clone();
                                                    spawn(async move {
                                                        if let Some(new_id) = api_design_blank(&ct, Some(&p), None).await {
                                                            // Materialize it by re-fetching; the blank node is already in the store.
                                                            let _ = api_design_materialize(
                                                                &r,
                                                                &p,
                                                                vec![serde_json::json!({
                                                                    "node_type": ct,
                                                                    "title": "",
                                                                    "body": "",
                                                                })],
                                                            ).await;
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
                        },
                        "← New tree"
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

    let mut message = use_signal(String::new);
    let mut sending = use_signal(|| false);
    let mut publish_repo = use_signal(String::new);
    let mut publishing = use_signal(|| false);
    let mut pub_msg = use_signal(String::new);

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

            // Proposed children section
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
                                rsx! {
                                    span { class: "design-proposed-chip", "{label}" }
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
                                            "body": "",
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
            }

            // Publish section
            div { class: "design-publish-section",
                p { class: "section-label", "Publish tree to GitHub" }
                p { class: "ws-hint",
                    "Creates GitHub issues for every node top-down, links sub-issues, and applies type labels."
                }
                div { class: "design-publish-row",
                    input {
                        class: "design-input",
                        placeholder: "owner/repo",
                        value: "{publish_repo}",
                        oninput: move |e| publish_repo.set(e.value()),
                    }
                    button {
                        class: "btn-run",
                        disabled: publishing(),
                        onclick: move |_| {
                            let repo = publish_repo().trim().to_string();
                            if repo.is_empty() { return; }
                            let root = root_id.clone();
                            publishing.set(true);
                            pub_msg.set(String::new());
                            spawn(async move {
                                let (ok, msg) = api_design_publish(&root, &repo).await;
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
        assert!(html.contains("New tree"), "new tree button renders");
        assert!(html.contains("Or load by root ID"), "load input renders");
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
        assert!(!html.contains("Proposed children"), "proposed section hidden when proposed_children is empty");
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
        let (ok, msg) = super::api_design_publish("root-1", "owner/repo").await;
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
                            },
                        ],
                        authoring: None,
                    },
                    root_id: "draft-root".to_string(),
                    on_refresh: |_| {},
                }
            }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("Feature: Auth UI"), "proposed child chip renders");
        assert!(html.contains("Materialize 1 child"), "materialize button renders");
    }
}
