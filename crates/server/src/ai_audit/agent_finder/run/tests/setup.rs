//! The finder setup: the one value the command line hands the run, and the agent the run builds
//! from it on its own shared meter.

use std::path::PathBuf;
use std::sync::Mutex;

use super::super::{
    find_repo_for_run_with, resolve_agent_build, with_finder_setup, EngagementKind, FinderSetup,
};
use super::metering::SelfMetering;
use super::*;
use crate::ai_audit::UsageMeter;
use crate::llm::{Backend, BackendResolution};

/// A model client that is never called (the finder's agent is the only caller).
struct NoModel;

#[async_trait::async_trait]
impl crate::llm::LlmPort for NoModel {
    async fn complete(
        &self,
        _req: crate::llm::LlmRequest,
    ) -> anyhow::Result<crate::llm::LlmResponse> {
        anyhow::bail!("the finder setup tests call no model")
    }
    async fn complete_streaming(
        &self,
        _req: crate::llm::LlmRequest,
        _on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<crate::llm::LlmResponse> {
        anyhow::bail!("the finder setup tests call no model")
    }
    fn as_any(&self) -> &dyn std::any::Any {
        // The wrapper the command line passes hides the key the same way.
        &()
    }
}

/// What the factory saw: the key, the rooms folder, the engagement kind and whether the meter it
/// was given is the run's own.
type Built = (Option<String>, PathBuf, EngagementKind, bool);

fn setup(engagement: EngagementKind) -> FinderSetup {
    FinderSetup {
        rooms_dir: PathBuf::from("/run/folder/finder-rooms"),
        api_key: Some("test-key".to_string()),
        engagement,
    }
}

#[test]
fn the_setup_names_the_rooms_the_key_and_the_engagement_kind() {
    let build =
        resolve_agent_build(Some(&setup(EngagementKind::Internal)), None, Backend::Cli).unwrap();
    assert_eq!(build.work_dir, PathBuf::from("/run/folder/finder-rooms"));
    assert_eq!(build.api_key.as_deref(), Some("test-key"));
    assert_eq!(build.engagement, EngagementKind::Internal);
}

#[test]
fn with_no_setup_the_command_line_backend_is_an_error_never_a_temp_folder() {
    let why = resolve_agent_build(None, None, Backend::Cli)
        .err()
        .expect("no setup, no rooms")
        .to_string();
    assert!(why.contains("finder setup"), "{why}");
}

#[test]
fn with_no_setup_the_engagement_kind_is_the_strict_one() {
    let build = resolve_agent_build(None, Some("held-key"), Backend::Api).unwrap();
    assert_eq!(build.engagement, EngagementKind::Client);
    assert_eq!(build.api_key.as_deref(), Some("held-key"));
}

#[test]
fn the_setup_never_prints_its_key() {
    let text = format!("{:?}", setup(EngagementKind::Client));
    assert!(!text.contains("test-key"), "{text}");
}

#[tokio::test]
async fn the_run_builds_the_agent_on_its_own_meter_and_its_spend_appears_once() {
    let meter = Arc::new(UsageMeter::default());
    let seen: Mutex<Vec<Built>> = Mutex::default();
    let run_meter = meter.clone();
    let factory = |_c: &RunControls,
                   build: &super::super::AgentBuild,
                   m: Arc<UsageMeter>|
     -> anyhow::Result<Box<dyn PieceAgent>> {
        seen.lock().unwrap().push((
            build.api_key.clone(),
            build.work_dir.clone(),
            build.engagement,
            Arc::ptr_eq(&m, &run_meter),
        ));
        Ok(Box::new(SelfMetering {
            inner: ScriptedAgent::new(good_script(vec![], vec![])).with_usage(usage(1.0)),
            meter: m,
        }))
    };
    let result = with_finder_setup(
        setup(EngagementKind::Internal),
        find_repo_for_run_with(
            &NoModel,
            REPO,
            &repo_files(),
            &rules(),
            Some(&corpus()),
            &RunControls::default(),
            &HashMap::new(),
            &BackendResolution::Api,
            Some(&meter),
            &factory,
        ),
    )
    .await;
    assert!(result.is_ok());
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "the agent is built once for the repository");
    assert_eq!(
        seen[0].0.as_deref(),
        Some("test-key"),
        "the key came from the setup"
    );
    assert_eq!(seen[0].1, PathBuf::from("/run/folder/finder-rooms"));
    assert_eq!(seen[0].2, EngagementKind::Internal);
    assert!(seen[0].3, "the agent got the run's own meter");
    let snap = meter.snapshot();
    assert_eq!(snap.calls, 1, "one turn, counted once");
    assert!((snap.cost_usd - 1.0).abs() < 1e-9, "{}", snap.cost_usd);
}

