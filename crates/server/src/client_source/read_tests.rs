//! Reading client code for display: the inspected commit, not the working tree; nothing at all
//! once the source is deleted.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use super::delete::{delete_source, DeleteOutcome, DeleteRequest};
use super::read::{source_for_read, SourceRead};
use super::test_support::{fixture, Fx, REPO};
use crate::onboard::finding_context::WORKING_TREE_READS;
use crate::test_git::{commit_all, write_file};

const QUERY_BASE: &str = "/api/onboard/finding-context?repo=acme%2Fapp&path=src%2Fmain.rs&line=2";

async fn get(fx: &Fx, uri: &str) -> (StatusCode, Option<String>, serde_json::Value) {
    let resp = crate::router(fx.state.clone())
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let cache = resp
        .headers()
        .get(header::CACHE_CONTROL)
        .map(|v| v.to_str().unwrap().to_string());
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, cache, serde_json::from_slice(&bytes).unwrap())
}

fn lines(json: &serde_json::Value) -> String {
    json["lines"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|l| l.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// Move the checkout past the inspected commit: edit and COMMIT a different line 2.
fn move_checkout_on(fx: &Fx) {
    write_file(
        &fx.checkout,
        "src/main.rs",
        "fn main() {\n    println!(\"changed after the inspection\");\n}\n",
    );
    commit_all(&fx.checkout, "newer");
}

#[tokio::test]
async fn finding_context_reads_inspected_commit_not_working_tree() {
    let fx = fixture();
    fx.inspect_at(&fx.head);
    move_checkout_on(&fx);
    // And an UNCOMMITTED edit on top, to prove neither is what the modal shows.
    write_file(
        &fx.checkout,
        "src/main.rs",
        "fn main() {\n    uncommitted();\n}\n",
    );

    let (status, _, json) = get(&fx, QUERY_BASE).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "ok", "{json}");
    assert_eq!(json["source"], "git");
    assert_eq!(json["commit"], fx.head.as_str());
    assert_eq!(json["head_differs"], true);
    let shown = lines(&json);
    assert!(
        shown.contains("println!(\"hi\")"),
        "the committed line: {shown}"
    );
    assert!(!shown.contains("changed after"), "{shown}");
    assert!(!shown.contains("uncommitted"), "{shown}");
}

#[tokio::test]
async fn finding_context_at_the_inspected_commit_does_not_say_the_source_moved_on() {
    let fx = fixture();
    fx.inspect_at(&fx.head);
    let (_, _, json) = get(&fx, QUERY_BASE).await;
    assert_eq!(json["source"], "git");
    assert_eq!(json["head_differs"], false);
}

#[tokio::test]
async fn finding_context_defaults_sha_to_latest_inspection_entry() {
    let fx = fixture();
    let first = fx.head.clone();
    fx.inspect_at(&first);
    move_checkout_on(&fx);
    let second = crate::client_source::git::head_sha(&fx.checkout)
        .await
        .unwrap();

    // The latest inspection (at the first commit) decides...
    let (_, _, json) = get(&fx, QUERY_BASE).await;
    assert_eq!(json["commit"], first.as_str());
    // ...until a newer inspection covers the newer commit.
    fx.inspect_at(&second);
    let (_, _, json) = get(&fx, QUERY_BASE).await;
    assert_eq!(json["commit"], second.as_str());
    assert!(lines(&json).contains("changed after the inspection"));

    // An explicit sha overrides the default.
    let (_, _, json) = get(&fx, &format!("{QUERY_BASE}&sha={first}")).await;
    assert_eq!(json["commit"], first.as_str());
    assert!(lines(&json).contains("println!(\"hi\")"));
}

#[tokio::test]
async fn a_malformed_sha_is_a_400_and_never_reaches_git() {
    let fx = fixture();
    for bad in ["abc", "--upload-pack=x", &"g".repeat(40), &"a".repeat(41)] {
        let (status, cache, json) = get(&fx, &format!("{QUERY_BASE}&sha={bad}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
        assert_eq!(json["code"], "invalid_sha");
        assert_eq!(cache.as_deref(), Some("no-store"));
    }
}

async fn delete_the_source(fx: &Fx) -> String {
    let Ok(DeleteOutcome::Done { record, .. }) = delete_source(
        &fx.state,
        &fx.project_id,
        REPO,
        &DeleteRequest {
            confirm: REPO.to_string(),
            include_uncommitted: false,
        },
    )
    .await
    else {
        panic!("deletion failed");
    };
    record.id
}

#[tokio::test]
async fn finding_context_reports_source_deleted_with_date_and_record() {
    let fx = fixture();
    fx.inspect_at(&fx.head);
    let record_id = delete_the_source(&fx).await;

    let (status, _, json) = get(&fx, QUERY_BASE).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "source_deleted");
    assert_eq!(json["record_id"], record_id.as_str());
    assert!(json["deleted_at"].as_str().unwrap().ends_with('Z'));
    assert!(json["reason"]
        .as_str()
        .unwrap()
        .starts_with("Source deleted on "));
    assert!(json["reason"]
        .as_str()
        .unwrap()
        .ends_with("re-fetch to view"));
    assert!(
        json.get("lines").is_none(),
        "no code is shown for a deleted source"
    );
    assert!(json.get("source").is_none() || json["source"].is_null());
}

/// THE invariant: with the folder gone the handler never reads the filesystem and never claims
/// `working_tree`. (The thread-local counts working-tree reads made on this test's thread; the
/// tests run on a current-thread runtime, so the handler's awaits stay on it.)
#[tokio::test]
async fn finding_context_never_reads_fs_when_directory_missing() {
    let fx = fixture();
    fx.inspect_at(&fx.head);
    delete_the_source(&fx).await;
    WORKING_TREE_READS.with(|c| c.set(0));

    let (_, _, deleted) = get(&fx, QUERY_BASE).await;
    assert_eq!(deleted["status"], "source_deleted");

    // A merely-missing folder (no deletion record) behaves the same way.
    let other = fixture();
    other.inspect_at(&other.head);
    std::fs::remove_dir_all(&other.checkout).unwrap();
    let (_, _, missing) = get(&other, QUERY_BASE).await;
    assert_eq!(missing["status"], "file_missing");

    assert_eq!(
        WORKING_TREE_READS.with(std::cell::Cell::get),
        0,
        "no working-tree read may happen when the directory does not exist"
    );
    for json in [&deleted, &missing] {
        assert_ne!(json["source"], "working_tree", "{json}");
    }
}

#[tokio::test]
async fn finding_context_commit_unavailable_labels_working_tree_fallback() {
    let fx = fixture();
    // The inspection recorded a commit this clone does not have (history was rewritten).
    fx.inspect_at(&"0".repeat(40));

    let (status, cache, json) = get(&fx, QUERY_BASE).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "commit_unavailable");
    assert_eq!(json["source"], "working_tree");
    assert!(json["reason"]
        .as_str()
        .unwrap()
        .starts_with("Inspected commit not available in this clone"));
    assert!(
        lines(&json).contains("println!(\"hi\")"),
        "the working-tree block rides along: {json}"
    );
    assert_eq!(cache.as_deref(), Some("no-store"));
}

#[tokio::test]
async fn finding_context_git_read_rejects_dotdot_and_absolute_paths() {
    let fx = fixture();
    fx.inspect_at(&fx.head);
    for path in [
        "..%2F..%2Fetc%2Fpasswd",
        "%2Fetc%2Fpasswd",
        "src%2F..%2F..%2Foutside.rs",
        "..%5C..%5Cwindows",
    ] {
        let uri = format!("/api/onboard/finding-context?repo=acme%2Fapp&path={path}&line=1");
        let (status, _, json) = get(&fx, &uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}: {json}");
        assert_eq!(json["status"], "path_traversal", "{path}");
    }
}

#[tokio::test]
async fn finding_context_git_read_applies_too_large_cap_and_redaction() {
    let fx = fixture();
    // A blob over the cap, and one that carries a secret-shaped value.
    write_file(&fx.checkout, "src/huge.rs", &"x".repeat(400_001));
    let token = format!("ghp_{}", "Zq9Xk3Pm7Rt2Vw5Yb8Nc4Jd6Hf1Ga0Lo3Ue9");
    write_file(
        &fx.checkout,
        "src/keys.rs",
        &format!("fn keys() {{\n    let t = \"{token}\";\n}}\n"),
    );
    let committed = commit_all(&fx.checkout, "add files");
    fx.inspect_at(&committed);

    let (_, _, huge) = get(
        &fx,
        "/api/onboard/finding-context?repo=acme%2Fapp&path=src%2Fhuge.rs&line=1",
    )
    .await;
    assert_eq!(huge["status"], "too_large");

    let (_, _, keys) = get(
        &fx,
        "/api/onboard/finding-context?repo=acme%2Fapp&path=src%2Fkeys.rs&line=2",
    )
    .await;
    assert_eq!(keys["status"], "ok", "{keys}");
    assert!(
        !lines(&keys).contains(&token),
        "secret must be redacted: {keys}"
    );
    assert!(lines(&keys).contains("let t ="));
}

#[tokio::test]
async fn a_file_that_is_not_in_the_inspected_commit_is_file_missing_even_if_it_is_on_disk() {
    let fx = fixture();
    fx.inspect_at(&fx.head);
    write_file(&fx.checkout, "src/later.rs", "fn later() {}\n");
    commit_all(&fx.checkout, "added after the inspection");

    let (_, _, json) = get(
        &fx,
        "/api/onboard/finding-context?repo=acme%2Fapp&path=src%2Flater.rs&line=1",
    )
    .await;
    assert_eq!(json["status"], "file_missing");
    assert_eq!(json["source"], "git");
    // A directory is not a file either.
    let (_, _, dir) = get(
        &fx,
        "/api/onboard/finding-context?repo=acme%2Fapp&path=src&line=1",
    )
    .await;
    assert_eq!(dir["status"], "file_missing");
}

#[tokio::test]
async fn a_scan_without_a_commit_reads_the_working_tree_and_says_so() {
    let fx = fixture();
    // No inspection at all: nothing names a commit.
    let (_, _, json) = get(&fx, QUERY_BASE).await;
    assert_eq!(json["status"], "ok");
    assert_eq!(json["source"], "working_tree");
}

#[tokio::test]
async fn code_endpoints_set_cache_control_no_store() {
    let fx = fixture();
    fx.inspect_at(&fx.head);
    let uris = [
        QUERY_BASE.to_string(),
        "/api/onboard/finding-context?repo=acme%2Fnever-linked&path=a&line=1".to_string(),
        "/api/onboard/finding-context?repo=acme%2Fapp&path=..%2Fx&line=1".to_string(),
        format!("{QUERY_BASE}&sha=bad"),
    ];
    for uri in uris {
        let (_, cache, _) = get(&fx, &uri).await;
        assert_eq!(cache.as_deref(), Some("no-store"), "{uri}");
    }
    delete_the_source(&fx).await;
    let (_, cache, _) = get(&fx, QUERY_BASE).await;
    assert_eq!(cache.as_deref(), Some("no-store"), "deleted");
}

#[tokio::test]
async fn the_resolver_says_where_code_may_be_read_from() {
    let fx = fixture();
    // No inspection: working tree.
    let read = source_for_read(&fx.state, None, REPO, None).await.unwrap();
    assert!(matches!(read, SourceRead::WorkingTree { .. }));
    // Inspected: git at that commit.
    fx.inspect_at(&fx.head);
    let read = source_for_read(&fx.state, Some(&fx.project_id), REPO, None)
        .await
        .unwrap();
    assert!(matches!(&read, SourceRead::Git { sha, head_differs: false, .. } if *sha == fx.head));
    // An unknown repo: unresolved.
    let read = source_for_read(&fx.state, None, "acme/unknown", None)
        .await
        .unwrap();
    assert!(matches!(read, SourceRead::Unresolved { .. }));
    // Deleted wins.
    delete_the_source(&fx).await;
    let read = source_for_read(&fx.state, None, REPO, None).await.unwrap();
    assert!(matches!(read, SourceRead::Deleted { .. }));
}
