//! Global ref-counted loading state.
//!
//! A single `Signal<usize>` in the Dioxus context tracks the number of
//! in-flight operations.  The background Bombe machine watches this count:
//!
//! - count > 0  →  `.bombe-running` class active (animations on, overlay lightens)
//! - count == 0 →  class absent (idle, dark overlay, animations paused)
//!
//! Two additional signals live alongside the count:
//!
//! - `BombeEnabled` — persisted ON/OFF toggle (localStorage key
//!   `camerata.bombe.enabled`).  When OFF, the bombe never animates.
//! - `BombePreview` — transient Play/Pause flag that lets Settings trigger
//!   the animation without touching real loading state.
//!
//! The bombe's effective running state is:
//!   `running = enabled && (count > 0 || preview)`
//!
//! # How to use
//!
//! **At the app root** (done once in `App`):
//! ```ignore
//! crate::loading::provide_loading_context();
//! ```
//!
//! **At any async call site** (wraps ANY awaited work):
//! ```ignore
//! let _guard = crate::loading::LoadingGuard::new();
//! let result = some_long_await.await;
//! // guard drops here → count decrements
//! ```
//!
//! **Critical behaviour (count, NOT toggle):**
//! If two operations overlap — op A starts (count 0→1), op B starts (1→2),
//! op A finishes (2→1) — the machine STAYS running until B also finishes
//! (1→0).  Decrement is saturating so a stray double-drop can never
//! underflow.

use dioxus::prelude::*;

/// The shared loading count type alias.
pub type LoadingCount = Signal<usize>;

/// Global animation enabled/disabled toggle.  Persisted to localStorage
/// under key `camerata.bombe.enabled` by the Settings panel.
/// Default: `true` (animations on).
pub type BombeEnabled = Signal<bool>;

/// Transient preview flag: lets Settings fire a Play/Pause preview WITHOUT
/// touching the real loading count or the enabled toggle.
/// Default: `false` (no preview active).
pub type BombePreview = Signal<bool>;

/// Provide the global loading count + bombe control signals into the Dioxus
/// context.  Call once at the app root before any child that might consume
/// them.
pub fn provide_loading_context() {
    use_context_provider(|| Signal::new(0usize));
    use_context_provider(|| Signal::new(true));   // BombeEnabled — on by default
    use_context_provider(|| Signal::new(false));  // BombePreview — no preview
}

/// RAII guard that increments the loading count on creation and decrements
/// it on `Drop`.  Create one at the start of any async op; it will
/// automatically decrement when it falls out of scope (including on early
/// return or panic unwind).
pub struct LoadingGuard {
    count: LoadingCount,
}

impl LoadingGuard {
    /// Increment the in-flight count.  Requires the loading context to have
    /// been provided by `provide_loading_context()` at some ancestor.
    pub fn new() -> Self {
        let mut count = consume_context::<LoadingCount>();
        count += 1;
        Self { count }
    }
}

impl Drop for LoadingGuard {
    fn drop(&mut self) {
        // Saturating: a stray double-drop is silently safe.
        let prev = *self.count.peek();
        self.count.set(prev.saturating_sub(1));
    }
}

/// Convenience: returns `true` when there is at least one in-flight
/// operation.  The `BombeBg` component uses this to toggle `.bombe-running`.
pub fn is_loading() -> bool {
    match try_consume_context::<LoadingCount>() {
        Some(c) => *c.read() > 0,
        None => false,
    }
}
