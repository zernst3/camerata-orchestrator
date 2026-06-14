//! camerata (cli): binary entrypoint wiring the orchestrator together.
//!
//! Exposes [`acceptance`] as a library module so both the `acceptance`
//! subcommand and the integration test (`tests/acceptance.rs`) drive the same
//! in-process, no-network planted-violation scenario.

pub mod acceptance;
pub mod live_demo;
