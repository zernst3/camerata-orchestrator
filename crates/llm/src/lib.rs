//! `camerata-llm` — the LLM/provider stack (Phase B of the headless-core extraction).
//!
//! Everything a caller needs to run a model completion lives here, extracted from
//! `camerata-server` so the provider stack has no dependency on the Axum adapter:
//!
//! - [`llm`] — the [`llm::LlmPort`] completion seam (formerly named `Completer`), the
//!   [`llm::Llm`] Anthropic transport (CLI + API + the Message Batches path), the
//!   [`llm::OpenRouterCompleter`], the provider-selecting [`llm::build_completer`]
//!   factory, and the [`llm::call_with_fallback`] chain walker.
//! - [`credentials`] — the [`credentials::CredentialStore`] trait + the keychain and
//!   in-memory implementations + the store-then-env [`credentials::resolve`] helper.
//! - [`model_registry`] — the app-wide [`model_registry::ModelRegistry`] (static Claude
//!   catalog + OpenRouter discovery/cache).
//! - [`rate_limit`] — the per-provider [`rate_limit::ProviderRateLimiter`] token bucket.
//! - [`usage_ledger`] — the cumulative [`usage_ledger::UsageLedger`] usage meter.
//!
//! The pure wire DTOs (`LlmResponse`, credential wire shapes + name consts, registry
//! wire shapes, `DEFAULT_MODEL`) live one layer down in `camerata-api-types` (Phase A);
//! the modules here re-export them so both this crate's callers and the server's
//! re-export shims resolve them through one path.
//!
//! Depends ONLY on `camerata-api-types` among workspace crates — deliberately below
//! `camerata-app-core` and `camerata-server` in the dependency graph so any adapter
//! (Axum today, others later) can pull in the provider stack without a cycle.

pub mod credentials;
pub mod llm;
pub mod model_registry;
pub mod rate_limit;
pub mod usage_ledger;

// Flat crate-root re-exports for the most-referenced types, so cross-crate callers can
// write `camerata_llm::Llm` / `camerata_llm::LlmPort` without the module hop.
pub use credentials::{CredentialStore, KeyringCredentialStore, MemoryCredentialStore};
pub use llm::{
    build_completer, call_with_fallback, Llm, LlmPort, LlmRequest, LlmResponse,
    OpenRouterCompleter,
};
pub use model_registry::ModelRegistry;
pub use rate_limit::ProviderRateLimiter;
pub use usage_ledger::UsageLedger;
