//! Attachment, diagram, mockup, design, rule drift, finding context and panic guard
//! tests.

use super::*;

#[test]
fn embed_attachments_no_attachments_returns_body_unchanged() {
    let body = "## Summary\nSome content.";
    assert_eq!(embed_attachments(body, &[]), body);
}

#[test]
fn embed_attachments_html_uses_html_lang_fence() {
    let att = crate::uow::UowAttachment {
        name: "mockup.html".to_string(),
        mime: "text/html".to_string(),
        content: "<html>Hi</html>".to_string(),
    };
    let result = embed_attachments("Body", &[att]);
    assert!(result.contains("<details>"), "details block present");
    assert!(result.contains("Attachment: mockup.html"), "name present");
    assert!(result.contains("```html"), "html lang fence");
    assert!(result.contains("<html>Hi</html>"), "content present");
}

#[test]
fn embed_attachments_mermaid_uses_mermaid_fence() {
    let att = crate::uow::UowAttachment {
        name: "arch.mmd".to_string(),
        mime: "text/plain".to_string(),
        content: "graph LR; A-->B".to_string(),
    };
    let result = embed_attachments("Body", &[att]);
    assert!(
        result.contains("```mermaid"),
        "mermaid fence for .mmd extension"
    );
}

/// End-to-end router test for UoW file attachments: add, list, get, remove.
#[tokio::test]
async fn uow_attachments_http_round_trip() {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let app = router(state.clone());

    // Create a blank draft UoW to attach files to.
    let blank_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/uow/blank")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(blank_resp.status(), StatusCode::OK);
    let blank_body = body_json(blank_resp).await;
    let uow_id = blank_body["uow_id"].as_str().expect("uow_id").to_string();

    // POST /api/uow/:id/attachments -- add an HTML mockup attachment.
    let add_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/uow/{uow_id}/attachments"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&serde_json::json!({
                        "name": "mockup.html",
                        "mime": "text/html",
                        "content": "<html><body>Hello</body></html>"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(add_resp.status(), StatusCode::OK);
    let add_body = body_json(add_resp).await;
    assert_eq!(add_body["attachments"].as_array().unwrap().len(), 1);

    // GET /api/uow/:id/attachments -- list (name + mime, no content).
    let list_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/uow/{uow_id}/attachments"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list_resp.status(), StatusCode::OK);
    let list_body = body_json(list_resp).await;
    let attachments = list_body["attachments"].as_array().unwrap();
    assert_eq!(attachments.len(), 1);
    assert_eq!(attachments[0]["name"].as_str().unwrap(), "mockup.html");
    assert!(
        attachments[0].get("content").is_none(),
        "content omitted from list"
    );

    // GET /api/uow/:id/attachments/:name -- fetch content.
    let get_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/uow/{uow_id}/attachments/mockup.html"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(get_resp.status(), StatusCode::OK);
    let get_body = body_json(get_resp).await;
    assert_eq!(
        get_body["attachment"]["content"].as_str().unwrap(),
        "<html><body>Hello</body></html>"
    );

    // DELETE /api/uow/:id/attachments/:name -- remove.
    let del_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/uow/{uow_id}/attachments/mockup.html"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(del_resp.status(), StatusCode::OK);

    // List after delete: empty.
    let list2_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/uow/{uow_id}/attachments"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list2_body = body_json(list2_resp).await;
    assert_eq!(
        list2_body["attachments"].as_array().unwrap().len(),
        0,
        "attachment removed"
    );
}

/// ROUTES-8: a `GET /api/uow/:id` with an UNKNOWN id returns a (transient) UoW body but
/// does NOT persist a junk record — the UoW list stays empty afterwards. Previously the
/// read `get_or_create`d the id, leaking an empty UoW into every list view.
#[tokio::test]
async fn uow_get_read_does_not_persist_unknown_id() {
    // An active project so /api/uow (list) is not short-circuited to empty.
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let pid = state
        .projects
        .create("Proj", vec!["me/api".to_string()])
        .unwrap()
        .id;
    state.projects.set_active(&pid);
    let app = router(state.clone());

    // GET a UoW for an id that was never created. A single-segment (slash-free) draft
    // id keeps the `:story_id` route matching clean while still being an unknown id.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/uow/draft-does-not-exist")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(
        body["story_id"], "draft-does-not-exist",
        "returns a UoW-shaped body"
    );

    // The store must not have materialized it — neither the direct getter nor the raw
    // list sees any record after a read of an unknown id.
    assert!(
        state.uow.get("draft-does-not-exist").is_none(),
        "a GET must not persist a UoW for an unknown id"
    );
    assert!(
        state.uow.list().is_empty(),
        "the junk UoW must not leak into the store"
    );
    let _ = pid; // project made active only so the list endpoint isn't short-circuited
}

// ── Diagram embed / strip tests ───────────────────────────────────────────

#[test]
fn embed_diagram_no_diagram_unchanged() {
    assert_eq!(embed_diagram("Body text", None), "Body text");
    assert_eq!(embed_diagram("Body text", Some("")), "Body text");
    assert_eq!(embed_diagram("Body text", Some("   ")), "Body text");
}

#[test]
fn embed_diagram_appends_mermaid_fence() {
    let result = embed_diagram("## Summary", Some("graph TD\n  A-->B"));
    assert!(result.starts_with("## Summary"), "original body intact");
    assert!(result.contains("```mermaid"), "mermaid fence present");
    assert!(result.contains("graph TD"), "diagram content present");
    assert!(result.contains("A-->B"), "diagram arrow present");
}

#[test]
fn strip_mermaid_fence_strips_fenced_output() {
    let raw = "```mermaid\ngraph TD\n  A-->B\n```";
    assert_eq!(strip_mermaid_fence(raw), "graph TD\n  A-->B");
}

#[test]
fn strip_mermaid_fence_passthrough_unfenced() {
    let raw = "graph TD\n  A-->B";
    assert_eq!(strip_mermaid_fence(raw), "graph TD\n  A-->B");
}

/// End-to-end router test: GET/DELETE diagram endpoints without AI (no token).
#[tokio::test]
async fn uow_diagram_http_round_trip() {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let app = router(state.clone());

    // Create a blank UoW.
    let blank_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/uow/blank")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    let blank_body = body_json(blank_resp).await;
    let uow_id = blank_body["uow_id"].as_str().expect("uow_id").to_string();

    // GET /api/uow/:id/diagram -- no diagram yet.
    let get1 = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/uow/{uow_id}/diagram"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(get1.status(), StatusCode::OK);
    let get1_body = body_json(get1).await;
    assert!(get1_body["diagram"].is_null(), "no diagram initially");

    // Inject a diagram directly via the store (bypasses LLM).
    state
        .uow
        .set_diagram(&uow_id, "graph TD\n  A-->B".to_string());

    // GET -- now returns the diagram.
    let get2 = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/uow/{uow_id}/diagram"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let get2_body = body_json(get2).await;
    assert_eq!(get2_body["diagram"].as_str().unwrap(), "graph TD\n  A-->B");

    // DELETE -- clears it.
    let del = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/uow/{uow_id}/diagram"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(del.status(), StatusCode::OK);

    // GET after delete -- null again.
    let get3 = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/uow/{uow_id}/diagram"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let get3_body = body_json(get3).await;
    assert!(get3_body["diagram"].is_null(), "diagram cleared");
}

/// New contract: an empty message is rejected ONLY when there is no node context at
/// all to ground the mockup on (a truly blank UoW). The rejection explains why.
#[tokio::test]
async fn uow_mockup_rejects_empty_message_when_no_node_context() {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let app = router(state.clone());
    // Create blank UoW (no title/body/requirements).
    let blank_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/uow/blank")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    let blank_body = body_json(blank_resp).await;
    let uow_id = blank_body["uow_id"].as_str().expect("uow_id").to_string();

    // POST with empty message + no node context -> still rejected (no AI call reached).
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/uow/{uow_id}/mockup"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"message":""}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    // ROUTES-7: an empty message with no node context is invalid input → 400 (was 500).
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_json(resp).await;
    assert!(
        body["error"]
            .as_str()
            .unwrap_or("")
            .contains("must not be empty"),
        "error message explains why an empty message with no context is rejected"
    );
}

/// New contract: an empty message is ACCEPTED past the guard when the node carries
/// context (title/body). The request must NOT be rejected with the empty-message
/// error — it proceeds to the AI step (which may fail in a token-free test env, but
/// the failure is NOT the "must not be empty" rejection). We assert on the prompt
/// builder directly (`build_mockup_prompt`) so this is deterministic and does not
/// depend on a live model.
#[test]
fn build_mockup_prompt_accepts_empty_message_with_node_context() {
    let authoring = camerata_app_core::uow::AuthoringState {
        requirements_prompt: String::new(),
        chat: Vec::new(),
        draft_title: "Login screen".to_string(),
        draft_body: "Email + password fields, submit button.".to_string(),
    };
    let prompt = build_mockup_prompt(&authoring, Some("Story"), None, "");
    assert!(
        prompt.contains("Work item title: Login screen"),
        "node title present"
    );
    assert!(prompt.contains("Email + password"), "node body present");
    assert!(
        prompt.contains("Work item type: Story"),
        "node type present"
    );
    assert!(
        prompt.contains("best represents this work item"),
        "empty message -> synthesized instruction, not a rejection"
    );
}

