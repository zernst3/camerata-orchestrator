//! `GET /api/onboard/symbol-refs`: where a symbol is used, at the inspected commit, redacted,
//! capped; nothing at all once the source is deleted.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use super::delete::{delete_source, DeleteOutcome, DeleteRequest};
use super::test_support::{fixture, Fx, REPO};
use crate::onboard::symbol_refs::{SearchLimits, GREP_RUNS, MAX_HITS};
use crate::test_git::{commit_all, git, write_file};

fn uri(symbol: &str, extra: &str) -> String {
    format!("/api/onboard/symbol-refs?repo=acme%2Fapp&symbol={symbol}{extra}")
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
    // A framework rejection (a missing parameter) is plain text, not JSON.
    let json = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, cache, json)
}

/// `(path, line)` of every hit.
fn locations(json: &serde_json::Value) -> Vec<(String, u64)> {
    json["hits"]
        .as_array()
        .map(|hits| {
            hits.iter()
                .map(|h| {
                    (
                        h["path"].as_str().unwrap().to_string(),
                        h["line"].as_u64().unwrap(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The reference answer: plain `git grep -n -w -I` on the commit, minus the scan's noise paths.
fn reference(fx: &Fx, sha: &str, symbol: &str) -> Vec<(String, u64)> {
    let out = std::process::Command::new("git")
        .current_dir(&fx.checkout)
        .args(["grep", "-n", "-w", "-I", "-F", "-e", symbol, sha, "--"])
        .output()
        .unwrap();
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter_map(|l| {
            let rest = l.strip_prefix(&format!("{sha}:"))?;
            // `path:line:text`, where the path may itself hold a colon: the first `:<digits>:`.
            let cut = rest.match_indices(':').find_map(|(i, _)| {
                let tail = &rest[i + 1..];
                let digits = tail.split(':').next()?;
                (!digits.is_empty()
                    && digits.bytes().all(|b| b.is_ascii_digit())
                    && tail.len() > digits.len())
                .then_some((i, digits))
            })?;
            Some((rest[..cut.0].to_string(), cut.1.parse().ok()?))
        })
        .filter(|(path, _)| !crate::onboard::files::is_noise_path(path, &[]))
        .collect()
}

fn delete_args() -> DeleteRequest {
    DeleteRequest {
        confirm: REPO.to_string(),
        include_uncommitted: false,
    }
}

/// A repo with the symbol `needle_fn` used in several places, including in noise paths, a
/// binary file and as a substring of longer words.
fn commit_usages(fx: &Fx) -> String {
    write_file(
        &fx.checkout,
        "src/lib.rs",
        "pub fn needle_fn() {}\nfn other() { needle_fn(); }\nfn longer() { needle_fn_extra(); }\n",
    );
    write_file(
        &fx.checkout,
        "src/dir:colon.rs",
        "// uses needle_fn here\r\nlet x = 1;\n",
    );
    write_file(&fx.checkout, "node_modules/dep/index.js", "needle_fn();\n");
    write_file(&fx.checkout, "Cargo.lock", "needle_fn\n");
    write_file(&fx.checkout, "web/app.min.js", "needle_fn()\n");
    std::fs::write(fx.checkout.join("blob.bin"), b"\x00\x01needle_fn\x00\x02\n").unwrap();
    commit_all(&fx.checkout, "usages")
}

#[tokio::test]
async fn hits_equal_git_grep_on_the_inspected_commit_minus_noise() {
    let fx = fixture();
    let sha = commit_usages(&fx);
    fx.inspect_at(&sha);
    // The checkout moves on: the answer is still the inspected commit's.
    write_file(&fx.checkout, "src/newer.rs", "needle_fn();\n");
    commit_all(&fx.checkout, "newer");

    let (status, cache, json) = get(&fx, &uri("needle_fn", "")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(cache.as_deref(), Some("no-store"));
    assert_eq!(json["status"], "ok", "{json}");
    assert_eq!(json["source"], "git");
    assert_eq!(json["commit"], sha.as_str());
    assert_eq!(json["head_differs"], true);
    assert_eq!(json["symbol"], "needle_fn");
    assert_eq!(json["more"], false);
    let got = locations(&json);
    assert_eq!(got, reference(&fx, &sha, "needle_fn"));
    // The whole-word search, the colon path and the noise filter, spelled out.
    assert_eq!(
        got,
        vec![
            ("src/dir:colon.rs".to_string(), 1),
            ("src/lib.rs".to_string(), 1),
            ("src/lib.rs".to_string(), 2),
        ]
    );
    assert_eq!(json["hits"][0]["text"], "// uses needle_fn here");
    assert_eq!(json["hits"][2]["text"], "fn other() { needle_fn(); }");
}

#[tokio::test]
async fn an_explicit_sha_overrides_the_inspection() {
    let fx = fixture();
    let first = commit_usages(&fx);
    fx.inspect_at(&fx.head);
    let (_, _, json) = get(&fx, &uri("needle_fn", &format!("&sha={first}"))).await;
    assert_eq!(json["commit"], first.as_str());
    assert_eq!(locations(&json).len(), 3);
    let (_, _, json) = get(&fx, &uri("needle_fn", "")).await;
    assert_eq!(json["commit"], fx.head.as_str());
    assert!(locations(&json).is_empty());
}

#[tokio::test]
async fn at_most_200_hits_and_a_more_flag() {
    let fx = fixture();
    let body: String = (0..MAX_HITS + 50)
        .map(|n| format!("let v{n} = many_uses({n});\n"))
        .collect();
    write_file(&fx.checkout, "src/many.rs", &body);
    let exact: String = (0..MAX_HITS)
        .map(|n| format!("exactly_cap({n});\n"))
        .collect();
    write_file(&fx.checkout, "src/exact.rs", &exact);
    let sha = commit_all(&fx.checkout, "many");
    fx.inspect_at(&sha);

    let (_, _, json) = get(&fx, &uri("many_uses", "")).await;
    assert_eq!(json["status"], "ok");
    assert_eq!(locations(&json).len(), MAX_HITS);
    assert_eq!(json["more"], true);
    // The first 200 in git's order, line by line.
    assert_eq!(
        locations(&json),
        reference(&fx, &sha, "many_uses")[..MAX_HITS]
    );

    let (_, _, json) = get(&fx, &uri("exactly_cap", "")).await;
    assert_eq!(locations(&json).len(), MAX_HITS);
    assert_eq!(json["more"], false, "exactly the cap is not 'more'");
}

#[tokio::test]
async fn every_hit_line_is_redacted_and_the_safe_twin_is_not() {
    use camerata_secrets::fixtures::{planted_all, stripe_docs_test_key};

    let fx = fixture();
    let planted = planted_all();
    // One secret per line, each beside the symbol. A multi-line shape (a key block) is planted
    // line by line: its body lines carry no symbol and are never hits.
    let mut body = String::new();
    for p in &planted {
        for (i, l) in p.text.lines().enumerate() {
            if i == 0 {
                body.push_str(&format!("leakmarker {l}\n"));
            } else {
                body.push_str(&format!("{l}\n"));
            }
        }
    }
    let twin = stripe_docs_test_key();
    body.push_str(&format!("leakmarker STRIPE_TEST_KEY={twin}\n"));
    // A bare base64 body line that happens to hold the symbol is key-like and is never shown.
    body.push_str("MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC7+leakmarker/VJT\n");
    write_file(&fx.checkout, "conf/secrets.txt", &body);
    let sha = commit_all(&fx.checkout, "secrets");
    fx.inspect_at(&sha);

    let (_, _, json) = get(&fx, &uri("leakmarker", "")).await;

    assert_eq!(json["status"], "ok", "{json}");
    let shown: Vec<&str> = json["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["text"].as_str().unwrap())
        .collect();
    let all = shown.join("\n");
    for p in &planted {
        for part in p.value.lines().filter(|l| !l.is_empty()) {
            assert!(!all.contains(part), "{:?} leaked: {all}", p.shape);
        }
    }
    assert!(
        all.contains(&twin),
        "the documented test key is a safe twin: {all}"
    );
    assert!(shown.contains(&"[redacted]"));
    assert!(!all.contains("MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC7"));
}

#[tokio::test]
async fn a_dash_led_or_metachar_symbol_is_data_never_an_option_or_a_pattern() {
    let fx = fixture();
    write_file(
        &fx.checkout,
        "src/odd.txt",
        "-rf literal\nabc and a.c\nuse --help here\nx-y_z\n$el\n",
    );
    let sha = commit_all(&fx.checkout, "odd");
    fx.inspect_at(&sha);
    let before = git(&fx.checkout, &["status", "--porcelain=v1"]);
    let _ = std::fs::remove_file(fx.root.join("pwned"));

    // Dash-led: searched literally (`-e`), not read as a flag.
    for (symbol, line) in [("-rf", 1u64), ("--help", 3), ("x-y_z", 4)] {
        let (status, _, json) = get(&fx, &uri(symbol, "")).await;
        assert_eq!(status, StatusCode::OK, "{symbol}");
        assert_eq!(json["status"], "ok", "{symbol}: {json}");
        assert_eq!(
            locations(&json),
            vec![("src/odd.txt".to_string(), line)],
            "{symbol}"
        );
    }
    // A dot is a dot: `a.c` does not match `abc`.
    let (_, _, json) = get(&fx, &uri("a.c", "")).await;
    assert_eq!(json["hits"].as_array().unwrap().len(), 1);
    assert_eq!(json["hits"][0]["text"], "abc and a.c");
    // A dash-led symbol that is also a real option name finds nothing and changes nothing.
    for symbol in ["-O", "--no-index", "--open-files-in-pager"] {
        let (status, _, json) = get(&fx, &uri(symbol, "")).await;
        assert_eq!(
            (status, json["status"].as_str()),
            (StatusCode::OK, Some("ok"))
        );
        assert!(locations(&json).is_empty(), "{symbol}");
    }
    assert!(!fx.root.join("pwned").exists());
    assert_eq!(git(&fx.checkout, &["status", "--porcelain=v1"]), before);
}

#[tokio::test]
async fn an_invalid_symbol_is_refused_before_anything_runs() {
    let fx = fixture();
    fx.inspect_at(&fx.head);
    GREP_RUNS.with(|c| c.set(0));
    for symbol in [
        "",
        "%20",
        "a%20b",
        "--open-files-in-pager%3Dtouch%20x",
        "-O%3Dtouch%20pwned",
        ".*",
        "(a%7Cb)",
        "%5Ba-z%5D%2B",
        "a%3Brm",
        "%60id%60",
        "%24%28id%29",
        "a%2Fb",
        "a%0Ab",
        "a%00b",
        "---",
        "%3A",
        &"a".repeat(129),
    ] {
        let (status, cache, json) = get(&fx, &uri(symbol, "")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{symbol:?}: {json}");
        assert_eq!(json["code"], "invalid_symbol", "{symbol:?}");
        assert_eq!(cache.as_deref(), Some("no-store"));
        assert!(json.get("hits").is_none());
    }
    // A missing parameter is the framework's own 400.
    let (status, _, _) = get(&fx, "/api/onboard/symbol-refs?repo=acme%2Fapp").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(GREP_RUNS.with(std::cell::Cell::get), 0);
    assert!(!fx.root.join("pwned").exists());
}

#[tokio::test]
async fn a_deleted_source_reports_the_date_and_runs_nothing() {
    let fx = fixture();
    let sha = commit_usages(&fx);
    fx.mark_head_pushed();
    fx.inspect_at(&sha);
    let Ok(DeleteOutcome::Done { record, .. }) =
        delete_source(&fx.state, &fx.project_id, REPO, &delete_args()).await
    else {
        panic!("deletion failed");
    };
    GREP_RUNS.with(|c| c.set(0));

    let (status, cache, json) = get(&fx, &uri("needle_fn", "")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(cache.as_deref(), Some("no-store"));
    assert_eq!(json["status"], "source_deleted");
    assert_eq!(json["record_id"], record.id.as_str());
    assert!(json["reason"]
        .as_str()
        .unwrap()
        .starts_with("Source deleted on "));
    assert!(json.get("hits").is_none());
    assert_ne!(json["source"], "working_tree");
    assert_eq!(
        GREP_RUNS.with(std::cell::Cell::get),
        0,
        "no process may run"
    );
}

#[tokio::test]
async fn a_missing_commit_searches_the_working_tree_and_says_so() {
    let fx = fixture();
    commit_usages(&fx);
    fx.inspect_at(&"0".repeat(40));
    // The working tree has an uncommitted use too.
    write_file(&fx.checkout, "src/wip.rs", "needle_fn();\n");

    let (status, _, json) = get(&fx, &uri("needle_fn", "")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "commit_unavailable");
    assert_eq!(json["source"], "working_tree");
    assert!(json["reason"]
        .as_str()
        .unwrap()
        .starts_with("Inspected commit not available in this clone"));
    assert!(json.get("commit").is_none());
    let got = locations(&json);
    assert!(got.contains(&("src/lib.rs".to_string(), 1)), "{got:?}");
    // `git grep` over the working tree sees tracked files only: the new file is untracked.
    assert!(got.iter().all(|(p, _)| p != "src/wip.rs"), "{got:?}");
    // The same status and reason source-file reports.
    let (_, _, ctx) = get(
        &fx,
        "/api/onboard/source-file?repo=acme%2Fapp&path=src%2Flib.rs",
    )
    .await;
    assert_eq!(ctx["status"], json["status"]);
    assert_eq!(ctx["source"], json["source"]);
    assert_eq!(ctx["reason"], json["reason"]);
}

#[tokio::test]
async fn no_recorded_commit_searches_the_working_tree_and_says_so() {
    let fx = fixture();
    commit_usages(&fx);
    let (_, _, json) = get(&fx, &uri("needle_fn", "")).await;
    assert_eq!(json["status"], "ok", "{json}");
    assert_eq!(json["source"], "working_tree");
    assert!(json.get("commit").is_none());
    assert_eq!(locations(&json).len(), 3);
}

/// Nothing was searched, so the answer is "unavailable" with its reason and no source: never an
/// empty list, and never the working-tree fallback banner (that is for a search that ran).
#[tokio::test]
async fn a_folder_that_is_not_a_git_checkout_is_unavailable_not_an_empty_search() {
    let fx = fixture();
    commit_usages(&fx);
    std::fs::remove_dir_all(fx.checkout.join(".git")).unwrap();
    let (status, _, json) = get(&fx, &uri("needle_fn", "")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "unavailable", "{json}");
    assert_eq!(json["reason"], "the search could not run");
    assert!(json.get("source").is_none(), "{json}");
    assert_eq!(json["hits"].as_array().unwrap().len(), 0);
    assert_eq!(json["more"], false);
    assert_eq!(json["symbol"], "needle_fn");
}

/// The same when the recorded commit is missing AND the working tree cannot be searched: the
/// fallback label is for a fallback that happened.
#[tokio::test]
async fn a_missing_commit_in_a_folder_that_cannot_be_searched_is_unavailable() {
    let fx = fixture();
    commit_usages(&fx);
    fx.inspect_at(&"0".repeat(40));
    // Break the checkout after the resolver would still see a folder: no `.git` at all makes the
    // commit check fail and the working-tree search fail.
    std::fs::remove_dir_all(fx.checkout.join(".git")).unwrap();
    let (_, _, json) = get(&fx, &uri("needle_fn", "")).await;
    assert_eq!(json["status"], "unavailable", "{json}");
    assert!(json.get("source").is_none());
    assert!(json["hits"].as_array().unwrap().is_empty());
}

async fn get_with_limits(fx: &Fx, symbol: &str, limits: SearchLimits) -> serde_json::Value {
    let params: crate::onboard::symbol_refs::SymbolRefsParams =
        serde_json::from_value(serde_json::json!({ "repo": REPO, "symbol": symbol })).unwrap();
    let response = crate::onboard::symbol_refs::symbol_refs_with(&fx.state, params, limits).await;
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

/// The search is stopped at its time limit and answers the shared "took too long" outcome with
/// the same fields as every other answer.
#[tokio::test]
async fn a_search_over_its_time_limit_is_stopped_and_says_so() {
    let fx = fixture();
    let sha = commit_usages(&fx);
    fx.inspect_at(&sha);
    let limits = SearchLimits {
        timeout: std::time::Duration::from_millis(1),
        ..SearchLimits::default()
    };
    let json = get_with_limits(&fx, "needle_fn", limits).await;
    assert_eq!(json["status"], "too_large", "{json}");
    assert_eq!(
        json["reason"],
        "the search took too long; try a more specific symbol"
    );
    assert!(json.get("source").is_none());
    assert_eq!(json["symbol"], "needle_fn");
    assert_eq!(json["hits"].as_array().unwrap().len(), 0);
    assert_eq!(json["more"], false);
    // And the same request without the squeeze finds the hits.
    let json = get_with_limits(&fx, "needle_fn", SearchLimits::default()).await;
    assert_eq!(json["status"], "ok");
    assert_eq!(json["hits"].as_array().unwrap().len(), 3);
}

/// The output cap kills the search and keeps only whole records: whatever byte the cut lands on,
/// every hit shown is a complete line.
#[tokio::test]
async fn an_output_over_the_cap_is_cut_at_a_whole_record() {
    let fx = fixture();
    let body: String = (0..3000)
        .map(|n| format!("let v{n} = many_uses({n});\n"))
        .collect();
    write_file(&fx.checkout, "src/many.rs", &body);
    let sha = commit_all(&fx.checkout, "many");
    fx.inspect_at(&sha);
    for cap in [997, 998, 999, 1000, 1001, 1002, 1003, 1500] {
        let limits = SearchLimits {
            max_output: cap,
            ..SearchLimits::default()
        };
        let json = get_with_limits(&fx, "many_uses", limits).await;
        assert_eq!(json["status"], "ok", "cap {cap}: {json}");
        assert_eq!(json["more"], true, "cap {cap}");
        let hits = json["hits"].as_array().unwrap();
        assert!(
            !hits.is_empty() && hits.len() < 100,
            "cap {cap}: {}",
            hits.len()
        );
        for hit in hits {
            let line = hit["line"].as_u64().unwrap();
            assert_eq!(hit["path"], "src/many.rs");
            assert_eq!(
                hit["text"].as_str().unwrap(),
                format!("let v{} = many_uses({});", line - 1, line - 1),
                "cap {cap}: a partial record was shown"
            );
        }
    }
    // A cap smaller than one record shows nothing, and says the list is not complete.
    let limits = SearchLimits {
        max_output: 5,
        ..SearchLimits::default()
    };
    let json = get_with_limits(&fx, "many_uses", limits).await;
    assert_eq!(json["hits"].as_array().unwrap().len(), 0);
    assert_eq!(json["more"], true);
}

/// At most a bounded number of searches run at once; one more waits briefly, then is told the
/// server is busy (the shared "unavailable" outcome), not queued without limit.
#[tokio::test]
async fn a_search_with_no_free_slot_is_busy_and_runs_nothing() {
    use crate::workspace::GitReadGate;
    let fx = fixture();
    let sha = commit_usages(&fx);
    fx.inspect_at(&sha);
    let gate: &'static GitReadGate = Box::leak(Box::new(GitReadGate::new(
        1,
        std::time::Duration::from_millis(30),
    )));
    let limits = SearchLimits {
        gate,
        ..SearchLimits::default()
    };
    let held = gate.enter().await.expect("the one slot");
    GREP_RUNS.with(|c| c.set(0));
    let json = get_with_limits(&fx, "needle_fn", limits).await;
    assert_eq!(json["status"], "unavailable", "{json}");
    assert_eq!(
        json["reason"],
        "too many code reads are running; try again in a moment"
    );
    assert!(json.get("source").is_none());
    assert_eq!(json["hits"].as_array().unwrap().len(), 0);
    assert_eq!(
        GREP_RUNS.with(std::cell::Cell::get),
        0,
        "no process started"
    );
    // Freed, the same request is answered.
    drop(held);
    let json = get_with_limits(&fx, "needle_fn", limits).await;
    assert_eq!(json["status"], "ok", "{json}");
    assert_eq!(json["hits"].as_array().unwrap().len(), 3);
}

/// An operator's `grep.column=true` adds a column field that would break the `-z` parse.
#[tokio::test]
async fn a_grep_column_setting_does_not_change_the_hits() {
    let fx = fixture();
    let sha = commit_usages(&fx);
    fx.inspect_at(&sha);
    git(&fx.checkout, &["config", "grep.column", "true"]);
    git(&fx.checkout, &["config", "grep.lineNumber", "false"]);
    git(&fx.checkout, &["config", "color.grep", "always"]);
    let (_, _, json) = get(&fx, &uri("needle_fn", "")).await;
    assert_eq!(json["status"], "ok", "{json}");
    assert_eq!(
        locations(&json),
        vec![
            ("src/dir:colon.rs".to_string(), 1),
            ("src/lib.rs".to_string(), 1),
            ("src/lib.rs".to_string(), 2),
        ]
    );
    assert_eq!(json["hits"][1]["text"], "pub fn needle_fn() {}");
}

/// Decorated key body lines (a YAML list item, a JSON array element) that hold the symbol are
/// never shown, though the `-----BEGIN` line is not among the hits.
#[tokio::test]
async fn decorated_pem_body_lines_holding_the_symbol_are_not_shown() {
    use camerata_secrets::fixtures::pem_block;
    let fx = fixture();
    let pem = pem_block("RSA ", 4);
    let mut yaml = String::from("tls:\n  key:\n    - \"-----BEGIN RSA PRIVATE KEY-----\"\n");
    let mut json_lines = String::from("{\"key\": [\n  \"-----BEGIN RSA PRIVATE KEY-----\",\n");
    let mut planted = Vec::new();
    for (i, body) in pem.lines().filter(|l| !l.starts_with("-----")).enumerate() {
        // Put the symbol inside the run, bounded by non-word base64 characters.
        let with = format!("{}+leakmarker+{}", &body[..20], &body[20..]);
        planted.push(body[..20].to_string());
        planted.push(body[20..].to_string());
        if i % 2 == 0 {
            yaml.push_str(&format!("    - \"{with}\"\n"));
            json_lines.push_str(&format!("  \"{with}\",\n"));
        } else {
            yaml.push_str(&format!("    - {with}\n"));
            json_lines.push_str(&format!("  '{with}',\n"));
        }
    }
    yaml.push_str("    - \"-----END RSA PRIVATE KEY-----\"\n");
    json_lines.push_str("  \"-----END RSA PRIVATE KEY-----\"\n]}\n");
    write_file(&fx.checkout, "conf/tls.yaml", &yaml);
    write_file(&fx.checkout, "conf/tls.json", &json_lines);
    let sha = commit_all(&fx.checkout, "decorated keys");
    fx.inspect_at(&sha);

    let (_, _, json) = get(&fx, &uri("leakmarker", "")).await;

    assert_eq!(json["status"], "ok", "{json}");
    let hits = json["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 8, "{json}");
    let all: String = hits
        .iter()
        .map(|h| h["text"].as_str().unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    for part in &planted {
        assert!(!all.contains(part.as_str()), "key material leaked: {all}");
    }
    assert!(!all.contains("leakmarker"), "{all}");
    assert!(hits.iter().all(|h| h["text"] == "[redacted]"));
    // The hits are still locations: path and line are there.
    assert!(hits.iter().all(|h| h["line"].as_u64().unwrap() > 0));
}

#[tokio::test]
async fn a_sha_that_is_not_forty_hex_characters_is_rejected_before_git() {
    let fx = fixture();
    GREP_RUNS.with(|c| c.set(0));
    for bad in [
        "--upload-pack=x",
        "-1",
        "abc",
        &"g".repeat(40),
        &"a".repeat(41),
        "HEAD",
    ] {
        let (status, cache, json) = get(&fx, &uri("needle_fn", &format!("&sha={bad}"))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
        assert_eq!(json["code"], "invalid_sha", "{bad}");
        assert_eq!(cache.as_deref(), Some("no-store"));
    }
    assert_eq!(GREP_RUNS.with(std::cell::Cell::get), 0);
}

/// The `repo` parameter becomes a directory under the workspace root: the same matrix the other
/// two code endpoints refuse.
#[tokio::test]
async fn a_repo_that_escapes_the_workspace_is_refused() {
    let fx = fixture();
    let sha = commit_usages(&fx);
    fx.inspect_at(&sha);
    let outside = fx.root.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("passwd"), "needle_fn\n").unwrap();
    let absolute = outside.to_string_lossy().replace('/', "%2F");
    let repos = [
        "..%2Foutside".to_string(),
        "..%2F..%2Fwork%2Facme%2Fapp".to_string(),
        "acme%2F..%2F..%2Foutside".to_string(),
        absolute,
        "acme%2Fapp%2Fextra".to_string(),
        "%2F".to_string(),
        "acme%2F".to_string(),
        "acme%2Funknown".to_string(),
        "evil%2Fapp".to_string(),
    ];
    GREP_RUNS.with(|c| c.set(0));
    for repo in &repos {
        let endpoint = format!("/api/onboard/symbol-refs?repo={repo}&symbol=needle_fn");
        let (status, _, json) = get(&fx, &endpoint).await;
        assert_eq!(status, StatusCode::OK, "{endpoint}: {json}");
        assert_eq!(json["status"], "file_missing", "{endpoint}: {json}");
        assert!(json.get("hits").is_none(), "{endpoint}: {json}");
    }
    assert_eq!(
        GREP_RUNS.with(std::cell::Cell::get),
        0,
        "a refused repo must not run a search"
    );
    let (_, _, ok) = get(&fx, &uri("needle_fn", "")).await;
    assert_eq!(ok["status"], "ok");
}

#[tokio::test]
async fn searching_never_changes_the_checkout() {
    let fx = fixture();
    let sha = commit_usages(&fx);
    fx.inspect_at(&sha);
    write_file(
        &fx.checkout,
        "src/dirty.rs",
        "needle_fn(); // uncommitted\n",
    );
    let before = git(&fx.checkout, &["status", "--porcelain=v1"]);
    let index_before = std::fs::read(fx.checkout.join(".git/index")).unwrap();
    let head = crate::client_source::git::head_sha(&fx.checkout).await;
    for extra in [
        "",
        "&sha=0000000000000000000000000000000000000000",
        &format!("&sha={sha}"),
    ] {
        get(&fx, &uri("needle_fn", extra)).await;
    }
    fx.inspect_at(&"0".repeat(40));
    get(&fx, &uri("needle_fn", "")).await;
    assert_eq!(git(&fx.checkout, &["status", "--porcelain=v1"]), before);
    assert_eq!(
        std::fs::read(fx.checkout.join(".git/index")).unwrap(),
        index_before,
        "the index file is byte-identical"
    );
    assert_eq!(
        crate::client_source::git::head_sha(&fx.checkout).await,
        head
    );
}
