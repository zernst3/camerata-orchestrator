//! `camerata-api-types` — the pure-serde wire-contract leaf crate (Phase A of the DTO
//! extraction, see `docs/plans/2026-07-01_backend-headless-core.md`).
//!
//! This crate holds the DTO/domain-serde shapes shared across the backend layers, with
//! NO dependency on any other camerata-* crate (only `serde`, `serde_json`, `chrono`,
//! and `thiserror`) — it is the bottom of the dependency graph, so any crate can depend
//! on it without pulling in transport/framework code.
//!
//! Modules mirror their source of origin:
//! - [`uow`] — relocated from `camerata_app_core::uow`.
//! - [`project`] — relocated from `camerata_app_core::project` (the fully self-contained
//!   sub-shapes only; `Project` itself stays in `camerata_app_core`).
//! - [`lifecycle`] — relocated from `camerata_app_core::lifecycle` (the `UowStage` enum +
//!   its pure inherent impl only; the `camerata_worktracker`-dependent transitions stay
//!   in `camerata_app_core::lifecycle`).
//! - [`llm`] — relocated from `camerata_server::llm` (`LlmResponse` only).
//! - [`credentials`] — relocated from `camerata_server::credentials` (the wire shapes +
//!   known-name constants + error enum only).
//! - [`model_registry`] — relocated from `camerata_server::model_registry` (the wire
//!   shapes only).
//!
//! Every relocated module's origin re-exports everything below so existing call sites
//! (`camerata_app_core::uow::X`, `crate::llm::LlmResponse`, etc.) resolve unchanged.

pub mod credentials;
pub mod lifecycle;
pub mod llm;
pub mod model_registry;
pub mod project;
pub mod uow;
