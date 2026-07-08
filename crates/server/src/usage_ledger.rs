//! Re-export shim: the cumulative usage ledger was EXTRACTED to `camerata-llm` (Phase B
//! of the headless-core extraction). This module re-exports everything so all existing
//! `crate::usage_ledger::*` call sites (`UsageLedger`, `UsageSnapshot`, `ModelUsage`,
//! `RateLimitEvent`, `is_rate_limit_signal`) keep resolving unchanged. See
//! `camerata_llm::usage_ledger` for the implementation.

pub use camerata_llm::usage_ledger::*;
