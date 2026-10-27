//! Deleted source stays deleted: no route may quietly bring a repo's folder back.
//!
//! Every test deletes the source of `acme/app` inside a sandbox, then drives one other route
//! that can write into a repo folder (emit-local, apply, generate-ci-workflow, the scan gate)
//! and proves it neither recreates the folder nor flips the status away from `deleted`.
//!
//! Git's network steps never leave the machine: a per-thread `url.<file>.insteadOf` rewrite of
//! the authed GitHub URL to the fixture's local bare remote, so a route that DOES resurrect the
//! folder is observable offline, and a route that does not is simply not asked.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use super::delete::{delete_source, DeleteOutcome, DeleteRequest};
use super::status::{source_status, RepoSourceStatus};
use super::test_support::{fixture, Fx, REPO};
use crate::project::RuleSelection;

const RULE: &str = "ARCH-CURRENCY-CONSISTENCY-1";
const TOKEN: &str = "test-token-never-sent-anywhere";

/// Redirect `git clone https://x-access-token:<token>@github.com/acme/app.git` to the
/// fixture's local bare remote. Hold the guard for the whole test. The rewrite is set on this
/// test thread's network commands (`workspace::test_seams`), not on the process environment, so
/// no other test's git can ever see it.
fn redirect_github_clone(fx: &Fx) -> crate::workspace::test_seams::RewriteGuard {
    // Q1: this must never reach the process environment, which every git in every parallel
    // test inherits.
    assert!(
        std::env::var_os("GIT_CONFIG_COUNT").is_none(),
        "a test set GIT_CONFIG_COUNT on the process"
    );
    crate::workspace::test_seams::rewrite_remote(
        &format!("https://x-access-token:{TOKEN}@github.com/{REPO}.git"),
        &fx.remote_url,
    )
}

async fn call(
    state: &crate::AppState,
    method: &str,
    uri: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let resp = crate::router(state.clone())
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, json)
}

/// Place a repo-local rule on `acme/app`, so emit-local has something to write.
fn select_a_rule(fx: &Fx) {
    fx.state
        .projects
        .update(&fx.project_id, |p| {
            p.ruleset.selections.push(RuleSelection {
                rule_id: RULE.to_string(),
                chosen_option: None,
                repos: vec![REPO.to_string()],
                ..Default::default()
            });
        })
        .unwrap();
    fx.state.projects.set_active(&fx.project_id);
}

async fn delete_the_source(fx: &Fx) {
    let outcome = delete_source(
        &fx.state,
        &fx.project_id,
        REPO,
        &DeleteRequest {
            confirm: REPO.to_string(),
            include_uncommitted: false,
        },
    )
    .await;
    assert!(matches!(
        outcome,
        Ok(DeleteOutcome::Done { partial: false, .. })
    ));
    assert!(!fx.checkout.exists());
}

async fn still_deleted(fx: &Fx) {
    assert!(!fx.checkout.exists(), "the folder came back");
    assert!(
        !fx.checkout.parent().unwrap().join("app").exists(),
        "the folder came back"
    );
    let status = source_status(&fx.state, &fx.project_id, REPO).await;
    assert!(
        matches!(status, RepoSourceStatus::Deleted { .. }),
        "the status flipped away from deleted: {status:?}"
    );
}

/// H1: "Emit rules locally" re-cloned the client repo through `apply_local`. The gate refuses
/// it up front; the body names no repos, so the scope is every repo of the project.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn emit_local_after_deletion_is_refused_and_does_not_recreate_the_folder() {
    let fx = fixture();
    let _redirect = redirect_github_clone(&fx);
    select_a_rule(&fx);
    delete_the_source(&fx).await;

    let (status, body) = call(
        &fx.state,
        "POST",
        &format!("/api/projects/{}/emit-local", fx.project_id),
        serde_json::json!({ "branch": false, "push": false, "open_pr": false }),
    )
    .await;

    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "source_deleted", "{body}");
    still_deleted(&fx).await;
}

