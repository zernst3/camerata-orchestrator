//! Source status and the three read endpoints.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use super::records::{DeletionRecord, RefetchRecord, METHOD_SENTENCE};
use super::status::{source_status, RepoSourceStatus};
use super::test_support::{fixture, REPO};
use crate::test_git::{commit_all, write_file};

fn deletion(repo: &str, path: &str) -> DeletionRecord {
    DeletionRecord {
        id: format!("del-x-{}", repo.replace('/', "-")),
        seq: 0,
        project_id: "x".to_string(),
        repo: repo.to_string(),
        origin_url: None,
        path: path.to_string(),
        inspected_sha: Some("abc".to_string()),
        inspection_id: None,
        head_sha_at_deletion: None,
        requested_at: "2026-10-24T17:40:12Z".to_string(),
        completed_at: "2026-10-24T17:40:19Z".to_string(),
        confirmed_with: repo.to_string(),
        include_uncommitted: false,
        items: Vec::new(),
        total_files: 12,
        total_bytes: 3400,
        method: METHOD_SENTENCE.to_string(),
        residuals: Vec::new(),
        camerata_version: "9.9.9".to_string(),
        machine: "test-machine".to_string(),
    }
}

fn refetch(repo: &str, path: &str) -> RefetchRecord {
    RefetchRecord {
        seq: 0,
        repo: repo.to_string(),
        path: path.to_string(),
        sha: None,
        target: "latest".to_string(),
        completed_at: "2026-10-24T17:40:19Z".to_string(),
    }
}

#[tokio::test]
async fn a_matching_clone_is_present_and_reports_its_distance_from_the_inspection() {
    let fx = fixture();
    fx.inspect_at(&fx.head);
    write_file(&fx.checkout, "b.txt", "b\n");
    commit_all(&fx.checkout, "newer one");
    write_file(&fx.checkout, "c.txt", "c\n");
    commit_all(&fx.checkout, "newer two");

    let status = source_status(&fx.state, &fx.project_id, REPO).await;

    let RepoSourceStatus::Present {
        inspected_sha,
        newer_commits,
        inspected_commit_reachable,
        dirty,
        head_branch,
        has_deletion_history,
        ..
    } = status
    else {
        panic!("expected Present, got {status:?}");
    };
    assert_eq!(inspected_sha.as_deref(), Some(fx.head.as_str()));
    assert_eq!(newer_commits, Some(2));
    assert!(inspected_commit_reachable);
    assert!(!dirty);
    assert_eq!(head_branch.as_deref(), Some("main"));
    assert!(!has_deletion_history);
}

#[tokio::test]
async fn an_inspected_commit_missing_from_the_clone_is_reported_unreachable() {
    let fx = fixture();
    fx.inspect_at(&"0".repeat(40));
    let RepoSourceStatus::Present {
        newer_commits,
        inspected_commit_reachable,
        ..
    } = source_status(&fx.state, &fx.project_id, REPO).await
    else {
        panic!("expected Present");
    };
    assert!(!inspected_commit_reachable);
    assert_eq!(newer_commits, None);
}

#[tokio::test]
async fn present_without_any_inspection_has_no_distance() {
    let fx = fixture();
    let RepoSourceStatus::Present {
        inspected_sha,
        newer_commits,
        ..
    } = source_status(&fx.state, &fx.project_id, REPO).await
    else {
        panic!("expected Present");
    };
    assert_eq!(inspected_sha, None);
    assert_eq!(newer_commits, None);
}

#[tokio::test]
async fn a_folder_that_is_another_repo_is_a_mismatch_and_never_present() {
    let fx = fixture();
    crate::test_git::git(
        &fx.checkout,
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/acme/other.git",
        ],
    );
    let status = source_status(&fx.state, &fx.project_id, REPO).await;
    let RepoSourceStatus::Mismatch { reason, .. } = status else {
        panic!("expected Mismatch, got {status:?}");
    };
    assert!(reason.contains("acme/other"), "{reason}");
}

#[tokio::test]
async fn nothing_on_disk_and_no_record_is_unresolved() {
    let fx = fixture();
    std::fs::remove_dir_all(&fx.checkout).unwrap();
    let status = source_status(&fx.state, &fx.project_id, REPO).await;
    assert!(
        matches!(status, RepoSourceStatus::Unresolved { .. }),
        "{status:?}"
    );
}

#[tokio::test]
async fn an_unset_path_is_unresolved_with_a_reason() {
    let fx = fixture();
    fx.state.settings.set_workspace_root(None);
    let RepoSourceStatus::Unresolved { path, reason, .. } =
        source_status(&fx.state, &fx.project_id, REPO).await
    else {
        panic!("expected Unresolved");
    };
    assert_eq!(path, None);
    assert!(reason.contains("no local path"), "{reason}");
}

