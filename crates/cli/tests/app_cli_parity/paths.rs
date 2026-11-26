//! The two entry points: the command line's pipeline and the app's own HTTP routes.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use camerata::inspect_cmd::{
    curated_rule_selection, run_inspect_pipeline_with, InspectArgs, Seams,
};
use camerata_server::llm::ProjectBackend;
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

use crate::fixture::{
    engine_env, Scripted, AUDIT_MODEL, CALIBRATION_MODEL, CLIENT, EXCLUSION, REPOS,
};

// ── The command line path ────────────────────────────────────────────────────────────────

pub(crate) fn cli_args(dirs: &[PathBuf; 2], export: &Path) -> InspectArgs {
    InspectArgs {
        repo: dirs[0].clone(),
        also_repos: vec![dirs[1].clone()],
        repo_names: REPOS.iter().map(|r| r.to_string()).collect(),
        export: export.to_path_buf(),
        backend: ProjectBackend::Api,
        batch: false,
        // The app is asked for `parallel` below: the same mode on both paths.
        realtime: true,
        model: Some(AUDIT_MODEL.to_string()),
        calibration_model: Some(CALIBRATION_MODEL.to_string()),
        fix_model: None,
        clone_judge_model: None,
        group_review_model: None,
        dup_check_model: None,
        escalation_rate: None,
        escalation_floor: None,
        full: true,
        verbose: false,
        max_cost_usd: None,
        efficiency: Vec::new(),
        client: CLIENT.to_string(),
        // The operator's own inspection on both paths: a client start is refused on a build with
        // uncommitted changes (`engine_pin::client_start_refusal`), which would make the result
        // depend on the checkout the test runs from.
        client_engagement: false,
        run_client_tooling: false,
        consensus: false,
        // The engine finder on both paths (set on the project below): the agent finder runs a
        // separate agent of its own that a scripted completion cannot stand in for.
        finder: camerata::inspect_cmd::finder::FinderArgs {
            finder: camerata_server::ai_audit::agent_finder::FinderChoice::Engine,
            ..Default::default()
        },
        exclude: vec![EXCLUSION.to_string()],
    }
}

/// Run `camerata inspect` over the project; returns the export zip and the model calls made.
pub(crate) async fn run_cli(root: &Path, dirs: &[PathBuf; 2]) -> (Vec<u8>, usize) {
    let _env = engine_env(&root.join("state-cli"));
    let export = root.join("cli-export.zip");
    let llm = Scripted::new();
    run_inspect_pipeline_with(
        cli_args(dirs, &export),
        true,
        &camerata_server::credentials::MemoryCredentialStore::new(),
        Seams {
            resume: None,
            llm: Some(&llm),
            preflight_sink: &|_| {},
        },
    )
    .await
    .unwrap_or_else(|e| panic!("the command line inspection completes: {e}"));
    (
        std::fs::read(&export).unwrap(),
        llm.calls.load(Ordering::SeqCst),
    )
}

/// The rules the command line selects for this project (its own curation of the proposal), sent
/// to the app as the architect's selection: the app scans the rules its request names.
pub(crate) async fn cli_rule_selection(dirs: &[PathBuf; 2]) -> Value {
    let args = cli_args(dirs, Path::new("unused.zip"));
    let scope = args.run_controls().unwrap().scope;
    let sources = args.resolve_sources().unwrap();
    let scan = camerata_server::onboard::scan_repos(&sources, Vec::new(), &scope).await;
    let rules: Vec<Value> = curated_rule_selection(&scan.proposed_rules)
        .into_iter()
        .map(|r| serde_json::json!({ "id": r.id, "directive": r.directive, "repos": r.repos }))
        .collect();
    Value::Array(rules)
}

// ── The app path ─────────────────────────────────────────────────────────────────────────

