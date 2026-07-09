//! Re-export shim: the app-wide credentials manager was EXTRACTED to `camerata-llm`
//! (Phase B of the headless-core extraction). This module re-exports everything so all
//! existing `crate::credentials::*` call sites keep resolving unchanged:
//!
//! - The behavior (`CredentialStore` trait, `KeyringCredentialStore`,
//!   `MemoryCredentialStore`, `resolve`, `mask_value`, `list_credentials`,
//!   `validate_name`) now lives in `camerata_llm::credentials`.
//! - The pure wire DTOs + name consts + `CredentialError` (relocated in Phase A) live in
//!   `camerata_api_types::credentials`; `camerata_llm::credentials` re-exports them, so
//!   the glob below covers BOTH layers through one path.

pub use camerata_llm::credentials::*;
