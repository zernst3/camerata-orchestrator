//! `gate-probe` — the #14 end-to-end gate-loop GO/NO-GO on a story.
//!
//! The probe itself lives in `camerata_fleet::gate_probe` so BOTH this CLI command and the
//! server's `/api/gate-probe` endpoint (surfaced in the Governed Development screen) run the
//! exact same deterministic loop. This module just re-exports it for the `gate-probe` subcommand.

pub use camerata_fleet::gate_probe::{run_gate_probe, GateProbeResult};
