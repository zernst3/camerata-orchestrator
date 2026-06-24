//! `LivenessTracker` — a pure, std-only, thread-safe stall-detection primitive.
//!
//! Tracks the most-recent activity on a unit of work (a scan job, a dev run, a dep
//! audit, etc.) and answers two questions: "how long has it been idle?" and "has it
//! crossed the stall threshold?"
//!
//! # Design goals
//!
//! - **Zero tokio/async deps** — lives in `camerata-core`, which is dep-light by
//!   design. The async helpers (mtime probe, output-line heartbeat) live in
//!   `camerata-agent`.
//! - **Thread-safe** — `Arc<AtomicU64>` for the timestamp; `Arc<Mutex<String>>` for
//!   the optional progress label. Clones cheaply (Arc bumps).
//! - **Pure stall math** — `idle_ms` and `is_stalled` are deterministic given the
//!   inputs; no hidden state. Callers supply `now_ms` so tests can control time.
//!
//! # Usage pattern
//!
//! ```text
//! let t = LivenessTracker::new();          // start from "now"
//! t.record_progress("clippy");             // fired on stdout line / mtime advance
//! t.idle_ms(now_ms())                     // → ms since last tick
//! t.is_stalled(120_000, now_ms())         // → true once past threshold
//! ```

use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};

/// A shared, cheap-to-clone handle to a liveness tracker.
///
/// Internally uses `Arc` for both the timestamp and the optional progress label, so
/// cloning this struct is O(1) and all clones share the same state.
#[derive(Clone, Debug)]
pub struct LivenessTracker {
    /// Epoch-milliseconds of the last recorded activity. Uses `u64` (saturates around
    /// year 584 million — not a practical limit). `AtomicU64` so `tick` is lock-free.
    last_activity_ms: Arc<AtomicU64>,
    /// Human-readable label of the most-recent progress event (e.g. "clippy",
    /// "stdout line 42"). `None` until the first `record_progress` call.
    last_label: Arc<Mutex<Option<String>>>,
}

impl LivenessTracker {
    /// Create a new tracker. `last_activity_ms` is initialised to the current wall
    /// clock so a fresh tracker is NOT stalled.
    #[must_use]
    pub fn new() -> Self {
        Self {
            last_activity_ms: Arc::new(AtomicU64::new(now_ms())),
            last_label: Arc::new(Mutex::new(None)),
        }
    }

    /// Create a tracker with an explicit starting timestamp (test helper).
    #[must_use]
    pub fn with_initial_ms(initial_ms: u64) -> Self {
        Self {
            last_activity_ms: Arc::new(AtomicU64::new(initial_ms)),
            last_label: Arc::new(Mutex::new(None)),
        }
    }

    /// Record a heartbeat tick without a label. Updates `last_activity_ms` to now.
    pub fn tick(&self) {
        self.last_activity_ms.store(now_ms(), Ordering::Relaxed);
    }

    /// Record a heartbeat with a human-readable progress label. Updates both
    /// `last_activity_ms` and `last_label`.
    pub fn record_progress(&self, label: impl Into<String>) {
        self.last_activity_ms.store(now_ms(), Ordering::Relaxed);
        if let Ok(mut guard) = self.last_label.lock() {
            *guard = Some(label.into());
        }
    }

    /// How many milliseconds has this tracker been idle relative to `now_ms`?
    ///
    /// Returns `0` when `now_ms` is at or before `last_activity_ms` (i.e. the clock
    /// moved backwards or the caller passed a stale `now_ms`).
    pub fn idle_ms(&self, now_ms: u64) -> u64 {
        now_ms.saturating_sub(self.last_activity_ms.load(Ordering::Relaxed))
    }

    /// Returns `true` when `idle_ms(now_ms) > threshold_ms`.
    ///
    /// Strictly greater than (not `>=`) so a threshold of 0 always returns `false`
    /// unless the clock has genuinely advanced past the last tick.
    pub fn is_stalled(&self, threshold_ms: u64, now_ms: u64) -> bool {
        self.idle_ms(now_ms) > threshold_ms
    }

    /// Snapshot the last progress label. Returns `None` until the first
    /// `record_progress` call.
    #[must_use]
    pub fn last_label(&self) -> Option<String> {
        self.last_label.lock().ok()?.clone()
    }

    /// Read the raw `last_activity_ms` epoch value. Useful for bridging into
    /// the existing `JobMeta.last_activity_ms` / `Run.last_activity_ms` fields.
    #[must_use]
    pub fn last_activity_ms(&self) -> u64 {
        self.last_activity_ms.load(Ordering::Relaxed)
    }
}

impl Default for LivenessTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Return the current epoch in milliseconds as a `u64`. Saturates on overflow (not a
/// practical concern for wall-clock timestamps).
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

// ─── tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_tracker_is_not_idle() {
        let t = LivenessTracker::new();
        // Immediately after creation the idle should be 0 or near-0.
        let now = now_ms();
        // Allow a 5-second fudge for extremely slow test infrastructure.
        assert!(
            t.idle_ms(now) < 5_000,
            "fresh tracker should have near-zero idle, got {}ms",
            t.idle_ms(now)
        );
    }

    #[test]
    fn tick_resets_idle() {
        // Start with a very old timestamp so idle is large.
        let t = LivenessTracker::with_initial_ms(0);
        let now = now_ms();
        // Idle should be huge (epoch).
        assert!(t.idle_ms(now) > 1_000, "expected large idle before tick");

        // Tick resets to now.
        t.tick();
        let now2 = now_ms();
        assert!(
            t.idle_ms(now2) < 5_000,
            "after tick idle should be near-zero, got {}ms",
            t.idle_ms(now2)
        );
    }

    #[test]
    fn record_progress_resets_idle_and_stores_label() {
        let t = LivenessTracker::with_initial_ms(0);
        assert_eq!(t.last_label(), None);

        t.record_progress("clippy");
        let now = now_ms();
        assert!(t.idle_ms(now) < 5_000, "idle should be near-zero after record_progress");
        assert_eq!(t.last_label(), Some("clippy".to_string()));

        // Label updates on subsequent calls.
        t.record_progress("ruff");
        assert_eq!(t.last_label(), Some("ruff".to_string()));
    }

    #[test]
    fn is_stalled_true_only_past_threshold() {
        // Fix the timestamp at 1_000ms (1 second past epoch, arbitrary).
        let t = LivenessTracker::with_initial_ms(1_000);

        // Now = 1_000ms → idle = 0 → NOT stalled at any threshold > 0.
        assert!(!t.is_stalled(120_000, 1_000));

        // Now = 2_000ms → idle = 1_000ms → NOT stalled at 120_000ms threshold.
        assert!(!t.is_stalled(120_000, 2_000));

        // Now = 122_001ms → idle = 121_001ms → STALLED (> 120_000ms).
        assert!(t.is_stalled(120_000, 122_001));

        // Threshold 0 is special: idle of 0 should NOT stall (strictly >).
        assert!(!t.is_stalled(0, 1_000));

        // Idle of 1 DOES stall at threshold 0.
        assert!(t.is_stalled(0, 1_001));
    }

    #[test]
    fn clone_shares_state() {
        let t1 = LivenessTracker::with_initial_ms(0);
        let t2 = t1.clone();

        t1.record_progress("shared");
        // t2 should see the same label and a near-zero idle.
        assert_eq!(t2.last_label(), Some("shared".to_string()));
        let now = now_ms();
        assert!(t2.idle_ms(now) < 5_000, "clone should share the reset timestamp");
    }
}
