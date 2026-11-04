use std::cell::Cell;

use camerata_api_types::decisions::{DecisionState, NewDecisionReq, Scope};
use camerata_api_types::edits::{EditField, NewEditReq};
use camerata_api_types::review_view::{ReviewViewError, TreeView, SITES_PAGE_SIZE};
use camerata_ui_core::test_support::review_fixtures::{dataset_30k, dataset_base};
use camerata_ui_core::tree::state::{TreeIndex, TreeState};
use wiremock::matchers::{body_json, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::review_source::FixtureReviewSource;

fn http(server: &MockServer) -> HttpReviewSource {
    HttpReviewSource::new(server.uri(), "p1".to_string())
}

#[tokio::test]
async fn the_http_source_reads_the_tree_from_the_review_route() {
    let server = MockServer::start().await;
    let tree = dataset_base().tree;
    Mock::given(method("GET"))
        .and(path("/api/projects/p1/review/tree"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&tree))
        .expect(1)
        .mount(&server)
        .await;
    let got = http(&server).tree().await.unwrap();
    assert_eq!(got, tree);
}

#[tokio::test]
async fn a_pinned_run_is_sent_as_run_id() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/projects/p1/review/tree"))
        .and(query_param("run_id", "insp-9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(dataset_base().tree))
        .expect(1)
        .mount(&server)
        .await;
    let src = http(&server).for_run("insp-9");
    assert!(src.tree().await.is_ok());
}

#[tokio::test]
async fn sites_come_with_the_node_the_cursor_and_the_limit() {
    let server = MockServer::start().await;
    let ds = dataset_base();
    let node = ds.tree.nodes[0].id.clone();
    let page = ds.sites_page(&node, None, 10).unwrap();
    Mock::given(method("GET"))
        .and(path("/api/projects/p1/review/sites"))
        .and(query_param("node", node.as_str()))
        .and(query_param("cursor", "abcd"))
        .and(query_param("limit", "10"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&page))
        .expect(1)
        .mount(&server)
        .await;
    let got = http(&server)
        .sites_page(&node, Some("abcd"), 10)
        .await
        .unwrap();
    assert_eq!(got, page);
}

#[tokio::test]
async fn a_refusal_comes_back_as_the_servers_own_words() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/projects/p1/review/tree"))
        .respond_with(
            ResponseTemplate::new(409).set_body_json(ReviewViewError {
                ok: false,
                code: "no_inspection".to_string(),
                message: "This project has no completed inspection to review. Run an audit first."
                    .to_string(),
            }),
        )
        .mount(&server)
        .await;
    let err = http(&server).tree().await.unwrap_err();
    assert_eq!(
        err.message,
        "This project has no completed inspection to review. Run an audit first."
    );
}

#[tokio::test]
async fn a_body_that_is_not_a_refusal_gets_the_status() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/projects/p1/review/tree"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&server)
        .await;
    let err = http(&server).tree().await.unwrap_err();
    assert_eq!(err.message, "The request failed with status 500.");
}

#[tokio::test]
async fn a_server_that_is_not_there_is_named() {
    let src = HttpReviewSource::new("http://127.0.0.1:1".to_string(), "p1".to_string());
    let err = src.tree().await.unwrap_err();
    assert!(
        err.message.contains("did not answer at http://127.0.0.1:1"),
        "{err}"
    );
}

#[tokio::test]
async fn a_decision_is_posted_as_json_and_the_record_comes_back() {
    let server = MockServer::start().await;
    let req = NewDecisionReq {
        scope: Scope::Node("sub:X/1".to_string()),
        key: String::new(),
        state: DecisionState::Confirmed,
        reason: "same pattern".to_string(),
        decided_by: String::new(),
        needed_me: None,
        sample_verdict: None,
        except: vec!["k1".to_string()],
        run_id: "insp-1".to_string(),
        decided_at: String::new(),
    };
    let writes = RecordingWrites::default();
    let recorded = writes.post_decision(req.clone()).await.unwrap();
    Mock::given(method("POST"))
        .and(path("/api/projects/p1/decisions"))
        .and(body_json(&req))
        .respond_with(ResponseTemplate::new(200).set_body_json(&recorded))
        .expect(1)
        .mount(&server)
        .await;
    let got = http(&server).post_decision(req).await.unwrap();
    assert_eq!(got, recorded);
}

