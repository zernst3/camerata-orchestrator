//! The review timer, the Dioxus side (code viewer step 7a). INTERNAL ONLY.
//!
//! Every rule is in `camerata_ui_core::review_timer` (the idle rule, the manual override, what is
//! credited to which group, the per-row record, the time-left estimate) and
//! `camerata_ui_core::review_stats` (the numbers for the internal view). This file owns only what
//! needs a window:
//!
//! * [`ReviewHandle`]: the one timer for the results view, with the few signals the screen reads.
//!   The timer itself sits behind a `RefCell` and not in a signal, because a key or click arrives
//!   about once a second while the reviewer works and a signal would re-render its readers each
//!   time; the signals below change only when what they show changes.
//! * [`use_review_clock`]: the bridge that turns the webview's key, click, focus and blur events
//!   into timer events, and the one-second tick that applies the idle rule.
//! * [`LiveProgressStrip`] (the strip with its time-left text and Pause / Resume button) and
//!   [`ReviewStatsModal`] (the internal stats view).
//!
//! The timer observes decisions; it never writes one. `apply_rows` writes the dispositions and then
//! tells the handle.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use camerata_ui_core::review_stats::{ReviewStats, StatLine, STATS_NOTE, STATS_TITLE};
use camerata_ui_core::review_timer::{
    eta, eta_text, ClockEvent, Moment, ReviewLog, ReviewTimer, RowRef, TimerStripView,
};
use camerata_ui_core::triage::{finding_key, FindingView, TriageCounts, TriageState};
use dioxus::prelude::*;

use super::scan::ProgressStrip;

/// The DOM id of the strip's Pause / Resume button.
pub(super) const REVIEW_TIMER_BUTTON_ID: &str = "camerata-review-timer-toggle";

/// The DOM id of the stats view, so it can take the keyboard when it opens.
pub(super) const REVIEW_STATS_ID: &str = "camerata-review-stats";

// The Pause / Resume button carries `data-review-no-activity` (see `ProgressStrip`), and the
// bridge below does not count a press inside such an element as the reviewer's activity: its own
// press must not first resume an idle timer (as a key or click would) and then have its action
// undo that. A test pins that the strip and the script use the same attribute name.

/// The one review timer for the results view, cheap to clone and compare (props).
#[derive(Clone)]
pub(super) struct ReviewHandle {
    timer: Rc<RefCell<ReviewTimer>>,
    /// What the strip shows about the timer; written only when it changes.
    strip: Signal<TimerStripView>,
    /// The decision times behind the time-left estimate; written only when a decision is made or
    /// undone.
    samples: Signal<Vec<u64>>,
    /// The log as last handed to the draft's auto-save. The draft effect reads THIS, not the
    /// timer, so it fires on decisions, group changes, pauses and every 30 seconds of work, and
    /// not on every key press.
    saved: Signal<ReviewLog>,
    /// The zero of the monotonic clock.
    epoch: Instant,
}

impl PartialEq for ReviewHandle {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.timer, &other.timer)
            && self.strip == other.strip
            && self.samples == other.samples
            && self.saved == other.saved
    }
}

/// Create the results view's timer. Call once, unconditionally, from the results component.
pub(super) fn use_review_handle() -> ReviewHandle {
    let strip = use_signal(|| ReviewTimer::default().strip_view());
    let samples = use_signal(Vec::<u64>::new);
    let saved = use_signal(ReviewLog::default);
    use_hook(move || ReviewHandle {
        timer: Rc::new(RefCell::new(ReviewTimer::default())),
        strip,
        samples,
        saved,
        epoch: Instant::now(),
    })
}