/// THE rule: a repo is `Deleted` only while its newest record is a deletion. A freshly imported
/// project (no record) must still land in the "link the repo" flow, a deleted one must not, and
/// a re-fetch clears the deleted state even when both happened in the same second.
#[tokio::test]
async fn source_status_deleted_only_when_record_newer_than_refetch() {
    let fx = fixture();
    let path = fx.checkout.to_string_lossy().into_owned();
    std::fs::remove_dir_all(&fx.checkout).unwrap();

    // Folder gone, nothing recorded: not deleted by us.
    assert!(matches!(
        source_status(&fx.state, &fx.project_id, REPO).await,
        RepoSourceStatus::Unresolved { .. }
    ));

    // Folder gone, deletion recorded: Deleted, carrying the record's facts.
    fx.state
        .source_records
        .add_deletion(&fx.project_id, deletion(REPO, &path))
        .unwrap();
    let status = source_status(&fx.state, &fx.project_id, REPO).await;
    let RepoSourceStatus::Deleted {
        deleted_at,
        total_files,
        inspected_sha,
        ..
    } = &status
    else {
        panic!("expected Deleted, got {status:?}");
    };
    assert_eq!(deleted_at, "2026-10-24T17:40:19Z");
    assert_eq!(*total_files, 12);
    assert_eq!(inspected_sha.as_deref(), Some("abc"));

    // A later re-fetch record (same second!) outranks it; if the folder is then gone again by
    // some other means, the repo is simply unresolved, not "deleted by Camerata".
    fx.state
        .source_records
        .add_refetch(&fx.project_id, refetch(REPO, &path))
        .unwrap();
    assert!(matches!(
        source_status(&fx.state, &fx.project_id, REPO).await,
        RepoSourceStatus::Unresolved { .. }
    ));

    // And a still-later deletion makes it Deleted again.
    fx.state
        .source_records
        .add_deletion(&fx.project_id, deletion(REPO, &path))
        .unwrap();
    assert!(matches!(
        source_status(&fx.state, &fx.project_id, REPO).await,
        RepoSourceStatus::Deleted { .. }
    ));
}

#[tokio::test]
async fn a_clone_on_disk_wins_over_an_old_deletion_record() {
    let fx = fixture();
    fx.state
        .source_records
        .add_deletion(
            &fx.project_id,
            deletion(REPO, &fx.checkout.to_string_lossy()),
        )
        .unwrap();
    // The operator cloned the repo back by hand: source is on disk.
    let RepoSourceStatus::Present {
        has_deletion_history,
        ..
    } = source_status(&fx.state, &fx.project_id, REPO).await
    else {
        panic!("expected Present");
    };
    assert!(has_deletion_history);
}

#[tokio::test]
async fn a_deletion_record_of_one_repo_does_not_delete_another() {
    let fx = fixture();
    std::fs::remove_dir_all(&fx.checkout).unwrap();
    fx.state
        .source_records
        .add_deletion(&fx.project_id, deletion("acme/other", "/x"))
        .unwrap();
    assert!(matches!(
        source_status(&fx.state, &fx.project_id, REPO).await,
        RepoSourceStatus::Unresolved { .. }
    ));
}

async fn get(app: axum::Router, uri: &str) -> (StatusCode, Vec<u8>, Option<String>) {
    let resp = app
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let ctype = resp
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap().to_string());
    let bytes = resp
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, bytes, ctype)
}

#[tokio::test]
async fn get_source_lists_every_repo_in_project_order() {
    let fx = fixture();
    let (status, body, _) = get(
        crate::router(fx.state.clone()),
        &format!("/api/projects/{}/source", fx.project_id),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["ok"], true);
    assert_eq!(json["repos"][0]["state"], "present");
    assert_eq!(json["repos"][0]["repo"], REPO);
}

#[tokio::test]
async fn get_source_on_an_unknown_project_is_a_404_with_a_code() {
    let fx = fixture();
    let (status, body, _) = get(crate::router(fx.state.clone()), "/api/projects/nope/source").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["ok"], false);
    assert_eq!(json["code"], "unknown_project");
}

#[tokio::test]
async fn records_endpoints_return_json_and_the_plain_text_proof() {
    let fx = fixture();
    let stored = fx
        .state
        .source_records
        .add_deletion(
            &fx.project_id,
            deletion(REPO, &fx.checkout.to_string_lossy()),
        )
        .unwrap();

    let (status, body, _) = get(
        crate::router(fx.state.clone()),
        &format!("/api/projects/{}/source/records", fx.project_id),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["records"][0]["id"], stored.id.as_str());
    assert_eq!(json["refetches"].as_array().unwrap().len(), 0);

    let (status, body, ctype) = get(
        crate::router(fx.state.clone()),
        &format!(
            "/api/projects/{}/source/records/{}/text",
            fx.project_id, stored.id
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ctype.as_deref(), Some("text/plain; charset=utf-8"));
    let text = String::from_utf8(body).unwrap();
    assert!(
        text.starts_with("Camerata source deletion record\n"),
        "{text}"
    );
    assert!(text.contains("acme/app"));

    let (status, _, _) = get(
        crate::router(fx.state.clone()),
        &format!("/api/projects/{}/source/records/nope/text", fx.project_id),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