/// The parent epic/story context IS included in the generated prompt, so the mockup is
/// grounded in the broader story, not just this node.
#[test]
fn build_mockup_prompt_includes_parent_context() {
    let parent = crate::uow::UnitOfWork {
        story_id: "draft-parent".to_string(),
        node_type: Some("Epic".to_string()),
        authoring: Some(camerata_app_core::uow::AuthoringState {
            requirements_prompt: String::new(),
            chat: Vec::new(),
            draft_title: "Checkout revamp".to_string(),
            draft_body: "Overhaul the checkout flow end to end.".to_string(),
        }),
        ..Default::default()
    };
    let child_authoring = camerata_app_core::uow::AuthoringState {
        requirements_prompt: "Seed: make the payment step".to_string(),
        chat: Vec::new(),
        draft_title: "Payment step".to_string(),
        draft_body: "Card entry form.".to_string(),
    };
    let prompt = build_mockup_prompt(&child_authoring, Some("Story"), Some(&parent), "");

    // Parent context is present and comes BEFORE the node's own title.
    assert!(
        prompt.contains("Parent Epic: Checkout revamp"),
        "parent title present"
    );
    assert!(
        prompt.contains("Overhaul the checkout flow"),
        "parent body present"
    );
    let parent_idx = prompt.find("Parent Epic").expect("parent header");
    let node_idx = prompt.find("Work item title:").expect("node title header");
    assert!(
        parent_idx < node_idx,
        "parent context precedes node context"
    );

    // The original requirements prompt is also folded in.
    assert!(
        prompt.contains("Original requirements prompt:")
            && prompt.contains("Seed: make the payment step"),
        "requirements prompt present"
    );
}

/// A non-empty message keeps the user's own instruction verbatim (not the synthesized one).
#[test]
fn build_mockup_prompt_keeps_user_instruction_when_present() {
    let authoring = camerata_app_core::uow::AuthoringState {
        draft_title: "Dashboard".to_string(),
        ..Default::default()
    };
    let prompt = build_mockup_prompt(&authoring, None, None, "make it a dark grid of cards");
    assert!(
        prompt.contains("Instruction: make it a dark grid of cards"),
        "user instruction preserved verbatim"
    );
    assert!(
        !prompt.contains("best represents this work item"),
        "synthesized instruction NOT used when the user typed one"
    );
}

// ── Design publish planner unit tests ────────────────────────────────────

/// Build a design UoW node with a story_id, optional draft parent, and a per-node
/// `publish_repos` assignment. Everything else is defaulted.
fn plan_node(story_id: &str, parent: Option<&str>, repos: &[&str]) -> crate::uow::UnitOfWork {
    crate::uow::UnitOfWork {
        story_id: story_id.to_string(),
        draft_parent_id: parent.map(String::from),
        publish_repos: repos.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    }
}

/// The effective-repos closure the handler uses: the node's own `publish_repos`, or
/// the given project fallback when the node's list is empty.
fn eff_with_fallback(fallback: Vec<String>) -> impl Fn(&crate::uow::UnitOfWork) -> Vec<String> {
    move |n: &crate::uow::UnitOfWork| {
        if n.publish_repos.is_empty() {
            fallback.clone()
        } else {
            n.publish_repos.clone()
        }
    }
}

#[test]
fn plan_design_publish_cross_repo_child_publishes_standalone() {
    // (a) epic in [A], childUI in [A], childApi in [B].
    let tree = vec![
        plan_node("epic", None, &["A"]),
        plan_node("childUI", Some("epic"), &["A"]),
        plan_node("childApi", Some("epic"), &["B"]),
    ];
    let actions = plan_design_publish(&tree, eff_with_fallback(vec![]));

    // Repo A: epic (no parent) then childUI (links to epic, same repo).
    let a: Vec<_> = actions.iter().filter(|x| x.repo == "A").collect();
    assert_eq!(a.len(), 2, "A gets epic + childUI");
    assert_eq!(a[0].node_story_id, "epic");
    assert!(!a[0].link_to_parent, "epic is a root");
    assert_eq!(a[1].node_story_id, "childUI");
    assert!(a[1].link_to_parent, "childUI links to epic in A");

    // Repo B: childApi ONLY, standalone (parent epic is not in B).
    let b: Vec<_> = actions.iter().filter(|x| x.repo == "B").collect();
    assert_eq!(b.len(), 1, "B gets only childApi");
    assert_eq!(b[0].node_story_id, "childApi");
    assert!(
        !b[0].link_to_parent,
        "childApi publishes standalone in B (parent not there)"
    );

    // Parent precedes child within A (BFS order preserved).
    let idx_epic = actions
        .iter()
        .position(|x| x.repo == "A" && x.node_story_id == "epic")
        .unwrap();
    let idx_child = actions
        .iter()
        .position(|x| x.repo == "A" && x.node_story_id == "childUI")
        .unwrap();
    assert!(
        idx_epic < idx_child,
        "parent action precedes child within a repo"
    );
}

#[test]
fn plan_design_publish_empty_nodes_fall_back_to_project_repos() {
    // (b) all nodes empty publish_repos -> falls back to P for every node.
    let tree = vec![
        plan_node("epic", None, &[]),
        plan_node("child", Some("epic"), &[]),
    ];
    let actions = plan_design_publish(&tree, eff_with_fallback(vec!["P1".into(), "P2".into()]));

    // Both nodes published into both fallback repos.
    for repo in ["P1", "P2"] {
        let r: Vec<_> = actions.iter().filter(|x| x.repo == repo).collect();
        assert_eq!(r.len(), 2, "{repo} gets both nodes");
        assert_eq!(r[0].node_story_id, "epic");
        assert_eq!(r[1].node_story_id, "child");
        assert!(
            r[1].link_to_parent,
            "child links to epic in {repo} (both present)"
        );
    }
    assert_eq!(actions.len(), 4, "2 nodes x 2 fallback repos");
}

#[test]
fn plan_design_publish_child_without_parent_in_repo_is_unlinked() {
    // (c) a node in a repo whose parent is NOT in that repo -> link_to_parent=false.
    // epic in [A], child in [A, B]. In B the parent is absent -> standalone.
    let tree = vec![
        plan_node("epic", None, &["A"]),
        plan_node("child", Some("epic"), &["A", "B"]),
    ];
    let actions = plan_design_publish(&tree, eff_with_fallback(vec![]));

    let child_a = actions
        .iter()
        .find(|x| x.repo == "A" && x.node_story_id == "child")
        .unwrap();
    assert!(
        child_a.link_to_parent,
        "child links to epic in A (parent present)"
    );

    let child_b = actions
        .iter()
        .find(|x| x.repo == "B" && x.node_story_id == "child")
        .unwrap();
    assert!(
        !child_b.link_to_parent,
        "child is standalone in B (parent epic not in B)"
    );
}

// ── Design draft-tree unit tests ─────────────────────────────────────────

#[test]
fn parse_design_author_response_extracts_all_fields() {
    let raw = r##"{"title":"Epic: Checkout revamp","body":"Summary: Revamp.","reply":"Here are some features I'd suggest:","proposed_children":[{"node_type":"Feature","title":"Auth UI","body":"Handle auth."},{"node_type":"Feature","title":"Payment UI","body":"Handle payments."}]}"##;
    let (title, body, reply, children) = parse_design_author_response(raw);
    assert_eq!(title, "Epic: Checkout revamp");
    assert_eq!(body, "Summary: Revamp.");
    assert!(reply.contains("features"));
    assert_eq!(children.len(), 2);
    assert_eq!(children[0].node_type, "Feature");
    assert_eq!(children[0].title, "Auth UI");
    assert_eq!(children[1].node_type, "Feature");
}

#[test]
fn parse_design_author_response_strips_code_fence() {
    let raw =
        "```json\n{\"title\":\"T\",\"body\":\"B\",\"reply\":\"R\",\"proposed_children\":[]}\n```";
    let (title, body, reply, children) = parse_design_author_response(raw);
    assert_eq!(title, "T");
    assert_eq!(body, "B");
    assert_eq!(reply, "R");
    assert!(children.is_empty());
}

#[test]
fn parse_design_author_response_gracefully_handles_malformed() {
    let raw = "This is not JSON at all.";
    let (title, body, reply, children) = parse_design_author_response(raw);
    assert!(title.is_empty());
    assert!(body.is_empty());
    assert_eq!(reply, "This is not JSON at all.");
    assert!(children.is_empty());
}

#[test]
fn validate_design_type_relation_allows_and_rejects() {
    let schema = crate::project::HierarchySchema {
        types: vec![
            crate::project::WorkType {
                name: "Epic".to_string(),
                builtin: true,
                is_design_root: true,
            },
            crate::project::WorkType {
                name: "Feature".to_string(),
                builtin: true,
                is_design_root: false,
            },
            crate::project::WorkType {
                name: "Story".to_string(),
                builtin: true,
                is_design_root: false,
            },
        ],
        relations: vec![
            crate::project::TypeRelation {
                parent: "Epic".to_string(),
                child: "Feature".to_string(),
            },
            crate::project::TypeRelation {
                parent: "Feature".to_string(),
                child: "Story".to_string(),
            },
        ],
    };
    assert!(validate_design_type_relation(&schema, "Epic", "Feature").is_ok());
    assert!(validate_design_type_relation(&schema, "Feature", "Story").is_ok());
    // Epic -> Story is NOT in the schema.
    let err = validate_design_type_relation(&schema, "Epic", "Story");
    assert!(err.is_err(), "Epic -> Story must be rejected");
    assert!(
        err.unwrap_err().contains("Story"),
        "error names the invalid type"
    );
    // Unknown types.
    assert!(validate_design_type_relation(&schema, "Epic", "Bug").is_err());
}

