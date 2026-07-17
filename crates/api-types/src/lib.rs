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
//! - [`workitems`] — relocated from `camerata_ui::cockpit::uow` (the UI-side `WorkItem`
//!   wire mirror only; Phase C of the UI-core extraction so `camerata-ui-core` can hold
//!   pulled items without a UI dep). Also carries the `POST /api/workitems/assign`
//!   request/response mirror (Phase D).
//! - [`stories`] — Phase D: a pure-serde mirror of `camerata_worktracker::CanonicalStory`
//!   (and its `RepoTarget`/`ExternalRef`/`FeatureStatus`/`Provider` sub-shapes) for
//!   `GET /api/stories`, added for `camerata-client`.
//! - [`run`] — Phase D: a pure-serde mirror of `camerata_app_core::run`'s `Run`/
//!   `RunStatus`/`GateEvent`/`RunKind`/`StallPolicy` plus the server's `RunStatusResponse`
//!   (`GET /api/runs/:id`) and `StartRunReq`/response (`POST /api/stories/:id/run`).
//! - [`governance`] — Phase H3: a pure-serde mirror of
//!   `camerata_persistence::GovernanceEvent`'s wire shape, for `GET /api/runs/:id/events`
//!   and `GET /api/governance/events` (added for `camerata-client`).
//! - [`feedback`] — the Product-Owner feedback loop's ingest contract: `DefectReport`
//!   (and its `DefectSource`/`DefectKind`/`DefectSeverity`/`DefectStatus`/`DefectContext`
//!   sub-shapes), for `POST /api/feedback`, `GET /api/projects/:id/feedback`, and
//!   `GET /api/feedback/recent`. Unlike `governance`, this is NOT a hand-mirrored DTO —
//!   `camerata-persistence` depends on this crate and stores `DefectReport` directly.
//!
//! Every relocated module's origin re-exports everything below so existing call sites
//! (`camerata_app_core::uow::X`, `crate::llm::LlmResponse`, etc.) resolve unchanged.

pub mod credentials;
pub mod feedback;
pub mod governance;
pub mod lifecycle;
pub mod llm;
pub mod model_registry;
pub mod project;
pub mod run;
pub mod stories;
pub mod uow;
pub mod workitems;
