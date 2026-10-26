//! `GET /api/onboard/source-file`: the whole file at the inspected commit, redacted and
//! highlighted; nothing at all once the source is deleted.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use super::delete::{delete_source, DeleteOutcome, DeleteRequest};
use super::test_support::{fixture, Fx, REPO};
use crate::onboard::finding_context::WORKING_TREE_READS;
use crate::test_git::{commit_all, write_file};

const BASE: &str = "/api/onboard/source-file?repo=acme%2Fapp";

fn uri(path: &str, extra: &str) -> String {
    format!("{BASE}&path={}{extra}", path.replace('/', "%2F"))
}

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

/// The returned lines as plain text, one string per line.
fn file_lines(json: &serde_json::Value) -> Vec<String> {
    json["lines"]
        .as_array()
        .map(|lines| {
            lines
                .iter()
                .map(|segments| {
                    segments
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|seg| seg[1].as_str().unwrap())
                        .collect::<String>()
                })
                .collect()
        })
        .unwrap_or_default()
}

fn delete_args() -> DeleteRequest {
    DeleteRequest {
        confirm: REPO.to_string(),
        include_uncommitted: false,
    }
}

/// Commit B edits the cited line 2 of `src/main.rs`; the checkout moves on to it.
fn move_checkout_on(fx: &Fx) {
    write_file(
        &fx.checkout,
        "src/main.rs",
        "fn main() {\n    println!(\"changed after the inspection\");\n}\n",
    );
    commit_all(&fx.checkout, "newer");
}

