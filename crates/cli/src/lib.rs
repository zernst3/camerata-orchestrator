//! camerata (cli): binary entrypoint wiring the orchestrator together.
//!
//! Exposes [`acceptance`] as a library module so both the `acceptance`
//! subcommand and the integration test (`tests/acceptance.rs`) drive the same
//! in-process, no-network planted-violation scenario.

pub mod acceptance;
pub mod build_demo;
pub mod deploy_demo;
pub mod eval_cmd;
pub mod fleet_support;
pub mod gate_probe;
pub mod live_demo;
pub mod maintenance_demo;
pub mod po_demo;
pub mod projects_live;
pub mod worktracker_demo;
pub mod worktracker_live;