#[tokio::test]
async fn the_settings_writes_go_to_their_routes_and_a_budget_request_carries_the_locked_reason() {
    use camerata_api_types::review::EscalationReason;
    use camerata_api_types::review_view::SampleSizeReq;
    use camerata_ui_core::review_settings::{escalation_req, SettingsDraft};

    let server = MockServer::start().await;
    let view = dataset_base().settings;
    // Every other reason switched off: the evidence-unverified one still rides along.
    let mut draft = SettingsDraft::from_view(&view);
    draft.reasons.clear();
    draft.rate = "2.5".to_string();
    let req = escalation_req(&draft, &view.limits).unwrap();
    assert_eq!(
        req.over_budget_reasons,
        [EscalationReason::EvidenceUnverified]
    );
    assert!(
        serde_json::to_string(&req)
            .unwrap()
            .contains("evidence_unverified"),
        "on the wire"
    );
    let outcome = FixtureReviewSource::base()
        .preview_budget(req.clone())
        .await
        .unwrap();
    for (route, applied) in [
        ("escalation-budget/preview", false),
        ("escalation-budget", true),
    ] {
        let mut body = outcome.clone();
        body.applied = applied;
        Mock::given(method("POST"))
            .and(path(format!("/api/projects/p1/{route}")))
            .and(body_json(&req))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .expect(1)
            .mount(&server)
            .await;
    }
    Mock::given(method("POST"))
        .and(path("/api/projects/p1/sample-size"))
        .and(body_json(SampleSizeReq {
            sample_size_per_run: 0,
        }))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            camerata_api_types::review_view::SampleSizeResp {
                sample_size_per_run: 0,
                lane_off: true,
            },
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/projects/p1/step-models"))
        .and(body_json(
            serde_json::json!({ "step": "group_review", "model": "claude-opus-5-5" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": true })))
        .expect(1)
        .mount(&server)
        .await;
    let src = http(&server);
    assert!(!src.preview_budget(req.clone()).await.unwrap().applied);
    assert!(src.apply_budget(req).await.unwrap().applied);
    assert!(src.set_sample_size(0).await.unwrap().lane_off);
    src.set_stage_model("group_review", "claude-opus-5-5")
        .await
        .unwrap();
}

#[tokio::test]
async fn a_step_models_refusal_arrives_with_a_200_and_is_shown_as_the_servers_message() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/projects/p1/step-models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            serde_json::json!({ "ok": false, "message": "model must not be empty" }),
        ))
        .mount(&server)
        .await;
    let err = http(&server)
        .set_stage_model("audit", " ")
        .await
        .unwrap_err();
    assert_eq!(err.message, "model must not be empty");
    // A reply with neither says so in words rather than passing as saved.
    assert_eq!(
        step_models_outcome(&serde_json::json!({}))
            .unwrap_err()
            .message,
        "The model was not saved."
    );
    assert!(step_models_outcome(&serde_json::json!({ "ok": true })).is_ok());
}

#[tokio::test]
async fn the_recording_writes_log_the_settings_writes_in_order() {
    let writes = RecordingWrites::default();
    let req = camerata_api_types::review_view::EscalationBudgetReq {
        rate_per_kloc: 2.0,
        floor: 20,
        over_budget_reasons: Vec::new(),
    };
    writes.preview_budget(req.clone()).await.unwrap();
    writes.apply_budget(req.clone()).await.unwrap();
    writes.set_sample_size(5).await.unwrap();
    writes.set_stage_model("fix", "m").await.unwrap();
    assert_eq!(
        *writes.log.borrow(),
        [
            Written::PreviewBudget(req.clone()),
            Written::ApplyBudget(req),
            Written::SampleSize(5),
            Written::StageModel("fix".to_string(), "m".to_string()),
        ]
    );
    *writes.refuse.borrow_mut() = Some("read only".to_string());
    assert!(writes.set_sample_size(1).await.is_err());
    assert_eq!(
        writes.log.borrow().len(),
        4,
        "a refused write is not logged"
    );
}

#[tokio::test]
async fn an_undo_posts_a_revoke_and_an_edit_posts_to_the_edits_route() {
    let server = MockServer::start().await;
    let revoke = RevokeResp {
        revoked: vec!["dec-000007".to_string()],
        revoked_at: "2026-11-04T09:00:05Z".to_string(),
    };
    Mock::given(method("POST"))
        .and(path("/api/projects/p1/decisions/dec-000007/revoke"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&revoke))
        .expect(1)
        .mount(&server)
        .await;
    let req = NewEditReq {
        key: "rule:SEC-1".to_string(),
        field: EditField::Summary,
        new: "x".to_string(),
        reason: "r".to_string(),
        by: String::new(),
        run_id: String::new(),
    };
    let writes = RecordingWrites::default();
    let recorded = writes.post_edit(req.clone()).await.unwrap();
    Mock::given(method("POST"))
        .and(path("/api/projects/p1/edits"))
        .and(body_json(&req))
        .respond_with(ResponseTemplate::new(200).set_body_json(&recorded))
        .expect(1)
        .mount(&server)
        .await;
    let src = http(&server);
    assert_eq!(src.revoke_decision("dec-000007").await.unwrap(), revoke);
    assert_eq!(src.post_edit(req).await.unwrap(), recorded);
}

#[tokio::test]
async fn the_recording_writes_keep_what_they_were_asked_and_can_refuse() {
    let writes = RecordingWrites::default();
    let req = NewEditReq {
        key: "n".to_string(),
        field: EditField::Summary,
        new: "x".to_string(),
        reason: "r".to_string(),
        by: String::new(),
        run_id: String::new(),
    };
    writes.post_edit(req.clone()).await.unwrap();
    writes.revoke_decision("dec-1").await.unwrap();
    assert_eq!(
        *writes.log.borrow(),
        vec![
            Written::Edit(req.clone()),
            Written::Revoke("dec-1".to_string())
        ]
    );
    *writes.refuse.borrow_mut() = Some("read only".to_string());
    assert_eq!(
        writes.post_edit(req).await.unwrap_err().message,
        "read only"
    );
}

#[test]
fn the_api_handle_compares_by_the_source_it_shares() {
    let a = ReviewApi::http("http://x".to_string(), "p".to_string());
    let b = a.clone();
    let c = ReviewApi::http("http://x".to_string(), "p".to_string());
    assert!(a == b);
    assert!(a != c);
}

#[test]
fn a_refusal_is_read_whatever_route_wrote_it() {
    use camerata_api_types::decisions::DecisionError;
    use camerata_api_types::edits::EditError;
    let d = serde_json::to_vec(&DecisionError {
        ok: false,
        code: "reason_required".to_string(),
        message: "a decision on a group always needs a reason".to_string(),
    })
    .unwrap();
    assert_eq!(
        refusal_message(422, &d),
        "a decision on a group always needs a reason"
    );
    let e = serde_json::to_vec(&EditError {
        ok: false,
        code: "no_change".to_string(),
        message: "the new value is the one the field already has".to_string(),
    })
    .unwrap();
    assert_eq!(
        refusal_message(422, &e),
        "the new value is the one the field already has"
    );
}

/// Counts the sites reads a host makes.
struct Counting {
    inner: FixtureReviewSource,
    pages: Cell<usize>,
}

impl ReviewSource for Counting {
    fn shell(&self) -> SourceFuture<'_, ReviewShellView> {
        self.inner.shell()
    }
    fn tree(&self) -> SourceFuture<'_, TreeView> {
        self.inner.tree()
    }
    fn sites_page<'a>(
        &'a self,
        node: &'a str,
        cursor: Option<&'a str>,
        limit: usize,
    ) -> SourceFuture<'a, SitesPage> {
        self.pages.set(self.pages.get() + 1);
        self.inner.sites_page(node, cursor, limit)
    }
    fn queue(&self, a: bool, b: usize, c: usize) -> SourceFuture<'_, QueueView> {
        self.inner.queue(a, b, c)
    }
    fn engine_check(&self) -> SourceFuture<'_, EngineCheckLane> {
        self.inner.engine_check()
    }
    fn export_gate(&self) -> SourceFuture<'_, ExportGateView> {
        self.inner.export_gate()
    }
    fn settings(&self) -> SourceFuture<'_, ReviewSettingsView> {
        self.inner.settings()
    }
}

