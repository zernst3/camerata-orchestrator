//! Layer-3 reviewer findings over HTTP (gap 4): an open HIGH or MEDIUM finding blocks sign-off,
//! accepting one needs a reason, and an acceptance lands in the unit's decisions and history.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use camerata_worktracker::investigation::DecisionOutcome;
use camerata_worktracker::InMemoryStoryStore;
use http_body_util::BodyExt;
use tower::ServiceExt;

use crate::review_agent::{ReviewFinding, ReviewSeverity};
use crate::{router, AppState};

async fn send(app: &Router, uri: &str, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

fn finding(severity: ReviewSeverity, title: &str) -> ReviewFinding {
    ReviewFinding {
        severity,
        title: title.to_string(),
        scenario: "a stolen token keeps working".to_string(),
        rule: Some("SEC-1".to_string()),
        location: Some("auth.rs:9".to_string()),
    }
}

/// A state with one finished-looking run and one recorded finding on its unit.
fn state_with_finding(severity: ReviewSeverity) -> (AppState, String) {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let story = "acme/api#7";
    let run_id = state.runs.create(story, "t", crate::run::RunKind::Watched);
    state.uow.record_review_findings(
        story,
        &run_id,
        0,
        &[finding(severity, "Token never expires")],
    );
    (state, run_id)
}

#[tokio::test]
async fn sign_off_is_blocked_by_an_open_high_review_finding_and_allowed_after_accept_with_reason() {
    let (state, run_id) = state_with_finding(ReviewSeverity::High);
    let app = router(state.clone());

    let (status, body) = send(
        &app,
        &format!("/api/runs/{run_id}/sign-off"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        body["reason"],
        "1 reviewer finding(s) are still open. Fix them or record a decision on each before signing off."
    );
    assert!(
        state.uow.get_or_create("acme/api#7").sign_off.is_none(),
        "a blocked sign-off records nothing"
    );

    let (status, _) = send(
        &app,
        "/api/uow/acme%2Fapi%237/review-findings/0/accept",
        serde_json::json!({ "reason": "tracked in the auth hardening epic" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = send(
        &app,
        &format!("/api/runs/{run_id}/sign-off"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "sign-off goes through once accepted"
    );
    assert!(state.uow.get_or_create("acme/api#7").sign_off.is_some());
}

/// The reviewer settings arrive on the wire, and a client that only knows `{ enabled, model }`
/// (the older settings panel) never resets them. Applying a model profile keeps them too.
#[tokio::test]
async fn l3_review_settings_round_trip_and_survive_an_old_client_and_a_profile_change() {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let project = state
        .projects
        .create("P", vec!["acme/api".to_string()])
        .unwrap();
    let app = router(state.clone());
    let uri = format!("/api/projects/{}/l3-review", project.id);

    let (status, _) = send(
        &app,
        &uri,
        serde_json::json!({
            "enabled": true, "model": "",
            "on_error": "fail_closed", "fix_rounds": 2, "tier": "strongest"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let l3 = state.projects.get(&project.id).unwrap().l3_review;
    assert_eq!(l3.on_error, crate::project::ReviewOnError::FailClosed);
    assert_eq!(l3.fix_rounds, 2);
    assert_eq!(l3.tier, crate::project::ReviewTier::Strongest);

    // An old client posts only enabled + model: the policy fields are kept.
    let (status, _) = send(
        &app,
        &uri,
        serde_json::json!({ "enabled": true, "model": "claude-opus-5" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let l3 = state.projects.get(&project.id).unwrap().l3_review;
    assert_eq!(l3.model, "claude-opus-5");
    assert_eq!(l3.on_error, crate::project::ReviewOnError::FailClosed);
    assert_eq!(l3.fix_rounds, 2);

    // Applying a profile assigns the reviewer's on/off and model but keeps the policy.
    let (status, _) = send(
        &app,
        &format!("/api/projects/{}/model-profile", project.id),
        serde_json::json!({ "profile": "max_quality" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let l3 = state.projects.get(&project.id).unwrap().l3_review;
    assert!(l3.enabled, "the profile turned the reviewer on");
    assert_eq!(l3.on_error, crate::project::ReviewOnError::FailClosed);
    assert_eq!(l3.fix_rounds, 2);
    assert_eq!(l3.tier, crate::project::ReviewTier::Strongest);
}

#[tokio::test]
async fn a_low_review_finding_does_not_block_sign_off() {
    let (state, run_id) = state_with_finding(ReviewSeverity::Low);
    let app = router(state);
    let (status, _) = send(
        &app,
        &format!("/api/runs/{run_id}/sign-off"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn accept_requires_a_reason() {
    let (state, _run) = state_with_finding(ReviewSeverity::Medium);
    let app = router(state.clone());
    for body in [
        serde_json::json!({}),
        serde_json::json!({ "reason": "" }),
        serde_json::json!({ "reason": "   " }),
    ] {
        let (status, _) = send(
            &app,
            "/api/uow/acme%2Fapi%237/review-findings/0/accept",
            body,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    assert_eq!(
        state
            .uow
            .get_or_create("acme/api#7")
            .open_blocking_review_findings(),
        1,
        "a refused accept leaves the finding open"
    );
}

#[tokio::test]
async fn accept_refuses_an_unknown_index_and_a_finding_already_accepted() {
    let (state, _run) = state_with_finding(ReviewSeverity::High);
    let app = router(state);
    let (status, _) = send(
        &app,
        "/api/uow/acme%2Fapi%237/review-findings/5/accept",
        serde_json::json!({ "reason": "x" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    for expected in [StatusCode::OK, StatusCode::CONFLICT] {
        let (status, _) = send(
            &app,
            "/api/uow/acme%2Fapi%237/review-findings/0/accept",
            serde_json::json!({ "reason": "fine" }),
        )
        .await;
        assert_eq!(status, expected);
    }
}

#[tokio::test]
async fn accepting_a_review_finding_writes_an_approved_decision_and_a_history_line() {
    let (state, _run) = state_with_finding(ReviewSeverity::High);
    let app = router(state.clone());
    let (status, _) = send(
        &app,
        "/api/uow/acme%2Fapi%237/review-findings/0/accept",
        serde_json::json!({ "reason": "tracked in the auth hardening epic" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let uow = state.uow.get_or_create("acme/api#7");
    assert!(matches!(
        uow.review_findings[0].status,
        crate::uow::ReviewFindingStatus::Accepted { .. }
    ));
    assert!(uow.history.iter().any(|h| h.text
        == "Reviewer finding accepted: Token never expires. Reason: tracked in the auth hardening epic"));

    let decisions = state.uow.decisions_for("acme/api#7");
    assert_eq!(decisions.len(), 1);
    let d = &decisions[0];
    assert_eq!(d.label, "Reviewer finding accepted: Token never expires");
    assert_eq!(d.question, "a stolen token keeps working");
    assert_eq!(d.rationale, "tracked in the auth hardening epic");
    assert!(matches!(d.outcome, DecisionOutcome::Approved));
    assert!(d.artifact_id.starts_with("acme/api#7/decision/"));
}