/// Defense in depth for H1: even when a request reaches the emit handler without passing the
/// gate, the handler itself reports the repo as source-deleted and clones nothing.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn the_emit_handler_itself_refuses_to_clone_a_deleted_repo() {
    let fx = fixture();
    let _redirect = redirect_github_clone(&fx);
    select_a_rule(&fx);
    delete_the_source(&fx).await;

    let axum::Json(body) = crate::emit_project_local(
        axum::extract::State(fx.state.clone()),
        axum::extract::Path(fx.project_id.clone()),
        None,
    )
    .await;

    assert_eq!(body["results"][0]["ok"], false, "{body}");
    assert_eq!(body["results"][0]["code"], "source_deleted", "{body}");
    still_deleted(&fx).await;
}

/// The same defense for the onboarding Apply handler.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn the_apply_handler_itself_refuses_to_clone_a_deleted_repo() {
    let fx = fixture();
    let _redirect = redirect_github_clone(&fx);
    select_a_rule(&fx);
    delete_the_source(&fx).await;

    let request: crate::ArmReq = serde_json::from_value(serde_json::json!({
        "rules": [{
            "id": RULE, "title": "One currency", "directive": "one currency",
            "enforcement": "prose", "scope": "repo-local", "repos": [REPO],
        }],
    }))
    .expect("an apply request");
    let axum::Json(body) =
        crate::onboard_apply(axum::extract::State(fx.state.clone()), axum::Json(request)).await;

    assert_eq!(body["results"][0]["ok"], false, "{body}");
    assert_eq!(body["results"][0]["code"], "source_deleted", "{body}");
    still_deleted(&fx).await;
}

/// L1: the generate-ci-workflow route wrote `.github/workflows/...` under any `repo_root`,
/// recreating a deleted repo's folder (which then blocked re-fetch).
#[tokio::test]
async fn generate_ci_workflow_will_not_recreate_a_deleted_folder() {
    let fx = fixture();
    delete_the_source(&fx).await;

    let (status, body) = call(
        &fx.state,
        "POST",
        "/api/projects/active/generate-ci-workflow",
        serde_json::json!({ "repo_root": fx.checkout.to_string_lossy() }),
    )
    .await;

    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "not_a_git_repo", "{body}");
    still_deleted(&fx).await;
}

/// The route still works for a real checkout.
#[tokio::test]
async fn generate_ci_workflow_still_writes_into_a_real_checkout() {
    let fx = fixture();

    let (status, body) = call(
        &fx.state,
        "POST",
        "/api/projects/active/generate-ci-workflow",
        serde_json::json!({ "repo_root": fx.checkout.to_string_lossy() }),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], true, "{body}");
    assert!(fx
        .checkout
        .join(".github/workflows/camerata-gates.yml")
        .is_file());
}

/// Owner decision (c): a request that names a repo whose source was deleted in ANOTHER project
/// is refused too. The folder is per machine and per repo, not per project.
#[tokio::test]
async fn the_gate_catches_a_deleted_repo_that_belongs_to_another_project() {
    let fx = fixture();
    // A second project that also contains `acme/app` (and holds no records), and a third,
    // ACTIVE one that does not contain it at all. The deletion is recorded on the first.
    let other = fx
        .state
        .projects
        .create("Other", vec![REPO.to_string()])
        .expect("second project");
    let third = fx
        .state
        .projects
        .create("Third", vec!["acme/unrelated".to_string()])
        .expect("third project");
    fx.state.projects.set_active(&third.id);
    delete_the_source(&fx).await;

    for (uri, body) in [
        (
            "/api/onboard/scan".to_string(),
            serde_json::json!({ "repos": [REPO] }),
        ),
        (
            "/api/onboard/apply".to_string(),
            serde_json::json!({ "repos": [REPO] }),
        ),
        (
            format!("/api/projects/{}/emit-local", other.id),
            serde_json::json!({}),
        ),
    ] {
        let (status, json) = call(&fx.state, "POST", &uri, body).await;
        assert_eq!(status, StatusCode::CONFLICT, "{uri}: {json}");
        assert_eq!(json["code"], "source_deleted", "{uri}: {json}");
    }

    // A request that names only an unrelated repo is not caught by the deletion.
    let (status, json) = call(
        &fx.state,
        "POST",
        "/api/onboard/scan",
        serde_json::json!({ "repos": ["acme/unrelated"] }),
    )
    .await;
    assert_ne!(json["code"], "source_deleted", "{status} {json}");
}