/// Run the finder for one repository with `setup` (or none) and return the engagement kind the
/// factory was handed.
async fn engagement_the_factory_saw(setup: Option<FinderSetup>) -> EngagementKind {
    let meter = Arc::new(UsageMeter::default());
    let seen: Mutex<Vec<EngagementKind>> = Mutex::default();
    let factory = |_c: &RunControls,
                   build: &super::super::AgentBuild,
                   m: Arc<UsageMeter>|
     -> anyhow::Result<Box<dyn PieceAgent>> {
        seen.lock().unwrap().push(build.engagement);
        Ok(Box::new(SelfMetering {
            inner: ScriptedAgent::new(good_script(vec![], vec![])).with_usage(usage(1.0)),
            meter: m,
        }))
    };
    let controls = RunControls::default();
    let files = repo_files();
    let selected = rules();
    let corpus = corpus();
    let chosen = HashMap::new();
    let call = find_repo_for_run_with(
        &NoModel,
        REPO,
        &files,
        &selected,
        Some(&corpus),
        &controls,
        &chosen,
        &BackendResolution::Api,
        Some(&meter),
        &factory,
    );
    let result = match setup {
        Some(s) => with_finder_setup(s, call).await,
        None => call.await,
    };
    assert!(result.is_ok());
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    seen[0]
}

#[tokio::test]
async fn a_client_setup_hands_the_factory_client() {
    assert_eq!(
        engagement_the_factory_saw(Some(setup(EngagementKind::Client))).await,
        EngagementKind::Client
    );
}

#[tokio::test]
async fn no_setup_hands_the_factory_client() {
    assert_eq!(
        engagement_the_factory_saw(None).await,
        EngagementKind::Client
    );
}

#[tokio::test]
async fn an_internal_setup_hands_the_factory_internal() {
    assert_eq!(
        engagement_the_factory_saw(Some(setup(EngagementKind::Internal))).await,
        EngagementKind::Internal
    );
}

#[tokio::test]
async fn the_production_factory_passes_the_engagement_on_so_a_client_run_is_refused_by_the_cli_agent(
) {
    let meter = Arc::new(UsageMeter::default());
    let result = with_finder_setup(
        setup(EngagementKind::Client),
        super::super::find_repo_for_run(
            &NoModel,
            REPO,
            &repo_files(),
            &rules(),
            Some(&corpus()),
            &RunControls::default(),
            &HashMap::new(),
            &BackendResolution::Cli,
            Some(&meter),
        ),
    )
    .await;
    let (_, _, _, failed, _, _) = result.expect("a refused piece is a failed pass, not an error");
    let text = failed
        .iter()
        .map(|f| f.reason.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("internal runs only"), "{text}");
}

// ── The setup for a run (the one function the command line and the server's scan path share) ──

fn creds_with_key(key: Option<&str>) -> crate::credentials::MemoryCredentialStore {
    let store = crate::credentials::MemoryCredentialStore::new();
    if let Some(k) = key {
        crate::credentials::CredentialStore::set(&store, crate::credentials::ANTHROPIC_API_KEY, k)
            .unwrap();
    }
    store
}

fn agent_controls() -> RunControls {
    RunControls {
        scope: Default::default(),
        finder: crate::ai_audit::agent_finder::FinderChoice::Agent,
        ..RunControls::default()
    }
}

#[test]
#[serial_test::serial(process_env)]
fn a_client_run_on_the_api_gets_the_run_folders_rooms_the_saved_key_and_client() {
    let _env = camerata_test_env::EnvGuard::remove("ANTHROPIC_API_KEY");
    let creds = creds_with_key(Some("sk-test"));
    let run_dir = PathBuf::from("/run/folder");
    let setup = super::super::finder_setup_for_run(
        &agent_controls(),
        false,
        Backend::Api,
        &creds,
        Some(&run_dir),
        true,
    )
    .unwrap()
    .expect("the agent finder gets a setup");
    assert_eq!(setup.rooms_dir, run_dir.join("finder-rooms"));
    assert_eq!(setup.api_key.as_deref(), Some("sk-test"));
    assert_eq!(setup.engagement, EngagementKind::Client);
}

