//! Fleet-support helpers: re-exported from `camerata_fleet` for backward
//! compatibility within the CLI crate.
//!
//! All items that previously lived here now live in `camerata-fleet`. This
//! module is kept as a thin re-export layer so that `build_demo.rs` and
//! `po_demo.rs` can keep their existing `use crate::fleet_support::...`
//! imports unchanged while both demos transition to calling
//! `camerata_fleet::build_from_plan` at the high level.

pub use camerata_fleet::{
    governed_role, locate_gateway_bin, run_cargo, scaffold_crate, tail_lines, CargoOutcome,
    NoopChecks, DEFAULT_CORPUS_PATH, FLEET_DOMAINS,
};
