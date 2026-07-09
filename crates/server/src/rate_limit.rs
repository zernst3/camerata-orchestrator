//! Re-export shim: the per-provider RPM rate limiter was EXTRACTED to `camerata-llm`
//! (Phase B of the headless-core extraction). This module re-exports everything so all
//! existing `crate::rate_limit::*` call sites (`ProviderRateLimiter`, `DEFAULT_RPM`)
//! keep resolving unchanged. See `camerata_llm::rate_limit` for the implementation.

pub use camerata_llm::rate_limit::*;