#[tokio::test]
async fn tree_never_requests_more_than_one_sites_page_per_open() {
    // Driven the way the host drives it: the state says what to read, the source is asked once.
    let source = Counting {
        inner: FixtureReviewSource::scale_30k(),
        pages: Cell::new(0),
    };
    let tree = source.tree().await.unwrap();
    let index = TreeIndex::new(&tree);
    let big = tree
        .nodes
        .iter()
        .filter(|n| {
            n.counts.sites > SITES_PAGE_SIZE
                && n.level == camerata_api_types::review_view::TreeLevel::Subgroup
        })
        .max_by_key(|n| n.counts.sites)
        .expect("a sub-group of more than one page")
        .id
        .clone();
    let mut state = TreeState::default();
    let requests = state.open_path(&index, &big);
    assert_eq!(requests.len(), 1);
    for req in &requests {
        let page = fetch_page(&source, req).await.unwrap();
        state.apply_page(&page);
    }
    assert_eq!(source.pages.get(), 1, "one page for one open");
    // Opening, closing and opening again reads nothing more.
    assert!(state.open_node(&index, &big).is_empty());
    state.close_node(&index, &big);
    assert!(state.open_node(&index, &big).is_empty());
    assert_eq!(source.pages.get(), 1);
    // "Show next 500" is one more page, once.
    let more = state.request_more(&big).unwrap();
    assert!(state.request_more(&big).is_none());
    let page = fetch_page(&source, &more).await.unwrap();
    state.apply_page(&page);
    assert_eq!(source.pages.get(), 2);
    let total = tree
        .nodes
        .iter()
        .find(|n| n.id == big)
        .unwrap()
        .counts
        .sites;
    assert_eq!(
        state.loaded[&big].rows.len(),
        (2 * SITES_PAGE_SIZE).min(total),
        "two pages are loaded and nothing more"
    );
}

