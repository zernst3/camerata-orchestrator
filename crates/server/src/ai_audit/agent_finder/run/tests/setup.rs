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
