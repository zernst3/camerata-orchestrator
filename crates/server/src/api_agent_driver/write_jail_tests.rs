//! A write-capable driver cannot exist without a worktree. The in-process loop offers
//! `gated_write`; with no worktree the write would go to the raw path, anywhere on disk. So no
//! builder returns this loop without a worktree (the command line driver is another driver and
//! is not covered here).

use std::sync::Arc;

use crate::credentials::CredentialStore as _;

use super::*;

fn registry_with_openrouter_model() -> crate::model_registry::ModelRegistry {
    let registry = crate::model_registry::ModelRegistry::new();
    registry.seed_openrouter_entries(vec![crate::model_registry::RegistryEntry {
        id: "openrouter/mistral-7b".to_string(),
        provider: "openrouter".to_string(),
        display: "Test".to_string(),
        free: true,
        tool_use: true,
        context: 4096,
        coding: 0.5,
        price_in: 0.0,
        price_out: 0.0,
        weight: 0,
        caching: false,
        vision: false,
        price_known: true,
    }]);
    registry
}

fn build(
    model: &str,
    creds: &crate::credentials::MemoryCredentialStore,
    worktree: Option<PathBuf>,
    backend: crate::llm::ProjectBackend,
) -> anyhow::Result<Arc<dyn AgentDriver>> {
    build_agent_driver(
        model,
        &registry_with_openrouter_model(),
        &crate::provider_policy::ProviderPolicy::default(),
        creds,
        "/tmp/fake-mcp.json",
        vec![],
        worktree,
        false,
        Arc::new(crate::rate_limit::ProviderRateLimiter::new()),
        None,
        false,
        None,
        None,
        backend,
    )
}

#[test]
fn the_anthropic_api_loop_is_refused_without_a_worktree() {
    let creds = crate::credentials::MemoryCredentialStore::new();
    creds
        .set(crate::credentials::ANTHROPIC_API_KEY, "sk-ant-test")
        .unwrap();
    let refused = build(
        "claude-sonnet-5",
        &creds,
        None,
        crate::llm::ProjectBackend::Api,
    );
    let why = refused
        .err()
        .expect("a write-capable loop with no worktree must not be built")
        .to_string();
    assert!(why.contains("worktree"), "{why}");
}

#[test]
fn the_openrouter_loop_is_refused_without_a_worktree() {
    let creds = crate::credentials::MemoryCredentialStore::new();
    creds
        .set(crate::credentials::OPENROUTER_API_KEY, "sk-or-test")
        .unwrap();
    let refused = build(
        "openrouter/mistral-7b",
        &creds,
        None,
        crate::llm::ProjectBackend::Cli,
    );
    let why = refused
        .err()
        .expect("a write-capable loop with no worktree must not be built")
        .to_string();
    assert!(why.contains("worktree"), "{why}");
}

#[test]
fn both_loops_build_with_a_worktree() {
    let creds = crate::credentials::MemoryCredentialStore::new();
    creds
        .set(crate::credentials::ANTHROPIC_API_KEY, "sk-ant-test")
        .unwrap();
    creds
        .set(crate::credentials::OPENROUTER_API_KEY, "sk-or-test")
        .unwrap();
    let wt = Some(PathBuf::from("/tmp/wt"));
    assert!(build(
        "claude-sonnet-5",
        &creds,
        wt.clone(),
        crate::llm::ProjectBackend::Api
    )
    .is_ok());
    assert!(build(
        "openrouter/mistral-7b",
        &creds,
        wt,
        crate::llm::ProjectBackend::Cli
    )
    .is_ok());
}

#[test]
fn the_driver_type_carries_its_worktree() {
    // `new` takes the worktree and the field is not optional: there is no way to make this
    // loop without one.
    let d = ApiAgentDriver::new(
        Arc::new(AnthropicNoopCompleter),
        "claude-sonnet-5",
        "/tmp/wt",
    );
    let _: &PathBuf = &d.worktree;
}
