//! Re-export shim: the OpenRouter provider-safety policy (the "trust core" — see
//! `docs/design/2026-07-28_openrouter-provider-safety.md`) lives in `camerata-llm`,
//! alongside the rest of the LLM/provider stack (Phase B of the headless-core
//! extraction). This module re-exports everything so `crate::provider_policy::*` call
//! sites resolve unchanged:
//!
//! - [`ProviderPolicy`] — the persisted safety toggle (`safe_mode`, default `true`) +
//!   optional provider pin. Persisted via `crate::settings::SettingsStore::
//!   provider_policy` / `set_provider_policy`.
//! - [`SafeProviders`] / [`provider_constraint_for_request`] — the enforcement seam both
//!   OpenRouter request-body call sites (`camerata_llm::llm::OpenRouterCompleter` and
//!   `crate::api_agent_driver::call_openrouter_with_tools`) go through.

pub use camerata_llm::provider_policy::*;
