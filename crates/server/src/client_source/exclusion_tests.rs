//! Item 31: every reader of a client checkout honors the project's exclusion list. A file
//! excluded by agreement is not shown by the code viewer, the finding context (single and
//! batch) or the symbol search, and the settings route refuses a pattern that is not valid.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use super::test_support::{fixture, Fx};
use crate::test_git::{commit_all, write_file};

/// A file in `seeds/` and a use of its symbol in `src/`, committed and inspected. The repo is
/// `acme/app`, so a pattern is written against `app/<path>`.
fn commit_seed(fx: &Fx) {
    write_file(
        &fx.checkout,
        "seeds/data.rs",
        "pub fn seeded_marker_fn() -> u32 {\n    7\n}\n",
    );
    write_file(
        &fx.checkout,
        "src/uses.rs",
        "fn go() {\n    let _ = seeded_marker_fn();\n}\n",
    );
    let sha = commit_all(&fx.checkout, "seed");
    fx.inspect_at(&sha);
}

fn exclude(fx: &Fx, patterns: &[&str]) {
    let patterns: Vec<String> = patterns.iter().map(|p| p.to_string()).collect();
    fx.state
        .projects
        .set_scan_exclusions(&fx.project_id, &patterns)
        .expect("valid patterns")
        .expect("project exists");
}