impl ReviewHandle {
    /// Now, on both clocks. The only place the timer's time is read.
    pub(super) fn now(&self) -> Moment {
        let wall = u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or(0);
        let mono = u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX);
        Moment::new(mono, wall)
    }

    /// Bring the signals up to date with the timer, writing only what changed. Returns whether
    /// the strip changed (a state transition), which is when the log is worth saving.
    fn refresh(&self, samples_changed: bool) -> bool {
        let timer = self.timer.borrow();
        let view = timer.strip_view();
        let mut strip = self.strip;
        let changed = *strip.peek() != view;
        if changed {
            strip.set(view);
        }
        if samples_changed {
            let mut samples = self.samples;
            samples.set(timer.log().decision_samples());
        }
        changed
    }

    /// Hand the current log to the draft's auto-save, if it differs from the last one handed over.
    pub(super) fn flush(&self) {
        let log = self.timer.borrow().log().clone();
        let mut saved = self.saved;
        if *saved.peek() != log {
            saved.set(log);
        }
    }

    /// The log as last handed to the draft's auto-save. Reading it subscribes the reader.
    pub(super) fn saved_log(&self) -> ReviewLog {
        self.saved.read().clone()
    }

    /// The live log, for the stats view.
    pub(super) fn log(&self) -> ReviewLog {
        self.timer.borrow().log().clone()
    }

    /// A draft was restored: take its log, keeping the session state.
    pub(super) fn restore(&self, log: ReviewLog) {
        self.timer.borrow_mut().set_log(log);
        self.refresh(true);
        self.flush();
    }

    /// A key, a click, or a focus change.
    pub(super) fn apply(&self, event: ClockEvent) {
        let now = self.now();
        let changed = self.timer.borrow_mut().apply(event, now);
        if self.refresh(false) || changed {
            self.flush();
        }
    }

    /// The once-a-second tick: applies the idle rule, and every 30 seconds saves the log.
    pub(super) fn tick(&self, count: u64) {
        let now = self.now();
        let needs = self.timer.borrow().needs_tick(now);
        if needs {
            self.timer.borrow_mut().tick(now);
            self.refresh(false);
            self.flush();
        } else if count.is_multiple_of(30) {
            self.flush();
        }
    }

    /// The reviewer opened a group (the cursor moved into it, or its stacked view opened).
    pub(super) fn open_group(&self, key: &str) {
        let now = self.now();
        self.timer.borrow_mut().open_group(key, now);
        self.refresh(false);
        self.flush();
    }

    /// `apply_rows` has just written `state` to these rows. A row moved back to Unresolved is no
    /// longer reviewed; anything else is a decision. Observes only: it writes no disposition.
    pub(super) fn decided(&self, rows: &[FindingView], state: TriageState) {
        let now = self.now();
        if state == TriageState::Unresolved {
            let keys: Vec<String> = rows.iter().map(finding_key).collect();
            self.timer.borrow_mut().undecided(&keys);
        } else {
            let refs: Vec<RowRef> = rows.iter().map(RowRef::of).collect();
            self.timer.borrow_mut().decided(&refs, now);
        }
        self.refresh(true);
        self.flush();
    }

    /// An undo has restored these rows to Unresolved.
    pub(super) fn undone(&self, keys: &[String]) {
        self.timer.borrow_mut().undecided(keys);
        self.refresh(true);
        self.flush();
    }

    /// The strip's Pause / Resume button.
    pub(super) fn toggle(&self) {
        let now = self.now();
        self.timer.borrow_mut().toggle(now);
        self.refresh(false);
        self.flush();
    }
}

// ── The window bridge ────────────────────────────────────────────────────────────────────────

/// The page-side half of the bridge: a key or a click (at most one message a second), and the
/// window gaining or losing focus or being hidden. It listens in the capture phase and never
/// stops, prevents or consumes an event, so it cannot take a key from the pane.
///
/// Desktop is Dioxus 0.7 on wry, so the signals are the webview's: `focus` / `blur` on the window
/// and `visibilitychange` on the document, read as `document.hasFocus()` plus visibility. Whether
/// wry delivers them for a real window switch is a manual check (see the step's report).
pub(super) const REVIEW_BRIDGE_JS: &str = r#"
    window.__camerataReviewSend = function (payload) {
        try { dioxus.send(payload); } catch (err) { /* channel mid-reinstall */ }
    };
    var reviewFocused = function () {
        return document.hasFocus() && document.visibilityState === "visible";
    };
    if (!window.__camerataReviewBridgeInstalled) {
        window.__camerataReviewBridgeInstalled = true;
        var lastActivity = 0;
        var activity = function (e) {
            var t = e.target;
            if (t && t.closest && t.closest("[data-review-no-activity]")) return;
            var now = Date.now();
            if (now - lastActivity < 1000) return;
            lastActivity = now;
            window.__camerataReviewSend({ k: "activity" });
        };
        document.addEventListener("keydown", activity, true);
        document.addEventListener("pointerdown", activity, true);
        var focusChanged = function () {
            window.__camerataReviewSend({ k: "focus", v: reviewFocused() });
        };
        window.addEventListener("focus", focusChanged);
        window.addEventListener("blur", focusChanged);
        document.addEventListener("visibilitychange", focusChanged);
    }
    window.__camerataReviewSend({ k: "focus", v: reviewFocused() });
    // Keep this eval (and its dioxus.send channel) alive until it is replaced.
    await new Promise(function () {});
