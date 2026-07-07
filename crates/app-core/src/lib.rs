//! `camerata-app-core` — framework-agnostic backend app-orchestration logic.
//!
//! The backend twin of `camerata-ui-core`. This crate owns the server's stateless domain types and
//! pure state transitions (`RUST-HEADLESS-CORE-1` + `RUST-PURE-STATE-TRANSITIONS-1`) with NO
//! dependency on any transport framework (no axum/http). The Axum adapter crate (`camerata-server`)
//! owns the stores (the `Arc<Mutex>` persistence) and drives these transitions to compute the next
//! state, then persists it.
//!
//! Extraction from `camerata-server` is incremental (see `docs/plans/2026-07-01_backend-headless-core.md`);
//! modules are promoted here as each surface's transition logic moves over. First beachhead: the
//! already-pure `schedule` decision functions (`is_due` / `next_fire`).

pub mod checkpoint;
pub mod escalation;
pub mod lifecycle;
pub mod project;
pub mod prompt_kernel;
pub mod prompt_layers;
pub mod readiness;
pub mod routine;
pub mod run;
pub mod schedule;
pub mod uow;

// The shared governance prompt kernel, re-exported at the crate root so prompt builders can
// reach it as `camerata_app_core::{GOVERNANCE_KERNEL, GOVERNANCE_KERNEL_READONLY, kernel_for}`.
pub use prompt_kernel::{
    kernel_for, tier_of, KernelTier, GOVERNANCE_KERNEL, GOVERNANCE_KERNEL_READONLY,
};

// The geological prompt layering (prefix-cache-optimal assembly): builders assemble Layer 1
// (global immutable) / Layer 2 (grounding) / Layer 3 (volatile) in order and read the stable
// prefix length off `LayeredPrompt` for a provider-neutral cache breakpoint.
pub use prompt_layers::LayeredPrompt;