/// End-to-end through the HTTP router: full design draft-tree flow.
/// Exercises schema enforcement, node materialization, tree listing, node deletion,
/// and publish (no-token error path). Mirrors `hierarchy_schema_http_round_trip_and_export`.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn design_draft_tree_http_round_trip() {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let p = state
        .projects
        .create("Acme", vec![])
        .expect("project created");
    let app = router(state.clone());
    let id = p.id.clone();

    // Set a hierarchy schema: Epic -> Feature -> Story
    let schema_json = r#"{"types":[
            {"name":"Epic","builtin":true,"is_design_root":true},
            {"name":"Feature","builtin":true,"is_design_root":false},
            {"name":"Story","builtin":true,"is_design_root":false}
        ],"relations":[
            {"parent":"Epic","child":"Feature"},
            {"parent":"Feature","child":"Story"}
        ]}"#;
    let schema_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/projects/{id}/hierarchy"))
                .header("content-type", "application/json")
                .body(Body::from(schema_json))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(schema_resp.status(), StatusCode::OK);

    // POST /api/designs/blank -- create the design root (Epic).
    let blank_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/designs/blank")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"root_type":"Epic"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(blank_resp.status(), StatusCode::OK);
    let blank_body = body_json(blank_resp).await;
    let design_id = blank_body["design_id"]
        .as_str()
        .expect("design_id")
        .to_string();
    assert!(design_id.starts_with("draft-"), "design_id is a draft id");

    // Verify the root node has node_type=Epic in the store.
    let root_uow = state.uow.get_or_create(&design_id);
    assert_eq!(root_uow.node_type.as_deref(), Some("Epic"));

    // POST /api/designs/:id/nodes -- materialize two Feature children.
    let nodes_body = serde_json::json!({
        "parent_draft_id": design_id,
        "nodes": [
            {"node_type": "Feature", "title": "Auth flow", "body": "Handle auth."},
            {"node_type": "Feature", "title": "Payment flow", "body": "Handle payments."}
        ]
    });
    let mat_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/designs/{design_id}/nodes"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&nodes_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(mat_resp.status(), StatusCode::OK, "materialize succeeds");
    let mat_body = body_json(mat_resp).await;
    let node_ids = mat_body["node_ids"].as_array().expect("node_ids array");
    assert_eq!(node_ids.len(), 2, "two feature nodes created");
    let feature_id = node_ids[0].as_str().unwrap().to_string();

    // POST /api/designs/:id/nodes -- try invalid type: Epic -> Story (not in schema).
    let invalid_body = serde_json::json!({
        "parent_draft_id": design_id,
        "nodes": [{"node_type": "Story", "title": "Stray story", "body": ""}]
    });
    let invalid_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/designs/{design_id}/nodes"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&invalid_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        invalid_resp.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "invalid parent-child type must be rejected"
    );

    // Add a Story under the first Feature (valid by schema).
    let story_body = serde_json::json!({
        "parent_draft_id": feature_id,
        "nodes": [{"node_type": "Story", "title": "Auth login", "body": "Login screen."}]
    });
    let story_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/designs/{design_id}/nodes"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&story_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(story_resp.status(), StatusCode::OK);
    let story_body = body_json(story_resp).await;
    let story_id = story_body["node_ids"][0].as_str().unwrap().to_string();

    // GET /api/designs/:id/nodes -- list the full tree (root + 2 features + 1 story).
    let list_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/designs/{design_id}/nodes"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list_resp.status(), StatusCode::OK);
    let list_body = body_json(list_resp).await;
    let nodes = list_body["nodes"].as_array().expect("nodes array");
    assert_eq!(nodes.len(), 4, "root + 2 features + 1 story");
    // Root is first.
    assert_eq!(nodes[0]["story_id"].as_str().unwrap(), design_id);
    // All node types present.
    let types: Vec<&str> = nodes
        .iter()
        .filter_map(|n| n["node_type"].as_str())
        .collect();
    assert!(types.contains(&"Epic"), "Epic in tree");
    assert_eq!(
        types.iter().filter(|&&t| t == "Feature").count(),
        2,
        "two Features"
    );
    assert!(types.contains(&"Story"), "Story in tree");

    // DELETE /api/designs/:id/nodes/:node_id -- delete the story node only.
    let del_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/designs/{design_id}/nodes/{story_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(del_resp.status(), StatusCode::OK);
    let del_body = body_json(del_resp).await;
    assert_eq!(del_body["ok"], true);
    let removed: Vec<&str> = del_body["removed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(removed.contains(&story_id.as_str()), "story was removed");

    // Tree should now be root + 2 features only.
    let list2_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/designs/{design_id}/nodes"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list2_body = body_json(list2_resp).await;
    assert_eq!(
        list2_body["nodes"].as_array().unwrap().len(),
        3,
        "story deleted; root + 2 features remain"
    );

    // POST /api/designs/:id/publish -- no GitHub token: expect a clear error, not a panic.
    let pub_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/designs/{design_id}/publish"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"repo":"acme/testrepo"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        pub_resp.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "publish without a token returns a server error, not a panic"
    );
    let pub_body = body_json(pub_resp).await;
    let msg = pub_body["error"].as_str().unwrap_or("");
    assert!(
        msg.contains("GitHub") || msg.contains("token"),
        "error field mentions GitHub/token; got: {msg}"
    );
}

/// End-to-end through the HTTP router: per-project designs list + status lifecycle
/// + whole-design delete. Exercises GET /api/projects/:id/designs, POST
/// /api/designs/:id/status (valid + bogus), and DELETE /api/designs/:id (root + 404).
#[tokio::test]
async fn designs_list_status_delete_http_round_trip() {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let p = state
        .projects
        .create("Acme", vec![])
        .expect("project created");
    let app = router(state.clone());
    let id = p.id.clone();

    // Create a design root (Epic) with a title, plus one child node.
    let root = state
        .uow
        .create_blank_design(Some("Epic".to_string()), None, Some(id.clone()));
    state.uow.append_authoring_turn(
        &root.story_id,
        "(seed)",
        "ok",
        "Checkout redesign",
        "The body.",
    );
    let child = state.uow.create_blank_design(
        Some("Feature".to_string()),
        Some(root.story_id.clone()),
        Some(id.clone()),
    );
    let design_id = root.story_id.clone();

    // GET /api/projects/:id/designs -- one design, node_count 2 (root + child), draft.
    let list_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/projects/{id}/designs"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list_resp.status(), StatusCode::OK);
    let list_body = body_json(list_resp).await;
    let designs = list_body["designs"].as_array().expect("designs array");
    assert_eq!(designs.len(), 1, "one design root for the project");
    let d = &designs[0];
    assert_eq!(d["id"].as_str().unwrap(), design_id);
    assert_eq!(d["title"].as_str().unwrap(), "Checkout redesign");
    assert_eq!(d["node_type"].as_str().unwrap(), "Epic");
    assert_eq!(d["status"].as_str().unwrap(), "draft", "default status");
    assert_eq!(d["node_count"].as_u64().unwrap(), 2, "root + 1 child");
    assert!(d["updated"].as_str().is_some(), "updated present");

    // POST /api/designs/:id/status -- archive it; returns the updated summary.
    let arch_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/designs/{design_id}/status"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"status":"archived"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(arch_resp.status(), StatusCode::OK);
    let arch_body = body_json(arch_resp).await;
    assert_eq!(arch_body["status"].as_str().unwrap(), "archived");
    // Persisted: re-reading the root shows archived.
    assert_eq!(
        state
            .uow
            .get_design_root(&design_id)
            .unwrap()
            .design_status
            .as_deref(),
        Some("archived")
    );

    // POST status with a bogus value -> 400.
    let bogus_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/designs/{design_id}/status"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"status":"nope"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(bogus_resp.status(), StatusCode::BAD_REQUEST);

    // POST status on the child (not a root) -> 404.
    let child_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/designs/{}/status", child.story_id))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"status":"draft"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(child_resp.status(), StatusCode::NOT_FOUND);

    // DELETE the child via the whole-design route -> 404 (child is not a root).
    let del_child_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/designs/{}", child.story_id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(del_child_resp.status(), StatusCode::NOT_FOUND);

    // DELETE /api/designs/:id -- delete the whole design (root + child).
    let del_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/designs/{design_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(del_resp.status(), StatusCode::OK);
    assert_eq!(body_json(del_resp).await["ok"], true);

    // The design is gone: list is empty and the tree is empty.
    let list2_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/projects/{id}/designs"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        body_json(list2_resp).await["designs"]
            .as_array()
            .unwrap()
            .len(),
        0,
        "design removed from the list"
    );
    assert!(
        state.uow.list_design_tree(&design_id).is_empty(),
        "tree gone"
    );
}

/// `design_publish` sets the design root's status to `published` on success.
/// We can't run a real publish (needs a GitHub token), so we assert the store-level
/// transition the publish handler drives: `set_design_status(root, "published")`.
#[test]
fn publish_sets_design_root_status_to_published() {
    let store = crate::uow::UowStore::new();
    let root = store.create_blank_design(Some("Epic".to_string()), None, Some("p".to_string()));
    assert_eq!(root.design_status.as_deref(), Some("draft"));
    let updated = store.set_design_status(&root.story_id, "published");
    assert_eq!(updated.unwrap().design_status.as_deref(), Some("published"));
}

// ── Rule-drift contract (bugs A & B) ─────────────────────────────────────────

/// Load the bundled corpus and return the id + current content hash of some rule
/// that resolves through `get_by_id`, so drift tests don't hard-code an id that
/// could be renamed. `None` when the corpus isn't available (test skips).
async fn any_corpus_rule() -> Option<(String, String)> {
    let path = camerata_rules::corpus_path();
    if !path.exists() {
        return None;
    }
    let (corpus, _errs) = camerata_rules::load_corpus_lenient(&path).await;
    let rule = corpus.iter().next()?;
    Some((rule.id.0.clone(), rule.content_hash()))
}

/// BUG A: `GET /api/projects/:id/rule-drift` returns `{ ok:true, drift:[...] }`
/// with the entry shape the UI's `RuleDriftEntry` deserializes: a baselined
/// selection whose stored hash no longer matches the corpus is reported drifted.
#[tokio::test]
async fn project_rule_drift_route_reports_baselined_mismatch() {
    let Some((rule_id, _current_hash)) = any_corpus_rule().await else {
        eprintln!(
            "corpus unavailable; skipping project_rule_drift_route_reports_baselined_mismatch"
        );
        return;
    };
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let p = state
        .projects
        .create("Drift", vec!["me/api".to_string()])
        .unwrap();
    // Seed a selection baselined to a STALE current-format hash (guaranteed != current).
    state.projects.update(&p.id, |proj| {
        proj.ruleset.selections.push(crate::project::RuleSelection {
            rule_id: rule_id.clone(),
            chosen_option: None,
            repos: vec!["me/api".to_string()],
            applied_hash: Some(format!("sha256:{}", "0".repeat(64))),
            applied_directive: Some("old directive text".to_string()),
            applied_version: Some(1),
        });
    });
    let app = router(state.clone());

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/projects/{}/rule-drift", p.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["ok"], true);
    let drift = json["drift"].as_array().expect("drift is an array");
    assert_eq!(
        drift.len(),
        1,
        "the single baselined-mismatch rule is reported"
    );
    let entry = &drift[0];
    // The exact keys the UI's RuleDriftEntry reads.
    assert_eq!(entry["rule_id"], rule_id);
    assert!(entry["title"].is_string(), "title present");
    assert_eq!(entry["applied_directive"], "old directive text");
    assert!(
        entry["corpus_directive"].is_string(),
        "corpus_directive present"
    );
    assert_eq!(entry["repos"][0], "me/api");
    // A current-format hash that differs is a real change, not an unknown.
    assert_eq!(entry["changed"], true);
    assert_eq!(entry["legacy_baseline"], false);
    assert_eq!(entry["applied_version"], 1);
    assert!(
        entry["current_version"].is_number(),
        "the corpus rule carries a version: {entry}"
    );
}