/// One HTTP round trip through the app's router.
pub(crate) async fn call(
    state: &camerata_server::AppState,
    method: &str,
    uri: &str,
    body: Value,
) -> (StatusCode, Vec<u8>) {
    let resp = camerata_server::router(state.clone())
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
    let bytes = resp
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, bytes)
}

/// A settings call that must succeed (`{ ok: true }` where the route answers with one).
pub(crate) async fn post_ok(state: &camerata_server::AppState, uri: &str, body: Value) -> Value {
    let (status, bytes) = call(state, "POST", uri, body).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{uri}: {}",
        String::from_utf8_lossy(&bytes)
    );
    let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    assert_ne!(v["ok"], Value::Bool(false), "{uri}: {v}");
    v
}

/// Run the app's scan job and its product export over the project; returns the export zip and
/// the model calls made.
pub(crate) async fn run_app(root: &Path, rules: Value) -> (Vec<u8>, usize) {
    let _env = engine_env(&root.join("state-app"));
    let llm = Arc::new(Scripted::new());
    let _scripted = camerata_server::scan_job_llm::ScanJobLlmGuard::install(llm.clone());
    let data = root.join("app-data");
    std::fs::create_dir_all(&data).unwrap();
    let state = camerata_server::AppState::from_data_dir(&data);
    // A placeholder key: the API backend needs one to be chosen, and the scripted model means
    // nothing is ever sent with it.
    state
        .credential_store
        .set(
            camerata_server::credentials::ANTHROPIC_API_KEY,
            "sk-ant-placeholder-never-sent",
        )
        .unwrap();

    post_ok(
        &state,
        "/api/settings/workspace",
        serde_json::json!({ "path": root.join("work").to_string_lossy() }),
    )
    .await;
    let created = post_ok(
        &state,
        "/api/projects",
        serde_json::json!({ "name": "Acme", "repos": REPOS }),
    )
    .await;
    let pid = created["project"]["id"].as_str().unwrap().to_string();
    post_ok(
        &state,
        "/api/projects/active",
        serde_json::json!({ "id": pid }),
    )
    .await;
    let p = |tail: &str| format!("/api/projects/{pid}/{tail}");
    post_ok(
        &state,
        &p("backend"),
        serde_json::json!({ "backend": "api" }),
    )
    .await;
    post_ok(
        &state,
        &p("client-engagement"),
        serde_json::json!({ "client_engagement": false, "client_tooling_opt_in": false }),
    )
    .await;
    post_ok(
        &state,
        &p("client-name"),
        serde_json::json!({ "client_name": CLIENT }),
    )
    .await;
    post_ok(
        &state,
        &p("scan-exclusions"),
        serde_json::json!({ "patterns": [EXCLUSION], "based_on": [] }),
    )
    .await;
    for (step, model) in [
        ("audit", AUDIT_MODEL),
        ("calibration", CALIBRATION_MODEL),
        ("finder", "engine"),
    ] {
        post_ok(
            &state,
            &p("step-models"),
            serde_json::json!({ "step": step, "model": model }),
        )
        .await;
    }

    let (status, bytes) = call(
        &state,
        "POST",
        "/api/onboard/audit/start",
        serde_json::json!({
            "repos": REPOS,
            "rules": rules,
            "mode": "parallel",
            "incremental": false,
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    let job_id = serde_json::from_slice::<Value>(&bytes).unwrap()["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    let mut finished = None;
    for _ in 0..2_400 {
        let (_, bytes) = call(
            &state,
            "GET",
            &format!("/api/onboard/audit/job/{job_id}"),
            Value::Null,
        )
        .await;
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        if v["job"]["status"] != "running" {
            finished = Some(v);
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let job = finished.expect("the app's scan job finishes");
    assert_eq!(job["job"]["status"], "done", "{}", job["job"]["message"]);

    let (status, zip) = call(&state, "POST", &p("product-export"), serde_json::json!({})).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&zip));
    (zip, llm.calls.load(Ordering::SeqCst))
}
