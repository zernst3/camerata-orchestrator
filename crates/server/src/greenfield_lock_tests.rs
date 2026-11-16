//! A greenfield run (no repositories) reaches the model through the same agentic loop as a dev
//! run, so a client engagement gets the same lock, the same spend limit and one shared meter and
//! read log for the lead and its children. A project is a client engagement by default.

use std::sync::Arc;

use camerata_worktracker::InMemoryStoryStore;

use crate::dev_implement_run::greenfield_client_lock_refusal;
use crate::live_fleet::greenfield_run_controls;
use crate::model_tier::TierMap;
use crate::project::Project;
use crate::run::{RunKind, RunStatus};
use crate::AppState;

/// A client-engagement project that has opted in to client tooling (so the spend limit is the
/// check under test), with the given dev run spend limit in cents.
fn client_project(limit_cents: Option<u64>) -> Project {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let p = state.projects.create("P", vec![]).unwrap();
    state
        .projects
        .set_client_engagement(&p.id, true, true)
        .unwrap();
    if let Some(cents) = limit_cents {
        state
            .projects
            .set_dev_run_spend_limit(&p.id, Some(cents))
            .unwrap();
    }
    state.projects.get(&p.id).unwrap()
}

#[test]
fn a_client_tier_run_with_no_spend_limit_is_refused_for_the_limit() {
    let project = client_project(None);
    let why = greenfield_client_lock_refusal(&project, Some(&TierMap::default()), None)
        .expect("a client engagement with no spend limit is refused");
    assert!(why.contains("spend limit"), "{why}");
}

#[test]
fn a_client_tier_run_with_a_spend_limit_and_claude_models_may_start() {
    let project = client_project(Some(500));
    assert_eq!(
        greenfield_client_lock_refusal(&project, Some(&TierMap::default()), None),
        None
    );
}

#[test]
fn every_tier_model_is_checked_not_only_the_strongest() {
    let project = client_project(Some(500));
    let map = TierMap {
        fast: vec![
            "claude-haiku-5-5".to_string(),
            "vendor/cheap-model".to_string(),
        ],
        ..TierMap::default()
    };
    let why = greenfield_client_lock_refusal(&project, Some(&map), None)
        .expect("a non-Anthropic model in a fallback chain is refused");
    assert!(why.contains("vendor/cheap-model"), "{why}");

    let map = TierMap {
        balanced: vec!["vendor/balanced-model".to_string()],
        ..TierMap::default()
    };
    assert!(greenfield_client_lock_refusal(&project, Some(&map), None).is_some());

    let map = TierMap {
        strongest: "vendor/strong-model".to_string(),
        ..TierMap::default()
    };
    assert!(greenfield_client_lock_refusal(&project, Some(&map), None).is_some());
}

#[test]
fn the_vision_band_is_checked_only_when_the_project_enables_it() {
    let mut project = client_project(Some(500));
    let map = TierMap {
        vision: vec!["vendor/vision-model".to_string()],
        ..TierMap::default()
    };
    assert_eq!(
        greenfield_client_lock_refusal(&project, Some(&map), None),
        None,
        "a disabled band is never called"
    );
    project.vision_enabled = true;
    assert!(greenfield_client_lock_refusal(&project, Some(&map), None).is_some());
}

#[test]
fn the_single_model_path_is_locked_too() {
    let no_limit = client_project(None);
    let why = greenfield_client_lock_refusal(&no_limit, None, Some("claude-sonnet-5"))
        .expect("no spend limit");
    assert!(why.contains("spend limit"), "{why}");
    // No model chosen: the default strongest model stands in for it.
    assert!(greenfield_client_lock_refusal(&no_limit, None, None).is_some());

    let limited = client_project(Some(500));
    assert_eq!(
        greenfield_client_lock_refusal(&limited, None, Some("claude-sonnet-5")),
        None
    );
    assert!(greenfield_client_lock_refusal(&limited, None, Some("vendor/model")).is_some());
}

