//! Re-export shim: the LLM provider seam was EXTRACTED to `camerata-llm` (Phase B of the
//! headless-core extraction). This module re-exports everything so all existing
//! `crate::llm::*` call sites keep resolving unchanged:
//!
//! - The behavior (the `LlmPort` trait (formerly named `Completer`), the `Llm` Anthropic
//!   transport with its CLI/API/Message-Batches paths, `OpenRouterCompleter`, the
//!   `build_completer` factory, `call_with_fallback`, `MODELS`, `Vendor`, `Backend`,
//!   `select_backend`, `kill_inflight_claude`, the batch item/result types, ...) now
//!   lives in `camerata_llm::llm`.
//! - The pure DTOs relocated in Phase A (`LlmResponse` in `camerata_api_types::llm`,
//!   `DEFAULT_MODEL` in `camerata_api_types::project`) are re-exported by
//!   `camerata_llm::llm`, so the glob below covers BOTH layers through one path.

pub use camerata_llm::llm::*;
