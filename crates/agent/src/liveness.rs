//! Re-exports of liveness primitives from `camerata-liveness`.
//!
//! The implementation has moved to the `camerata-liveness` leaf crate (Phase 1b).
//! This module is preserved so existing `use camerata_agent::liveness::*` paths
//! keep working; callers can also import directly from `camerata_liveness`.

pub use camerata_liveness::{
    newest_mtime, spawn_mtime_probe, HeartbeatFn, MTIME_PROBE_INTERVAL, MTIME_PROBE_MAX_DURATION,
};