/// BUG B: `POST /api/projects/:id/rule-drift/:rule_id/accept` baselines the rule
/// to the current corpus hash so a subsequent drift check no longer reports it.
/// The seeded baseline is a LEGACY (unprefixed) hash, so this also proves accept
/// re-baselines it to the current format.
#[tokio::test]
async fn accept_rule_drift_clears_the_drift() {
    let Some((rule_id, current_hash)) = any_corpus_rule().await else {
        eprintln!("corpus unavailable; skipping accept_rule_drift_clears_the_drift");
        return;
    };
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let p = state
        .projects
        .create("Drift", vec!["me/api".to_string()])
        .unwrap();
    state.projects.update(&p.id, |proj| {
        proj.ruleset.selections.push(crate::project::RuleSelection {
            rule_id: rule_id.clone(),
            chosen_option: None,
            repos: vec!["me/api".to_string()],
            applied_hash: Some("stale-baseline-hash".to_string()),
            applied_directive: Some("old".to_string()),
            applied_version: None,
        });
    });
    let app = router(state.clone());

    // Pre-condition: the legacy baseline is reported (as unknown), not as in sync.
    let pre = compute_rule_drift(&state, Some(&p.id)).await;
    assert_eq!(pre.len(), 1, "reported before accept");
    assert!(pre[0]["changed"].is_null(), "legacy is unknown: {pre:?}");

    // Accept the update.
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/projects/{}/rule-drift/{}/accept",
                    p.id, rule_id
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["ok"], true);

    // Persistence: the selection is now baselined to the CURRENT corpus hash.
    let stored = state.projects.get(&p.id).unwrap();
    let sel = stored
        .ruleset
        .selections
        .iter()
        .find(|s| s.rule_id == rule_id)
        .expect("selection persisted");
    assert_eq!(sel.applied_hash.as_deref(), Some(current_hash.as_str()));
    assert!(
        current_hash.starts_with(camerata_rules::RULE_HASH_PREFIX),
        "re-baselined to the current format: {current_hash}"
    );
    assert!(
        sel.applied_version.is_some(),
        "accept stores the rule's version beside the hash"
    );

    // Post-condition: a fresh drift computation no longer reports the rule.
    let post = compute_rule_drift(&state, Some(&p.id)).await;
    assert!(post.is_empty(), "accept cleared the drift; got {post:?}");
}

// ── /api/updates/check payload (the network-free part of the handler) ────────

#[tokio::test]
async fn updates_payload_carries_app_rule_drift_and_corpus_freshness() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let payload = updates_payload(&state, serde_json::json!({ "update_available": false })).await;

    assert_eq!(payload["ok"], true);
    assert_eq!(payload["app"]["update_available"], false);
    assert!(payload["rule_drift"].is_array(), "{payload}");
    // The bundled corpus + watermark are present in a checkout, so freshness is reported;
    // when they are not (a packaged binary), it must be an explicit null, never absent.
    assert!(
        payload.get("corpus_freshness").is_some(),
        "the key is always present: {payload}"
    );
    let cf = &payload["corpus_freshness"];
    if !cf.is_null() {
        assert!(cf["overdue"].is_array(), "{cf}");
        assert!(cf["due_soon"].is_array(), "{cf}");
        assert!(cf["never_confirmed_count"].is_number(), "{cf}");
    }
}

// ── Drift detection: content hash, legacy baselines, versions ────────────────

/// A one-rule synthetic corpus whose single option carries `directive`, so a test can
/// change ONLY an option and keep id/title/summary/enforcement identical: exactly the
/// change the old id/title/summary/enforcement fingerprint could not see.
fn drift_corpus(directive: &str, version: Option<u32>) -> camerata_rules::RuleSet {
    let mut rule = camerata_rules::bare_rule("DRIFT-FIX-1", "universal");
    rule.title = "Drift fixture".to_string();
    rule.version = version;
    rule.default_option = Some("only".to_string());
    rule.options = vec![camerata_rules::RuleOption {
        id: "only".to_string(),
        label: "The only option".to_string(),
        directive: directive.to_string(),
        why: "Fixture.".to_string(),
        remediation: None,
        finding_headline: None,
        finding_detail: None,
        escalation: None,
        effort: None,
        violates: false,
    }];
    let mut set = camerata_rules::RuleSet::default();
    set.push(rule);
    set
}

fn corpus_rule(set: &camerata_rules::RuleSet) -> &camerata_rules::Rule {
    set.get_by_id("DRIFT-FIX-1").expect("fixture rule")
}

/// A project holding one selection of `DRIFT-FIX-1` with the given baseline.
fn project_with_baseline(
    hash: Option<&str>,
    applied_version: Option<u32>,
) -> crate::project::Project {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let p = state
        .projects
        .create("Drift", vec!["me/api".to_string()])
        .unwrap();
    state
        .projects
        .update(&p.id, |proj| {
            proj.ruleset.selections.push(crate::project::RuleSelection {
                rule_id: "DRIFT-FIX-1".to_string(),
                chosen_option: None,
                repos: vec!["me/api".to_string()],
                applied_hash: hash.map(str::to_string),
                applied_directive: Some("the directive as baselined".to_string()),
                applied_version,
            });
        })
        .unwrap()
}

#[test]
fn drift_verdict_classifies_in_sync_changed_legacy_and_removed() {
    let set = drift_corpus("Do the thing.", Some(1));
    let rule = corpus_rule(&set);
    let current = rule.content_hash();

    assert_eq!(
        drift_verdict(&current, Some(rule)),
        DriftVerdict::InSync,
        "a baseline equal to the current hash is in sync"
    );
    assert_eq!(
        drift_verdict(&format!("sha256:{}", "0".repeat(64)), Some(rule)),
        DriftVerdict::Changed
    );
    // The old fingerprint: 16 hex chars, no prefix. NOT comparable, so NOT "changed".
    assert_eq!(
        drift_verdict("00ff00ff00ff00ff", Some(rule)),
        DriftVerdict::Legacy
    );
    assert_eq!(
        drift_verdict("stale-baseline-hash", Some(rule)),
        DriftVerdict::Legacy
    );
    assert_eq!(drift_verdict("", Some(rule)), DriftVerdict::Legacy);
    // A removed rule is removed whatever its stored hash looks like.
    assert_eq!(drift_verdict(&current, None), DriftVerdict::Removed);
    assert_eq!(
        drift_verdict("00ff00ff00ff00ff", None),
        DriftVerdict::Removed
    );
}

/// The first defect: the old hash covered id/title/summary/enforcement only, so a changed
/// option or directive never showed as drift.
#[test]
fn a_changed_option_directive_is_reported_as_drift() {
    let before = drift_corpus("Do the thing the old way.", Some(1));
    let baseline = corpus_rule(&before).content_hash();
    let project = project_with_baseline(Some(&baseline), Some(1));

    // Same rule, same title/summary/enforcement; ONLY the option's directive moved.
    let after = drift_corpus("Do the thing the new way.", Some(2));
    let drift = drift_for_projects(std::slice::from_ref(&project), None, &after);

    assert_eq!(drift.len(), 1, "the changed directive must be reported");
    assert_eq!(drift[0]["changed"], true);
    assert_eq!(drift[0]["legacy_baseline"], false);
    assert_eq!(drift[0]["corpus_directive"], "Do the thing the new way.");
    assert_eq!(drift[0]["applied_directive"], "the directive as baselined");

    // And a baseline that matches the corpus reports nothing.
    let in_sync = drift_for_projects(&[project], None, &before);
    assert!(in_sync.is_empty(), "{in_sync:?}");
}

/// The second defect: an upgrade must not report every applied rule as changed.
#[test]
fn a_legacy_baseline_is_reported_as_unknown_never_as_changed() {
    let set = drift_corpus("Do the thing.", Some(3));
    let project = project_with_baseline(Some("00ff00ff00ff00ff"), None);
    let drift = drift_for_projects(&[project], None, &set);

    assert_eq!(drift.len(), 1, "the legacy baseline is surfaced: {drift:?}");
    let entry = &drift[0];
    assert!(
        entry["changed"].is_null(),
        "legacy must be `changed: null`, never true or false: {entry}"
    );
    assert_eq!(entry["legacy_baseline"], true);
    assert!(entry["applied_version"].is_null());
    assert_eq!(entry["current_version"], 3);
    assert_eq!(entry["corpus_directive"], "Do the thing.");
}

#[test]
fn drift_entries_carry_applied_and_current_version() {
    let before = drift_corpus("Old.", Some(2));
    let baseline = corpus_rule(&before).content_hash();
    let project = project_with_baseline(Some(&baseline), Some(2));
    let after = drift_corpus("New.", Some(4));

    let drift = drift_for_projects(&[project], None, &after);
    assert_eq!(drift[0]["applied_version"], 2);
    assert_eq!(drift[0]["current_version"], 4);
}

#[test]
fn drift_versions_are_null_when_unknown() {
    let before = drift_corpus("Old.", None);
    let baseline = corpus_rule(&before).content_hash();
    // Baselined before versions existed (`None`), against a rule that is still unversioned.
    let project = project_with_baseline(Some(&baseline), None);
    let after = drift_corpus("New.", None);

    let drift = drift_for_projects(&[project], None, &after);
    assert!(drift[0]["applied_version"].is_null());
    assert!(drift[0]["current_version"].is_null());
}

