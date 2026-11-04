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
