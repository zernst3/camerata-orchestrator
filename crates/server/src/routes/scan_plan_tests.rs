//! #564: a client inspection started from the app needs a spend limit, a limit under the
//! projection refuses as the command line's does, and the pre-flight route shows the plan
//! `camerata inspect --plan-only` prints. No test here starts a run that could call a model: every
//! start is refused before anything is spawned, and the passing cases call the decision itself.

use axum::body::Body;
use axum::http::{header, Request};
use http_body_util::BodyExt;
use tower::ServiceExt;

use crate::client_source::test_support::{fixture, Fx};

/// The fixture's project as a client engagement on the API backend.
fn client_project() -> Fx {
    let fx = fixture();
    fx.state
        .projects
        .set_backend(&fx.project_id, crate::project::ProjectBackend::Api)
        .expect("project exists");
    fx.state
        .projects
        .set_client_engagement(&fx.project_id, true, false)
        .expect("project exists");
    fx
}

async fn post(fx: &Fx, uri: &str, body: serde_json::Value) -> serde_json::Value {
    let resp = crate::router(fx.state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        status,
        axum::http::StatusCode::OK,
        "{uri}: {}",
        String::from_utf8_lossy(&bytes)
    );
    serde_json::from_slice(&bytes).unwrap_or_else(|e| {
        panic!(
            "{uri} answered with no JSON ({e}): {}",
            String::from_utf8_lossy(&bytes)
        )
    })
}

fn request(limit: Option<f64>) -> serde_json::Value {
    serde_json::json!({ "repos": ["acme/app"], "rules": [], "max_cost_usd": limit })
}

/// Start through `POST /api/onboard/audit/start` and read the job back.
async fn start(fx: &Fx, limit: Option<f64>) -> crate::jobs::JobState {
    let body = post(fx, "/api/onboard/audit/start", request(limit)).await;
    let id = body["job_id"].as_str().expect("a job id").to_string();
    fx.state.jobs.get(&id).expect("the job")
}

fn audit_req(limit: Option<f64>) -> crate::AuditReq {
    serde_json::from_value(request(limit)).unwrap()
}

const NEEDS_LIMIT: &str = "needs a spend limit";

#[tokio::test]
async fn a_client_inspection_with_a_blank_limit_does_not_start() {
    let fx = client_project();
    let job = start(&fx, None).await;
    let why = job.message.clone().unwrap_or_default();
    assert_eq!(job.status, "failed", "{why}");
    assert!(why.contains(NEEDS_LIMIT), "{why}");
}

#[tokio::test]
async fn a_client_inspection_with_a_zero_limit_does_not_start() {
    let fx = client_project();
    let job = start(&fx, Some(0.0)).await;
    let why = job.message.clone().unwrap_or_default();
    assert_eq!(job.status, "failed", "{why}");
    assert!(why.contains(NEEDS_LIMIT), "{why}");
}

#[tokio::test]
async fn a_client_inspection_with_a_negative_limit_does_not_start() {
    let fx = client_project();
    let job = start(&fx, Some(-5.0)).await;
    let why = job.message.clone().unwrap_or_default();
    assert_eq!(job.status, "failed", "{why}");
    assert!(why.contains(NEEDS_LIMIT), "{why}");
}

#[tokio::test]
async fn a_limit_under_the_projection_does_not_start() {
    let fx = client_project();
    let job = start(&fx, Some(0.0001)).await;
    let why = job.message.clone().unwrap_or_default();
    assert_eq!(job.status, "failed", "{why}");
    assert!(why.contains("under the projection"), "{why}");
    assert!(!why.contains(NEEDS_LIMIT), "{why}");
}

/// The synchronous route is a start too: the same refusal, as its refusal report.
#[tokio::test]
async fn the_synchronous_start_refuses_a_blank_limit_and_a_limit_under_the_projection() {
    let fx = client_project();
    let report = post(&fx, "/api/onboard/audit", request(None)).await;
    assert_eq!(report["blocked"], true, "{report}");
    assert!(
        report["message"]
            .as_str()
            .unwrap_or("")
            .contains(NEEDS_LIMIT),
        "{report}"
    );
    let report = post(&fx, "/api/onboard/audit", request(Some(0.0001))).await;
    assert_eq!(report["blocked"], true, "{report}");
    assert!(
        report["message"]
            .as_str()
            .unwrap_or("")
            .contains("under the projection"),
        "{report}"
    );
}

