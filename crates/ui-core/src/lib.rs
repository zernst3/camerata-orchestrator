//! `camerata-ui-core` — framework-agnostic UI logic and state for the cockpit.
//!
//! This crate owns the cockpit's pure logic, data shapes, and state transitions with NO dependency on
//! any rendering framework (RUST-HEADLESS-CORE-1). The Dioxus adapter crate (`camerata-ui`) depends on
//! it and renders its state; everything here is unit-testable with no VirtualDom.
//!
//! Extraction from `camerata-ui` is incremental (see the UI-core extraction notes);
//! modules are added here as each surface's logic moves over.

pub mod adopt;
pub mod backend;
pub mod decision_memory;
pub mod designs;
pub mod git;
pub mod govdev;
pub mod lenient;
pub mod models;
pub mod order_preset;
pub mod palette;
pub mod pane_context;
pub mod patterns;
pub mod product_export;
pub mod progress;
pub mod provider_safety;
pub mod readiness;
pub mod review_stats;
pub mod review_timer;
pub mod rules;
pub mod run;
pub mod save_queue;
pub mod scan;
pub mod schedule;
pub mod source;
pub mod stack;
#[cfg(test)]
pub(crate) mod test_support;
pub mod triage;
pub mod viewer;
pub mod viewer_find;
pub mod walkthrough;