"#;

/// A bridge message as a timer event; `None` for anything else.
pub(super) fn parse_clock_message(msg: &serde_json::Value) -> Option<ClockEvent> {
    match msg.get("k")?.as_str()? {
        "activity" => Some(ClockEvent::Activity),
        "focus" => msg.get("v")?.as_bool().map(ClockEvent::Focus),
        _ => None,
    }
}

/// Start the bridge and the one-second tick for `review`. Both stop when the component unmounts.
/// Call once, unconditionally, from the results component.
pub(super) fn use_review_clock(review: &ReviewHandle) {
    let bridge = review.clone();
    use_future(move || {
        let review = bridge.clone();
        async move {
            loop {
                let mut listener = dioxus::document::eval(REVIEW_BRIDGE_JS);
                // When recv errors the channel died (navigation / reload / eval error): fall out
                // and reinstall to refresh the binding.
                while let Ok(msg) = listener.recv::<serde_json::Value>().await {
                    if let Some(event) = parse_clock_message(&msg) {
                        review.apply(event);
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            }
        }
    });
    let ticker = review.clone();
    use_future(move || {
        let review = ticker.clone();
        async move {
            let mut count = 0u64;
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                count += 1;
                review.tick(count);
            }
        }
    });
}

// ── The strip ────────────────────────────────────────────────────────────────────────────────

/// The progress strip with the review timer's part drawn: the time-left text in the strip's ETA
/// seam, the timer's state, and the Pause / Resume button. Its own component so that the one
/// readers of the timer's signals are this strip and not the whole table.
#[component]
pub(super) fn LiveProgressStrip(
    counts: TriageCounts,
    review: ReviewHandle,
    /// Open the internal stats view.
    on_open_stats: Callback<()>,
    /// The Pause / Resume button was used (the caller gives the keyboard back to the pane).
    #[props(default)]
    on_toggled: Callback<()>,
) -> Element {
    let timer = review.strip.read().clone();
    let left = counts.outstanding();
    // Never an invented number: `eta` is None until 20 decisions carry measured time.
    let estimate = (left > 0)
        .then(|| eta(left, &review.samples.read()))
        .flatten()
        .map(eta_text);
    let toggle = {
        let review = review.clone();
        Callback::new(move |()| {
            review.toggle();
            on_toggled.call(());
        })
    };
    rsx! {
        ProgressStrip {
            counts,
            eta: estimate,
            timer: Some(timer),
            on_toggle_timer: toggle,
            on_open_stats: Some(on_open_stats),
        }
    }
}

// ── The internal stats view ──────────────────────────────────────────────────────────────────

/// One table of the stats view.
#[component]
fn StatsTable(title: String, lines: Vec<StatLine>, by_group: bool) -> Element {
    rsx! {
        h4 { class: "review-stats-head", "{title}" }
        if lines.is_empty() {
            p { class: "section-hint", "Nothing recorded yet." }
        } else {
            table { class: "review-stats-table",
                thead {
                    tr {
                        th { "" }
                        th { "Active time" }
                        th { "Hours" }
                        if by_group {
                            th { "Groups reviewed" }
                        }
                        th { "Rows reviewed" }
                    }
                }
                tbody {
                    for line in lines.iter() {
                        tr { key: "{line.label}",
                            td { "{line.label}" }
                            td { "{line.time_text()}" }
                            td { "{line.active_hours}" }
                            if by_group {
                                td { "{line.groups_reviewed} of {line.groups}" }
                            }
                            td { "{line.rows_reviewed}" }
                        }
                    }
                }
            }
        }
    }
}

/// THE INTERNAL STATS VIEW: review hours per repo, per group type and per severity. Props-only.
/// A data view for pricing; it says on its face that no client sees it.
#[component]
pub(super) fn ReviewStatsModal(
    stats: ReviewStats,
    on_save: Callback<()>,
    on_close: Callback<()>,
) -> Element {
    // Take the keyboard when it opens, so Esc closes it and no key reaches the pane under it.
    use_effect(|| super::viewer::focus_element(REVIEW_STATS_ID));
    let total = stats.total_text();
    let unattributed = stats.unattributed_text();
    rsx! {
        div { class: "rule-modal-overlay", onclick: move |_| on_close.call(()),
            div {
                class: "rule-modal review-stats",
                id: "{REVIEW_STATS_ID}",
                tabindex: "0",
                role: "dialog",
                "aria-label": "{STATS_TITLE}",
                onclick: move |e| e.stop_propagation(),
                onkeydown: move |e| {
                    if e.key() == Key::Escape {
                        e.stop_propagation();
                        on_close.call(());
                    }
                },
                div { class: "rule-modal-head",
                    span { class: "rule-modal-id", "{STATS_TITLE}" }
                    button {
                        class: "rule-modal-close",
                        "aria-label": "Close",
                        onclick: move |_| on_close.call(()),
                        "\u{2715}"
                    }
                }
                p { class: "section-hint", "{STATS_NOTE}" }
                p { class: "review-stats-total",
                    "Total active time: {total} ({stats.total_active_hours} hours). "
                    "Not yet tied to a decision: {unattributed}."
                }
                StatsTable { title: "Per repo", lines: stats.by_repo.clone(), by_group: true }
                StatsTable { title: "Per group type", lines: stats.by_group_type.clone(), by_group: true }
                StatsTable { title: "Per severity", lines: stats.by_severity.clone(), by_group: false }
                div { class: "queue-prompt-actions",
                    button {
                        class: "btn-run",
                        onclick: move |_| on_save.call(()),
                        "Save stats JSON"
                    }
                    button {
                        class: "btn-edit-sm",
                        onclick: move |_| on_close.call(()),
                        "Close"
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_ui_core::review_timer::group_key;
    use camerata_ui_core::triage::TriageState;

    #[test]
    fn bridge_messages_map_to_timer_events_and_nothing_else_does() {
        let p = |s: &str| parse_clock_message(&serde_json::from_str(s).expect("json"));
        assert_eq!(p(r#"{"k":"activity"}"#), Some(ClockEvent::Activity));
        assert_eq!(
            p(r#"{"k":"focus","v":true}"#),
            Some(ClockEvent::Focus(true))
        );
        assert_eq!(
            p(r#"{"k":"focus","v":false}"#),
            Some(ClockEvent::Focus(false))
        );
        for bad in [
            r#"{"k":"focus"}"#,
            r#"{"k":"focus","v":"yes"}"#,
            r#"{"k":"other"}"#,
            r#"{"op":"paste"}"#,
            r#"[]"#,
            r#"null"#,
        ] {
            assert_eq!(p(bad), None, "{bad}");
        }
    }

    /// The bridge only observes: it never stops, prevents or consumes an event, so it cannot take
    /// a key from the pane; it listens in the capture phase; it skips the timer's own button; it
    /// reports focus from the window's focus and blur and the document's visibility.
    #[test]
    fn the_bridge_script_observes_and_never_consumes() {
        for forbidden in [
            "preventDefault",
            "stopPropagation",
            "stopImmediatePropagation",
        ] {
            assert!(!REVIEW_BRIDGE_JS.contains(forbidden), "{forbidden}");
        }
        assert!(REVIEW_BRIDGE_JS.contains(", true);"), "capture phase");
        for event in [
            "keydown",
            "pointerdown",
            "focus",
            "blur",
            "visibilitychange",
        ] {
            assert!(
                REVIEW_BRIDGE_JS.contains(&format!("\"{event}\"")),
                "{event}"
            );
        }
        assert!(REVIEW_BRIDGE_JS.contains("data-review-no-activity"));
        assert!(REVIEW_BRIDGE_JS.contains("document.hasFocus()"));
    }

    /// A log of `n` decisions that each took 30 seconds of active time.
    fn log_of(n: u64) -> ReviewLog {
        let mut log = ReviewLog::default();
        for i in 0..n {
            log.review_rows.insert(
                format!("row-{i}"),
                camerata_ui_core::review_timer::RowReview {
                    reviewed_by: camerata_ui_core::review_timer::LOCAL_OPERATOR.to_string(),
                    reviewed_at_ms: i,
                    review_decision_ms: 30_000,
                },
            );
        }
        log
    }

    #[component]
    fn StripHarness(decisions: u64, undecided: usize) -> Element {
        let review = use_review_handle();
        use_hook({
            let review = review.clone();
            move || review.restore(log_of(decisions))
        });
        rsx! {
            LiveProgressStrip {
                counts: TriageCounts { unresolved: undecided, confirmed: decisions as usize, ..Default::default() },
                review,
                on_open_stats: Callback::new(|()| {}),
            }
        }
    }

    fn strip_html(decisions: u64, undecided: usize) -> String {
        let mut vdom = VirtualDom::new_with_props(
            StripHarness,
            StripHarnessProps {
                decisions,
                undecided,
            },
        );
        vdom.rebuild_in_place();
        dioxus_ssr::render(&vdom)
    }

    /// B.7: the time left appears in the strip's seam as the work order words it, only once 20
    /// decisions carry measured time, and only while rows are left.
    #[test]
    fn the_strip_shows_time_left_only_after_twenty_decisions_and_never_invents_one() {
        // 19 decisions: nothing, however many rows are left.
        let page = strip_html(19, 100);
        assert!(!page.contains("progress-eta"), "{page}");
        assert!(!page.contains("left at your pace"), "{page}");
        // 20 decisions of 30s each, 100 rows left: 50 minutes.
        let page = strip_html(20, 100);
        assert!(page.contains("progress-eta"), "{page}");
        assert!(page.contains("about 50m left at your pace"), "{page}");
        // 400 rows left: 200 minutes.
        assert!(strip_html(30, 400).contains("about 3h 20m left at your pace"));
        // Nothing left to decide: no estimate to give.
        assert!(!strip_html(30, 0).contains("left at your pace"));
        assert!(!strip_html(30, 5).contains('\u{2014}'));
    }

    /// The timer's part of the strip: the state, a native button (so Tab reaches it) that is not
    /// counted as activity, the door to the stats, and no live region and no key binding.
    #[test]
    fn the_strip_carries_the_timer_state_a_tab_reachable_button_and_the_stats_door() {
        let page = strip_html(0, 3);
        assert!(
            page.contains("Review timer starts when you open a group"),
            "{page}"
        );
        assert!(page.contains("<button"), "{page}");
        assert!(
            page.contains("id=\"camerata-review-timer-toggle\""),
            "{page}"
        );
        assert!(page.contains("data-review-no-activity=\"true\""), "{page}");
        assert!(page.contains("Review time</button>"), "{page}");
        assert!(
            !page.contains("tabindex"),
            "a native button is already in the tab order"
        );
        assert!(
            !page.contains("accesskey"),
            "no key of its own: the pane keeps its keys"
        );
        assert!(!page.contains("aria-live"), "{page}");
        assert!(!page.contains('\u{2014}'));
    }

    fn stats() -> ReviewStats {
        use camerata_ui_core::review_timer::ReviewTimer;
        let f: FindingView = serde_json::from_value(serde_json::json!({
            "repo": "me/api", "path": "a.rs", "line": 1, "rule_id": "R-1",
            "severity": "high", "snippet": "x", "detail": "d", "status": "active",
            "kind": "defect"
        }))
        .expect("fixture");
        let mut t = ReviewTimer::default();
        t.open_group(&group_key("me/api", "R-1"), Moment::new(0, 1));
        t.decided(&[RowRef::of(&f)], Moment::new(90_000, 90_001));
        let mut disp = std::collections::HashMap::new();
        disp.insert(
            finding_key(&f),
            camerata_ui_core::triage::Disposition {
                state: TriageState::Confirmed,
                ..Default::default()
            },
        );
        ReviewStats::compute(t.log(), &[f], &disp)
    }

    #[test]
    fn the_stats_view_names_the_three_tables_and_says_it_is_internal() {
        #[component]
        fn Harness(stats: ReviewStats) -> Element {
            rsx! {
                ReviewStatsModal {
                    stats,
                    on_save: Callback::new(|()| {}),
                    on_close: Callback::new(|()| {}),
                }
            }
        }
        let mut vdom = VirtualDom::new_with_props(Harness, HarnessProps { stats: stats() });
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        for needle in [
            "Active review time (internal)",
            "Internal data for pricing",
            "not in any export",
            "Per repo",
            "Per group type",
            "Per severity",
            "me/api",
            "defect",
            "1 of 1",
            "2m",
            "Save stats JSON",
            "role=\"dialog\"",
        ] {
            assert!(html.contains(needle), "missing {needle:?} in\n{html}");
        }
        assert!(!html.contains('\u{2014}'), "{html}");
        // An empty view says so, in each table, and is not a blank hole.
        let mut vdom = VirtualDom::new_with_props(
            Harness,
            HarnessProps {
                stats: ReviewStats::default(),
            },
        );
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert_eq!(html.matches("Nothing recorded yet.").count(), 3, "{html}");
    }
}
