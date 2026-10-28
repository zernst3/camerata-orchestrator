//! Re-fetch: a full-history clone back to the recorded folder, the inspected commit or the
//! latest, and every way it can refuse or fail. The remote is a local bare repository.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use super::delete::{delete_source, DeleteOutcome, DeleteRequest};
use super::remote::GitRemotes;
use super::test_support::{fixture, Fx, REPO};
use crate::test_git::{git, push_commit};

async fn call(
    fx: &Fx,
    method: &str,
    suffix: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::builder().method(method).uri(format!(
        "/api/projects/{}/repos/{}/source/{suffix}",
        fx.project_id,
        REPO.replace('/', "%2F")
    ));
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
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

fn latest() -> serde_json::Value {
    serde_json::json!({ "target": { "kind": "latest" } })
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
async fn refetch_latest_clones_full_history() {
    let fx = fixture();
    push_commit(&fx.remote_base, "b.txt", "2\n", "second");
    push_commit(&fx.remote_base, "c.txt", "3\n", "third");
    git(
        &fx.remote_base.join("seed"),
        &["push", "-q", "origin", "main:release"],
    );
    delete_the_source(&fx).await;
    assert!(!fx.checkout.exists());

    let (status, json) = call(&fx, "POST", "refetch", Some(latest())).await;

    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["ok"], true);
    assert_eq!(json["status"]["state"], "present");
    assert_eq!(json["renamed_to"], serde_json::Value::Null);
    let remote = fx.remote_base.join("remote.git");
    assert_eq!(
        git(&fx.checkout, &["rev-list", "--count", "--all"]),
        git(&remote, &["rev-list", "--count", "--all"])
    );
    assert_eq!(
        git(&fx.checkout, &["rev-parse", "--is-shallow-repository"]),
        "false"
    );
    assert!(git(&fx.checkout, &["branch", "-r"]).contains("origin/release"));
    assert_eq!(
        git(&fx.checkout, &["config", "--get", "remote.origin.url"]),
        "https://github.com/acme/app.git",
        "origin is the tokenless GitHub URL"
    );
}

#[tokio::test]
async fn refetch_clears_deleted_state_and_keeps_deletion_records() {
    let fx = fixture();
    delete_the_source(&fx).await;
    let (_, before) = call_get(&fx, "source").await;
    assert_eq!(before["repos"][0]["state"], "deleted");

    let (status, _) = call(&fx, "POST", "refetch", Some(latest())).await;
    assert_eq!(status, StatusCode::OK);

    let (_, after) = call_get(&fx, "source").await;
    assert_eq!(after["repos"][0]["state"], "present");
    assert_eq!(after["repos"][0]["has_deletion_history"], true);
    assert_eq!(
        fx.state.source_records.deletions(&fx.project_id).len(),
        1,
        "the proof is kept"
    );
    let refetches = fx.state.source_records.refetches(&fx.project_id);
    assert_eq!(refetches.len(), 1);
    assert_eq!(refetches[0].target, "latest");
    assert_eq!(refetches[0].sha.as_deref(), Some(fx.head.as_str()));
    // The scan gate is open again.
    let (status, _) = post_scan(&fx).await;
    assert_eq!(status, StatusCode::OK);
}

async fn call_get(fx: &Fx, path: &str) -> (StatusCode, serde_json::Value) {
    let resp = crate::router(fx.state.clone())
        .oneshot(
            Request::builder()
                .uri(format!("/api/projects/{}/{path}", fx.project_id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn post_scan(fx: &Fx) -> (StatusCode, serde_json::Value) {
    let resp = crate::router(fx.state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/onboard/scan")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "repos": [REPO] }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

#[tokio::test]
async fn refetch_inspected_commit_checks_out_detached_sha() {
    let fx = fixture();
    fx.inspect_at(&fx.head);
    let second = push_commit(&fx.remote_base, "b.txt", "2\n", "newer on the remote");
    delete_the_source(&fx).await;

    let (status, json) = call(
        &fx,
        "POST",
        "refetch",
        Some(serde_json::json!({ "target": { "kind": "commit", "sha": fx.head } })),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(git(&fx.checkout, &["rev-parse", "HEAD"]), fx.head);
    assert_eq!(
        git(&fx.checkout, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "HEAD",
        "detached"
    );
    assert_eq!(json["status"]["newer_commits"], 0);
    assert_eq!(json["status"]["inspected_commit_reachable"], true);
    // The newer commit is still in the clone (full history), just not checked out.
    assert!(crate::client_source::git::commit_exists(&fx.checkout, &second).await);
    assert_eq!(
        fx.state.source_records.refetches(&fx.project_id)[0].target,
        format!("commit:{}", fx.head)
    );
}

#[tokio::test]
async fn refetch_reports_newer_commit_count_and_reachability() {
    let fx = fixture();
    fx.inspect_at(&fx.head);
    push_commit(&fx.remote_base, "b.txt", "2\n", "one");
    push_commit(&fx.remote_base, "c.txt", "3\n", "two");
    delete_the_source(&fx).await;

    let (_, json) = call(&fx, "POST", "refetch", Some(latest())).await;

    assert_eq!(json["status"]["state"], "present");
    assert_eq!(json["status"]["newer_commits"], 2);
    assert_eq!(json["status"]["inspected_commit_reachable"], true);
    assert_eq!(json["status"]["inspected_sha"], fx.head.as_str());
}

#[tokio::test]
async fn refetch_reports_commit_not_on_remote_and_keeps_latest_clone() {
    let fx = fixture();
    fx.inspect_at(&"1".repeat(40));
    delete_the_source(&fx).await;

    let (status, json) = call(
        &fx,
        "POST",
        "refetch",
        Some(serde_json::json!({ "target": { "kind": "commit", "sha": "1".repeat(40) } })),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["ok"], false);
    assert_eq!(json["code"], "commit_not_on_remote");
    assert!(json["message"]
        .as_str()
        .unwrap()
        .contains("latest source was fetched instead"));
    assert_eq!(
        json["status"]["state"], "present",
        "the latest clone stays in place"
    );
    assert_eq!(json["status"]["inspected_commit_reachable"], false);
    assert_eq!(git(&fx.checkout, &["rev-parse", "HEAD"]), fx.head);
}

#[tokio::test]
async fn refetch_refuses_non_empty_destination() {
    let fx = fixture();
    delete_the_source(&fx).await;
    std::fs::create_dir_all(&fx.checkout).unwrap();
    std::fs::write(fx.checkout.join("precious.txt"), "the operator's own file").unwrap();

    let (status, json) = call(&fx, "POST", "refetch", Some(latest())).await;

    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(json["code"], "destination_not_empty");
    assert!(json["message"]
        .as_str()
        .unwrap()
        .contains("move them aside"));
    assert!(
        fx.checkout.join("precious.txt").exists(),
        "nothing is overwritten or removed"
    );
    assert!(!fx.checkout.join(".git").exists());

    // An existing EMPTY folder is fine.
    std::fs::remove_file(fx.checkout.join("precious.txt")).unwrap();
    let (status, _) = call(&fx, "POST", "refetch", Some(latest())).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn refetch_uses_recorded_override_path_when_present() {
    let fx = fixture();
    let handmade = fx.root.join("hand-made").join("app");
    crate::test_git::clone_as(&fx.remote_url, &handmade, "https://github.com/acme/app.git");
    fx.state
        .settings
        .set_repo_path(REPO, Some(handmade.to_string_lossy().into_owned()));
    std::fs::remove_dir_all(&fx.checkout).unwrap();
    delete_the_source(&fx).await;
    assert!(!handmade.exists());

    let (status, json) = call(&fx, "POST", "refetch", Some(latest())).await;

    assert_eq!(status, StatusCode::OK, "{json}");
    assert!(handmade.join(".git").exists(), "it came back where it was");
    assert!(
        !fx.checkout.exists(),
        "the workspace layout path was not used"
    );
    assert_eq!(json["status"]["path"], handmade.to_string_lossy().as_ref());
}

#[tokio::test]
async fn refetch_detects_renamed_origin() {
    let mut fx = fixture();
    fx.state = fx.state.clone().with_git_remotes(
        GitRemotes::default()
            .with_url(REPO, &fx.remote_url)
            .with_full_name(REPO, "acme/renamed-app"),
    );
    delete_the_source(&fx).await;

    let (status, json) = call(&fx, "POST", "refetch", Some(latest())).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["ok"], true);
    assert_eq!(json["renamed_to"], "acme/renamed-app");
    assert_eq!(
        json["status"]["state"], "present",
        "the clone is still valid"
    );
}

#[tokio::test]
async fn refetch_refuses_when_already_present() {
    let fx = fixture();
    let (status, json) = call(&fx, "POST", "refetch", Some(latest())).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(json["code"], "already_present");
    assert!(json["message"]
        .as_str()
        .unwrap()
        .contains("already on disk"));
    assert!(fx.checkout.join(".git").exists());
}

#[tokio::test]
async fn refetch_refuses_while_scan_job_is_running() {
    let fx = fixture();
    delete_the_source(&fx).await;
    let job = fx.state.jobs.create("audit", Some(fx.project_id.clone()));
    let (status, json) = call(&fx, "POST", "refetch", Some(latest())).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(json["code"], "scan_in_progress");
    assert!(!fx.checkout.exists());
    fx.state
        .jobs
        .finish(&job, crate::onboard::ScanReport::gated(&[REPO.to_string()]));
    // Another operation on the same repo also refuses.
    let claim = fx.state.source_ops.try_begin(&fx.project_id, REPO).unwrap();
    let (_, busy) = call(&fx, "POST", "refetch", Some(latest())).await;
    assert_eq!(busy["code"], "scan_in_progress");
    drop(claim);
    let (status, _) = call(&fx, "POST", "refetch", Some(latest())).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn refetch_refuses_without_a_token_a_bad_sha_and_an_unknown_repo() {
    let fx = fixture();
    delete_the_source(&fx).await;

    let (_, bad_sha) = call(
        &fx,
        "POST",
        "refetch",
        Some(serde_json::json!({ "target": { "kind": "commit", "sha": "--upload-pack=x" } })),
    )
    .await;
    assert_eq!(bad_sha["code"], "invalid_sha");

    // No token anywhere: not in the keychain store, not in the environment fallback.
    let _env = camerata_test_env::EnvGuard::remove("CAMERATA_GITHUB_TOKEN");
    fx.state
        .credential_store
        .set(crate::credentials::GITHUB_TOKEN, "")
        .unwrap();
    let (_, no_token) = call(&fx, "POST", "refetch", Some(latest())).await;
    assert_eq!(no_token["code"], "no_token");
    assert_eq!(
        no_token["message"],
        "No GitHub token is set. Add one under Credentials."
    );
    let (_, probe) = call(&fx, "GET", "remote", None).await;
    assert_eq!(probe["code"], "no_token");

    let resp = crate::router(fx.state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/projects/{}/repos/acme%2Fnope/source/refetch",
                    fx.project_id
                ))
                .header("content-type", "application/json")
                .body(Body::from(latest().to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_remote_that_cannot_be_reached_is_reported_plainly_and_leaves_no_clone() {
    let mut fx = fixture();
    fx.state = fx.state.clone().with_git_remotes(
        GitRemotes::default().with_url(REPO, "file:///definitely/not/a/repository.git"),
    );
    delete_the_source(&fx).await;

    let (status, json) = call(&fx, "POST", "refetch", Some(latest())).await;

    assert_eq!(status, StatusCode::BAD_GATEWAY, "{json}");
    assert_eq!(json["code"], "network");
    assert!(!json["message"].as_str().unwrap().is_empty());
    assert!(!fx.checkout.exists());
    assert!(fx.state.source_records.refetches(&fx.project_id).is_empty());
    // Still deleted: nothing was recorded as fetched back.
    let (_, source) = call_get(&fx, "source").await;
    assert_eq!(source["repos"][0]["state"], "deleted");
}

#[tokio::test]
async fn the_remote_probe_over_http_reports_default_branch_and_head() {
    let fx = fixture();
    let (status, json) = call(&fx, "GET", "remote", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["ok"], true);
    assert_eq!(json["default_branch"], "main");
    assert_eq!(json["head_sha"], fx.head.as_str());
}

#[tokio::test]
async fn a_full_disk_refuses_the_refetch_before_cloning() {
    use super::refetch::{refetch_source_with_headroom, RefetchRequest, RefetchTarget};
    let fx = fixture();
    delete_the_source(&fx).await;

    // An absurd headroom requirement: no machine has it free.
    let refused = refetch_source_with_headroom(
        &fx.state,
        &fx.project_id,
        REPO,
        &RefetchRequest {
            target: RefetchTarget::Latest,
        },
        u64::MAX,
    )
    .await
    .expect_err("not enough free disk");

    assert_eq!(refused.code, "no_disk_headroom");
    assert!(!fx.checkout.join(".git").exists());
    assert!(fx.state.source_records.refetches(&fx.project_id).is_empty());
}