#[test]
fn an_internal_project_is_never_locked() {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let p = state.projects.create("P", vec![]).unwrap();
    state
        .projects
        .set_client_engagement(&p.id, false, false)
        .unwrap();
    let project = state.projects.get(&p.id).unwrap();
    let map = TierMap {
        fast: vec!["vendor/cheap-model".to_string()],
        ..TierMap::default()
    };
    assert_eq!(
        greenfield_client_lock_refusal(&project, Some(&map), None),
        None
    );
}

#[test]
fn the_runs_controls_carry_one_meter_one_read_log_and_the_governance_trail() {
    let project = client_project(Some(500));
    let controls = greenfield_run_controls(Some(&project), None, "run-1");
    let meter = controls.spend.expect("one shared meter");
    assert_eq!(meter.limit_usd(), Some(5.0), "the project's dev run limit");
    assert!(controls.read_log.is_some(), "a read log the run keeps");
    assert!(
        controls.governance.is_none(),
        "no governance log in this state"
    );
}

#[tokio::test]
async fn the_governance_trail_is_tagged_with_the_run_id_when_a_log_is_wired() {
    let project = client_project(Some(500));
    let log = Arc::new(
        camerata_persistence::GovernanceLog::open_in_memory()
            .await
            .unwrap(),
    );
    let controls = greenfield_run_controls(Some(&project), Some(log), "run-9");
    let (_, run_id) = controls.governance.expect("governance wired");
    assert_eq!(run_id, "run-9");
}

#[test]
fn a_project_with_no_limit_still_gets_a_shared_counting_meter() {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let p = state.projects.create("P", vec![]).unwrap();
    state
        .projects
        .set_client_engagement(&p.id, false, false)
        .unwrap();
    let project = state.projects.get(&p.id).unwrap();
    let controls = greenfield_run_controls(Some(&project), None, "run-1");
    let meter = controls.spend.expect("a meter even with no limit");
    assert_eq!(meter.limit_usd(), None);
}

const CLIENT_GREENFIELD_REASON: &str = "A client engagement works in a repository. Add one, or turn off Client engagement for this project.";

/// THE ROUTE: a client engagement (API backend, client tooling opted in, a spend limit, no
/// repositories) never runs a greenfield run, tiered or single-model. The run fails with the
/// repository reason before anything is spawned (no fleet event, no lead, no CLI stage).
async fn client_greenfield_run_is_refused(tier_map: Option<TierMap>, model: Option<String>) {
    let _live = camerata_test_env::EnvGuard::set("CAMERATA_LIVE_BUILD", "1");
    // If the refusal ever regressed, the run must still reach no model: no keys anywhere.
    let _a = camerata_test_env::EnvGuard::remove("ANTHROPIC_API_KEY");
    let _o = camerata_test_env::EnvGuard::remove("OPENROUTER_API_KEY");
    let mut state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    state.credential_store = Arc::new(crate::credentials::MemoryCredentialStore::new());
    let p = state.projects.create("P", vec![]).unwrap();
    state
        .projects
        .set_backend(&p.id, crate::project::ProjectBackend::Api)
        .unwrap();
    state
        .projects
        .set_client_engagement(&p.id, true, true)
        .unwrap();
    state
        .projects
        .set_dev_run_spend_limit(&p.id, Some(500))
        .unwrap();

    let (run_id, _) =
        crate::start_governed_run(&state, "story-1", model, tier_map, false, RunKind::Watched)
            .await;

    let run = state.runs.get(&run_id).expect("the run exists");
    assert!(
        matches!(run.status, RunStatus::Failed { .. }),
        "the run failed: {:?}",
        run.status
    );
    assert_eq!(
        run.failure_reason.clone().unwrap_or_default(),
        CLIENT_GREENFIELD_REASON
    );
    assert!(
        run.events
            .iter()
            .all(|e| e.layer != "fleet" && e.layer != "run"),
        "nothing was spawned or announced: {:#?}",
        run.events.iter().map(|e| &e.detail).collect::<Vec<_>>()
    );
}

#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial(process_env)]
async fn a_client_greenfield_tiered_run_is_refused_outright() {
    client_greenfield_run_is_refused(Some(TierMap::default()), None).await;
}

#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial(process_env)]
async fn a_client_greenfield_single_model_run_is_refused_outright() {
    client_greenfield_run_is_refused(None, Some("claude-sonnet-5".to_string())).await;
}