#[tokio::test]
async fn shows_the_inspected_commit_not_the_working_tree() {
    let fx = fixture();
    fx.inspect_at(&fx.head);
    move_checkout_on(&fx);
    write_file(
        &fx.checkout,
        "src/main.rs",
        "fn main() {\n    uncommitted();\n}\n",
    );

    let (status, cache, json) = get(&fx, &uri("src/main.rs", "&around=2")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(cache.as_deref(), Some("no-store"));
    assert_eq!(json["status"], "ok", "{json}");
    assert_eq!(json["source"], "git");
    assert_eq!(json["commit"], fx.head.as_str());
    assert_eq!(json["head_differs"], true);
    assert_eq!(json["language"], "rust");
    assert_eq!(json["total_lines"], 3);
    assert_eq!(json["window"]["from"], 1);
    assert_eq!(json["window"]["to"], 3);
    assert_eq!(
        file_lines(&json),
        vec!["fn main() {", "    println!(\"hi\");", "}"],
        "line numbers equal `git show <sha>:<path> | nl`"
    );
    assert_eq!(json["block"]["start_line"], 1);
    assert_eq!(json["block"]["end_line"], 3);
    assert_eq!(json["block"]["kind"], "function");
}

#[tokio::test]
async fn deleting_the_working_tree_file_does_not_change_the_output() {
    let fx = fixture();
    fx.inspect_at(&fx.head);
    let (_, _, before) = get(&fx, &uri("src/main.rs", "&around=2")).await;
    std::fs::remove_file(fx.checkout.join("src/main.rs")).unwrap();
    // Not even staged: the working tree simply has no such file now.
    let (_, _, after) = get(&fx, &uri("src/main.rs", "&around=2")).await;
    assert_eq!(before["status"], "ok");
    assert_eq!(
        before, after,
        "a git-backed read must not depend on the working tree"
    );
}

#[tokio::test]
async fn an_explicit_sha_overrides_the_inspection() {
    let fx = fixture();
    let first = fx.head.clone();
    fx.inspect_at(&first);
    move_checkout_on(&fx);
    let second = crate::client_source::git::head_sha(&fx.checkout)
        .await
        .unwrap();
    let (_, _, json) = get(&fx, &uri("src/main.rs", &format!("&sha={second}"))).await;
    assert_eq!(json["commit"], second.as_str());
    assert!(file_lines(&json)[1].contains("changed after the inspection"));
    let (_, _, json) = get(&fx, &uri("src/main.rs", &format!("&sha={first}"))).await;
    assert!(file_lines(&json)[1].contains("hi"));
}

#[tokio::test]
async fn a_missing_object_falls_back_to_the_labelled_working_tree() {
    let fx = fixture();
    fx.inspect_at(&"0".repeat(40));

    let (status, _, json) = get(&fx, &uri("src/main.rs", "&around=2")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "commit_unavailable");
    assert_eq!(json["source"], "working_tree");
    assert!(json["reason"]
        .as_str()
        .unwrap()
        .starts_with("Inspected commit not available in this clone"));
    assert_eq!(file_lines(&json)[1], "    println!(\"hi\");");

    // The working tree file gone too: nothing to show.
    std::fs::remove_file(fx.checkout.join("src/main.rs")).unwrap();
    let (_, _, json) = get(&fx, &uri("src/main.rs", "&around=2")).await;
    assert_eq!(json["status"], "file_missing");
    assert_eq!(json["source"], "working_tree");
    assert!(json.get("lines").is_none());
}

#[tokio::test]
async fn no_recorded_commit_shows_the_working_tree_and_says_so() {
    let fx = fixture();
    let (_, _, json) = get(&fx, &uri("src/main.rs", "")).await;
    assert_eq!(json["status"], "commit_unavailable");
    assert_eq!(json["source"], "working_tree");
    assert!(json["reason"].as_str().unwrap().contains("No commit"));
    assert_eq!(file_lines(&json).len(), 3);
}

#[tokio::test]
async fn a_deleted_source_reports_the_date_and_reads_nothing() {
    let fx = fixture();
    fx.inspect_at(&fx.head);
    let Ok(DeleteOutcome::Done { record, .. }) =
        delete_source(&fx.state, &fx.project_id, REPO, &delete_args()).await
    else {
        panic!("deletion failed");
    };
    WORKING_TREE_READS.with(|c| c.set(0));

    let (status, cache, json) = get(&fx, &uri("src/main.rs", "&around=2")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(cache.as_deref(), Some("no-store"));
    assert_eq!(json["status"], "source_deleted");
    assert_eq!(json["record_id"], record.id.as_str());
    assert!(json["deleted_at"].as_str().unwrap().ends_with('Z'));
    assert!(json["reason"]
        .as_str()
        .unwrap()
        .starts_with("Source deleted on "));
    assert!(json.get("lines").is_none());
    assert_ne!(json["source"], "working_tree");
    assert_eq!(
        WORKING_TREE_READS.with(std::cell::Cell::get),
        0,
        "no file may be read for a deleted source"
    );
}

#[tokio::test]
async fn an_unlinked_repo_is_file_missing() {
    let fx = fixture();
    let (status, _, json) = get(
        &fx,
        "/api/onboard/source-file?repo=acme%2Fnever-linked&path=a.rs",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "file_missing");
}

#[tokio::test]
async fn every_line_is_redacted_and_the_safe_twins_are_not() {
    use camerata_secrets::fixtures::{planted_all, stripe_docs_test_key};

    let fx = fixture();
    let planted = planted_all();
    let twin = stripe_docs_test_key();
    let mut body = planted
        .iter()
        .map(|p| p.text.clone())
        .collect::<Vec<_>>()
        .join("\n");
    body.push_str(&format!(
        "\nSTRIPE_TEST_KEY={twin}\nconst a = process.env.X;\n"
    ));
    write_file(&fx.checkout, "conf/secrets.txt", &body);
    let committed = commit_all(&fx.checkout, "add secrets fixture");
    fx.inspect_at(&committed);

    let (_, _, json) = get(&fx, &uri("conf/secrets.txt", "&around=1")).await;

    assert_eq!(json["status"], "ok", "{json}");
    let shown = file_lines(&json).join("\n");
    assert_eq!(
        shown.lines().count(),
        body.lines().count(),
        "redaction must keep the line count"
    );
    for p in &planted {
        for part in p.value.lines().filter(|l| !l.is_empty()) {
            assert!(!shown.contains(part), "{:?} leaked: {shown}", p.shape);
        }
    }
    assert!(
        shown.contains(&twin),
        "the documented test key is a safe twin"
    );
    assert!(shown.contains("const a = process.env.X;"));
}

#[tokio::test]
async fn redaction_also_covers_the_working_tree_fallback() {
    use camerata_secrets::fixtures::planted;
    use camerata_secrets::SecretShape;

    let fx = fixture();
    let secret = planted(SecretShape::GitHubToken);
    write_file(&fx.checkout, "k.env", &format!("{}\n", secret.text));
    // No inspection: the working tree is what is shown.
    let (_, _, json) = get(&fx, &uri("k.env", "")).await;
    assert_eq!(json["source"], "working_tree");
    assert!(!file_lines(&json).join("\n").contains(&secret.value));
}

#[tokio::test]
async fn path_traversal_is_a_400_for_all_three_shapes() {
    let fx = fixture();
    fx.inspect_at(&fx.head);
    // A symlink inside the checkout pointing outside it (working-tree jail).
    let outside = fx.root.join("outside.txt");
    std::fs::write(&outside, "outside\n").unwrap();
    std::os::unix::fs::symlink(&outside, fx.checkout.join("escape.txt")).unwrap();

    for path in [
        "..%2F..%2Fetc%2Fpasswd",
        "%2Fetc%2Fpasswd",
        "src%2F..%2F..%2Foutside.rs",
        "escape.txt",
    ] {
        for extra in ["", "&sha=0000000000000000000000000000000000000000"] {
            let (status, cache, json) = get(&fx, &format!("{BASE}&path={path}{extra}")).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{path}{extra}: {json}");
            assert_eq!(json["status"], "path_traversal", "{path}{extra}");
            assert!(json.get("lines").is_none());
            assert_eq!(cache.as_deref(), Some("no-store"));
        }
    }
    // The same symlink with no inspected commit reads the working tree and is refused too.
    let fresh = fixture();
    let outside = fresh.root.join("outside.txt");
    std::fs::write(&outside, "outside\n").unwrap();
    std::os::unix::fs::symlink(&outside, fresh.checkout.join("escape.txt")).unwrap();
    let (status, _, json) = get(&fresh, &uri("escape.txt", "")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
}

#[tokio::test]
async fn a_sha_that_is_not_forty_hex_characters_is_rejected_before_git() {
    let fx = fixture();
    for bad in [
        "--upload-pack=x",
        "-1",
        "-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "abc",
        &"g".repeat(40),
        &"a".repeat(41),
        "HEAD",
    ] {
        let (status, cache, json) = get(&fx, &uri("src/main.rs", &format!("&sha={bad}"))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
        assert_eq!(json["code"], "invalid_sha", "{bad}");
        assert_eq!(cache.as_deref(), Some("no-store"));
    }
}

#[tokio::test]
async fn a_path_that_starts_with_a_dash_is_a_file_name_not_an_option() {
    let fx = fixture();
    write_file(&fx.checkout, "-rf", "fn dashed() {}\n");
    let committed = commit_all(&fx.checkout, "dash file");
    fx.inspect_at(&committed);
    let (_, _, json) = get(&fx, &uri("-rf", "")).await;
    assert_eq!(json["status"], "ok", "{json}");
    assert_eq!(file_lines(&json), vec!["fn dashed() {}"]);
}

#[tokio::test]
async fn size_policy_whole_then_windowed_then_refused() {
    let fx = fixture();
    // 600 KB of short lines: windowed around the requested line.
    let windowed: String = (1..=30_000).map(|n| format!("let v{n} = {n};\n")).collect();
    assert!(windowed.len() > 400_000 && windowed.len() < 4_000_000);
    write_file(&fx.checkout, "src/windowed.rs", &windowed);
    // Just under the whole-file cap: returned whole.
    let whole = "x\n".repeat(150_000);
    write_file(&fx.checkout, "src/whole.txt", &whole);
    // Over 4 MB: refused.
    write_file(
        &fx.checkout,
        "src/huge.txt",
        &"x".repeat(4 * 1024 * 1024 + 1),
    );
    // Not UTF-8.
    std::fs::write(
        fx.checkout.join("src/binary.rs"),
        [0xFFu8, 0xFE, 0x00, 0xC0],
    )
    .unwrap();
    let committed = commit_all(&fx.checkout, "sizes");
    fx.inspect_at(&committed);

    let (_, _, json) = get(&fx, &uri("src/whole.txt", "&around=5")).await;
    assert_eq!(json["status"], "ok");
    assert_eq!(json["window"]["from"], 1);
    assert_eq!(json["window"]["to"], 150_000);
    assert_eq!(file_lines(&json).len(), 150_000);

    let (_, _, json) = get(&fx, &uri("src/windowed.rs", "&around=15000")).await;
    assert_eq!(json["status"], "ok");
    assert_eq!(json["total_lines"], 30_000);
    assert_eq!(json["window"]["from"], 14_600);
    assert_eq!(json["window"]["to"], 15_399);
    let lines = file_lines(&json);
    assert_eq!(lines.len(), 800);
    assert_eq!(lines[0], "let v14600 = 14600;");
    assert_eq!(json["block"]["kind"], "window");

    let (_, _, json) = get(&fx, &uri("src/windowed.rs", "")).await;
    assert_eq!(
        json["window"]["from"], 1,
        "no `around` windows from the top"
    );

    let (status, _, json) = get(&fx, &uri("src/huge.txt", "")).await;
    assert_eq!(
        (status, json["status"].as_str()),
        (StatusCode::OK, Some("too_large"))
    );
    assert!(json.get("lines").is_none());

    let (_, _, json) = get(&fx, &uri("src/binary.rs", "")).await;
    assert_eq!(json["status"], "not_utf8");
}

#[tokio::test]
async fn the_working_tree_fallback_applies_the_same_size_policy() {
    let fx = fixture();
    write_file(&fx.checkout, "huge.txt", &"x".repeat(4 * 1024 * 1024 + 1));
    let (_, _, json) = get(&fx, &uri("huge.txt", "")).await;
    assert_eq!(json["status"], "too_large");
    assert_eq!(json["source"], "working_tree");
}

#[tokio::test]
async fn lines_come_back_highlighted_with_a_class_table() {
    let fx = fixture();
    write_file(
        &fx.checkout,
        "src/h.rs",
        "// note\nfn f() {\n    let s = \"x\";\n}\n",
    );
    let committed = commit_all(&fx.checkout, "h");
    fx.inspect_at(&committed);

    let (_, _, json) = get(&fx, &uri("src/h.rs", "&around=3")).await;

    assert_eq!(json["highlight"], "ok");
    let classes: Vec<&str> = json["classes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap())
        .collect();
    assert_eq!(classes.len(), 10);
    let used = |class: &str| {
        let id = classes.iter().position(|c| *c == class).unwrap();
        json["lines"].as_array().unwrap().iter().any(|line| {
            line.as_array()
                .unwrap()
                .iter()
                .any(|seg| seg[0].as_u64() == Some(id as u64))
        })
    };
    assert!(
        used("keyword") && used("string") && used("comment"),
        "{json}"
    );
    // Plain segments carry a null class.
    assert!(json["lines"][2][0][0].is_null());
}

#[tokio::test]
async fn reading_never_changes_the_checkout() {
    let fx = fixture();
    fx.inspect_at(&fx.head);
    let before = crate::test_git::git(&fx.checkout, &["status", "--porcelain=v1"]);
    let head = crate::client_source::git::head_sha(&fx.checkout).await;
    for extra in [
        "",
        "&around=2",
        "&sha=0000000000000000000000000000000000000000",
    ] {
        get(&fx, &uri("src/main.rs", extra)).await;
    }
    assert_eq!(
        crate::test_git::git(&fx.checkout, &["status", "--porcelain=v1"]),
        before
    );
    assert_eq!(
        crate::client_source::git::head_sha(&fx.checkout).await,
        head
    );
}
