//! #565: a client engagement runs on the API backend and the Agent finder, for a new project and
//! for one saved before those settings existed. The old project file is a fixture of the old shape
//! (`tests/fixtures/old_project_store/projects.json`): no `backend`, no `client_engagement`, no
//! `review` block, and the retired `cli_active` key.

use super::*;
use crate::project::ProjectBackend;
use camerata_api_types::project::{ProjectFinder, FINDER_MOVED_NOTE};

const OLD_STORE: &str = include_str!("../../tests/fixtures/old_project_store/projects.json");

/// A scratch copy of the old store file, loaded the way the app loads it.
fn load_old_store() -> (crate::project::ProjectStore, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "camerata-old-store-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("projects.json");
    std::fs::write(&path, OLD_STORE).unwrap();
    (
        crate::project::ProjectStore::load_or_new(path.clone()),
        path,
    )
}

/// The old file still loads: every project, the active pointer and the counter survive.
#[test]
fn an_old_project_file_still_loads() {
    let (store, _) = load_old_store();
    let names: Vec<String> = store.list().into_iter().map(|p| p.name).collect();
    assert_eq!(
        names,
        ["acme-ledger", "acme-billing", "acme-internal-tools"],
        "every project in the old file loads"
    );
    assert_eq!(store.active().unwrap().id, "proj-1");
    let next = store.create("acme-next", vec![]).unwrap();
    assert_eq!(next.id, "proj-4", "the saved counter is kept");
}

/// An old client engagement with no backend setting opens on the API backend, the only backend
/// a client engagement may run on, and the file is written back so it stays there.
#[test]
fn an_old_client_engagement_with_no_backend_setting_opens_on_the_api_backend() {
    let (store, path) = load_old_store();
    let ledger = store.get("proj-1").unwrap();
    assert!(
        ledger.client_engagement,
        "an old project is a client engagement"
    );
    assert_eq!(
        ledger.backend,
        ProjectBackend::Api,
        "an old client engagement with no backend setting must open on the API backend"
    );
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        written["projects"][0]["backend"], "api",
        "the move is written back to the file"
    );
    // A backend the project saved is kept: the CLI stays the CLI (and is refused below).
    assert_eq!(store.get("proj-2").unwrap().backend, ProjectBackend::Cli);
    // A project that is not a client engagement keeps the old default.
    let internal = store.get("proj-3").unwrap();
    assert!(!internal.client_engagement);
    assert_eq!(internal.backend, ProjectBackend::Cli);
}

/// An old project with no finder setting opens on the Agent finder with the one line note; a
/// saved finder is kept and has no note.
#[test]
fn an_old_project_with_no_finder_setting_opens_on_the_agent_finder_with_a_note() {
    let (store, _) = load_old_store();
    for id in ["proj-1", "proj-3"] {
        let p = store.get(id).unwrap();
        assert_eq!(p.review.finder, ProjectFinder::Agent, "{id}");
        assert!(p.review.finder_moved_to_agent, "{id} carries the note");
    }
    assert_eq!(
        store.get("proj-3").unwrap().review.escalation_floor,
        9,
        "the rest of a saved review block is kept"
    );
    let billing = store.get("proj-2").unwrap();
    assert_eq!(billing.review.finder, ProjectFinder::Engine);
    assert!(!billing.review.finder_moved_to_agent);
    assert_eq!(billing.review.escalation_floor, 7);
    assert!(
        !FINDER_MOVED_NOTE.contains('\u{2014}') && !FINDER_MOVED_NOTE.contains('\u{2013}'),
        "no dash in operator text: {FINDER_MOVED_NOTE}"
    );
}

async fn post_step(state: &AppState, id: &str, step: &str, model: &str) -> serde_json::Value {
    let resp = router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/projects/{id}/step-models"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "step": step, "model": model }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    body_json(resp).await
}

/// Picking a finder on the settings route clears the note: the operator has now chosen.
#[tokio::test]
async fn picking_a_finder_clears_the_moved_note() {
    let (store, _) = load_old_store();
    let mut state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    state.projects = store;
    let body = post_step(&state, "proj-1", "finder", "agent").await;
    assert_eq!(body["ok"], true, "{body}");
    let p = state.projects.get("proj-1").unwrap();
    assert_eq!(p.review.finder, ProjectFinder::Agent);
    assert!(
        !p.review.finder_moved_to_agent,
        "picking a finder must clear the moved note"
    );
    assert!(
        body["project"]["review"]
            .get("finder_moved_to_agent")
            .is_none(),
        "{body}"
    );
}

/// A new client engagement starts on the API backend and the Agent finder, and every model row
/// that has a run time default is left unset, so the settings show "(not set)" with the default
/// named.
#[tokio::test]
async fn a_new_client_engagement_starts_on_the_api_and_the_agent_finder_with_unset_models() {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let resp = router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/projects")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "name": "acme-new", "repos": ["acme/new"] }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = body_json(resp).await;
    assert_eq!(body["ok"], true, "{body}");
    let p: crate::project::Project = serde_json::from_value(body["project"].clone()).unwrap();
    assert!(p.client_engagement);
    assert_eq!(p.backend, ProjectBackend::Api);
    assert_eq!(p.review.finder, ProjectFinder::Agent);
    assert!(!p.review.finder_moved_to_agent, "a new project has no note");
    for (row, value) in [
        ("finder model", &p.review.finder_model),
        ("duplicate check", &p.review.dup_check_model),
        ("clone judge", &p.review.clone_judge_model),
        ("fix", &p.step_models.fix),
    ] {
        assert!(value.is_empty(), "{row} starts unset, got {value}");
    }
    // The engine's lock lets the new engagement through on its defaults.
    assert_eq!(
        client_lock_refusal_for_scan(
            Some(&p),
            None,
            None,
            false,
            &crate::cost_cap::RunControls::default(),
        ),
        None
    );
}

/// An old client engagement whose file saved the CLI backend is still refused before any
/// repository is read (the repository does not exist, so reaching the scan would say "not found").
#[tokio::test]
async fn an_old_client_engagement_saved_on_the_cli_is_still_refused_before_any_file_is_read() {
    let (store, _) = load_old_store();
    let mut state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    state.projects = store;
    assert!(state.projects.set_active("proj-2"));
    let req = AuditReq {
        repos: vec!["acme/billing".to_string()],
        ..serde_json::from_value(serde_json::json!({ "repos": [] })).unwrap()
    };
    let Json(report) = onboard_audit(State(state.clone()), Json(req)).await;
    assert!(report.blocked, "{:?}", report.message);
    assert!(
        report.message.as_deref().unwrap().contains("`cli` backend"),
        "{:?}",
        report.message
    );
    assert_eq!(report.files_scanned, 0);
}
