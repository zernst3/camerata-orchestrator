//! `POST /api/onboard/finding-context/batch`: the stored line plus three lines either side for
//! many findings at once, from the inspected commit, redacted before it is cut; nothing at all
//! once the source is deleted.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use super::delete::{delete_source, DeleteOutcome, DeleteRequest};
use super::test_support::{fixture, Fx, REPO};
use crate::onboard::finding_context::WORKING_TREE_READS;
use crate::test_git::{commit_all, write_file};

const URI: &str = "/api/onboard/finding-context/batch";

async fn post(fx: &Fx, body: serde_json::Value) -> (StatusCode, Option<String>, serde_json::Value) {
    let resp = crate::router(fx.state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(URI)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
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

fn item(path: &str, line: usize) -> serde_json::Value {
    serde_json::json!({ "repo": REPO, "path": path, "line": line })
}

fn lines_of(entry: &serde_json::Value) -> Vec<String> {
    entry["lines"]
        .as_array()
        .map(|l| l.iter().map(|s| s.as_str().unwrap().to_string()).collect())
        .unwrap_or_default()
}

/// A 20 line file whose line `n` reads `row n`, committed and recorded as the inspection.
fn commit_numbered(fx: &Fx, path: &str) -> String {
    let body: String = (1..=20).map(|n| format!("row {n}\n")).collect();
    write_file(&fx.checkout, path, &body);
    let sha = commit_all(&fx.checkout, "numbered");
    fx.inspect_at(&sha);
    sha
}

#[tokio::test]
async fn returns_the_stored_line_and_three_either_side_keyed_for_the_client() {
    let fx = fixture();
    let sha = commit_numbered(&fx, "src/rows.txt");

    let (status, cache, json) = post(
        &fx,
        serde_json::json!({ "items": [
            { "repo": REPO, "path": "src/rows.txt", "line": 10, "key": "k-ten" },
            { "repo": REPO, "path": "src/rows.txt", "line": 1 },
            { "repo": REPO, "path": "src/rows.txt", "line": 20, "sha": sha },
        ]}),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(cache.as_deref(), Some("no-store"));
    let items = json["items"].as_array().unwrap();
    assert_eq!(items.len(), 3, "{json}");

    let ten = &items[0];
    assert_eq!(ten["status"], "ok", "{ten}");
    assert_eq!(ten["source"], "git");
    assert_eq!(ten["commit"], sha.as_str());
    assert_eq!(
        (ten["index"].as_u64(), ten["key"].as_str()),
        (Some(0), Some("k-ten"))
    );
    assert_eq!(ten["repo"], REPO);
    assert_eq!(ten["path"], "src/rows.txt");
    assert_eq!(ten["line"], 10);
    assert_eq!(ten["sha"], "", "the sha is echoed as sent");
    assert_eq!(
        (ten["start_line"].as_u64(), ten["end_line"].as_u64()),
        (Some(7), Some(13))
    );
    assert_eq!(ten["violation_line"], 10);
    assert_eq!(ten["kind"], "window");
    assert_eq!(
        lines_of(ten),
        ["row 7", "row 8", "row 9", "row 10", "row 11", "row 12", "row 13"]
    );

    // Clipped at both ends of the file; the lines stay numbered by `start_line`.
    let first = &items[1];
    assert_eq!(
        (first["start_line"].as_u64(), first["end_line"].as_u64()),
        (Some(1), Some(4))
    );
    assert!(first.get("key").is_none(), "no key sent, none echoed");
    let last = &items[2];
    assert_eq!(
        (last["start_line"].as_u64(), last["end_line"].as_u64()),
        (Some(17), Some(20))
    );
    assert_eq!(last["sha"], sha.as_str());
}

#[tokio::test]
async fn shows_the_inspected_commit_not_the_working_tree() {
    let fx = fixture();
    commit_numbered(&fx, "src/rows.txt");
    write_file(&fx.checkout, "src/rows.txt", "changed\nafter\nthe scan\n");
    commit_all(&fx.checkout, "newer");
    write_file(&fx.checkout, "src/rows.txt", "uncommitted\n");

    let (_, _, json) = post(
        &fx,
        serde_json::json!({ "items": [item("src/rows.txt", 2)] }),
    )
    .await;

    let entry = &json["items"][0];
    assert_eq!(entry["status"], "ok", "{entry}");
    assert_eq!(entry["head_differs"], true);
    assert_eq!(lines_of(entry)[0], "row 1");
    std::fs::remove_file(fx.checkout.join("src/rows.txt")).unwrap();
    let (_, _, after) = post(
        &fx,
        serde_json::json!({ "items": [item("src/rows.txt", 2)] }),
    )
    .await;
    assert_eq!(
        &json, &after,
        "a git-backed read must not depend on the working tree"
    );
}

#[tokio::test]
async fn one_request_mixes_repos_and_every_status_in_the_shared_vocabulary() {
    let fx = fixture();
    commit_numbered(&fx, "src/rows.txt");
    let other = fx.add_repo("acme/other");
    let other_head = crate::client_source::git::head_sha(&other).await.unwrap();
    // A binary file and a file over the 400 KB cap, both committed.
    std::fs::write(fx.checkout.join("bin.dat"), [0xFFu8, 0xFE, 0x00, 0x01]).unwrap();
    write_file(&fx.checkout, "big.txt", &"x".repeat(400_001));
    let head = commit_all(&fx.checkout, "binary and big");
    let absent = "0".repeat(40);

    let (status, _, json) = post(
        &fx,
        serde_json::json!({ "items": [
            { "repo": REPO, "path": "src/rows.txt", "line": 5, "sha": head },
            { "repo": "acme/other", "path": "lib.rs", "line": 1, "sha": other_head },
            { "repo": REPO, "path": "src/gone.rs", "line": 1, "sha": head },
            { "repo": REPO, "path": "src/rows.txt", "line": 999, "sha": head },
            { "repo": REPO, "path": "src/rows.txt", "line": 0, "sha": head },
            { "repo": REPO, "path": "bin.dat", "line": 1, "sha": head },
            { "repo": REPO, "path": "big.txt", "line": 1, "sha": head },
            { "repo": REPO, "path": "src/rows.txt", "line": 5, "sha": absent },
            { "repo": "acme/never-linked", "path": "a.rs", "line": 1 },
            { "repo": "../../etc", "path": "passwd", "line": 1 },
        ]}),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let items = json["items"].as_array().unwrap();
    let statuses: Vec<&str> = items
        .iter()
        .map(|i| i["status"].as_str().unwrap())
        .collect();
    assert_eq!(
        statuses,
        [
            "ok",
            "ok",
            "file_missing",
            "line_gone",
            "line_gone",
            "not_utf8",
            "too_large",
            "commit_unavailable",
            "file_missing",
            "file_missing",
        ],
        "{json}"
    );
    assert_eq!(items[1]["repo"], "acme/other");
    assert_eq!(lines_of(&items[1]), ["pub fn f() {}"]);
    // No working-tree fallback: a missing commit shows no code under the stored snippet.
    assert!(items[7].get("lines").is_none(), "{}", items[7]);
    assert!(items[7]["reason"]
        .as_str()
        .unwrap()
        .starts_with("Inspected commit not available in this clone"));
    // Every refusal carries its echo, in request order.
    for (i, entry) in items.iter().enumerate() {
        assert_eq!(entry["index"], i);
    }
}

#[tokio::test]
async fn no_recorded_commit_is_the_labelled_working_tree() {
    let fx = fixture();
    let (_, _, json) = post(
        &fx,
        serde_json::json!({ "items": [item("src/main.rs", 2)] }),
    )
    .await;
    let entry = &json["items"][0];
    assert_eq!(entry["status"], "ok", "{entry}");
    assert_eq!(entry["source"], "working_tree");
    assert!(entry.get("commit").is_none());
    assert_eq!(
        lines_of(entry),
        ["fn main() {", "    println!(\"hi\");", "}"]
    );
}

#[tokio::test]
async fn a_file_cited_many_times_is_read_once() {
    let fx = fixture();
    WORKING_TREE_READS.with(|c| c.set(0));
    let items: Vec<_> = (1..=3).map(|l| item("src/main.rs", l)).collect();
    let (_, _, json) = post(&fx, serde_json::json!({ "items": items })).await;
    assert!(json["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|i| i["status"] == "ok"));
    assert_eq!(WORKING_TREE_READS.with(std::cell::Cell::get), 1);
}

#[tokio::test]
async fn expect_flags_a_line_that_no_longer_matches_the_stored_snippet() {
    let fx = fixture();
    commit_numbered(&fx, "src/rows.txt");
    let (_, _, json) = post(
        &fx,
        serde_json::json!({ "items": [
            { "repo": REPO, "path": "src/rows.txt", "line": 4, "expect": "row 4" },
            { "repo": REPO, "path": "src/rows.txt", "line": 4, "expect": "something else" },
            { "repo": REPO, "path": "src/rows.txt", "line": 4 },
        ]}),
    )
    .await;
    let m: Vec<bool> = json["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["matches_snippet"].as_bool().unwrap())
        .collect();
    assert_eq!(m, [true, false, true]);
}

#[tokio::test]
async fn a_deleted_source_reports_the_date_and_reads_nothing() {
    let fx = fixture();
    let sha = commit_numbered(&fx, "src/rows.txt");
    fx.mark_head_pushed();
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
    WORKING_TREE_READS.with(|c| c.set(0));

    let (status, cache, json) = post(
        &fx,
        serde_json::json!({ "items": [item("src/rows.txt", 5), { "repo": REPO, "path": "src/rows.txt", "line": 6, "sha": sha }] }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(cache.as_deref(), Some("no-store"));
    for entry in json["items"].as_array().unwrap() {
        assert_eq!(entry["status"], "source_deleted", "{entry}");
        assert_eq!(entry["record_id"], record.id.as_str());
        assert!(entry["reason"]
            .as_str()
            .unwrap()
            .starts_with("Source deleted on "));
        assert!(entry.get("lines").is_none());
        assert_ne!(entry["source"], "working_tree");
    }
    assert_eq!(WORKING_TREE_READS.with(std::cell::Cell::get), 0);
}

#[tokio::test]
async fn path_traversal_is_refused_per_item_and_reads_nothing() {
    let fx = fixture();
    let sha = commit_numbered(&fx, "src/rows.txt");
    WORKING_TREE_READS.with(|c| c.set(0));
    let outside = fx.root.join("outside.txt");
    std::fs::write(&outside, "outside\n").unwrap();
    std::os::unix::fs::symlink(&outside, fx.checkout.join("escape.txt")).unwrap();
    commit_all(&fx.checkout, "symlink");

    let traversals = ["../../etc/passwd", "/etc/passwd", "src/../../outside.txt"];
    let mut items: Vec<_> = traversals
        .iter()
        .map(|p| serde_json::json!({ "repo": REPO, "path": p, "line": 1, "sha": sha }))
        .collect();
    // Without a recorded sha the same shapes go through the working-tree jail.
    items.extend(traversals.iter().map(|p| item(p, 1)));
    items.push(serde_json::json!({ "repo": REPO, "path": "src/rows.txt", "line": 2, "sha": sha }));
    // A symlink committed into the repo that points outside it.
    items.push(serde_json::json!({ "repo": REPO, "path": "escape.txt", "line": 1, "sha": crate::client_source::git::head_sha(&fx.checkout).await.unwrap() }));

    let (status, _, json) = post(&fx, serde_json::json!({ "items": items })).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "one bad tuple must not blank the rest"
    );
    let items = json["items"].as_array().unwrap();
    for entry in items.iter().take(6) {
        assert_eq!(entry["status"], "path_traversal", "{entry}");
        assert!(entry.get("lines").is_none());
    }
    assert_eq!(items[6]["status"], "ok", "the good tuple still reads");
    // A symlink out of the checkout is caught by the same jail as the other shapes.
    assert_eq!(items[7]["status"], "path_traversal", "{}", items[7]);
    assert_eq!(WORKING_TREE_READS.with(std::cell::Cell::get), 0);
    assert!(!json.to_string().contains("outside\n"));
}

#[tokio::test]
async fn more_than_200_tuples_and_a_malformed_sha_are_400s() {
    let fx = fixture();
    let many: Vec<_> = (0..201).map(|_| item("src/main.rs", 1)).collect();
    let (status, cache, json) = post(&fx, serde_json::json!({ "items": many })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(cache.as_deref(), Some("no-store"));
    assert_eq!(json["code"], "too_many_items");

    let exactly: Vec<_> = (0..200).map(|_| item("src/main.rs", 1)).collect();
    let (status, _, json) = post(&fx, serde_json::json!({ "items": exactly })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["items"].as_array().unwrap().len(), 200);

    let (status, _, json) = post(
        &fx,
        serde_json::json!({ "items": [
            item("src/main.rs", 1),
            { "repo": REPO, "path": "src/main.rs", "line": 1, "sha": "not-a-sha" },
        ]}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["code"], "invalid_sha");

    let (status, _, json) = post(&fx, serde_json::json!({ "items": [] })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["items"], serde_json::json!([]));
}

/// Forty one-line files in one commit, recorded as the inspection; 200 tuples over them.
fn many_files(fx: &Fx) -> Vec<serde_json::Value> {
    for n in 0..40 {
        write_file(
            &fx.checkout,
            &format!("src/f{n}.rs"),
            &format!("fn f{n}() {{}}\n// two\n"),
        );
    }
    let sha = commit_all(&fx.checkout, "many files");
    fx.inspect_at(&sha);
    (0..200)
        .map(|i| item(&format!("src/f{}.rs", i % 40), 1 + i % 2))
        .collect()
}

/// M2: the gate seam. A 200-tuple batch holds at most `READ_CONCURRENCY` read slots at once
/// (raising the concurrency, or reading every file at once, would show 3 or 4 here), takes
/// exactly one slot per distinct file (the git path reads each file once, however many tuples
/// cite it), and checks the commit once, not once per file.
#[tokio::test]
async fn a_200_item_batch_holds_few_slots_reads_each_file_once_and_checks_the_commit_once() {
    use crate::onboard::finding_context::trace;
    use crate::onboard::finding_context_batch::READ_CONCURRENCY;

    let fx = fixture();
    let items = many_files(&fx);
    trace::reset();

    let (status, _, json) = post(&fx, serde_json::json!({ "items": items })).await;

    assert_eq!(status, StatusCode::OK);
    let answered = json["items"].as_array().unwrap();
    assert_eq!(answered.len(), 200);
    assert!(answered.iter().all(|i| i["status"] == "ok"), "{json}");
    let (most_at_once, taken) = trace::slots();
    assert!(
        (1..=READ_CONCURRENCY).contains(&most_at_once),
        "held {most_at_once} slots at once; the batch may hold at most {READ_CONCURRENCY}"
    );
    assert_eq!(taken, 40, "one slot per distinct file, not per tuple");
    assert_eq!(
        trace::VERIFIES.with(std::cell::Cell::get),
        1,
        "one commit check for the whole batch"
    );
}

/// The same batch with a second request racing it, as the viewer would: neither starves.
#[tokio::test]
async fn a_200_item_batch_racing_another_request_starves_neither() {
    let fx = fixture();
    let items = many_files(&fx);
    let (a, b) = tokio::join!(
        post(&fx, serde_json::json!({ "items": items })),
        post(&fx, serde_json::json!({ "items": [item("src/f3.rs", 1)] })),
    );
    let statuses: Vec<String> = a.2["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["status"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(statuses.len(), 200);
    assert!(statuses.iter().all(|s| s == "ok"), "{statuses:?}");
    assert_eq!(b.2["items"][0]["status"], "ok");
}

/// L5: one check per distinct (repo, sha), whatever the number of files.
#[tokio::test]
async fn the_commit_is_checked_once_per_repo_and_sha() {
    use crate::onboard::finding_context::trace;

    let fx = fixture();
    let other = fx.add_repo("acme/other");
    let other_head = crate::client_source::git::head_sha(&other).await.unwrap();
    let sha = commit_numbered(&fx, "src/rows.txt");
    trace::reset();
    let (_, _, json) = post(
        &fx,
        serde_json::json!({ "items": [
            { "repo": REPO, "path": "src/rows.txt", "line": 5, "sha": sha },
            { "repo": REPO, "path": "src/main.rs", "line": 1, "sha": sha },
            { "repo": REPO, "path": "src/gone.rs", "line": 1, "sha": sha },
            { "repo": "acme/other", "path": "lib.rs", "line": 1, "sha": other_head },
        ]}),
    )
    .await;
    assert_eq!(json["items"][0]["status"], "ok", "{json}");
    assert_eq!(json["items"][3]["status"], "ok", "{json}");
    assert_eq!(
        trace::VERIFIES.with(std::cell::Cell::get),
        2,
        "two (repo, sha) pairs, two checks"
    );
}

/// L6: every sha is validated before anything is resolved, so a malformed one starts no process
/// at all, even when valid items come first.
#[tokio::test]
async fn a_malformed_sha_is_refused_before_any_git_process_starts() {
    use crate::onboard::finding_context::trace;
    use crate::workspace::test_seams::commands_built;

    let fx = fixture();
    fx.inspect_at(&fx.head);
    trace::reset();
    let before = commands_built();

    let (status, cache, json) = post(
        &fx,
        serde_json::json!({ "items": [
            item("src/main.rs", 1),
            { "repo": REPO, "path": "src/main.rs", "line": 2, "sha": "nope" },
        ]}),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(cache.as_deref(), Some("no-store"));
    assert_eq!(json["code"], "invalid_sha");
    assert_eq!(commands_built(), before, "no git command was even built");
    assert_eq!(trace::VERIFIES.with(std::cell::Cell::get), 0);
}

/// L12: the staleness check compares the line AS DISPLAYED. A line inside a key body is shown
/// redacted, so a caller's `expect` holding the real text must not match it, on either endpoint.
#[tokio::test]
async fn expect_cannot_probe_a_line_the_display_redacts() {
    use camerata_secrets::fixtures::pem_block;

    let fx = fixture();
    let pem = pem_block("RSA ", 6);
    let pem_lines: Vec<String> = pem.lines().map(str::to_string).collect();
    let mut body = String::from("# keys\n");
    body.push_str(&pem);
    body.push_str("\nplain line\n");
    write_file(&fx.checkout, "conf/key.txt", &body);
    let sha = commit_all(&fx.checkout, "key file");
    fx.inspect_at(&sha);
    // Line 1 is the comment, line 2 the BEGIN line, line 4 the second body line.
    let secret = pem_lines[2].clone();

    let (_, _, batch) = post(
        &fx,
        serde_json::json!({ "items": [
            { "repo": REPO, "path": "conf/key.txt", "line": 4, "expect": secret },
            { "repo": REPO, "path": "conf/key.txt", "line": 1, "expect": "# keys" },
        ]}),
    )
    .await;
    assert_eq!(batch["items"][0]["status"], "ok", "{batch}");
    assert_eq!(
        batch["items"][0]["matches_snippet"], false,
        "the real key text must not match the redacted line it hides"
    );
    assert!(!batch.to_string().contains(&secret), "and it never leaves");
    assert_eq!(batch["items"][1]["matches_snippet"], true, "the control");

    let ctx = |line: usize, expect: &str| {
        let query = format!(
            "/api/onboard/finding-context?repo=acme%2Fapp&path=conf%2Fkey.txt&line={line}&expect={}",
            expect.replace(' ', "%20").replace('+', "%2B").replace('/', "%2F")
        );
        let router = crate::router(fx.state.clone());
        async move {
            let resp = router
                .oneshot(Request::builder().uri(query).body(Body::empty()).unwrap())
                .await
                .unwrap();
            let bytes = resp.into_body().collect().await.unwrap().to_bytes();
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()
        }
    };
    let single = ctx(4, &secret).await;
    assert_eq!(single["status"], "ok", "{single}");
    assert_eq!(
        single["matches_snippet"], false,
        "finding-context compares the displayed line too"
    );
    assert!(!single.to_string().contains(&secret));
    assert_eq!(
        ctx(1, "# keys").await["matches_snippet"],
        true,
        "the control"
    );
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
    let total = body.lines().count();
    write_file(&fx.checkout, "conf/secrets.txt", &body);
    let sha = commit_all(&fx.checkout, "add secrets fixture");
    fx.inspect_at(&sha);

    // A window around every line of the file, so every planted line is shown at least once.
    let items: Vec<_> = (1..=total).map(|l| item("conf/secrets.txt", l)).collect();
    let (_, _, json) = post(&fx, serde_json::json!({ "items": items })).await;

    let shown: String = json["items"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(lines_of)
        .collect::<Vec<_>>()
        .join("\n");
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

/// A key block that starts far above the window of the finding is still caught inside the
/// window: the whole file is redacted before it is cut.
#[tokio::test]
async fn a_pem_block_starting_above_the_window_is_redacted_inside_it() {
    use camerata_secrets::fixtures::pem_block;

    let fx = fixture();
    let pem = pem_block("RSA ", 12);
    let pem_lines: Vec<String> = pem.lines().map(str::to_string).collect();
    let mut lines: Vec<String> = (1..=60).map(|n| format!("let v{n} = {n};")).collect();
    // The block's first line is on line 20; the finding is on line 30, whose window (27..33)
    // holds only key BODY lines, none of them the `-----BEGIN` line.
    for (i, l) in pem_lines.iter().enumerate() {
        lines[19 + i] = l.clone();
    }
    write_file(&fx.checkout, "src/key.rs", &(lines.join("\n") + "\n"));
    let sha = commit_all(&fx.checkout, "pem");
    fx.inspect_at(&sha);

    let (_, _, json) = post(
        &fx,
        serde_json::json!({ "items": [item("src/key.rs", 30)] }),
    )
    .await;

    let entry = &json["items"][0];
    assert_eq!(entry["status"], "ok", "{entry}");
    assert_eq!(entry["start_line"], 27);
    let shown = lines_of(entry);
    assert_eq!(shown.len(), 7);
    for body_line in &pem_lines[1..pem_lines.len() - 1] {
        assert!(
            !shown.iter().any(|l| l.contains(body_line.as_str())),
            "key body leaked"
        );
    }
}