/// Not a client engagement: no limit is needed (the switch is what asks for one).
#[tokio::test]
async fn an_internal_inspection_needs_no_spend_limit() {
    let fx = client_project();
    fx.state
        .projects
        .set_client_engagement(&fx.project_id, false, false)
        .expect("project exists");
    let project = fx.state.projects.get(&fx.project_id);
    assert_eq!(
        super::start_spend_refusal(&fx.state, project.as_ref(), &audit_req(None)).await,
        None
    );
    assert_eq!(super::client_limit_refusal(false, None), None);
    assert_eq!(super::client_limit_refusal(false, Some(0.0)), None);
}

/// A limit above the projection passes the spend checks; the start goes on to its others.
#[tokio::test]
async fn a_limit_above_the_projection_passes_the_spend_checks() {
    let fx = client_project();
    let project = fx.state.projects.get(&fx.project_id);
    assert_eq!(
        super::start_spend_refusal(&fx.state, project.as_ref(), &audit_req(Some(1000.0))).await,
        None
    );
    assert_eq!(super::client_limit_refusal(true, Some(0.01)), None);
}

/// A run with the AI review off calls no model, so it has nothing to project and is not refused
/// for a small limit.
#[tokio::test]
async fn a_run_with_no_ai_review_is_not_refused_for_its_projection() {
    let fx = client_project();
    let project = fx.state.projects.get(&fx.project_id);
    let mut req = audit_req(Some(0.0001));
    req.run_ai_review = false;
    assert_eq!(
        super::start_spend_refusal(&fx.state, project.as_ref(), &req).await,
        None
    );
}

/// The pre-flight route: the plan's figures and wording, the suggested limit, the warnings and
/// what Start would say.
#[tokio::test]
async fn the_plan_route_shows_the_plan_and_what_start_would_say() {
    let fx = client_project();
    crate::test_git::write_file(
        &fx.checkout,
        "seeds/big.js",
        &"x".repeat(crate::onboard::files::MAX_FILE_BYTES + 1),
    );
    let body = post(&fx, "/api/onboard/plan", request(None)).await;
    assert_eq!(body["ok"], true, "{body}");
    let plan = &body["plan"];
    assert_eq!(plan["repos"][0]["repo"], "acme/app", "{body}");
    assert!(plan["repos"][0]["files"].as_u64().unwrap() > 0, "{body}");
    assert!(plan["total_lines"].as_u64().unwrap() > 0, "{body}");
    assert!(
        plan["repos"][0]["left_out"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["files"] == 1),
        "the file over the read limit is left out: {body}"
    );
    assert!(plan["projected_usd"].as_f64().unwrap() > 0.0, "{body}");
    assert!(plan["full_scan"]["limit"].as_f64().unwrap() > 0.0, "{body}");
    let text = plan["text"].as_str().unwrap();
    for want in [
        "== ADMISSION (what a real run would read) ==",
        "== PRE-FLIGHT (as a real run prints it) ==",
        "Suggested spend limit: $",
        "spend limit it would run under: none",
    ] {
        assert!(text.contains(want), "missing {want:?} in:\n{text}");
    }
    assert!(
        plan["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap_or("").contains("seeds/big.js")),
        "the file over the read limit is a warning: {body}"
    );
    assert!(
        body["start_refusal"]
            .as_str()
            .unwrap_or("")
            .contains(NEEDS_LIMIT),
        "{body}"
    );

    let body = post(&fx, "/api/onboard/plan", request(Some(0.0001))).await;
    assert!(
        body["start_refusal"]
            .as_str()
            .unwrap_or("")
            .contains("under the projection"),
        "{body}"
    );
    let body = post(&fx, "/api/onboard/plan", request(Some(1000.0))).await;
    assert_eq!(body["start_refusal"], serde_json::Value::Null, "{body}");
    assert!(
        body["plan"]["text"]
            .as_str()
            .unwrap()
            .contains("spend limit it would run under: $1000.00"),
        "{body}"
    );
}
