//! Re-export shim: the model registry was EXTRACTED to `camerata-llm` (Phase B of the
//! headless-core extraction). This module re-exports everything so all existing
//! `crate::model_registry::*` call sites keep resolving unchanged:
//!
//! - The behavior (`ModelRegistry` + the OpenRouter discovery/fetch path,
//!   `CLAUDE_REGISTRY_MODELS`, `claude_entries`, `caching_heuristic`) now lives in
//!   `camerata_llm::model_registry`.
//! - The pure wire DTOs (`RegistryEntry`, `RegistryEntryStatic`, `RegistryResp`,
//!   `RefreshResp` — relocated in Phase A) live in `camerata_api_types::model_registry`;
//!   `camerata_llm::model_registry` re-exports them, so the glob below covers BOTH
//!   layers through one path.

pub use camerata_llm::model_registry::*;
