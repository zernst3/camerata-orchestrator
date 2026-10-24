//! Deleted source does not pause the project, and the operations that need the code say why
//! they cannot run (design, section 6).

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use super::delete::{delete_source, DeleteOutcome, DeleteRequest};
use super::test_support::{fixture, Fx, REPO};

async fn call(
    fx: &Fx,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    let payload = match body {
        Some(v) => {
            builder = builder.header("content-type", "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let resp = crate::router(fx.state.clone())
        .oneshot(builder.body(payload).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
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
    assert!(matches!(outcome, Ok(DeleteOutcome::Done { .. })));
}

#[tokio::test]
async fn readiness_is_ready_with_a_deleted_repo_and_marks_it() {
    let fx = fixture();
    let (_, before) = call(
        &fx,
        "GET",
        &format!("/api/projects/{}/readiness", fx.project_id),
        None,
    )
    .await;
    assert_eq!(before["readiness"], "ready");
    assert_eq!(before["repos"][0]["source_deleted"], false);

    delete_the_source(&fx).await;

    let (status, after) = call(
        &fx,
        "GET",
        &format!("/api/projects/{}/readiness", fx.project_id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        after["readiness"], "ready",
        "deleted source does not pause the project"
    );
    assert_eq!(after["repos"][0]["resolved"], false);
    assert_eq!(after["repos"][0]["source_deleted"], true);
    assert!(after["repos"][0]["deleted_at"]
        .as_str()
        .unwrap()
        .ends_with('Z'));
    assert!(after["repos"][0]["reason"]
        .as_str()
        .unwrap()
        .contains("source deleted on"));

    let (_, health) = call(
        &fx,
        "GET",
        &format!("/api/projects/{}/repo-health", fx.project_id),
        None,
    )
    .await;
    assert_eq!(health["repos"][0]["source_deleted"], true);
}

/// A project that was never deleted-from, or an imported one with no clone and no record, is
/// still paused behind the "link the repo" flow: only a DELETION unpauses.
#[tokio::test]
async fn a_repo_that_is_merely_missing_still_pauses_the_project() {
    let fx = fixture();
    std::fs::remove_dir_all(&fx.checkout).unwrap();
    let (_, json) = call(
        &fx,
        "GET",
        &format!("/api/projects/{}/readiness", fx.project_id),
        None,
    )
    .await;
    assert_eq!(json["readiness"], "unlinked");
    assert_eq!(json["repos"][0]["source_deleted"], false);
}

#[tokio::test]
async fn one_deleted_and_one_unlinked_repo_is_partial() {
    let fx = fixture();
    fx.add_repo("acme/lib");
    delete_the_source(&fx).await;
    std::fs::remove_dir_all(fx.root.join("work/acme/lib")).unwrap();
    let (_, json) = call(
        &fx,
        "GET",
        &format!("/api/projects/{}/readiness", fx.project_id),
        None,
    )
    .await;
    assert_eq!(json["readiness"], "partial");
}

#[tokio::test]
async fn checkout_status_reads_source_deleted_with_the_date() {
    let fx = fixture();
    delete_the_source(&fx).await;
    let (status, json) = call(
        &fx,
        "GET",
        &format!("/api/projects/{}/checkout", fx.project_id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let repo = &json[0];
    assert_eq!(repo["repo"], REPO);
    assert_eq!(repo["cloned"], false);
    assert_eq!(repo["source_deleted"], true);
    assert_eq!(repo["source"]["state"], "deleted");
    let detail = repo["detail"].as_str().unwrap();
    assert!(detail.starts_with("source deleted on "), "{detail}");
    assert!(detail.ends_with("use Re-fetch source"), "{detail}");
}

#[tokio::test]
async fn clone_update_all_skips_a_deleted_repo_instead_of_recloning_it() {
    let fx = fixture();
    fx.state
        .credential_store
        .set(crate::credentials::GITHUB_TOKEN, "test-token-not-used")
        .unwrap();
    delete_the_source(&fx).await;

    let (status, json) = call(
        &fx,
        "POST",
        &format!("/api/projects/{}/checkout", fx.project_id),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json[0]["source_deleted"], true);
    assert_eq!(json[0]["cloned"], false);
    assert!(
        !fx.checkout.exists(),
        "updating the others must not re-clone a repo whose source was deleted"
    );
}

#[tokio::test]
async fn scan_and_apply_endpoints_answer_409_source_deleted_before_reading_anything() {
    let fx = fixture();
    delete_the_source(&fx).await;

    let named = serde_json::json!({ "repos": [REPO] });
    let project_level = serde_json::json!({
        "rules": [{ "id": "SEC-1", "directive": "d", "scope": "repo", "repos": [], "options": [] }],
    });
    for (uri, body) in [
        ("/api/onboard/scan".to_string(), named.clone()),
        ("/api/onboard/audit".to_string(), named.clone()),
        ("/api/onboard/audit/start".to_string(), named.clone()),
        ("/api/onboard/open-pr".to_string(), named.clone()),
        ("/api/onboard/apply".to_string(), project_level.clone()),
        ("/api/onboard/arm".to_string(), project_level.clone()),
        ("/api/onboard/apply/preflight".to_string(), project_level),
        (
            format!("/api/projects/{}/rescan-alternatives", fx.project_id),
            serde_json::json!({ "overrides": {} }),
        ),
    ] {
        let (status, json) = call(&fx, "POST", &uri, Some(body)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{uri}");
        assert_eq!(json["ok"], false, "{uri}");
        assert_eq!(json["code"], "source_deleted", "{uri}");
        assert!(
            json["message"]
                .as_str()
                .unwrap()
                .contains("Re-fetch source"),
            "{uri}: {json}"
        );
    }
}

/// The gate is about the repos in scope: a request for a repo whose source is present goes
/// through to the handler even when a sibling repo of the project was deleted.
#[tokio::test]
async fn the_gate_does_not_block_a_request_for_a_repo_whose_source_is_present() {
    let fx = fixture();
    fx.add_repo("acme/lib");
    delete_the_source(&fx).await;

    let (status, json) = call(
        &fx,
        "POST",
        "/api/onboard/scan",
        Some(serde_json::json!({ "repos": ["acme/lib"] })),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{json}");
    assert_ne!(json["code"], "source_deleted");
}

#[tokio::test]
async fn once_the_source_is_back_the_gate_opens() {
    let fx = fixture();
    delete_the_source(&fx).await;
    let (status, _) = call(
        &fx,
        "POST",
        "/api/onboard/scan",
        Some(serde_json::json!({ "repos": [REPO] })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // Fetched back (by hand here; the re-fetch endpoint has its own tests).
    crate::test_git::clone_as(
        &fx.remote_url,
        &fx.checkout,
        "https://github.com/acme/app.git",
    );

    let (status, _) = call(
        &fx,
        "POST",
        "/api/onboard/scan",
        Some(serde_json::json!({ "repos": [REPO] })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn suppressions_report_source_deleted_with_empty_entries() {
    let fx = fixture();
    delete_the_source(&fx).await;
    let (status, json) = call(
        &fx,
        "GET",
        &format!("/api/projects/{}/suppressions", fx.project_id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["ok"], true);
    assert_eq!(json["source_deleted"], true);
    assert!(json["deleted_at"].as_str().unwrap().ends_with('Z'));
    assert_eq!(json["entries"].as_array().unwrap().len(), 0);
    assert_eq!(json["deleted_repos"][0]["repo"], REPO);
}

/// D3: a GET must not change the checkout. The suppressions listing used to run a fast-forward
/// `git pull` on every read, which moves a tree past the inspected commit just by looking at a
/// screen. (The Workspace "Clone / update" button is the deliberate refresh.)
#[tokio::test]
async fn suppressions_get_does_not_touch_the_checkout() {
    let fx = fixture();
    // Make the pull possible: origin is the local bare remote, which gains a newer commit.
    crate::test_git::git(
        &fx.checkout,
        &["remote", "set-url", "origin", &fx.remote_url],
    );
    let newer =
        crate::test_git::push_commit(&fx.remote_base, "b.txt", "2\n", "newer on the remote");
    assert_ne!(newer, fx.head);

    let (status, _) = call(
        &fx,
        "GET",
        &format!("/api/projects/{}/suppressions", fx.project_id),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        crate::test_git::git(&fx.checkout, &["rev-parse", "HEAD"]),
        fx.head,
        "reading the suppressions moved the checkout"
    );
    assert!(!fx.checkout.join("b.txt").exists());
}

#[tokio::test]
async fn suppressions_for_a_present_repo_are_an_envelope_without_the_flag() {
    let fx = fixture();
    let (_, json) = call(
        &fx,
        "GET",
        &format!("/api/projects/{}/suppressions", fx.project_id),
        None,
    )
    .await;
    assert_eq!(json["ok"], true);
    assert_eq!(json["source_deleted"], false);
    assert!(json["entries"].is_array());
}