#[test]
fn a_removed_rule_is_reported_changed_even_with_a_legacy_hash() {
    let empty = camerata_rules::RuleSet::default();
    let project = project_with_baseline(Some("00ff00ff00ff00ff"), Some(1));
    let drift = drift_for_projects(&[project], None, &empty);

    assert_eq!(drift.len(), 1);
    assert_eq!(drift[0]["changed"], true, "removal is a definite change");
    assert_eq!(drift[0]["legacy_baseline"], false);
    assert_eq!(drift[0]["corpus_directive"], "(removed from corpus)");
    assert!(drift[0]["current_version"].is_null());
    assert_eq!(drift[0]["applied_version"], 1);
}

#[test]
fn a_selection_that_was_never_baselined_is_not_reported() {
    let set = drift_corpus("Do the thing.", Some(1));
    let project = project_with_baseline(None, None);
    assert!(drift_for_projects(&[project], None, &set).is_empty());
}

#[test]
fn the_project_filter_scopes_drift_to_one_project() {
    let set = drift_corpus("Do the thing.", Some(1));
    let a = project_with_baseline(Some("00ff00ff00ff00ff"), None);
    // Each helper call builds its own store, so both projects get the same first id.
    let mut b = project_with_baseline(Some("00ff00ff00ff00ff"), None);
    b.id = "proj-other".to_string();
    assert_ne!(a.id, b.id);
    let only_a = drift_for_projects(&[a.clone(), b.clone()], Some(&a.id), &set);
    assert_eq!(only_a.len(), 1);
    assert_eq!(only_a[0]["project_id"], a.id.as_str());
    assert_eq!(
        drift_for_projects(&[a, b], None, &set).len(),
        2,
        "no filter scans every project"
    );
}

// ── Drift baseline at APPLY time ──────────────────────────────────────────────
//
// Defect: the baseline (`applied_hash` / `applied_version` / `applied_directive`) was only
// ever written by `accept_rule_drift`, so a freshly applied rule had NONE and could never
// report drift; and `upsert_base_rules` replaced the lists wholesale, erasing any baseline
// on re-apply. Semantics fixed here: APPLYING a rule means accepting the corpus as it
// stands (the apply emits the current directive into the repos), so every apply
// (re)baselines against the current corpus; any save that is NOT an apply (an import, an
// override edit, the onboarding "complete") keeps an existing baseline and only fills a
// missing one.

/// The active project of `state`, after a `save_armed_to_project` call.
fn applied_project(state: &AppState) -> crate::project::Project {
    state.projects.active().expect("a project exists")
}

#[test]
fn a_freshly_applied_rule_shows_drift_after_an_upstream_change() {
    let before = drift_corpus("Do the thing the old way.", Some(1));
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    save_armed_to_project(
        &state,
        &[arm_rule("DRIFT-FIX-1", "repo-local", &["me/api"])],
        &[],
        Some(&before),
    );

    let sel = &applied_project(&state).ruleset.selections[0];
    let rule = corpus_rule(&before);
    assert_eq!(
        sel.applied_hash.as_deref(),
        Some(rule.content_hash().as_str())
    );
    assert_eq!(sel.applied_version, Some(1));
    assert_eq!(
        sel.applied_directive.as_deref(),
        Some("Do the thing the old way.")
    );

    // Nothing changed upstream yet: in sync.
    let projects = state.projects.list();
    assert!(drift_for_projects(&projects, None, &before).is_empty());

    // The corpus moves: the applied rule now reports drift (this was silently empty).
    let after = drift_corpus("Do the thing the new way.", Some(2));
    let drift = drift_for_projects(&projects, None, &after);
    assert_eq!(drift.len(), 1, "{drift:?}");
    assert_eq!(drift[0]["changed"], true);
    assert_eq!(drift[0]["applied_version"], 1);
    assert_eq!(drift[0]["current_version"], 2);
}

#[test]
fn apply_baselines_cross_repo_and_process_selections_too() {
    let set = drift_corpus("Do the thing.", Some(4));
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    save_armed_to_project(
        &state,
        &[
            arm_rule("DRIFT-FIX-1", "cross-repo", &["me/api", "me/web"]),
            arm_rule("DRIFT-FIX-1", "process", &["me/api"]),
        ],
        &[],
        Some(&set),
    );
    let p = applied_project(&state);
    let hash = corpus_rule(&set).content_hash();
    assert_eq!(
        p.ruleset.cross_repo[0].applied_hash.as_deref(),
        Some(hash.as_str())
    );
    assert_eq!(
        p.ruleset.process[0].applied_hash.as_deref(),
        Some(hash.as_str())
    );
    assert_eq!(p.ruleset.process[0].applied_version, Some(4));
}

#[test]
fn a_rule_the_corpus_does_not_know_is_left_unbaselined() {
    let set = drift_corpus("Do the thing.", Some(1));
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    save_armed_to_project(
        &state,
        &[arm_rule("GATE-ONLY-1", "repo-local", &["me/api"])],
        &[],
        Some(&set),
    );
    let sel = &applied_project(&state).ruleset.selections[0];
    assert!(sel.applied_hash.is_none() && sel.applied_version.is_none());
}

#[test]
fn re_applying_re_baselines_to_the_current_corpus() {
    let before = drift_corpus("Old.", Some(1));
    let after = drift_corpus("New.", Some(2));
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let rules = [arm_rule("DRIFT-FIX-1", "repo-local", &["me/api"])];

    save_armed_to_project(&state, &rules, &[], Some(&before));
    assert_eq!(
        drift_for_projects(&state.projects.list(), None, &after).len(),
        1,
        "drifted after the upstream change"
    );

    // Re-apply against the new corpus: the emitted directive is now v2, so the baseline
    // follows it and the drift is cleared (exactly what accept_rule_drift would do).
    save_armed_to_project(&state, &rules, &[], Some(&after));
    let sel = &applied_project(&state).ruleset.selections[0];
    assert_eq!(sel.applied_version, Some(2));
    assert_eq!(sel.applied_directive.as_deref(), Some("New."));
    assert!(drift_for_projects(&state.projects.list(), None, &after).is_empty());
}

#[test]
fn re_applying_without_a_corpus_keeps_the_existing_baseline() {
    let before = drift_corpus("Old.", Some(1));
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let rules = [arm_rule("DRIFT-FIX-1", "repo-local", &["me/api"])];
    save_armed_to_project(&state, &rules, &[], Some(&before));
    let baselined = applied_project(&state).ruleset.selections[0].clone();

    // Corpus unavailable at re-apply time: the baseline must survive, not be erased.
    save_armed_to_project(&state, &rules, &[], None);
    assert_eq!(applied_project(&state).ruleset.selections[0], baselined);
}

#[test]
fn saving_other_selections_keeps_an_untouched_selections_baseline() {
    let before = drift_corpus("Old.", Some(1));
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    save_armed_to_project(
        &state,
        &[arm_rule("DRIFT-FIX-1", "repo-local", &["me/api"])],
        &[],
        Some(&before),
    );
    let baselined = applied_project(&state).ruleset.selections[0].clone();
    let pid = applied_project(&state).id;

    // An unrelated edit that resends the selections WITHOUT their baseline fields (an
    // import / a client that does not round-trip them) plus a brand-new rule.
    state.projects.update(&pid, |p| {
        p.upsert_base_rules(
            vec![
                crate::project::RuleSelection {
                    rule_id: "DRIFT-FIX-1".to_string(),
                    chosen_option: None,
                    repos: vec!["me/api".to_string()],
                    ..Default::default()
                },
                crate::project::RuleSelection {
                    rule_id: "OTHER-1".to_string(),
                    repos: vec!["me/api".to_string()],
                    ..Default::default()
                },
            ],
            Vec::new(),
            Vec::new(),
        );
    });
    let p = applied_project(&state);
    assert_eq!(p.ruleset.selections[0], baselined, "baseline carried over");
    assert!(
        p.ruleset.selections[1].applied_hash.is_none(),
        "the new, never-applied rule is not given a baseline by an upsert"
    );

    // ...and the preserved baseline still detects upstream drift.
    let after = drift_corpus("New.", Some(2));
    assert_eq!(drift_for_projects(&[p], None, &after).len(), 1);
}

#[test]
fn a_changed_chosen_option_is_a_new_decision_and_does_not_inherit_the_old_baseline() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let p = state
        .projects
        .create("P", vec!["me/api".to_string()])
        .unwrap();
    state.projects.update(&p.id, |proj| {
        proj.ruleset.selections.push(crate::project::RuleSelection {
            rule_id: "DRIFT-FIX-1".to_string(),
            chosen_option: Some("a".to_string()),
            repos: vec!["me/api".to_string()],
            applied_hash: Some("sha256:abc".to_string()),
            applied_directive: Some("A".to_string()),
            applied_version: Some(1),
        });
        proj.upsert_base_rules(
            vec![crate::project::RuleSelection {
                rule_id: "DRIFT-FIX-1".to_string(),
                chosen_option: Some("b".to_string()),
                repos: vec!["me/api".to_string()],
                ..Default::default()
            }],
            Vec::new(),
            Vec::new(),
        );
    });
    let sel = &state.projects.get(&p.id).unwrap().ruleset.selections[0];
    assert!(sel.applied_hash.is_none() && sel.applied_directive.is_none());
}

