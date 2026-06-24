//! `camerata-liveness` — thread-safe stall detection primitives.
//!
//! This is a **leaf crate**: no other camerata crate depends on it (it has none of them
//! as dependencies), so it can be adopted by any crate in the workspace without
//! introducing dependency cycles.
//!
//! # What lives here
//!
//! - [`LivenessTracker`] — pure, std-only, thread-safe stall-detection primitive. Tracks
//!   the most-recent activity on a unit of work and answers "how long has it been idle?"
//!   and "has it crossed the stall threshold?". Moved from `camerata-core` so `core`
//!   stays zero-dep on tokio.
//!
//! - [`HeartbeatFn`] — the shared callback type fired per stdout line / per mtime
//!   advance. Moved from `camerata-agent` so `camerata-checks` can adopt liveness
//!   without depending on the full agent crate.
//!
//! - [`newest_mtime`] / [`spawn_mtime_probe`] / [`MTIME_PROBE_INTERVAL`] /
//!   [`MTIME_PROBE_MAX_DURATION`] — the async mtime-probe helpers. Moved from
//!   `camerata-agent::liveness`.

pub mod tracker;
pub use tracker::LivenessTracker;

pub mod probe;
pub use probe::{
    newest_mtime, spawn_mtime_probe, HeartbeatFn, MTIME_PROBE_INTERVAL, MTIME_PROBE_MAX_DURATION,
};
