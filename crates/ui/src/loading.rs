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
///
/// Newtype (not a type alias) so Dioxus context can distinguish it from
/// `BombePreview` — both wrap `Signal<bool>` but type aliases resolve to the
/// same type and collide in the context map.
#[derive(Clone, Copy)]
pub struct BombeEnabled(pub Signal<bool>);

/// Transient preview flag: lets Settings fire a Play/Pause preview WITHOUT
/// touching the real loading count or the enabled toggle.
/// Default: `false` (no preview active).
///
/// Newtype (not a type alias) so Dioxus context can distinguish it from
/// `BombeEnabled` — both wrap `Signal<bool>` but type aliases resolve to the
/// same type and collide in the context map.
#[derive(Clone, Copy)]
pub struct BombePreview(pub Signal<bool>);

/// Provide the global loading count + bombe control signals into the Dioxus
/// context.  Call once at the app root before any child that might consume
/// them.
pub fn provide_loading_context() {
    use_context_provider(|| Signal::new(0usize));
    use_context_provider(|| BombeEnabled(Signal::new(true)));   // on by default
    use_context_provider(|| BombePreview(Signal::new(false)));  // no preview
}

/// RAII guard that increments the loading count on creation and decrements
/// it on `Drop`.  Create one at the start of any async op; it will
/// automatically decrement when it falls out of scope (including on early
/// return or panic unwind).
pub struct LoadingGuard {
    // `None` when there is no loading context (no Dioxus runtime, or created before
    // `provide_loading_context()`): the guard degrades to a no-op instead of panicking. This keeps
    // the async helpers that wrap themselves in a guard callable from plain unit tests.
    count: Option<LoadingCount>,
}

impl LoadingGuard {
    /// Increment the in-flight count.  When the loading context is present (provided by
    /// `provide_loading_context()` at an ancestor) this drives the Bombe animation; when it is
    /// absent (e.g. a unit test with no runtime) it is a no-op.
    pub fn new() -> Self {
        // `try_consume_context` still requires an active runtime (it calls
        // `Runtime::with_current_scope`, which panics with no VirtualDom), so gate on
        // `Runtime::try_current()` first. With no runtime — e.g. a plain unit test exercising an
        // async helper that wraps itself in a guard — this degrades to a no-op.
        let count = if dioxus::core::Runtime::try_current().is_some() {
            try_consume_context::<LoadingCount>()
        } else {
            None
        };
        if let Some(mut c) = count {
            c += 1;
        }
        Self { count }
    }
}

impl Drop for LoadingGuard {
    fn drop(&mut self) {
        if let Some(count) = self.count.as_mut() {
            // Saturating: a stray double-drop is silently safe.
            let prev = *count.peek();
            count.set(prev.saturating_sub(1));
        }
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

/// The Bombe's effective running state, as a pure function so it can be unit-tested and stays the
/// single definition shared with [`crate::bombe_bg`].
///
/// The Bombe is RESERVED for genuine AI / heavy work — it is the visual "the machine is doing
/// real thinking" signal, and its gravitas only holds if it is not spent on trivial loads. A
/// [`LoadingGuard`] is therefore created ONLY around AI / long-running operations (chat turns,
/// authoring, investigation/development runs, scans, audits), never around a quick list fetch.
/// The guard's RAII drop is what stops the animation when the work finishes (including at the end
/// of a streamed reply, where the guard lives for the whole stream).
///
/// `running = enabled && (count > 0 || preview)` — animations fire when the Bombe is enabled AND
/// either real AI work is in flight OR Settings is previewing it.
pub fn bombe_running(enabled: bool, count: usize, preview: bool) -> bool {
    enabled && (count > 0 || preview)
}

#[cfg(test)]
mod tests {
    use super::bombe_running;

    #[test]
    fn idle_and_enabled_is_not_running() {
        assert!(!bombe_running(true, 0, false));
    }

    #[test]
    fn in_flight_ai_work_runs_when_enabled() {
        assert!(bombe_running(true, 1, false));
        assert!(bombe_running(true, 3, false));
    }

    #[test]
    fn disabled_never_runs_even_with_work_or_preview() {
        assert!(!bombe_running(false, 5, true));
        assert!(!bombe_running(false, 0, true));
    }

    #[test]
    fn preview_runs_with_no_real_work_when_enabled() {
        assert!(bombe_running(true, 0, true));
    }
}