async fn get(fx: &Fx, uri: &str) -> serde_json::Value {
    let resp = crate::router(fx.state.clone())
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

async fn post(fx: &Fx, uri: &str, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
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
    (status, serde_json::from_slice(&bytes).unwrap())
}

const VIEWER: &str = "/api/onboard/source-file?repo=acme%2Fapp&path=seeds%2Fdata.rs";
const VIEWER_OTHER: &str = "/api/onboard/source-file?repo=acme%2Fapp&path=src%2Fuses.rs";
const CONTEXT: &str = "/api/onboard/finding-context?repo=acme%2Fapp&path=seeds%2Fdata.rs&line=2";
const SYMBOL: &str = "/api/onboard/symbol-refs?repo=acme%2Fapp&symbol=seeded_marker_fn";

#[tokio::test]
async fn the_code_viewer_refuses_an_excluded_file_and_still_shows_the_others() {
    let fx = fixture();
    commit_seed(&fx);
    assert_eq!(
        get(&fx, VIEWER).await["status"],
        "ok",
        "shown before the exclusion"
    );

    exclude(&fx, &["app/seeds/**"]);
    let json = get(&fx, VIEWER).await;
    assert_eq!(json["status"], "excluded_by_agreement", "{json}");
    assert!(
        json.get("lines").is_none(),
        "no content of an excluded file: {json}"
    );
    assert_eq!(get(&fx, VIEWER_OTHER).await["status"], "ok");
}

#[tokio::test]
async fn the_finding_context_refuses_an_excluded_file() {
    let fx = fixture();
    commit_seed(&fx);
    assert_eq!(get(&fx, CONTEXT).await["status"], "ok");

    exclude(&fx, &["app/seeds/data.rs"]);
    let json = get(&fx, CONTEXT).await;
    assert_eq!(json["status"], "excluded_by_agreement", "{json}");
    assert!(json.get("lines").is_none(), "{json}");
}

#[tokio::test]
async fn the_batch_finding_context_refuses_only_the_excluded_files() {
    let fx = fixture();
    commit_seed(&fx);
    exclude(&fx, &["app/seeds/**"]);
    let (status, json) = post(
        &fx,
        "/api/onboard/finding-context/batch",
        serde_json::json!({ "items": [
            { "repo": "acme/app", "path": "seeds/data.rs", "line": 2 },
            { "repo": "acme/app", "path": "src/uses.rs", "line": 2 },
        ]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let statuses: Vec<&str> = json["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["status"].as_str().unwrap())
        .collect();
    assert_eq!(statuses, ["excluded_by_agreement", "ok"]);
}

#[tokio::test]
async fn the_symbol_search_drops_hits_in_an_excluded_file() {
    let fx = fixture();
    commit_seed(&fx);
    let paths = |json: &serde_json::Value| -> Vec<String> {
        json["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["path"].as_str().unwrap().to_string())
            .collect()
    };
    let before = get(&fx, SYMBOL).await;
    assert!(
        paths(&before).contains(&"seeds/data.rs".to_string()),
        "{before}"
    );

    exclude(&fx, &["app/seeds/**"]);
    let after = get(&fx, SYMBOL).await;
    assert_eq!(paths(&after), ["src/uses.rs"], "{after}");
}

#[tokio::test]
async fn the_settings_route_saves_a_list_and_refuses_an_invalid_pattern_by_name() {
    let fx = fixture();
    let uri = format!("/api/projects/{}/scan-exclusions", fx.project_id);
    let (_, saved) = post(
        &fx,
        &uri,
        serde_json::json!({ "patterns": ["  app/seeds/**  ", "", "app/seeds/**", "app/dump.sql"], "based_on": [] }),
    )
    .await;
    assert_eq!(saved["ok"], true, "{saved}");
    assert_eq!(
        saved["project"]["scan_exclusions"],
        serde_json::json!(["app/seeds/**", "app/dump.sql"]),
        "blank lines and repeats are dropped, each pattern trimmed"
    );

    let (_, refused) = post(
        &fx,
        &uri,
        serde_json::json!({ "patterns": ["app/ok/**", "app/[broken"], "based_on": ["app/seeds/**", "app/dump.sql"] }),
    )
    .await;
    assert_eq!(refused["ok"], false);
    assert!(
        refused["message"].as_str().unwrap().contains("app/[broken"),
        "the refusal names the pattern: {refused}"
    );
    // Nothing was saved by the refused request.
    let kept = fx
        .state
        .projects
        .get(&fx.project_id)
        .unwrap()
        .scan_exclusions;
    assert_eq!(kept, ["app/seeds/**", "app/dump.sql"]);
}

#[tokio::test]
async fn a_project_saved_before_the_field_existed_loads_with_an_empty_list() {
    let project: camerata_app_core::project::Project =
        serde_json::from_value(serde_json::json!({ "id": "old", "name": "Old" })).unwrap();
    assert!(project.scan_exclusions.is_empty());
}

#[tokio::test]
async fn adding_one_file_excludes_exactly_it_and_is_idempotent() {
    let fx = fixture();
    let uri = format!("/api/projects/{}/scan-exclusions/add", fx.project_id);
    for _ in 0..2 {
        let (_, saved) = post(
            &fx,
            &uri,
            serde_json::json!({ "repo": "acme/app", "path": "seeds/[v1] data.js" }),
        )
        .await;
        assert_eq!(saved["ok"], true, "{saved}");
    }
    let kept = fx
        .state
        .projects
        .get(&fx.project_id)
        .unwrap()
        .scan_exclusions;
    assert_eq!(
        kept,
        ["app/seeds/\\[v1\\] data.js"],
        "escaped, and not added twice"
    );
}

// ── Round 1 item 3: the server start refusal, through the real routes ─────────────────────

fn plant_big_file(fx: &Fx) {
    write_file(
        &fx.checkout,
        "seeds/big.js",
        &"x".repeat(crate::onboard::files::MAX_FILE_BYTES + 1),
    );
}

fn set_list(fx: &Fx, patterns: &[&str]) {
    let patterns: Vec<String> = patterns.iter().map(|p| p.to_string()).collect();
    fx.state
        .projects
        .update(&fx.project_id, |p| p.scan_exclusions = patterns)
        .expect("project exists");
}

/// Start a job through `POST /api/onboard/audit/start` and read its state back. The refusal fails
/// the job before anything is spawned: no report, no model call.
async fn start_job(fx: &Fx) -> crate::jobs::JobState {
    let (_, body) = post(
        fx,
        "/api/onboard/audit/start",
        serde_json::json!({ "repos": ["acme/app"], "rules": [] }),
    )
    .await;
    let id = body["job_id"].as_str().expect("a job id").to_string();
    fx.state.jobs.get(&id).expect("the job")
}

async fn start_sync(fx: &Fx) -> serde_json::Value {
    post(
        fx,
        "/api/onboard/audit",
        serde_json::json!({ "repos": ["acme/app"], "rules": [] }),
    )
    .await
    .1
}

#[tokio::test]
async fn a_job_start_with_a_file_over_the_limit_is_not_refused() {
    let fx = fixture();
    plant_big_file(&fx);
    let job = start_job(&fx).await;
    let why = job.message.clone().unwrap_or_default();
    assert!(!why.contains("Cannot start"), "{why}");
    assert!(!why.contains("file size limit"), "{why}");
    assert_ne!(job.status, "failed", "{why}");
}

#[tokio::test]
async fn a_job_start_with_an_unusable_list_fails_the_job() {
    let fx = fixture();
    set_list(&fx, &["app/[broken"]);
    let job = start_job(&fx).await;
    assert_eq!(job.status, "failed");
    assert!(job.message.unwrap().contains("app/[broken"));
}

#[tokio::test]
async fn a_job_start_with_a_pattern_that_matches_nothing_fails_the_job_naming_it() {
    let fx = fixture();
    commit_seed(&fx);
    set_list(&fx, &["app/seeds/**", "app/sedes/**"]);
    let job = start_job(&fx).await;
    assert_eq!(job.status, "failed");
    let why = job.message.unwrap();
    assert!(
        why.contains("app/sedes/**") && why.contains("matches no file"),
        "{why}"
    );
}

#[tokio::test]
async fn the_synchronous_start_refuses_an_unusable_list_and_a_void_pattern_but_not_a_big_file() {
    let fx = fixture();
    plant_big_file(&fx);
    let refused = |report: &serde_json::Value| {
        assert_eq!(report["blocked"], true, "{report}");
        assert!(
            report["message"]
                .as_str()
                .unwrap_or_default()
                .starts_with("Cannot start"),
            "{report}"
        );
    };
    let report = start_sync(&fx).await;
    assert_ne!(
        report["blocked"], true,
        "a file over the limit never refuses: {report}"
    );
    let text = report.to_string();
    assert!(
        text.contains("will not be read for the inspection") && text.contains("app/seeds/big.js"),
        "the warning is in the report notes: {text}"
    );
    set_list(&fx, &["app/[broken"]);
    let report = start_sync(&fx).await;
    refused(&report);
    assert!(report.to_string().contains("app/[broken"));
    set_list(&fx, &["app/seeds/big.js", "app/sedes/**"]);
    let report = start_sync(&fx).await;
    refused(&report);
    let text = report.to_string();
    assert!(
        text.contains("app/sedes/**") && text.contains("matches no file"),
        "{text}"
    );
}

/// Round 1 item 13: the symbol search drops excluded hits BEFORE it applies its hit limit, so a
/// large excluded file cannot crowd the real uses out of the answer.
#[tokio::test]
async fn excluded_hits_do_not_use_up_the_symbol_search_limit() {
    let fx = fixture();
    let many: String = (0..260)
        .map(|i| format!("let _{i} = seeded_marker_fn();\n"))
        .collect();
    write_file(&fx.checkout, "seeds/many.rs", &many);
    write_file(
        &fx.checkout,
        "src/uses.rs",
        "fn go() {\n    let _ = seeded_marker_fn();\n}\n",
    );
    let sha = commit_all(&fx.checkout, "many");
    fx.inspect_at(&sha);
    exclude(&fx, &["app/seeds/**"]);
    let json = get(&fx, SYMBOL).await;
    let paths: Vec<&str> = json["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, ["src/uses.rs"], "{json}");
    assert_eq!(
        json["more"], false,
        "nothing is left over once the excluded hits are gone"
    );
}

/// Round 1 item 9, round 3 ruling: the scan route no longer refuses a file over the limit; the
/// report notes warn about it, naming the file.
#[tokio::test]
async fn the_scan_route_warns_about_a_file_over_the_limit_and_runs() {
    let fx = fixture();
    plant_big_file(&fx);
    let (_, json) = post(
        &fx,
        "/api/onboard/scan",
        serde_json::json!({ "repos": ["acme/app"] }),
    )
    .await;
    let text = json.to_string();
    assert!(text.contains("app/seeds/big.js"), "{text}");
    assert!(
        text.contains("will not be read for the inspection"),
        "{text}"
    );
    assert!(!text.contains("Cannot start"), "{text}");
    set_list(&fx, &["app/seeds/**"]);
    let (_, json) = post(
        &fx,
        "/api/onboard/scan",
        serde_json::json!({ "repos": ["acme/app"] }),
    )
    .await;
    assert!(!json.to_string().contains("will not be read"), "{json}");
}

// ── Round 1 item 8: lost updates and stale saves ──────────────────────────────────────────

#[tokio::test]
async fn two_quick_additions_both_land() {
    let fx = fixture();
    let uri = format!("/api/projects/{}/scan-exclusions/add", fx.project_id);
    let a = post(
        &fx,
        &uri,
        serde_json::json!({ "repo": "acme/app", "path": "seeds/a.js" }),
    );
    let b = post(
        &fx,
        &uri,
        serde_json::json!({ "repo": "acme/app", "path": "seeds/b.js" }),
    );
    let _ = tokio::join!(a, b);
    let mut kept = fx
        .state
        .projects
        .get(&fx.project_id)
        .unwrap()
        .scan_exclusions;
    kept.sort();
    assert_eq!(kept, ["app/seeds/a.js", "app/seeds/b.js"]);
}

#[tokio::test]
async fn the_add_route_checks_the_repo_and_the_path() {
    let fx = fixture();
    let uri = format!("/api/projects/{}/scan-exclusions/add", fx.project_id);
    let (_, other) = post(
        &fx,
        &uri,
        serde_json::json!({ "repo": "acme/elsewhere", "path": "a.js" }),
    )
    .await;
    assert_eq!(other["ok"], false);
    assert!(
        other["message"]
            .as_str()
            .unwrap()
            .contains("acme/elsewhere"),
        "{other}"
    );
    let (_, blank) = post(
        &fx,
        &uri,
        serde_json::json!({ "repo": "acme/app", "path": "  " }),
    )
    .await;
    assert_eq!(blank["ok"], false, "{blank}");
    assert!(fx
        .state
        .projects
        .get(&fx.project_id)
        .unwrap()
        .scan_exclusions
        .is_empty());
}

#[tokio::test]
async fn a_save_from_a_list_that_has_since_changed_is_refused() {
    let fx = fixture();
    let add = format!("/api/projects/{}/scan-exclusions/add", fx.project_id);
    let save = format!("/api/projects/{}/scan-exclusions", fx.project_id);
    // The editor loaded an empty list; meanwhile the pre-start check added a file.
    let _ = post(
        &fx,
        &add,
        serde_json::json!({ "repo": "acme/app", "path": "seeds/big.js" }),
    )
    .await;
    let (_, stale) = post(
        &fx,
        &save,
        serde_json::json!({ "patterns": ["app/dump.sql"], "based_on": [] }),
    )
    .await;
    assert_eq!(stale["ok"], false, "{stale}");
    assert!(
        stale["message"].as_str().unwrap().contains("changed since"),
        "{stale}"
    );
    assert_eq!(
        fx.state
            .projects
            .get(&fx.project_id)
            .unwrap()
            .scan_exclusions,
        ["app/seeds/big.js"],
        "the agreed exclusion is kept"
    );
    // A save based on what is there goes through.
    let (_, fresh) = post(
        &fx,
        &save,
        serde_json::json!({ "patterns": ["app/seeds/big.js", "app/dump.sql"], "based_on": ["app/seeds/big.js"] }),
    )
    .await;
    assert_eq!(fresh["ok"], true, "{fresh}");
}

/// Round 1 item 13: what the desktop's pre-start check shows comes from the preflight route, and
/// the route leaves out a file Excluded by agreement names, shows the others in pattern form, and
/// lists a pattern that matches nothing.
#[tokio::test]
async fn the_preflight_route_filters_what_the_pre_start_check_shows() {
    let fx = fixture();
    plant_big_file(&fx);
    let (_, rows) = post(
        &fx,
        "/api/onboard/preflight",
        serde_json::json!({ "repos": ["acme/app"] }),
    )
    .await;
    assert_eq!(rows[0]["over_limit"][0]["path"], "seeds/big.js", "{rows}");
    assert_eq!(
        rows[0]["over_limit"][0]["display"], "app/seeds/big.js",
        "{rows}"
    );
    assert_eq!(rows[0]["read_limit"], "1 MB");
    set_list(&fx, &["app/seeds/**", "app/sedes/**"]);
    let (_, rows) = post(
        &fx,
        "/api/onboard/preflight",
        serde_json::json!({ "repos": ["acme/app"] }),
    )
    .await;
    assert_eq!(
        rows[0]["over_limit"],
        serde_json::json!([]),
        "excluded, so left off the wire: {rows}"
    );
    assert_eq!(
        rows[0]["unmatched_patterns"],
        serde_json::json!(["app/sedes/**"]),
        "{rows}"
    );
}

/// Item 9, round 3 ruling: `rescan-alternatives` no longer refuses a file over the limit. The
/// request names a real multi-option semantic rule from the corpus.
#[tokio::test]
async fn the_rescan_alternatives_route_does_not_refuse_a_file_over_the_limit() {
    let fx = fixture();
    plant_big_file(&fx);
    let (corpus, _) = camerata_rules::load_corpus_lenient(&camerata_rules::corpus_path()).await;
    let rule = corpus
        .iter()
        .find(|r| crate::onboard::audit::is_semantic_multi_option_rule(r) && !r.options.is_empty())
        .expect("the corpus has a multi-option semantic rule")
        .clone();
    let body = serde_json::json!({ "overrides": [{
        "rule_id": rule.id, "chosen_option_id": rule.options[0].id
    }]});
    let uri = format!("/api/projects/{}/rescan-alternatives", fx.project_id);
    let (_, json) = post(&fx, &uri, body.clone()).await;
    let message = json["message"].as_str().unwrap_or_default();
    assert!(!message.contains("file size limit"), "{json}");
    assert!(!message.contains("Cannot start"), "{json}");
}

/// Item 7: with two repos called `app` in the project, "Exclude by agreement" writes the
/// `owner/repo/...` form, and a bare `app/...` pattern is refused as ambiguous.
#[tokio::test]
async fn the_add_route_writes_the_owner_form_when_two_repos_share_a_name() {
    let fx = fixture();
    let _other = fx.add_repo("other/app");
    let add = format!("/api/projects/{}/scan-exclusions/add", fx.project_id);
    let (_, saved) = post(
        &fx,
        &add,
        serde_json::json!({ "repo": "other/app", "path": "seeds/a.js" }),
    )
    .await;
    assert_eq!(saved["ok"], true, "{saved}");
    assert_eq!(
        fx.state
            .projects
            .get(&fx.project_id)
            .unwrap()
            .scan_exclusions,
        ["other/app/seeds/a.js"]
    );
    let save = format!("/api/projects/{}/scan-exclusions", fx.project_id);
    let (_, bare) = post(
        &fx,
        &save,
        serde_json::json!({ "patterns": ["app/seeds/**"], "based_on": ["other/app/seeds/a.js"] }),
    )
    .await;
    assert_eq!(bare["ok"], false, "{bare}");
    let message = bare["message"].as_str().unwrap();
    assert!(
        message.contains("acme/app") && message.contains("other/app"),
        "{bare}"
    );
}

/// Gap 5, round 3 ruling: the server start no longer refuses a file with a line longer than
/// 97 KB; the preflight still names the reason.
#[tokio::test]
async fn a_job_start_with_a_line_longer_than_the_page_is_not_refused() {
    let fx = fixture();
    write_file(&fx.checkout, "seeds/wide.js", &"y".repeat(100_001));
    let job = start_job(&fx).await;
    let why = job.message.clone().unwrap_or_default();
    assert!(!why.contains("Cannot start"), "{why}");
    assert_ne!(job.status, "failed", "{why}");
    let (_, rows) = post(
        &fx,
        "/api/onboard/preflight",
        serde_json::json!({ "repos": ["acme/app"] }),
    )
    .await;
    assert_eq!(
        rows[0]["over_limit"][0]["reason"], "has a line longer than 97 KB",
        "{rows}"
    );
}

/// Zach's ruling: a pattern in a never-read folder is a WARNING on the preflight wire (never an
/// unmatched entry), so the desktop offers Remove pattern but does not hold the start.
#[tokio::test]
async fn the_preflight_route_warns_about_a_noise_folder_pattern_without_blocking() {
    let fx = fixture();
    set_list(&fx, &["app/node_modules/**"]);
    let (_, rows) = post(
        &fx,
        "/api/onboard/preflight",
        serde_json::json!({ "repos": ["acme/app"] }),
    )
    .await;
    assert_eq!(
        rows[0]["unmatched_patterns"],
        serde_json::json!([]),
        "{rows}"
    );
    assert_eq!(
        rows[0]["warned_patterns"],
        serde_json::json!(["app/node_modules/**"]),
        "{rows}"
    );
    assert_eq!(
        rows[0]["warned_reasons"],
        serde_json::json!(["node_modules is never read, so this pattern is not needed"]),
        "{rows}"
    );
}

/// Ruling test 1 (server start): the warning lets the start proceed; a pattern matching nothing
/// at all still refuses it (ruling test 3).
#[tokio::test]
async fn a_never_read_pattern_does_not_refuse_a_job_start_but_a_void_pattern_does() {
    let fx = fixture();
    set_list(&fx, &["app/node_modules/**"]);
    let job = start_job(&fx).await;
    let why = job.message.clone().unwrap_or_default();
    assert!(!why.contains("Cannot start"), "{why}");
    assert!(!why.contains("matches no file"), "{why}");
    let fx = fixture();
    set_list(&fx, &["app/sedes/**"]);
    let job = start_job(&fx).await;
    assert_eq!(job.status, "failed");
    assert!(job.message.unwrap().contains("app/sedes/**"));
}

// ── Round 2 ───────────────────────────────────────────────────────────────────────────────

/// A project whose second repo cannot be read (no local folder): `other/zzz`.
fn add_unresolved_repo(fx: &Fx) {
    fx.state
        .projects
        .update(&fx.project_id, |p| p.repos.push("other/zzz".to_string()))
        .expect("project exists");
}

/// Item 1: a pattern naming the unread repo is "cannot be checked", not "matches no file": the
/// wire has no unmatched entry for it (so the desktop shows no Remove button), and every start
/// still refuses with the new reason.
#[tokio::test]
async fn a_pattern_for_an_unread_repo_is_unchecked_on_the_wire_and_still_blocks_a_start() {
    let fx = fixture();
    commit_seed(&fx);
    add_unresolved_repo(&fx);
    set_list(&fx, &["app/seeds/**", "zzz/seeds/**"]);
    let (_, rows) = post(
        &fx,
        "/api/onboard/preflight",
        serde_json::json!({ "repos": ["acme/app", "other/zzz"] }),
    )
    .await;
    assert_eq!(
        rows[0]["unmatched_patterns"],
        serde_json::json!([]),
        "{rows}"
    );
    assert_eq!(
        rows[0]["unchecked_patterns"],
        serde_json::json!(["zzz/seeds/**"]),
        "{rows}"
    );
    assert_eq!(
        rows[0]["unchecked_reasons"],
        serde_json::json!(["cannot be checked: other/zzz was not read"]),
        "{rows}"
    );
    // The job path (allow_partial lets the run go on without the unread repo).
    let (_, body) = post(
        &fx,
        "/api/onboard/audit/start",
        serde_json::json!({ "repos": ["acme/app", "other/zzz"], "rules": [], "allow_partial": true }),
    )
    .await;
    let job = fx.state.jobs.get(body["job_id"].as_str().unwrap()).unwrap();
    assert_eq!(job.status, "failed");
    assert!(job
        .message
        .unwrap()
        .contains("cannot be checked: other/zzz was not read"));
}

/// Item 2: an unusable list (a repo sharing a name was added after `app/...` was saved) refuses
/// the scan and the rescan-alternatives routes with the reason; neither runs on zero files.
#[tokio::test]
async fn an_unusable_list_refuses_the_scan_and_rescan_alternatives_routes() {
    let fx = fixture();
    commit_seed(&fx);
    set_list(&fx, &["app/seeds/**"]);
    fx.state
        .projects
        .update(&fx.project_id, |p| p.repos.push("other/app".to_string()))
        .unwrap();
    let (_, report) = post(
        &fx,
        "/api/onboard/scan",
        serde_json::json!({ "repos": ["acme/app"] }),
    )
    .await;
    assert_eq!(report["blocked"], true, "{report}");
    let message = report["message"].as_str().unwrap_or_default();
    assert!(
        message.starts_with(
            "Cannot start: Excluded by agreement in the inspection settings cannot be used:"
        ) && message.contains("ambiguous"),
        "{report}"
    );
    let (corpus, _) = camerata_rules::load_corpus_lenient(&camerata_rules::corpus_path()).await;
    let rule = corpus
        .iter()
        .find(|r| crate::onboard::audit::is_semantic_multi_option_rule(r) && !r.options.is_empty())
        .unwrap()
        .clone();
    let uri = format!("/api/projects/{}/rescan-alternatives", fx.project_id);
    let (_, json) = post(
        &fx,
        &uri,
        serde_json::json!({ "allow_partial": true, "overrides": [{
            "rule_id": rule.id, "chosen_option_id": rule.options[0].id }]}),
    )
    .await;
    assert_eq!(json["ok"], false, "{json}");
    let message = json["message"].as_str().unwrap_or_default();
    assert!(
        message.starts_with(
            "Cannot start: Excluded by agreement in the inspection settings cannot be used:"
        ),
        "{json}"
    );
}

/// Item 5: a save racing an add never drops it, and a save that omits `based_on` is refused.
#[tokio::test]
async fn a_save_racing_an_add_never_drops_the_added_exclusion() {
    for _ in 0..8 {
        let fx = fixture();
        let add = format!("/api/projects/{}/scan-exclusions/add", fx.project_id);
        let save = format!("/api/projects/{}/scan-exclusions", fx.project_id);
        let a = post(
            &fx,
            &add,
            serde_json::json!({ "repo": "acme/app", "path": "seeds/a.js" }),
        );
        let b = post(
            &fx,
            &save,
            serde_json::json!({ "patterns": ["app/dump.sql"], "based_on": [] }),
        );
        let (_, (_, saved)) = tokio::join!(a, b);
        let list = fx
            .state
            .projects
            .get(&fx.project_id)
            .unwrap()
            .scan_exclusions;
        assert!(
            list.contains(&"app/seeds/a.js".to_string()),
            "the add was dropped: {list:?}"
        );
        if saved["ok"] == true {
            assert!(list.contains(&"app/dump.sql".to_string()), "{list:?}");
        }
    }
    let fx = fixture();
    let save = format!("/api/projects/{}/scan-exclusions", fx.project_id);
    let (_, refused) = post(&fx, &save, serde_json::json!({ "patterns": ["app/x"] })).await;
    assert_eq!(refused["ok"], false, "{refused}");
    assert!(
        refused["message"].as_str().unwrap().contains("based_on"),
        "{refused}"
    );
    assert!(fx
        .state
        .projects
        .get(&fx.project_id)
        .unwrap()
        .scan_exclusions
        .is_empty());
}