#[tokio::test]
async fn a_batch_plan_reads_every_page_of_the_node_but_holds_none_of_it() {
    let ds = dataset_30k();
    let node = ds
        .tree
        .nodes
        .iter()
        .filter(|n| n.level == camerata_api_types::review_view::TreeLevel::Category)
        .max_by_key(|n| n.counts.sites)
        .unwrap();
    let total = node.counts.sites;
    let source = Counting {
        inner: FixtureReviewSource::scale_30k(),
        pages: Cell::new(0),
    };
    let mut seen = Vec::new();
    let rows = read_all_sites(&source, &node.id, |read, of| seen.push((read, of)))
        .await
        .unwrap();
    assert_eq!(rows.len(), total, "every site of the node");
    assert_eq!(source.pages.get(), total.div_ceil(SITES_PAGE_SIZE));
    assert_eq!(seen.len(), source.pages.get(), "told after each page");
    assert_eq!(seen.last(), Some(&(total, total)));
}

#[tokio::test]
async fn a_read_that_fails_part_way_fails_the_plan() {
    let source = FixtureReviewSource::failing("the server is down");
    let err = read_all_sites(&source, "cat:authorization", |_, _| {})
        .await
        .unwrap_err();
    assert_eq!(err.message, "the server is down");
}

// ── The site detail's writes (spec section 8) ─────────────────────────────────────────────

fn an_edit(id: &str) -> Edit {
    Edit {
        id: id.to_string(),
        key: "k".to_string(),
        field: EditField::Headline,
        old: "Old".to_string(),
        new: "New".to_string(),
        reason: "r".to_string(),
        by: "zach".to_string(),
        at: "2026-11-04T09:00:00Z".to_string(),
        run_id: "insp-1".to_string(),
        fingerprint_at_edit: "f".to_string(),
        stale: false,
        discarded_at: None,
        reapplied_from: None,
    }
}