#[test]
fn legacy_and_unbaselined_selections_still_report_as_before() {
    let set = drift_corpus("Do the thing.", Some(3));
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let p = state
        .projects
        .create("P", vec!["me/api".to_string()])
        .unwrap();
    // One legacy-hash selection, one never-baselined selection.
    state.projects.update(&p.id, |proj| {
        proj.ruleset.selections = vec![
            crate::project::RuleSelection {
                rule_id: "DRIFT-FIX-1".to_string(),
                repos: vec!["me/api".to_string()],
                applied_hash: Some("00ff00ff00ff00ff".to_string()),
                ..Default::default()
            },
            crate::project::RuleSelection {
                rule_id: "UNBASELINED-1".to_string(),
                repos: vec!["me/api".to_string()],
                ..Default::default()
            },
        ];
    });
    let before_save = drift_for_projects(&state.projects.list(), None, &set);
    assert_eq!(before_save.len(), 1, "legacy surfaced, unbaselined silent");
    assert_eq!(before_save[0]["legacy_baseline"], true);

    // A non-apply save (resending the same selections) must not rewrite the legacy
    // baseline or conjure one for the unbaselined rule that the corpus does not know.
    state.projects.update(&p.id, |proj| {
        let same = proj.ruleset.selections.clone();
        proj.upsert_base_rules(same, Vec::new(), Vec::new());
    });
    let after_save = drift_for_projects(&state.projects.list(), None, &set);
    assert_eq!(after_save, before_save);
    let sels = state.projects.get(&p.id).unwrap().ruleset.selections;
    assert_eq!(sels[0].applied_hash.as_deref(), Some("00ff00ff00ff00ff"));
    assert!(sels[1].applied_hash.is_none());
}

#[test]
fn baseline_missing_fills_only_unbaselined_selections_from_the_corpus() {
    let set = drift_corpus("Do the thing.", Some(2));
    let mut sels = vec![
        crate::project::RuleSelection {
            rule_id: "DRIFT-FIX-1".to_string(),
            ..Default::default()
        },
        crate::project::RuleSelection {
            rule_id: "DRIFT-FIX-1".to_string(),
            applied_hash: Some("00ff00ff00ff00ff".to_string()),
            ..Default::default()
        },
        crate::project::RuleSelection {
            rule_id: "NOT-IN-CORPUS-1".to_string(),
            ..Default::default()
        },
    ];
    baseline_missing(&mut sels, Some(&set));
    assert_eq!(
        sels[0].applied_hash.as_deref(),
        Some(corpus_rule(&set).content_hash().as_str())
    );
    assert_eq!(
        sels[1].applied_hash.as_deref(),
        Some("00ff00ff00ff00ff"),
        "an existing (even legacy) baseline is never overwritten by a non-apply save"
    );
    assert!(sels[2].applied_hash.is_none());
    baseline_missing(&mut sels, None); // no corpus: a no-op, never a panic
}

#[test]
fn baselining_stores_hash_version_and_directive_and_clears_a_legacy_baseline() {
    let set = drift_corpus("Do the thing.", Some(5));
    let rule = corpus_rule(&set);
    let mut sel = crate::project::RuleSelection {
        rule_id: "DRIFT-FIX-1".to_string(),
        applied_hash: Some("00ff00ff00ff00ff".to_string()), // legacy
        applied_directive: Some("stale".to_string()),
        applied_version: Some(1),
        ..Default::default()
    };

    baseline_selection(&mut sel, rule);

    assert_eq!(
        sel.applied_hash.as_deref(),
        Some(rule.content_hash().as_str())
    );
    assert_eq!(sel.applied_version, Some(5));
    assert_eq!(sel.applied_directive.as_deref(), Some("Do the thing."));
    assert_eq!(
        drift_verdict(sel.applied_hash.as_deref().unwrap(), Some(rule)),
        DriftVerdict::InSync,
        "a freshly baselined selection is in sync"
    );
}

#[test]
fn baselining_an_unversioned_rule_clears_a_stale_applied_version() {
    let set = drift_corpus("Do the thing.", None);
    let mut sel = crate::project::RuleSelection {
        rule_id: "DRIFT-FIX-1".to_string(),
        applied_version: Some(9),
        ..Default::default()
    };
    baseline_selection(&mut sel, corpus_rule(&set));
    assert_eq!(
        sel.applied_version, None,
        "a version recorded for a rule that no longer has one would be a lie"
    );
}

/// BUG C: `set_repo_rule_override` (the repo-level route the fixed client now
/// targets) reads `{ chosen_option }` and lands a REPO-SCOPED selection.
#[tokio::test]
async fn set_repo_rule_override_reads_chosen_option_and_scopes_to_repo() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let p = state
        .projects
        .create("RepoOverride", vec!["me/api".to_string()])
        .unwrap();
    let app = router(state.clone());

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                // `owner/repo` is percent-encoded into one path segment; axum's
                // Path extractor decodes `%2F` back to `/` (see the UoW-id test).
                .uri(format!(
                    "/api/projects/{}/repos/me%2Fapi/rules/RUST-FMT-1",
                    p.id
                ))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"chosen_option":"b"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["ok"], true);

    // The override landed as a REPO-SCOPED selection (repos carries the repo),
    // not a bare project-level one.
    let stored = state.projects.get(&p.id).unwrap();
    let sel = stored
        .ruleset
        .selections
        .iter()
        .find(|s| s.rule_id == "RUST-FMT-1")
        .expect("repo override persisted");
    assert_eq!(sel.chosen_option.as_deref(), Some("b"));
    assert_eq!(sel.repos, vec!["me/api".to_string()], "scoped to the repo");
}

// ── Single-repo sentinel normalization (fix/rules-single-repo-sentinel) ──────

/// A leaked single-repo sentinel is replaced by the project's real repos; real repos
/// pass through; a mix keeps the reals and expands the sentinel, deduping the result.
#[test]
fn normalize_repos_replaces_sentinel_keeps_real_and_dedups() {
    let sentinel = "\u{0}__single_repo__".to_string();
    let project_repos = vec!["me/api".to_string()];

    // Pure sentinel → the project's single repo.
    assert_eq!(
        normalize_repos(std::slice::from_ref(&sentinel), &project_repos),
        vec!["me/api".to_string()]
    );

    // A real repo passes straight through untouched.
    assert_eq!(
        normalize_repos(&["owner/repo".to_string()], &project_repos),
        vec!["owner/repo".to_string()]
    );

    // Multi-repo project: the sentinel expands to ALL of the project's repos.
    let multi = vec!["me/api".to_string(), "me/web".to_string()];
    assert_eq!(
        normalize_repos(std::slice::from_ref(&sentinel), &multi),
        vec!["me/api".to_string(), "me/web".to_string()]
    );

    // Mixed: sentinel replaced (expanded to project repos), the real one kept, deduped so
    // the sentinel-expansion of `me/api` doesn't duplicate the explicit `me/api`.
    assert_eq!(
        normalize_repos(&["me/api".to_string(), sentinel.clone()], &multi),
        vec!["me/api".to_string(), "me/web".to_string()],
    );

    // A sentinel on a project with no real repos normalizes to empty.
    assert!(normalize_repos(&[sentinel], &[]).is_empty());
}

/// `resolve_project_arm_rules` on a project whose selection carries the nul-byte sentinel
/// yields ArmRules targeting the project's REAL repo, never the sentinel. Uses a rule id
/// with no corpus/registry entry so the minimal-emit fallback path runs (no corpus file
/// needed in the test).
#[tokio::test]
async fn resolve_project_arm_rules_normalizes_leaked_sentinel() {
    let sentinel = "\u{0}__single_repo__".to_string();
    let project: crate::project::Project = serde_json::from_value(serde_json::json!({
        "id": "p1",
        "name": "P",
        "repos": ["me/api"],
        "ruleset": {
            "selections": [
                { "rule_id": "ZZ-NONCORPUS-RULE-1", "chosen_option": null, "repos": [sentinel] }
            ]
        }
    }))
    .expect("project deserializes");

    let rules = resolve_project_arm_rules(&project).await;
    assert_eq!(rules.len(), 1, "the one selection resolves to one ArmRule");
    assert_eq!(
        rules[0].repos,
        vec!["me/api".to_string()],
        "the sentinel was normalized to the project's real repo"
    );
    assert!(
        !rules[0].repos.iter().any(|r| r.contains('\0')),
        "no nul-byte sentinel survives into the emit rules"
    );
}

/// The low-level clone helper rejects a nul-byte repo with the clear error WITHOUT ever
/// shelling out to git (which would panic with the opaque "nul byte found in provided data").
#[tokio::test]
async fn clone_or_pull_rejects_nul_byte_repo() {
    let tmp = std::env::temp_dir();
    let res = crate::workspace::clone_or_pull(&tmp, "\u{0}__single_repo__", "unused-token").await;
    assert!(!res.cloned);
    assert!(
        res.detail.contains("internal sentinel leaked"),
        "clear sentinel error, not an opaque git failure: {}",
        res.detail
    );
}

/// `apply_local` likewise bails with the clear error for a nul-byte repo (no git call).
#[tokio::test]
async fn apply_local_rejects_nul_byte_repo() {
    let tmp = std::env::temp_dir();
    let err = crate::workspace::apply_local(
        &tmp,
        "\u{0}__single_repo__",
        None,
        "camerata/onboard-governance",
        &[],
        "msg",
        "unused-token",
        true,
        false,
    )
    .await
    .expect_err("nul-byte repo must be rejected");
    assert!(
        err.to_string().contains("internal sentinel leaked"),
        "clear sentinel error: {err}"
    );
}

// ── `GET /api/onboard/finding-context` (finding-code-context feature) ──────────

fn finding_context_state_with_fixture(
    repo: &str,
    rel_path: &str,
    content: &str,
) -> (AppState, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let full = tmp.path().join(rel_path);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, content).unwrap();
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    state
        .settings
        .set_repo_path(repo, Some(tmp.path().to_string_lossy().into_owned()));
    // The shared code resolver only reads repos a project owns.
    state
        .projects
        .create("Finding context fixture", vec![repo.to_string()])
        .expect("project");
    (state, tmp)
}

/// Happy path: a real fixture file + a real finding line returns the enclosing function,
/// with the violation line, kind, and language all populated.
#[tokio::test]
async fn finding_context_returns_the_enclosing_block_for_a_real_fixture() {
    let (state, _tmp) = finding_context_state_with_fixture(
        "me/api",
        "src/lib.rs",
        "fn f() -> i32 {\n    let bad = 1;\n    bad\n}\n",
    );
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/onboard/finding-context?repo=me%2Fapi&path=src%2Flib.rs&line=2")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["status"], "ok");
    assert_eq!(json["kind"], "function");
    assert_eq!(json["language"], "rust");
    assert_eq!(json["start_line"], 1);
    assert_eq!(json["end_line"], 4);
    assert_eq!(json["violation_line"], 2);
    assert!(json["lines"].as_array().unwrap().len() == 4);
}