/// The first run from the app: the command line transport, internal mode, no key needed.
#[test]
#[serial_test::serial(process_env)]
fn an_internal_run_on_the_cli_gets_internal_the_rooms_and_traces_beside_them() {
    let _env = camerata_test_env::EnvGuard::remove("ANTHROPIC_API_KEY");
    let creds = creds_with_key(None);
    let run_dir = PathBuf::from("/run/folder");
    let setup = super::super::finder_setup_for_run(
        &agent_controls(),
        false,
        Backend::Cli,
        &creds,
        Some(&run_dir),
        false,
    )
    .unwrap()
    .expect("the agent finder gets a setup");
    assert_eq!(setup.engagement, EngagementKind::Internal);
    assert_eq!(setup.rooms_dir, run_dir.join("finder-rooms"));
    // The build the factory sees names the same rooms folder, and the transport puts its stream
    // records in `<run folder>/finder-traces` (the parent of the rooms folder).
    let build = resolve_agent_build(Some(&setup), None, Backend::Cli).unwrap();
    assert_eq!(build.engagement, EngagementKind::Internal);
    assert_eq!(
        build.work_dir.parent().unwrap().join("finder-traces"),
        run_dir.join("finder-traces")
    );
}

#[test]
#[serial_test::serial(process_env)]
fn the_engine_finder_a_blocked_review_and_a_missing_api_key_are_handled_before_any_spend() {
    let _env = camerata_test_env::EnvGuard::remove("ANTHROPIC_API_KEY");
    let creds = creds_with_key(None);
    let dir = PathBuf::from("/run/folder");
    let engine = super::super::finder_setup_for_run(
        &RunControls::default(),
        false,
        Backend::Api,
        &creds,
        Some(&dir),
        true,
    );
    assert_eq!(engine, Ok(None), "the engine finder builds no setup");
    let blocked = super::super::finder_setup_for_run(
        &agent_controls(),
        true,
        Backend::Cli,
        &creds,
        Some(&dir),
        true,
    );
    assert_eq!(blocked, Ok(None), "a blocked AI review builds none");
    let no_key = super::super::finder_setup_for_run(
        &agent_controls(),
        false,
        Backend::Api,
        &creds,
        Some(&dir),
        true,
    );
    assert!(no_key.is_err(), "the API backend needs a saved key");
}

/// The shared function is the only place that builds a `FinderSetup` outside tests.
#[test]
fn only_the_shared_function_builds_a_finder_setup() {
    fn scan(dir: &std::path::Path, hits: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            let name = path.to_string_lossy().to_string();
            if path.is_dir() {
                scan(&path, hits);
            } else if name.ends_with(".rs")
                && !name.contains("/tests/")
                && !name.ends_with("_tests.rs")
                && !name.ends_with("/tests.rs")
            {
                let text = std::fs::read_to_string(&path).unwrap();
                if text.contains("FinderSetup {") && !text.contains("pub struct FinderSetup") {
                    hits.push(name);
                }
            }
        }
    }
    let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut hits = Vec::new();
    for c in std::fs::read_dir(&crates).unwrap().flatten() {
        let src = c.path().join("src");
        if src.is_dir() {
            scan(&src, &mut hits);
        }
    }
    assert!(
        hits.is_empty(),
        "built outside the shared function: {hits:?}"
    );
}

/// A rerun (#588) whose finder has lost its store-only twin never falls through to building a real
/// agent: it refuses, and the factory is never called.
#[tokio::test]
async fn a_rerun_without_its_store_only_twin_refuses_instead_of_building_an_agent() {
    let meter = Arc::new(UsageMeter::default());
    let built = std::sync::atomic::AtomicUsize::new(0);
    let factory = |_c: &RunControls,
                   _b: &super::super::AgentBuild,
                   m: Arc<UsageMeter>|
     -> anyhow::Result<Box<dyn PieceAgent>> {
        built.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(Box::new(SelfMetering {
            inner: ScriptedAgent::new(good_script(vec![], vec![])).with_usage(usage(1.0)),
            meter: m,
        }))
    };
    let result = crate::checkpoint::with_rerun_scope(
        true,
        with_finder_setup(
            setup(EngagementKind::Internal),
            find_repo_for_run_with(
                &NoModel,
                REPO,
                &repo_files(),
                &rules(),
                Some(&corpus()),
                &RunControls::default(),
                &HashMap::new(),
                &BackendResolution::Api,
                Some(&meter),
                &factory,
            ),
        ),
    )
    .await;
    assert!(result.is_err(), "a rerun with no store-only twin refuses");
    assert_eq!(built.load(std::sync::atomic::Ordering::SeqCst), 0);
}
