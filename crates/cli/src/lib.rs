//! camerata (cli): binary entrypoint wiring the orchestrator together.
//!
//! Exposes [`acceptance`] as a library module so both the `acceptance`
//! subcommand and the integration test (`tests/acceptance.rs`) drive the same
//! in-process, no-network planted-violation scenario.

pub mod acceptance;
pub mod build_demo;
pub mod corpus_cmd;
/// `camerata cost-regression`: compare two inspections' cost per 1K counted lines (no model).
pub mod cost_regression_cmd;
/// `camerata count <repo>`: the inspection's own file filter as a size count (no model).
pub mod count_cmd;
pub mod deploy_demo;
pub mod eval_cmd;
pub mod fleet_support;
pub mod gate_probe;
pub mod http_cmd;
pub mod inspect_cmd;
/// W6 / I2: `camerata integrity-gate` — evaluate a persisted scan ledger and exit non-zero
/// when anything that should have run did not. See the module doc comment.
pub mod integrity_gate_cmd;
pub mod live_demo;
pub mod maintenance_demo;
pub mod po_demo;
pub mod projects_live;
/// `camerata release-gate <zip>`: the export release gate over an existing product zip.
pub mod release_gate_cmd;
pub mod worktracker_demo;
pub mod worktracker_live;