/// A `../../etc/passwd`-style traversal in `path` is refused with 400, never a read.
#[tokio::test]
async fn finding_context_refuses_path_traversal() {
    let (state, _tmp) = finding_context_state_with_fixture("me/api", "src/lib.rs", "fn f() {}\n");
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri(
                    "/api/onboard/finding-context?repo=me%2Fapi&path=..%2F..%2F..%2F..%2Fetc%2Fpasswd&line=1",
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let json = body_json(resp).await;
    assert_eq!(json["status"], "path_traversal");
}

/// A repo with no local path set at all (never linked) degrades to `file_missing` — the
/// SAME shape the UI already handles, never a 500 or a distinct error surface.
#[tokio::test]
async fn finding_context_unresolved_repo_degrades_to_file_missing() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/onboard/finding-context?repo=me%2Fnever-linked&path=src%2Flib.rs&line=1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["status"], "file_missing");
}

/// A file over the `MAX_FILE_BYTES` cap degrades to `too_large` rather than reading it.
#[tokio::test]
async fn finding_context_oversized_file_is_capped() {
    let big = "x".repeat(crate::onboard::files::MAX_FILE_BYTES + 1);
    let (state, _tmp) = finding_context_state_with_fixture("me/api", "src/huge.rs", &big);
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/onboard/finding-context?repo=me%2Fapi&path=src%2Fhuge.rs&line=1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["status"], "too_large");
}

/// A missing file (never written / already deleted) degrades to `file_missing`.
#[tokio::test]
async fn finding_context_missing_file_degrades_gracefully() {
    let tmp = tempfile::tempdir().unwrap();
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    state
        .settings
        .set_repo_path("me/api", Some(tmp.path().to_string_lossy().into_owned()));
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/onboard/finding-context?repo=me%2Fapi&path=src%2Fgone.rs&line=1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["status"], "file_missing");
}

// ── `GET /api/onboard/finding-fix` (recommended-fix-in-modal feature) ──────────

/// A rule with a real corpus entry + adopted default option's AUTHORED `remediation`
/// returns that text (placeholder tokens filled with the readable generic, since this
/// endpoint has no specific finding to substitute from) — the SAME string `resolve_fix`
/// (and therefore the PDF's "Fix:" line and the xlsx "Recommended Fix" column) would
/// produce for the same rule id + a default `Finding`.
#[tokio::test]
async fn finding_fix_returns_resolve_fix_output_for_a_known_rule() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/onboard/finding-fix?rule_id=SUPABASE-RLS-ENABLED-1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, _errs) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    let expected = crate::report_export::resolve_fix(
        "SUPABASE-RLS-ENABLED-1",
        Some(&corpus),
        &crate::onboard::Finding::default(),
        None,
    );
    assert!(
        expected.is_some(),
        "fixture rule must have authored remediation"
    );
    assert_eq!(json["fix"], expected.unwrap());
}

/// A rule id absent from the corpus returns a NULL fix — never a fabricated sentence.
#[tokio::test]
async fn finding_fix_is_null_for_an_unknown_rule() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/onboard/finding-fix?rule_id=NO-SUCH-RULE-EVER-1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["fix"], serde_json::Value::Null);
}

/// A rule id WITH a real corpus entry but no authored `remediation` also returns a NULL
/// fix — never falls back to the rule's `directive` (the detection-recipe leak Fix 1
/// closed). Pins the fail-safe at the API layer, not just in `resolve_fix` directly.
///
/// `onboard_finding_fix` always resolves against the REAL on-disk bundled corpus (via
/// `camerata_rules::corpus_path()` / `load_corpus_lenient`) — it has no seam to inject a
/// test corpus, and every real corpus rule's default option now has authored `remediation`
/// (the corpus-wide authoring passes closed that gap), so no real rule id can demonstrate
/// the unauthored path through the live route anymore. Rather than mutate the
/// process-global `CAMERATA_CORPUS_PATH` env var here (which would race every other test in
/// this binary that expects the real bundled corpus, since Rust runs unit tests in parallel
/// threads within one process) or teach the handler to accept an injected `RuleSet` (out of
/// scope for this fix), this test exercises the EXACT contract the handler implements —
/// `Json(json!({ "fix": resolve_fix(...) }))` — directly, against the synthetic
/// `ruleset_with_unauthored_rule` corpus. That keeps the assertion (JSON `null`, never the
/// directive text) permanently decoupled from corpus authoring state.
#[test]
fn finding_fix_is_null_when_remediation_is_unauthored() {
    let corpus = camerata_rules::ruleset_with_unauthored_rule("SEC-TEST-UNAUTHORED-1");
    let fix = crate::report_export::resolve_fix(
        "SEC-TEST-UNAUTHORED-1",
        Some(&corpus),
        &crate::onboard::Finding::default(),
        None,
    );
    let json = serde_json::json!({ "fix": fix });
    assert_eq!(json["fix"], serde_json::Value::Null);
}

// ── Silent-death-class fix: the background job panic/failure guard ─────────────

#[tokio::test]
async fn panic_guard_fails_the_job_when_the_body_panics() {
    // Stand in for the audit body panicking (a hung subprocess turning into an
    // unwrap, an indexing bug, whatever) BEFORE it reaches its own `jobs.fail` /
    // `jobs.finish` call. Without the guard, this job would stay `running` forever.
    let jobs = crate::jobs::JobStore::new();
    let jid = jobs.create("audit", None);
    run_job_body_with_panic_guard(&jobs, &jid, async {
        panic!("boom: simulated AI pipeline crash");
    })
    .await;
    let job = jobs.get(&jid).expect("job exists");
    assert_eq!(
        job.status, "failed",
        "a panicking body must not leave the job running"
    );
    let msg = job.message.expect("a failure message must be set");
    assert!(msg.contains("crashed unexpectedly"), "message: {msg}");
    assert!(
        msg.contains("boom: simulated AI pipeline crash"),
        "message: {msg}"
    );
}

#[tokio::test]
async fn panic_guard_does_not_clobber_a_clean_finish() {
    // When the body reaches its own `jobs.finish` normally, the guard must be a
    // total no-op — it must never overwrite a successful result.
    let jobs = crate::jobs::JobStore::new();
    let jid = jobs.create("audit", None);
    let (body_jobs, body_jid) = (jobs.clone(), jid.clone());
    run_job_body_with_panic_guard(&jobs, &jid, async move {
        body_jobs.finish(&body_jid, crate::onboard::ScanReport::gated(&[]));
    })
    .await;
    let job = jobs.get(&jid).expect("job exists");
    assert_eq!(
        job.status, "done",
        "the guard must not touch a job the body already finished"
    );
}

#[tokio::test]
async fn panic_guard_does_not_clobber_a_bodys_own_fail_message() {
    // When the body reaches its own `jobs.fail` (e.g. "no repos to audit") normally,
    // the guard must not overwrite that specific message with its generic one.
    let jobs = crate::jobs::JobStore::new();
    let jid = jobs.create("audit", None);
    let (body_jobs, body_jid) = (jobs.clone(), jid.clone());
    run_job_body_with_panic_guard(&jobs, &jid, async move {
        body_jobs.fail(&body_jid, "No local repos to audit.");
    })
    .await;
    let job = jobs.get(&jid).expect("job exists");
    assert_eq!(job.status, "failed");
    assert_eq!(job.message.as_deref(), Some("No local repos to audit."));
}

#[test]
fn panic_message_extracts_str_and_string_payloads_with_generic_fallback() {
    let payload: Box<dyn std::any::Any + Send> = Box::new("literal panic message");
    assert_eq!(panic_message(&payload), "literal panic message");
    let payload: Box<dyn std::any::Any + Send> = Box::new(String::from("owned panic message"));
    assert_eq!(panic_message(&payload), "owned panic message");
    // A non-string payload (a custom panic type) still produces a usable message
    // rather than failing to report one at all.
    let payload: Box<dyn std::any::Any + Send> = Box::new(42i32);
    assert_eq!(panic_message(&payload), "no panic message available");
}

/// Regression guard for the exact footgun `panic_message`'s signature was designed to
/// avoid: a caught panic's payload MUST downcast successfully through this function
/// when called the natural way (`panic_message(&caught)`), not just when manually
/// dereferenced first. A prior version of this function took `&(dyn Any + Send)` and
/// looked correct, compiled, and ran without erroring — but silently reported EVERY
/// panic as "no panic message available", because `Box<dyn Any + Send>` itself
/// satisfies `Any`'s blanket impl: `&caught` coerced by unsizing the Box (as if the Box
/// were the payload) rather than dereferencing into it. This test exercises the actual
/// `catch_unwind` path (not a hand-built `Box::new(...)`) so a regression back to that
/// signature would be caught here even though it type-checks.
#[tokio::test]
async fn panic_message_recovers_the_real_text_from_an_actual_caught_panic() {
    use futures::FutureExt;
    let caught = std::panic::AssertUnwindSafe(async {
        panic!("this exact text must survive catch_unwind intact");
    })
    .catch_unwind()
    .await
    .expect_err("the async block panics");
    assert_eq!(
        panic_message(&caught),
        "this exact text must survive catch_unwind intact"
    );
}

/// A background scan whose pipeline crashes must never present as a silent, blank,
/// zero-findings result — it renders through the same `ai_error` banner path as any
/// other AI-review failure (see `ScanReport::crashed`'s doc comment).
#[test]
fn scan_report_crashed_is_shaped_like_an_ai_error_never_a_clean_empty_scan() {
    let repos = vec!["me/api".to_string()];
    let report = crate::onboard::ScanReport::crashed(&repos, "boom".to_string());
    assert_eq!(report.repos, repos);
    assert!(
        !report.gated,
        "must not be mistaken for the GitHub-connect gate"
    );
    assert!(
        !report.blocked,
        "must not be mistaken for the compliance block"
    );
    assert_eq!(report.ai_error.as_deref(), Some("boom"));
    assert_eq!(report.message.as_deref(), Some("boom"));
    assert!(report.findings.is_empty());
}

