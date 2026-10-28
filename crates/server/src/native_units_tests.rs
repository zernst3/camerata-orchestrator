//! Native units of work through the server path: created with no board and no token, listed
//! by the console, and run end to end (gate, branch, implement, commit) with a fake agent. The
//! same run on a GitHub-backed unit proves the issue adapter still works.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use camerata_worktracker::investigation::DecisionRecord;
use camerata_worktracker::InMemoryStoryStore;
use http_body_util::BodyExt;
use tower::ServiceExt;

use crate::run::RunStatus;
use crate::{router, AppState};

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
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
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

/// Percent-encode a unit key for a path segment (`/` and `#` in a GitHub issue key).
fn seg(id: &str) -> String {
    id.replace('%', "%25")
        .replace('/', "%2F")
        .replace('#', "%23")
}

/// `true` when `text` has a `#` directly followed by a digit anywhere.
fn has_hash_number(text: &str) -> bool {
    text.chars()
        .zip(text.chars().skip(1))
        .any(|(a, b)| a == '#' && b.is_ascii_digit())
}

fn state_with_project(repos: &[&str]) -> AppState {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    state
        .projects
        .create("P", repos.iter().map(|r| r.to_string()).collect())
        .unwrap();
    state
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_native_unit_is_created_and_listed_with_no_board_and_no_token() {
    let _token = camerata_test_env::EnvGuard::remove("CAMERATA_GITHUB_TOKEN");
    let state = state_with_project(&["acme/api"]);
    let app = router(state.clone());

    let (status, created) = send(
        &app,
        "POST",
        "/api/uow/native",
        serde_json::json!({
            "title": "Rate-limit the login endpoint",
            "description": "Five attempts per minute per account.",
            "repo": "acme/api"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let id = created["uow_id"].as_str().unwrap().to_string();
    assert!(id.starts_with("uow-"), "native key, not an issue key: {id}");
    assert_eq!(created["native_number"], 1);

    // The unit carries its own identity; no tracker is linked.
    let unit = state.uow.get(&id).expect("persisted");
    assert_eq!(unit.provider(), camerata_worktracker::Provider::Native);
    assert!(unit.external_ref.is_none());
    assert_eq!(unit.repo(), Some("acme/api"));

    // A spine story with no external link backs it, so a run has a spec to read.
    let story = state.stories.get(&id).await.unwrap().expect("spine story");
    assert!(story.external_ref.is_none());
    assert_eq!(story.title, "Rate-limit the login endpoint");

    // The console lists it like any other unit, as a native work item.
    let (status, list) = send(&app, "GET", "/api/uows", serde_json::Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    let row = list["uows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["id"] == id.as_str())
        .expect("the native unit is listed");
    assert_eq!(row["provider"], "native");
    assert_eq!(row["repo"], "acme/api");
    assert_eq!(row["authoring"], false);
    assert_eq!(row["work_item"]["provider"], "native");
    assert_eq!(row["work_item"]["title"], "Rate-limit the login endpoint");
    assert_eq!(row["work_item"]["number"], 1);
}

#[tokio::test]
async fn a_native_unit_needs_a_title_and_one_of_the_projects_repositories() {
    let state = state_with_project(&["acme/api"]);
    let app = router(state.clone());

    let (status, _) = send(
        &app,
        "POST",
        "/api/uow/native",
        serde_json::json!({ "title": "  " }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, body) = send(
        &app,
        "POST",
        "/api/uow/native",
        serde_json::json!({ "title": "T", "repo": "other/repo" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("acme/api"));
    assert!(
        state.uow.list().is_empty(),
        "a refused request creates nothing"
    );

    // The repository is optional: it can be picked at Intake.
    let (status, _) = send(
        &app,
        "POST",
        "/api/uow/native",
        serde_json::json!({ "title": "T" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

/// A repository that is not `OWNER/REPO` never reaches a path or a push URL: it is refused at
/// creation whether or not the project lists repositories.
#[tokio::test]
async fn a_native_unit_refuses_a_repository_that_is_not_owner_slash_repo() {
    for repos in [&["acme/api"][..], &[][..]] {
        let state = state_with_project(repos);
        let app = router(state.clone());
        for bad in [
            "../../other",
            "..",
            "a/b/c",
            "acme/a pi",
            "acme/",
            "/etc/passwd",
        ] {
            let (status, body) = send(
                &app,
                "POST",
                "/api/uow/native",
                serde_json::json!({ "title": "T", "repo": bad }),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{bad:?}: {body}");
            assert!(
                body["error"].as_str().unwrap().contains("OWNER/REPO"),
                "{bad:?}: {body}"
            );
        }
        assert!(state.uow.list().is_empty(), "nothing was created");
    }
}

/// The Intake scope takes the same check, before the project-membership check.
#[tokio::test]
async fn the_intake_scope_refuses_a_repository_that_is_not_owner_slash_repo() {
    let state = state_with_project(&[]);
    let app = router(state.clone());
    let (_, created) = send(
        &app,
        "POST",
        "/api/uow/native",
        serde_json::json!({ "title": "T", "repo": "acme/api" }),
    )
    .await;
    let id = created["uow_id"].as_str().unwrap().to_string();
    for bad in ["../../other", "a/b/c", "acme/a pi"] {
        let (status, body) = send(
            &app,
            "POST",
            &format!("/api/uow/{}/intake/repos", seg(&id)),
            serde_json::json!({ "repos": [{ "repo": bad, "branch": { "mode": "new_from_base" } }] }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad:?}: {body}");
        assert!(state.uow.get(&id).unwrap().intake.repos.is_empty());
    }
    let (status, _) = send(
        &app,
        "POST",
        &format!("/api/uow/{}/intake/repos", seg(&id)),
        serde_json::json!({ "repos": [{ "repo": "acme/api", "branch": { "mode": "new_from_base" } }] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

/// A write that names a native-shaped key the store never created is a 404 that creates
/// nothing. Before, every `/api/uow/:id/*` write materialized a unit for any id.
#[tokio::test]
async fn a_write_for_an_unknown_native_key_creates_no_phantom_unit() {
    let state = state_with_project(&["acme/api"]);
    let app = router(state.clone());
    let writes = [
        (
            "POST",
            "/api/uow/uow-ghost-1/branch",
            serde_json::json!({ "branch": "b" }),
        ),
        (
            "POST",
            "/api/uow/uow-ghost-1/status",
            serde_json::json!({ "status": "in_progress" }),
        ),
        (
            "POST",
            "/api/uow/uow-ghost-1/history",
            serde_json::json!({ "kind": "note", "text": "x" }),
        ),
        (
            "POST",
            "/api/uow/uow-ghost-1/intake/context",
            serde_json::json!({ "context": "x" }),
        ),
        (
            "POST",
            "/api/uow/uow-ghost-1/intake/repos",
            serde_json::json!({ "repos": [{ "repo": "acme/api", "branch": { "mode": "new_from_base" } }] }),
        ),
        (
            "POST",
            "/api/stories/uow-ghost-1/run",
            serde_json::json!({}),
        ),
        // Percent-encoding does not get around it.
        (
            "POST",
            "/api/uow/%75ow-ghost-1/branch",
            serde_json::json!({ "branch": "b" }),
        ),
    ];
    for (method, uri, body) in writes {
        let (status, resp) = send(&app, method, uri, body).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}: {resp}");
    }
    assert!(
        state.uow.get("uow-ghost-1").is_none(),
        "no unit was materialized for the unknown key"
    );
    assert!(state.uow.list().is_empty());

    // A real native unit is written to normally.
    let (_, created) = send(
        &app,
        "POST",
        "/api/uow/native",
        serde_json::json!({ "title": "T", "repo": "acme/api" }),
    )
    .await;
    let id = created["uow_id"].as_str().unwrap().to_string();
    let (status, _) = send(
        &app,
        "POST",
        &format!("/api/uow/{}/history", seg(&id)),
        serde_json::json!({ "kind": "note", "text": "hello" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Other key shapes keep their create-on-write behaviour (GitHub issue keys, demo ids).
    let (status, _) = send(
        &app,
        "POST",
        &format!("/api/uow/{}/history", seg("acme/api#9")),
        serde_json::json!({ "kind": "note", "text": "hello" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

/// Native numbers are not reused after a delete, through the HTTP surface.
#[tokio::test]
async fn deleting_a_native_unit_does_not_free_its_number() {
    let state = state_with_project(&["acme/api"]);
    let app = router(state.clone());
    let create = |title: &'static str| {
        let app = app.clone();
        async move {
            send(
                &app,
                "POST",
                "/api/uow/native",
                serde_json::json!({ "title": title, "repo": "acme/api" }),
            )
            .await
            .1
        }
    };
    let first = create("one").await;
    let second = create("two").await;
    assert_eq!(first["native_number"], 1);
    assert_eq!(second["native_number"], 2);
    let (status, _) = send(
        &app,
        "DELETE",
        &format!("/api/uow/{}", seg(second["uow_id"].as_str().unwrap())),
        serde_json::Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(create("three").await["native_number"], 3);
}

// ── End to end through the run path with a fake agent ──────────────────────────────────

fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A local clone with one commit and no remote: nothing in the run may need GitHub.
fn local_clone(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.name", "Camerata Test"]);
    git(dir, &["config", "user.email", "test@camerata.invalid"]);
    std::fs::write(dir.join("README.md"), "fixture\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "chore: initial commit"]);
}

/// A fake `claude` (first on PATH): the "agent" writes one file in its working directory (the
/// unit's worktree) and answers with a well-formed result. No model, no network.
fn install_fake_agent(bin_dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let script = "#!/bin/sh\n\
        printf 'implemented by the fake agent\\n' > IMPLEMENTED.md\n\
        printf '%s\\n' '{\"type\":\"result\",\"is_error\":false,\"result\":\"done\",\"session_id\":\"fake\",\"total_cost_usd\":0}'\n";
    let bin = bin_dir.join("claude");
    std::fs::write(&bin, script).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn approved(story: &str) -> DecisionRecord {
    DecisionRecord::ai_proposed(
        story,
        format!("{story}/decision/approach"),
        "Approach",
        "How?",
        "Keep it small.",
        vec![],
        chrono::Utc::now(),
    )
    .approve(chrono::Utc::now())
}

fn event_details(run: &crate::run::Run) -> Vec<String> {
    run.events
        .iter()
        .map(|e| format!("[{}] {}", e.verdict, e.detail))
        .collect()
}

/// Approve a decision, record the branch, start a run with Layer 2 skipped (the fixture has no
/// toolchain) and wait for it to finish. Returns the finished run.
async fn run_unit(app: &Router, state: &AppState, id: &str, branch: &str) -> crate::run::Run {
    state.uow.set_decisions(id, vec![approved(id)]);
    let (status, body) = send(
        app,
        "POST",
        &format!("/api/uow/{}/branch", seg(id)),
        serde_json::json!({ "branch": branch }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = send(
        app,
        "POST",
        &format!("/api/stories/{}/run", seg(id)),
        serde_json::json!({ "skip_layer2": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let run_id = body["run_id"].as_str().expect("run id").to_string();
    for _ in 0..600 {
        if let Some(run) = state.runs.get(&run_id).filter(|r| r.done) {
            return run;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!(
        "run {run_id} did not finish: {:?}",
        state.runs.get(&run_id).map(|r| event_details(&r))
    );
}

/// Live-run environment for one test: live mode, the fake agent first on PATH, no GitHub
/// token, no disk-headroom floor. `None` (with a printed reason) when the gateway binary the
/// run hands the agent has not been built.
struct LiveEnv {
    _guards: Vec<camerata_test_env::EnvGuard>,
    _bin: tempfile::TempDir,
}

fn live_env() -> Option<LiveEnv> {
    if let Err(e) = camerata_fleet::locate_gateway_bin() {
        eprintln!(
            "SKIP: the governed run needs the camerata-gateway binary \
             (`cargo build -p camerata-gateway`): {e}"
        );
        return None;
    }
    let bin = tempfile::tempdir().unwrap();
    install_fake_agent(bin.path());
    let path = format!(
        "{}:{}",
        bin.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    Some(LiveEnv {
        _guards: vec![
            camerata_test_env::EnvGuard::set("CAMERATA_LIVE_BUILD", "1"),
            camerata_test_env::EnvGuard::set("PATH", path),
            camerata_test_env::EnvGuard::remove("CAMERATA_GITHUB_TOKEN"),
            camerata_test_env::EnvGuard::set("CAMERATA_MIN_DISK_HEADROOM_GB", "0"),
        ],
        _bin: bin,
    })
}

/// A project on the CLI backend (so the fake `claude` is the agent) whose `acme/api` is the
/// local clone at `clone`.
fn live_state(clone: &Path) -> AppState {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let project = state
        .projects
        .create("P", vec!["acme/api".to_string()])
        .unwrap();
    state
        .projects
        .set_backend(&project.id, crate::llm::ProjectBackend::Cli);
    state
        .settings
        .set_repo_path("acme/api", Some(clone.display().to_string()));
    state
}

/// THE CLAIM: a unit with no board and no GitHub issue is created, run through the governed
/// development path, branched and committed. The commit carries the unit's own number.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial(process_env)]
async fn a_native_unit_runs_end_to_end_with_a_fake_agent() {
    let Some(_env) = live_env() else { return };
    let clone = tempfile::tempdir().unwrap();
    local_clone(clone.path());
    let state = live_state(clone.path());
    let app = router(state.clone());

    let (status, created) = send(
        &app,
        "POST",
        "/api/uow/native",
        serde_json::json!({ "title": "Add a notice", "description": "Write it.", "repo": "acme/api" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let id = created["uow_id"].as_str().unwrap().to_string();
    let branch = format!("camerata/{id}");

    let run = run_unit(&app, &state, &id, &branch).await;
    assert_eq!(
        run.status,
        RunStatus::AwaitingQa,
        "the run completes: {:?} {:#?}",
        run.failure_reason,
        event_details(&run)
    );

    // Branched and committed in the unit's repository, with the unit's own reference.
    let message = git(clone.path(), &["log", "-1", "--format=%B", &branch]);
    assert!(message.starts_with("feat: implement story"), "{message}");
    assert!(
        message.contains("Refs uow-1."),
        "native number as the story ref, in the form that links to nothing: {message}"
    );
    assert!(
        !has_hash_number(&message),
        "a native unit's commit must not carry a bare #<n> (GitHub links it to issue or PR n): \
         {message}"
    );
    assert_eq!(
        git(clone.path(), &["show", &format!("{branch}:IMPLEMENTED.md")]),
        "implemented by the fake agent"
    );
    assert!(
        run.events
            .iter()
            .any(|e| e.detail.contains("No GitHub token: committed locally")),
        "no token: committed locally, nothing pushed"
    );
}

/// The GitHub adapter still works: a unit created from issue `acme/api#7` runs the same path,
/// in the issue's repository, and its commit references the issue.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial(process_env)]
async fn a_github_backed_unit_still_runs_end_to_end() {
    let Some(_env) = live_env() else { return };
    let clone = tempfile::tempdir().unwrap();
    local_clone(clone.path());
    let state = live_state(clone.path());
    let app = router(state.clone());

    let (status, created) = send(
        &app,
        "POST",
        "/api/uow/from-workitem",
        serde_json::json!({ "work_item_id": "github:acme/api#7" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let id = created["uow_id"].as_str().unwrap().to_string();
    assert_eq!(id, "acme/api#7", "existing GitHub keys are unchanged");
    let unit = state.uow.get(&id).unwrap();
    assert_eq!(unit.provider(), camerata_worktracker::Provider::GitHub);
    assert_eq!(unit.repo(), Some("acme/api"));

    let branch = "camerata/acme-api-7";
    let run = run_unit(&app, &state, &id, branch).await;
    assert_eq!(
        run.status,
        RunStatus::AwaitingQa,
        "the run completes: {:?} {:#?}",
        run.failure_reason,
        event_details(&run)
    );
    let message = git(clone.path(), &["log", "-1", "--format=%B", branch]);
    assert!(
        message.contains("Refs #7."),
        "issue number as the story ref: {message}"
    );
    assert_eq!(
        git(clone.path(), &["show", &format!("{branch}:IMPLEMENTED.md")]),
        "implemented by the fake agent"
    );
}
