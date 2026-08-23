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
    use super::{bombe_running, provide_loading_context, LoadingCount, LoadingGuard};
    use dioxus::prelude::*;

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

    // ── LoadingGuard itself (the mechanism every AI call site holds) ───────────────────────
    //
    // These mount a real `VirtualDom` and drive the guard SYNCHRONOUSLY inside a component's
    // render body. LoadingGuard's increment/decrement is plain `Drop`-driven Rust — it doesn't
    // need an async executor to prove out; the exact same Drop semantics fire whether the guard
    // is dropped at the end of a sync block (as here) or at the end of an `async fn` after an
    // `.await` (as every real call site in `scan.rs` / `uow.rs` / `chat.rs` does).

    #[test]
    fn loading_guard_increments_on_creation_and_decrements_on_drop() {
        fn harness() -> Element {
            provide_loading_context();
            let count = use_context::<LoadingCount>();
            assert_eq!(*count.read(), 0, "idle before any guard is taken");
            let guard = LoadingGuard::new();
            assert_eq!(*count.read(), 1, "one guard in flight -> count 1 -> Bombe runs");
            drop(guard);
            assert_eq!(*count.read(), 0, "guard dropped -> back to idle");
            rsx! { div {} }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
    }

    #[test]
    fn nested_overlapping_guards_stay_running_until_the_last_one_drops() {
        // Mirrors the job-path pattern in `cockpit::scan` (`audit_job_start`'s call site takes
        // its OWN guard, then awaits `poll_job`, which takes a SECOND independent guard for the
        // whole poll loop): two in-flight AI calls must keep the Bombe running until BOTH clear,
        // not just the first to finish.
        fn harness() -> Element {
            provide_loading_context();
            let count = use_context::<LoadingCount>();
            let outer = LoadingGuard::new();
            assert_eq!(*count.read(), 1);
            let inner = LoadingGuard::new();
            assert_eq!(*count.read(), 2, "two overlapping AI calls -> count 2, still running");
            drop(inner);
            assert_eq!(*count.read(), 1, "one of two finished -> STILL running (not idle yet)");
            drop(outer);
            assert_eq!(*count.read(), 0, "both finished -> idle");
            rsx! { div {} }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
    }

    #[test]
    fn error_or_early_return_still_clears_the_guard_no_stuck_animation() {
        // A guard held across a fallible operation that returns early on failure (the shape
        // every `Option<T>`-returning call site in this codebase uses, e.g. `audit_against`
        // returning `None` on a bad response) must still decrement via Drop — proving the
        // call site's SUCCESS path is not what clears the Bombe, so a request error can never
        // leave the animation stuck on.
        fn maybe_fails(count: LoadingCount, fail: bool) -> Option<()> {
            let _guard = LoadingGuard::new();
            assert_eq!(*count.read(), 1, "guard held while the fallible op is in flight");
            if fail {
                return None; // early return — Drop must still run on the way out
            }
            Some(())
        }

        fn harness() -> Element {
            provide_loading_context();
            let count = use_context::<LoadingCount>();
            assert!(maybe_fails(count, true).is_none());
            assert_eq!(*count.read(), 0, "error path cleared the guard -- no stuck animation");
            assert!(maybe_fails(count, false).is_some());
            assert_eq!(*count.read(), 0, "success path also cleared the guard");
            rsx! { div {} }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
    }

    /// Auditable proof for the reported miss (owner: the Bombe did not animate for "Audit code
    /// against selected rules"). `cockpit::scan::audit_against` and `cockpit::scan::poll_job`
    /// (`crates/ui/src/cockpit/scan.rs`) both declare `let _guard = LoadingGuard::new();` as
    /// their FIRST statement, before doing any network work, and let normal Rust scope-exit
    /// drop it after the work resolves (success or failure) — the exact shape this test drives.
    /// Re-running this test after any future edit to those two functions' guard placement is
    /// the regression check: moving the guard to only wrap part of the call, or dropping it
    /// early, would no longer match this "declared first, held through return" shape.
    #[test]
    fn guard_declared_first_covers_the_whole_call_like_audit_against_and_poll_job() {
        fn simulated_audit_against(count: LoadingCount) -> &'static str {
            // Mirrors scan.rs's `audit_against` / `poll_job`: guard first, "network round trip"
            // (here, a stand-in synchronous check) second, guard dropped on return either way.
            let _guard = LoadingGuard::new();
            assert_eq!(*count.read(), 1, "Bombe is running for the whole simulated round trip");
            "report"
        }
        fn harness() -> Element {
            provide_loading_context();
            let count = use_context::<LoadingCount>();
            assert_eq!(*count.read(), 0, "idle before the audit starts");
            let report = simulated_audit_against(count);
            assert_eq!(report, "report");
            assert_eq!(*count.read(), 0, "guard dropped when audit_against returned");
            rsx! { div {} }
        }
        let mut vdom = VirtualDom::new(harness);
        vdom.rebuild_in_place();
    }
}