// ── req_w13: the corpus-load failures production used to throw away ──────────────────
//
// Every production corpus load was `let (corpus, _errs) = load_corpus_lenient(..)` or
// `.await.0`. A rule file with a typo'd enum value failed to parse, left the corpus, and
// nothing reported it: not the ledger, not the coverage sheet, not `camerata
// integrity-gate`, not a log line. These tests cover the PLUMBING half (the loader keeps
// the errors and the recording helper routes them); the end-to-end proof over a real scan
// is `tests/corpus_load_failure_e2e.rs`, and the gate/ledger halves live in
// `integrity_gate.rs` / `scan_ledger.rs`.

/// A synthetic corpus directory: `good` real rule files copied verbatim out of the bundled
/// corpus, plus one real rule file whose `enforcement` value has been TYPO'D so it no
/// longer parses. Returns the temp dir (keep it alive) — nothing global is mutated, because
/// `split_scannable_rules_from` / `load_corpus_for_scan` take the path as a parameter.
///
/// Copied rather than hand-written on purpose: a hand-written "rule" proves the loader
/// rejects a file the real corpus never contains. Taking a REAL file and breaking ONE
/// character reproduces the actual reported shape.
fn synthetic_corpus_with_one_malformed_rule(
    good: &[&str],
    malformed: &str,
) -> Option<tempfile::TempDir> {
    let real = camerata_rules::corpus_path();
    if !real.exists() {
        return None;
    }
    let find = |rule_id: &str| -> Option<(String, String)> {
        let mut stack = vec![real.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).ok()?.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|e| e == "toml") {
                    let body = std::fs::read_to_string(&p).ok()?;
                    if body.contains(&format!("id = \"{rule_id}\"")) {
                        let name = p.file_name()?.to_str()?.to_string();
                        return Some((name, body));
                    }
                }
            }
        }
        None
    };

    let dir = tempfile::TempDir::new().ok()?;
    for id in good {
        let (name, body) = find(id)?;
        std::fs::write(dir.path().join(name), body).ok()?;
    }
    let (name, body) = find(malformed)?;
    // THE PLANTED DEFECT: a typo'd enum value, exactly the reported shape. `enforcement`
    // deserializes into an enum, so an unknown variant is a TOML deserialize error and the
    // whole file is skipped by `load_corpus_lenient`.
    let broken = body
        .replacen(
            "enforcement = \"architectural\"",
            "enforcement = \"architectual\"",
            1,
        )
        .replacen(
            "enforcement = \"mechanical\"",
            "enforcement = \"mechanicle\"",
            1,
        )
        .replacen("enforcement = \"prose\"", "enforcement = \"proze\"", 1)
        .replacen(
            "enforcement = \"structured\"",
            "enforcement = \"structered\"",
            1,
        );
    assert_ne!(
        broken, body,
        "the fixture builder must actually have broken something — a malformed-corpus test \
             over an intact file would pass vacuously"
    );
    std::fs::write(dir.path().join(name), broken).ok()?;
    Some(dir)
}

/// req_w13: `load_corpus_for_scan` keeps what `load_corpus_lenient`'s callers discarded.
/// The good rules still load (so one bad file does NOT fail a whole client scan — the
/// explicit non-goal: we are not switching production to the strict loader), and the bad
/// one comes back as a named failure instead of vanishing.
#[tokio::test]
async fn req_w13_the_scan_corpus_loader_keeps_the_errors_instead_of_discarding_them() {
    let Some(dir) = synthetic_corpus_with_one_malformed_rule(
        &["SEC-NO-RAW-SQL-CONCAT-1", "SEC-NO-HARDCODED-SECRETS-1"],
        "SUPABASE-RLS-ENABLED-1",
    ) else {
        eprintln!("corpus unavailable; skipping req_w13 corpus-loader test");
        return;
    };

    let (corpus, failures) = load_corpus_for_scan(dir.path()).await;
    let corpus = corpus.expect("the directory exists, so a RuleSet must come back");

    // Lenient, still: the two intact rules loaded.
    assert!(corpus.get_by_id("SEC-NO-RAW-SQL-CONCAT-1").is_some());
    assert!(corpus.get_by_id("SEC-NO-HARDCODED-SECRETS-1").is_some());
    // THE DEFECT: the malformed rule is gone from the corpus.
    assert!(
        corpus.get_by_id("SUPABASE-RLS-ENABLED-1").is_none(),
        "precondition: a file that does not parse really does leave the corpus"
    );
    // THE FIX: and it is now REPORTED rather than silently absent.
    assert_eq!(failures.len(), 1, "{failures:#?}");
    assert_eq!(failures[0].rule_id, "SUPABASE-RLS-ENABLED-1");
    assert!(
        failures[0].path.ends_with("supabase-rls-enabled-1.toml"),
        "the operator-facing path must name the actual file: {:?}",
        failures[0].path
    );
    assert!(
        !failures[0].error.is_empty(),
        "the parser's own message must survive, for the operator log"
    );
}

/// req_w13: the same thing through the function the SCAN path actually calls. Before the
/// fix `split_scannable_rules` returned a 4-tuple whose corpus came from
/// `load_corpus_lenient(..).await.0`, so this information had no channel at all.
#[tokio::test]
async fn req_w13_the_scan_rule_split_reports_corpus_load_failures_to_its_caller() {
    let Some(dir) = synthetic_corpus_with_one_malformed_rule(
        &["SEC-NO-RAW-SQL-CONCAT-1"],
        "SUPABASE-RLS-ENABLED-1",
    ) else {
        eprintln!("corpus unavailable; skipping req_w13 rule-split test");
        return;
    };
    let selected = vec![crate::onboard::SelectedRule {
        id: "SEC-NO-RAW-SQL-CONCAT-1".to_string(),
        directive: "no raw SQL built by string concatenation".to_string(),
        repos: Vec::new(),
    }];

    let (_scannable, _excluded, _preview, corpus, load_failures) =
        split_scannable_rules_from(dir.path(), selected).await;

    assert!(corpus.is_some(), "the partition still gets its corpus");
    assert_eq!(
        load_failures
            .iter()
            .map(|f| f.rule_id.as_str())
            .collect::<Vec<_>>(),
        vec!["SUPABASE-RLS-ENABLED-1"],
        "the scan path's own loader must hand the failures to its caller"
    );
}

/// req_w13: `record_corpus_load_failures` routes each failure to all four destinations, and
/// keeps the operator detail (path, parser message) OUT of the ledger's client-facing
/// `skip_reason` while keeping it available on the internal coverage note.
#[test]
fn req_w13_recording_a_failure_discloses_it_without_leaking_operator_detail_to_the_client() {
    // `ScanReport` has no public `Default`/`empty`; `gated` is simply the cheapest
    // constructor that yields a report with a fresh `ScanLedger` and empty
    // `coverage_notes`, which is all this test reads. Nothing here asserts over the
    // gated-ness itself.
    let mut report = crate::onboard::ScanReport::gated(&["me/api".to_string()]);
    let failures = vec![CorpusLoadFailure {
        rule_id: "SUPABASE-RLS-ENABLED-1".to_string(),
        path: std::path::PathBuf::from("/Users/auditor/corpus/supabase/rls/x.toml"),
        error: "TOML parse error in x.toml: unknown variant `architectual`".to_string(),
    }];

    record_corpus_load_failures(&mut report, &failures);

    // 1. the ledger
    let entry = report
        .ledger
        .rule("SUPABASE-RLS-ENABLED-1")
        .expect("the ledger must record it");
    assert!(!entry.ran);
    assert_eq!(
        entry.skip_reason.as_deref(),
        Some(crate::scan_ledger::CORPUS_LOAD_FAILED_REASON)
    );

    // 2. the gate
    let verdict = crate::integrity_gate::evaluate(
        &report.ledger,
        &crate::integrity_gate::IntegrityGateOptions::smoke(),
    );
    assert!(
        verdict
            .failures
            .iter()
            .any(|f| f.code == "corpus_load_failure"),
        "{:#?}",
        verdict.failures
    );

    // 3. the client line, via the coverage-note channel
    assert_eq!(report.coverage_notes.len(), 1);
    assert_eq!(report.coverage_notes[0].tool, RULE_CORPUS_COVERAGE_TOOL);

    // 4. the operator detail: on the note (UI + ledger), never in the ledger's reason.
    let note = &report.coverage_notes[0].message;
    assert!(
        note.contains("/Users/auditor/corpus/supabase/rls/x.toml") && note.contains("architectual"),
        "the operator must be able to find the file and the parse error: {note}"
    );
    let reason = entry.skip_reason.clone().unwrap_or_default();
    assert!(
        !reason.contains("/Users/auditor") && !reason.contains("architectual"),
        "the client-facing reason must carry NEITHER the path nor the parser text: {reason}"
    );
}

/// req_w13: the rule id for an unparseable file comes from its STEM, because the id
/// declared inside the file is unreadable by definition. The one error shape that is not a
/// per-file parse failure (the recursive walk itself failing, which reports the corpus
/// DIRECTORY) gets an explicit sentinel rather than a directory name dressed up as a rule.
#[test]
fn req_w13_the_rule_id_for_an_unparseable_file_comes_from_its_stem() {
    use std::path::Path;
    assert_eq!(
        rule_id_from_corpus_path(Path::new("/c/supabase/rls/supabase-rls-enabled-1.toml")),
        "SUPABASE-RLS-ENABLED-1"
    );
    assert_eq!(
        rule_id_from_corpus_path(Path::new("/c/universal/SEC-NO-RAW-SQL-CONCAT-1.TOML")),
        "SEC-NO-RAW-SQL-CONCAT-1",
        "the extension check is case-insensitive"
    );
    for not_a_rule_file in ["/c/principles", "/c/principles/api-layer", "/c/notes.md"] {
        assert_eq!(
            rule_id_from_corpus_path(Path::new(not_a_rule_file)),
            "RULE-CORPUS",
            "a non-`.toml` path must not be dressed up as a rule id ({not_a_rule_file})"
        );
    }
}