#[tokio::test]
async fn a_discard_and_a_reapply_post_to_the_edit_s_own_routes() {
    use camerata_api_types::edits::EditDiscarded;
    let server = MockServer::start().await;
    let mut gone = an_edit("edit-000001");
    gone.discarded_at = Some("2026-11-04T09:00:05Z".to_string());
    Mock::given(method("POST"))
        .and(path("/api/projects/p1/edits/edit-000001/discard"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(EditDiscarded { edit: gone.clone() }),
        )
        .expect(1)
        .mount(&server)
        .await;
    let mut again = an_edit("edit-000002");
    again.reapplied_from = Some("edit-000001".to_string());
    Mock::given(method("POST"))
        .and(path("/api/projects/p1/edits/edit-000001/reapply"))
        .respond_with(ResponseTemplate::new(200).set_body_json(EditRecorded {
            edit: again.clone(),
        }))
        .expect(1)
        .mount(&server)
        .await;
    let src = http(&server);
    assert_eq!(src.discard_edit("edit-000001").await.unwrap().edit, gone);
    assert_eq!(src.reapply_edit("edit-000001").await.unwrap().edit, again);
}

#[tokio::test]
async fn an_added_finding_posts_the_form_and_a_refusal_comes_back_in_the_servers_words() {
    use camerata_api_types::edits::{AddFindingReq, FindingAdded};
    let server = MockServer::start().await;
    let req = AddFindingReq {
        repo: "acme/app".to_string(),
        path: "web/env.ts".to_string(),
        line: 3,
        rule_id: "SEC-KEY-2".to_string(),
        severity: "high".to_string(),
        snippet: "const k = import.meta.env.VITE_KEY;".to_string(),
        headline: "Key in a public variable".to_string(),
        detail: "It ships to the browser.".to_string(),
        fix: String::new(),
        kind: Some(camerata_api_types::audit::FindingKind::Defect),
        category: None,
        reason: "missed".to_string(),
        by: String::new(),
    };
    let held = FindingAdded {
        key: "k".to_string(),
        evidence_verified: false,
        evidence_note: "The quoted code was not found near line 3.".to_string(),
    };
    Mock::given(method("POST"))
        .and(path("/api/projects/p1/added-findings"))
        .and(body_json(&req))
        .respond_with(ResponseTemplate::new(200).set_body_json(&held))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(http(&server).add_finding(req.clone()).await.unwrap(), held);

    // An incomplete form is a 422 with the store's own words.
    let refused = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/projects/p1/added-findings"))
        .respond_with(ResponseTemplate::new(422).set_body_json(
            camerata_api_types::edits::EditError {
                ok: false,
                code: "added_finding_incomplete".to_string(),
                message: "a finding the reviewer adds needs a rule id".to_string(),
            },
        ))
        .mount(&refused)
        .await;
    let err = http(&refused).add_finding(req).await.unwrap_err();
    assert_eq!(err.message, "a finding the reviewer adds needs a rule id");
}

#[tokio::test]
async fn the_client_acknowledgement_posts_who_and_the_note_on_the_decision() {
    use camerata_api_types::decisions::ClientAck;
    use camerata_api_types::edits::{ClientAckRecorded, ClientAckReq};
    let server = MockServer::start().await;
    let ack = ClientAckReq {
        by: "cto@acme.test".to_string(),
        note: "signed off".to_string(),
    };
    let dec = FixtureReviewSource::base()
        .recorded
        .post_decision(NewDecisionReq {
            scope: Scope::Site,
            key: "k".to_string(),
            state: DecisionState::Ignored,
            reason: "behind the proxy".to_string(),
            decided_by: String::new(),
            needed_me: None,
            sample_verdict: None,
            except: Vec::new(),
            run_id: String::new(),
            decided_at: String::new(),
        })
        .await
        .unwrap()
        .decision;
    let mut acked = dec.clone();
    acked.confirmed_by_client = Some(ClientAck {
        by: ack.by.clone(),
        at: "2026-11-05T10:00:00Z".to_string(),
        note: ack.note.clone(),
    });
    let body = ClientAckRecorded {
        decision: acked,
        also_acknowledged: 0,
    };
    Mock::given(method("POST"))
        .and(path("/api/projects/p1/decisions/dec-000001/client-ack"))
        .and(body_json(&ack))
        .respond_with(ResponseTemplate::new(200).set_body_json(&body))
        .expect(1)
        .mount(&server)
        .await;
    let got = http(&server).client_ack("dec-000001", ack).await.unwrap();
    assert_eq!(got, body);
}

#[tokio::test]
async fn the_source_window_is_read_from_finding_context_for_this_project() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/onboard/finding-context"))
        .and(query_param("repo", "acme/app"))
        .and(query_param("path", "web/env.ts"))
        .and(query_param("line", "3"))
        .and(query_param("project", "p1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "ok", "lines": ["a", "b", "c"], "start_line": 2, "end_line": 4,
            "violation_line": 3, "kind": "function", "language": "ts", "matches_snippet": true
        })))
        .expect(1)
        .mount(&server)
        .await;
    let got = http(&server)
        .source_window("acme/app", "web/env.ts", 3)
        .await
        .unwrap();
    assert_eq!(got.start_line, 2);
    assert_eq!(got.lines, ["a", "b", "c"]);

    // The server's one-line reason is what the form shows when the code cannot be read.
    let gone = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/onboard/finding-context"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "file_missing", "reason": "file not found, changed since scan?"
        })))
        .mount(&gone)
        .await;
    let err = http(&gone)
        .source_window("acme/app", "web/env.ts", 3)
        .await
        .unwrap_err();
    assert_eq!(err.message, "file not found, changed since scan?");
}

