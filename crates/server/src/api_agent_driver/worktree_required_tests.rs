//! The in-process API loop can write (`gated_write`), and its write jail and its read jail are
//! both rooted at the worktree. A driver with no worktree would write wherever a path pointed,
//! so one is never built: the type requires a worktree, and the builders refuse to make an API
//! driver without one.

use std::path::PathBuf;
use std::sync::Arc;

use crate::credentials::CredentialStore as _;

fn openrouter_registry() -> crate::model_registry::ModelRegistry {
    let registry = crate::model_registry::ModelRegistry::new();
    registry.seed_openrouter_entries(vec![crate::model_registry::RegistryEntry {
        id: "vendor/synthetic-coder".to_string(),
        provider: "openrouter".to_string(),
        display: "Synthetic".to_string(),
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
    registry: &crate::model_registry::ModelRegistry,
    creds: &crate::credentials::MemoryCredentialStore,
    worktree: Option<PathBuf>,
) -> anyhow::Result<Arc<dyn camerata_core::AgentDriver>> {
    super::build_agent_driver(
        model,
        registry,
        &crate::provider_policy::ProviderPolicy::default(),
        creds,
        "/nonexistent/mcp.json",
        vec![],
        worktree,
        false,
        Arc::new(crate::rate_limit::ProviderRateLimiter::new()),
        None,
        false,
        None,
        None,
        crate::llm::ProjectBackend::Api,
        super::ApiLoopContext::default(),
    )
}

#[test]
fn an_openrouter_api_driver_is_not_built_without_a_worktree() {
    let creds = crate::credentials::MemoryCredentialStore::new();
    creds
        .set(crate::credentials::OPENROUTER_API_KEY, "sk-or-synthetic")
        .unwrap();
    let registry = openrouter_registry();
    let result = build("vendor/synthetic-coder", &registry, &creds, None);
    assert!(
        result.is_err(),
        "a write-capable API driver must not be built without a worktree"
    );
    let msg = result.err().map(|e| e.to_string()).unwrap_or_default();
    assert!(msg.contains("worktree"), "{msg}");
    assert!(
        build(
            "vendor/synthetic-coder",
            &registry,
            &creds,
            Some(PathBuf::from("/tmp/wt"))
        )
        .is_ok(),
        "with a worktree it builds"
    );
}

#[test]
fn an_anthropic_api_driver_is_not_built_without_a_worktree() {
    let creds = crate::credentials::MemoryCredentialStore::new();
    creds
        .set(crate::credentials::ANTHROPIC_API_KEY, "sk-ant-synthetic")
        .unwrap();
    let registry = crate::model_registry::ModelRegistry::new();
    let result = build("claude-sonnet-5", &registry, &creds, None);
    assert!(
        result.is_err(),
        "a write-capable Anthropic API driver must not be built without a worktree"
    );
    assert!(build(
        "claude-sonnet-5",
        &registry,
        &creds,
        Some(PathBuf::from("/tmp/wt"))
    )
    .is_ok());
}
