//! A greenfield run's lead and its children act as one run: the factory hands both the run's
//! read log and spend meter. The lead and the child here are built by the real factories (the
//! lead through `build_lead`, the child through `build_child`), not only the controls helpers.

use std::path::PathBuf;
use std::sync::Arc;

use camerata_fleet::orchestrator::{
    LeadBuildContext, OrchestratorDriverFactory as _, OrchestratorSession,
};
use camerata_gateway::delegate::ChildDriverFactory as _;

use super::*;

#[test]
fn a_real_lead_and_a_real_child_share_the_runs_read_log_and_meter() {
    use crate::credentials::CredentialStore as _;
    let creds = crate::credentials::MemoryCredentialStore::new();
    creds
        .set(crate::credentials::ANTHROPIC_API_KEY, "sk-ant-test")
        .unwrap();
    let log = ReadLog::new();
    let meter = SpendMeter::with_limit(5.0).unwrap();
    let controls = DriverControls {
        read_log: Some(log.clone()),
        spend: Some(meter.clone()),
        ..DriverControls::default()
    };
    let factory = ServerOrchestratorDriverFactory::new(
        crate::model_registry::ModelRegistry::new(),
        crate::provider_policy::ProviderPolicy::default(),
        Arc::new(creds),
        Arc::new(crate::rate_limit::ProviderRateLimiter::new()),
        PathBuf::from("/tmp/fake-camerata-gateway"), // path only; the binary need not exist
        None,
        None,
        crate::llm::ProjectBackend::Api,
    )
    .with_controls(controls);

    let wt = tempfile::tempdir().unwrap();
    let session = OrchestratorSession {
        rules_file: wt.path().join("rules.json"),
        mcp_config: wt.path().join("gateway.json"),
        role_rule_subset: vec![],
        _dir: tempfile::tempdir().unwrap(),
    };
    let tier_map = camerata_fleet::tier::TierMap::default();
    let ctx = LeadBuildContext {
        strongest_model: "claude-sonnet-5",
        session: &session,
        worktree: wt.path(),
        tier_map: &tier_map,
        vision_enabled: false,
        on_activity: None,
    };
    factory.build_lead(&ctx).expect("the lead builds");

    let child_wt = tempfile::tempdir().unwrap();
    factory
        .child_factory(vec![])
        .build_child("claude-sonnet-5", child_wt.path(), &[])
        .expect("the child builds");

    let held = built_spy::holding(&log, &meter);
    let labels: Vec<&str> = held.iter().map(|(l, _)| l.as_str()).collect();
    assert!(
        labels.contains(&"lead"),
        "the lead holds the run's log: {labels:?}"
    );
    assert!(
        labels.contains(&"delegate-child"),
        "the child holds the run's log: {labels:?}"
    );
    assert!(
        held.iter().all(|(_, same_meter)| *same_meter),
        "every driver of the run spends against the one meter: {held:?}"
    );
}