#[tokio::test]
async fn the_recording_writes_keep_the_detail_s_writes_in_order_and_can_refuse() {
    use camerata_api_types::edits::{AddFindingReq, ClientAckReq};
    let writes = RecordingWrites::default();
    let add = AddFindingReq {
        repo: "r".to_string(),
        path: "p".to_string(),
        line: 1,
        rule_id: "X".to_string(),
        severity: "low".to_string(),
        snippet: "s".to_string(),
        headline: "h".to_string(),
        detail: "d".to_string(),
        fix: String::new(),
        kind: Some(camerata_api_types::audit::FindingKind::Defect),
        category: None,
        reason: "why".to_string(),
        by: String::new(),
    };
    let ack = ClientAckReq {
        by: "c".to_string(),
        note: String::new(),
    };
    writes.discard_edit("edit-000001").await.unwrap();
    writes.reapply_edit("edit-000002").await.unwrap();
    let added = writes.add_finding(add.clone()).await.unwrap();
    assert!(added.evidence_verified);
    writes.client_ack("dec-000009", ack.clone()).await.unwrap();
    assert_eq!(
        *writes.log.borrow(),
        vec![
            Written::DiscardEdit("edit-000001".to_string()),
            Written::ReapplyEdit("edit-000002".to_string()),
            Written::AddFinding(add.clone()),
            Written::ClientAck("dec-000009".to_string(), ack.clone()),
        ]
    );
    *writes.refuse.borrow_mut() = Some("read only".to_string());
    assert_eq!(
        writes.add_finding(add).await.unwrap_err().message,
        "read only"
    );
    assert_eq!(
        writes.client_ack("d", ack).await.unwrap_err().message,
        "read only"
    );
    assert_eq!(
        writes.log.borrow().len(),
        4,
        "a refused write is not logged"
    );
}

#[tokio::test]
async fn a_write_side_that_predates_the_detail_refuses_in_words() {
    struct Old;
    impl ReviewWrites for Old {
        fn post_decision(&self, _: NewDecisionReq) -> SourceFuture<'_, DecisionRecorded> {
            unreachable!()
        }
        fn revoke_decision<'a>(&'a self, _: &'a str) -> SourceFuture<'a, RevokeResp> {
            unreachable!()
        }
        fn post_edit(&self, _: NewEditReq) -> SourceFuture<'_, EditRecorded> {
            unreachable!()
        }
        fn preview_budget(
            &self,
            _: camerata_api_types::review_view::EscalationBudgetReq,
        ) -> SourceFuture<'_, camerata_api_types::review_view::EscalationBudgetOutcome> {
            unreachable!()
        }
        fn apply_budget(
            &self,
            _: camerata_api_types::review_view::EscalationBudgetReq,
        ) -> SourceFuture<'_, camerata_api_types::review_view::EscalationBudgetOutcome> {
            unreachable!()
        }
        fn set_sample_size(
            &self,
            _: usize,
        ) -> SourceFuture<'_, camerata_api_types::review_view::SampleSizeResp> {
            unreachable!()
        }
        fn set_stage_model<'a>(&'a self, _: &'a str, _: &'a str) -> SourceFuture<'a, ()> {
            unreachable!()
        }
    }
    let err = Old.discard_edit("e").await.unwrap_err();
    assert!(
        err.message.contains("cannot do discarding an edit"),
        "{err}"
    );
}

#[tokio::test]
async fn the_fixture_source_serves_a_window_and_answers_the_detail_writes() {
    let fixture = FixtureReviewSource::base();
    let row = dataset_base().rows[0].clone();
    let window = fixture
        .source_window(&row.repo, &row.path, row.line)
        .await
        .unwrap();
    let at = row.line - window.start_line;
    assert_eq!(
        window.lines[at].trim(),
        row.snippet.lines().next().unwrap_or_default().trim(),
        "the line asked about is the site's own snippet"
    );
    fixture.discard_edit("edit-000001").await.unwrap();
    assert!(fixture.calls().contains(&"discard_edit".to_string()));
}
