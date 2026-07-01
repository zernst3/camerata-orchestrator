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
pub mod lifecycle;
pub mod routine;
pub mod schedule;
