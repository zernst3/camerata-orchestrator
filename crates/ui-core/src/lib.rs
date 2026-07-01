//! `camerata-ui-core` — framework-agnostic UI logic and state for the cockpit.
//!
//! This crate owns the cockpit's pure logic, data shapes, and state transitions with NO dependency on
//! any rendering framework (RUST-HEADLESS-CORE-1). The Dioxus adapter crate (`camerata-ui`) depends on
//! it and renders its state; everything here is unit-testable with no VirtualDom.
//!
//! Extraction from `camerata-ui` is incremental (see `docs/plans/2026-07-01_ui-core-extraction.md`);
//! modules are added here as each surface's logic moves over.

pub mod models;
pub mod scan;
pub mod schedule;
