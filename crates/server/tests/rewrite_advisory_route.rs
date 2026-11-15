#![allow(clippy::unwrap_used)]
//! The operator's rewrite advisory route: the latest inspection's advisory, computed by the same
//! pure function `camerata inspect` writes beside the failure report; a refusal for a project
//! that does not exist or has no inspection yet. Synthetic data only.

#[path = "common/review.rs"]
mod review;

use std::sync::Arc;

use camerata_server::rewrite_advisory::{compute, AdvisoryDocument};
use camerata_server::{router, AppState};
use camerata_worktracker::InMemoryStoryStore;
use review::{call, review_fixture};

#[tokio::test]
async fn the_route_answers_the_latest_inspections_advisory() {
    let f = review_fixture(6);
    let (status, body) = call(
        &f.app,
        "GET",
        &format!("/api/projects/{}/rewrite-advisory", f.project_id),
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let doc: AdvisoryDocument = serde_json::from_str(&body).expect("the advisory document");
    let latest = f
        .state
        .inspections()
        .latest(&f.project_id)
        .expect("the fixture's inspection");
    let corpus_path = camerata_rules::corpus_path();
    let corpus = if corpus_path.exists() {
        Some(camerata_rules::load_corpus_lenient(&corpus_path).await.0)
    } else {
        None
    };
    assert_eq!(
        doc.rewrite_advisory,
        compute(&latest.report, corpus.as_ref()),
        "the route says what the file beside the failure report says"
    );
    assert!(!doc.rewrite_advisory.lines.is_empty());
    assert_eq!(doc.rewrite_advisory.code_lines, 10_000);
}

#[tokio::test]
async fn the_route_refuses_an_unknown_project_and_a_project_with_no_inspection() {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let project = state
        .projects()
        .create("Acme", vec!["acme/app".to_string()])
        .expect("project");
    let app = router(state);
    let (status, body) = call(&app, "GET", "/api/projects/nope/rewrite-advisory", None).await;
    assert_eq!(status, 404, "{body}");
    assert!(body.contains("no_project"), "{body}");
    let (status, body) = call(
        &app,
        "GET",
        &format!("/api/projects/{}/rewrite-advisory", project.id),
        None,
    )
    .await;
    assert_eq!(status, 409, "{body}");
    assert!(body.contains("no_inspection"), "{body}");
}
