//! `camerata-scaffold` — Part 1 of the Rust-fullstack PWA scaffolder
//! (`docs/plans/2026-07-09_product-owner-head-vibe-mode.md`, section 4 "Rust-fullstack
//! scaffolder" + the "Feedback loop" section).
//!
//! This crate materializes a vetted, working Dioxus-fullstack (web) PWA app skeleton
//! into a target directory, and provides the `ScaffoldStrategy` fit-check seam the
//! (future) orchestrator uses to decide whether a given app's requirements fit the
//! vetted skeleton or need a from-scratch build.
//!
//! Two things ship here, deliberately kept separate:
//! - [`choose_strategy`] — a pure, mechanical fit-check over [`AppRequirements`]. No
//!   I/O, no template materialization; just a decision.
//! - [`scaffold_skeleton`] — the skeleton materializer. Always emits the Skeleton path;
//!   the caller is responsible for calling `choose_strategy` first and only invoking
//!   this when it returned [`ScaffoldStrategy::Skeleton`].
//!
//! The `FromScratch` path itself is NOT implemented here — per the design doc's
//! skeleton-first decision, it is an honest entry point ([`ScaffoldStrategy::FromScratch`])
//! that a future orchestrator (not this crate) fulfills. This crate never fakes a
//! from-scratch generator.

mod error;
mod outcome;
mod requirements;
mod skeleton;
mod strategy;
mod substitution;

mod custom_rules;

pub use custom_rules::default_custom_rules;
pub use error::ScaffoldError;
pub use outcome::ScaffoldOutcome;
pub use requirements::{AppRequirements, AppTarget, Visibility};
pub use skeleton::scaffold_skeleton;
pub use strategy::{choose_strategy, ScaffoldStrategy};
