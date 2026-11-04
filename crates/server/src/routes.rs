//! Axum handlers and their request types, grouped by area. `lib.rs` keeps the router and the
//! shared state; each module here re-exports its items so every `crate::name` path is unchanged.

mod chat_models;
mod decompose_routines;
mod design_routes;
mod drift_updates;
mod investigation_pr;
mod onboarding;
mod project_context;
mod projects;
mod runs;
mod scan;
mod scan_jobs;
mod settings_git;
mod uow_routes;
mod uow_state;

pub use chat_models::*;
pub(crate) use decompose_routines::*;
pub(crate) use design_routes::*;
pub(crate) use drift_updates::*;
pub(crate) use investigation_pr::*;
pub(crate) use onboarding::*;
pub use project_context::*;
pub use projects::*;
pub use runs::*;
pub use scan::*;
pub use scan_jobs::*;
pub(crate) use settings_git::*;
pub(crate) use uow_routes::*;
pub use uow_state::*;
