//! The review timer and the per-row "reviewed" record (code viewer step 7a; work order
//! `WORK_ORDER_REVIEW_TIME.md` item 2 as ruled on by Zach on 2026-10-23).
//!
//! INTERNAL ONLY. Nothing in this module reaches a client export. The timings and the reviewed
//! record are saved in the onboarding draft, shown in the internal stats view
//! ([`crate::review_stats`]) and used for the time-left estimate ([`eta`]); the export request
//! (`export_product_zip`) carries the disposition map and the report options and not this log, and
//! the server's release-gate tests inspect every client artifact for the field names listed in
//! `camerata_api_types::review_timing`.
//!
//! # Pure, with the clock passed in
//!
//! Nothing here reads a clock. Every event takes a [`Moment`]: a monotonic millisecond count (which
//! the idle rule and the accounting use, so a system clock change can neither add nor remove time)
//! and a wall-clock millisecond count (which only stamps `opened_at_ms` / `reviewed_at_ms`). Tests
//! build moments by hand.
//!
//! # The rules (Zach's ruling)
//!
//! * The timer starts by itself when a group is opened or a finding is acted on ([`ReviewTimer::open_group`],
//!   [`ReviewTimer::decided`]).
//! * After [`IDLE_LIMIT_MS`] (3 minutes) with no key or click it pauses, and **the idle stretch is
//!   not counted**: time is credited only as the gap between one activity and the next, and a gap
//!   that reaches the limit is credited as nothing, so the three minutes before the pause are never
//!   in the total. (Credit is made when the next activity arrives, so a tick can never have
//!   counted time that is later discovered to be idle.)
//! * It pauses whenever the window is not focused, and counts nothing for that stretch.
//! * A manual pause always wins: activity and focus never resume it. Only [`ReviewTimer::resume`]
//!   (the strip's button) does.
//! * An idle pause **is** resumed automatically by the next key or click ("automatic as much as it
//!   can"), provided a group has been opened. An unfocused pause is resumed by the window gaining
//!   focus, unless the timer was idle before it lost focus.
//!
//! # What is recorded
//!
//! Per group (a rule group, keyed by repo and rule id so it does not move when the Queue is
//! re-cut): when it was first opened, its active milliseconds, and whether every Queue row of it
//! is decided. Per row: who reviewed it (the local operator: the app has no sign-in, so this is the
//! fixed [`LOCAL_OPERATOR`]), when, and the active time attributed to that decision (the active
//! time since the previous decision, divided across the rows of a batch decision). The time-left
//! estimate reads the per-row samples.
//!
//! The timer observes decisions and never writes one: `apply_rows` stays the only writer of
//! dispositions, and calls [`ReviewTimer::decided`] / [`ReviewTimer::undecided`] after it has.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::triage::{finding_key, finding_state, Disposition, FindingView, TriageState};

/// No key or click for this long pauses the timer (Zach: 3 minutes).
pub const IDLE_LIMIT_MS: u64 = 3 * 60 * 1000;

/// Who the per-row record names. The app has no sign-in and no notion of an operator identity, and
/// inventing one would put a made-up name in the data, so every row names the one local operator.
pub const LOCAL_OPERATOR: &str = "operator";

/// The time-left estimate shows nothing before this many decisions carry measured time (work order
/// B.7): it never invents a number.
pub const ETA_MIN_DECISIONS: usize = 20;

/// The estimate uses the median of the most recent this many decisions.
pub const ETA_WINDOW: usize = 50;

/// An instant, passed in by the caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Moment {
    /// Monotonic milliseconds since an arbitrary start (the timer's accounting and idle rule).
    pub mono_ms: u64,
    /// Wall-clock milliseconds since the Unix epoch (only for the `*_at_ms` stamps).
    pub wall_ms: u64,
}

impl Moment {
    pub fn new(mono_ms: u64, wall_ms: u64) -> Self {
        Self { mono_ms, wall_ms }
    }
}

/// Why the timer is not counting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PauseReason {
    /// The reviewer pressed Pause. Only Resume undoes it.
    Manual,
    /// No key or click for [`IDLE_LIMIT_MS`], or nothing has started it yet.
    Idle,
    /// The Camerata window is not focused.
    Unfocused,
}

/// What the timer is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimerState {
    Running,
    Paused(PauseReason),
}

// ── The saved record ─────────────────────────────────────────────────────────────────────────

/// One rule group's timing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupTiming {
    /// Wall-clock ms the group was first opened (or first acted on).
    #[serde(default)]
    pub opened_at_ms: u64,
    /// Active milliseconds spent on the group.
    #[serde(default)]
    pub active_ms: u64,
    /// Every Queue row of the group is decided. Refreshed from the dispositions
    /// ([`ReviewLog::sync_groups`]) whenever the draft is saved.
    #[serde(default)]
    pub group_reviewed: bool,
}

/// One row's reviewed record.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowReview {
    /// Who decided it ([`LOCAL_OPERATOR`]).
    #[serde(default)]
    pub reviewed_by: String,
    /// Wall-clock ms of the decision.
    #[serde(default)]
    pub reviewed_at_ms: u64,
    /// The active milliseconds attributed to this decision.
    #[serde(default)]
    pub review_decision_ms: u64,
}

/// The saved review log: the part of the timer that persists with the draft. Internal only.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewLog {
    /// Per rule group, keyed by [`group_key`].
    #[serde(default)]
    pub review_groups: BTreeMap<String, GroupTiming>,
    /// Per row, keyed by [`finding_key`]. Only decided rows have an entry: undoing a decision, or
    /// moving a row back to Unresolved, removes it.
    #[serde(default)]
    pub review_rows: BTreeMap<String, RowReview>,
}

/// The key of the rule group a row belongs to: repo and rule id. Independent of the Queue's order
/// preset, so a group's time does not move when the Queue is re-cut.
pub fn group_key(repo: &str, rule_id: &str) -> String {
    format!("{repo}\u{1f}{rule_id}")
}

/// The repo and rule id back out of a [`group_key`].
pub fn split_group_key(key: &str) -> (&str, &str) {
    key.split_once('\u{1f}').unwrap_or(("", key))
}

/// The group a finding belongs to.
pub fn group_key_of(f: &FindingView) -> String {
    group_key(&f.repo, &f.rule_id)
}

/// For every rule group present in `rows`, how many of its rows are still Unresolved.
pub fn undecided_by_group(
    rows: &[FindingView],
    dispositions: &HashMap<String, Disposition>,
) -> HashMap<String, usize> {
    let mut out: HashMap<String, usize> = HashMap::new();
    for f in rows {
        let undecided = finding_state(dispositions, f) == TriageState::Unresolved;
        *out.entry(group_key_of(f)).or_insert(0) += usize::from(undecided);
    }
    out
}

impl ReviewLog {
    /// Set each group's reviewed flag from the dispositions: reviewed when none of its rows is
    /// undecided. Groups with no rows in `undecided` keep their flag.
    pub fn sync_groups(&mut self, undecided: &HashMap<String, usize>) {
        for (key, timing) in &mut self.review_groups {
            if let Some(n) = undecided.get(key) {
                timing.group_reviewed = *n == 0;
            }
        }
    }

    /// Total active milliseconds over every group.
    pub fn total_active_ms(&self) -> u64 {
        self.review_groups.values().map(|g| g.active_ms).sum()
    }

    /// The decision times that carry measured time, oldest first. A decision with no measured
    /// active time (made while the timer was paused) says nothing about pace, so it is left out.
    pub fn decision_samples(&self) -> Vec<u64> {
        let mut rows: Vec<(&u64, &String, u64)> = self
            .review_rows
            .iter()
            .filter(|(_, r)| r.review_decision_ms > 0)
            .map(|(k, r)| (&r.reviewed_at_ms, k, r.review_decision_ms))
            .collect();
        rows.sort();
        rows.into_iter().map(|(_, _, ms)| ms).collect()
    }

    /// The estimate for `undecided` rows left, from this log's decisions.
    pub fn eta(&self, undecided: usize) -> Option<Duration> {
        eta(undecided, &self.decision_samples())
    }
}

// ── The estimate (B.7) ───────────────────────────────────────────────────────────────────────

/// Time left at the reviewer's pace: the median active time of the most recent [`ETA_WINDOW`]
/// decisions times the undecided rows. `None` until [`ETA_MIN_DECISIONS`] decisions exist, so it
/// never invents a number. `recent_active_ms` is oldest first; only its tail is used.
pub fn eta(undecided: usize, recent_active_ms: &[u64]) -> Option<Duration> {
    if recent_active_ms.len() < ETA_MIN_DECISIONS {
        return None;
    }
    let from = recent_active_ms.len().saturating_sub(ETA_WINDOW);
    let mut window: Vec<u64> = recent_active_ms[from..].to_vec();
    window.sort_unstable();
    let mid = window.len() / 2;
    let median = if window.len() % 2 == 1 {
        window[mid]
    } else {
        (window[mid - 1] + window[mid]) / 2
    };
    Some(Duration::from_millis(
        median.saturating_mul(undecided as u64),
    ))
}

/// `3h 10m`, `42m`, `0m`: a duration in hours and whole minutes, rounded to the nearest minute.
pub fn short_duration(ms: u64) -> String {
    let minutes = (ms + 30_000) / 60_000;
    let (h, m) = (minutes / 60, minutes % 60);
    if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    }
}

/// The strip's words: `about 3h 10m left at your pace`.
pub fn eta_text(left: Duration) -> String {
    let ms = u64::try_from(left.as_millis()).unwrap_or(u64::MAX);
    if ms < 30_000 {
        "less than a minute left at your pace".to_string()
    } else {
        format!("about {} left at your pace", short_duration(ms))
    }
}

// ── The timer ────────────────────────────────────────────────────────────────────────────────

/// What the window tells the timer. The Dioxus layer turns the webview's key, click, focus and
/// blur events into these; the timer's reaction to each is [`ReviewTimer::apply`], tested here
/// with no window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockEvent {
    /// A key or a click.
    Activity,
    /// The window gained (`true`) or lost (`false`) focus, or became visible or hidden.
    Focus(bool),
}

/// One decided row, as the timer is told about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowRef {
    /// [`finding_key`] of the row.
    pub row_key: String,
    /// [`group_key_of`] the row.
    pub group_key: String,
}

impl RowRef {
    pub fn of(f: &FindingView) -> Self {
        Self {
            row_key: finding_key(f),
            group_key: group_key_of(f),
        }
    }
}

/// What the strip shows about the timer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimerStripView {
    /// `Review timer running`, `Review timer paused (window not focused)`, ...
    pub label: String,
    /// The button's word: `Pause` or `Resume`.
    pub button: &'static str,
    /// Counting right now.
    pub running: bool,
}

/// The timer: the saved [`ReviewLog`] plus the session state that is not saved (whether it is
/// paused, and why).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewTimer {
    log: ReviewLog,
    manual: bool,
    focused: bool,
    /// Not counting for want of activity. True until a group is opened or a finding is acted on.
    idle: bool,
    /// Monotonic ms of the last activity (or of the last credit, which is also an activity).
    last_activity_ms: u64,
    current_group: Option<String>,
    /// Active ms credited since the last decision, to be attributed to the next one.
    since_decision_ms: u64,
}

impl Default for ReviewTimer {
    fn default() -> Self {
        Self::with_log(ReviewLog::default())
    }
}

impl ReviewTimer {
    /// A timer over a restored log. Not running: it starts when a group is opened.
    pub fn with_log(log: ReviewLog) -> Self {
        Self {
            log,
            manual: false,
            focused: true,
            idle: true,
            last_activity_ms: 0,
            current_group: None,
            since_decision_ms: 0,
        }
    }

    /// The saved record.
    pub fn log(&self) -> &ReviewLog {
        &self.log
    }

    /// Replace the saved record (a draft restored after the timer was created), keeping the
    /// session state.
    pub fn set_log(&mut self, log: ReviewLog) {
        self.log = log;
    }

    /// What the timer is doing. A manual pause outranks an unfocused window, which outranks idle.
    pub fn state(&self) -> TimerState {
        if self.manual {
            TimerState::Paused(PauseReason::Manual)
        } else if !self.focused {
            TimerState::Paused(PauseReason::Unfocused)
        } else if self.idle {
            TimerState::Paused(PauseReason::Idle)
        } else {
            TimerState::Running
        }
    }

    fn running(&self) -> bool {
        self.state() == TimerState::Running
    }

    /// Whether a group has been opened or a finding acted on, so there is something to time.
    pub fn started(&self) -> bool {
        self.current_group.is_some()
    }

    /// The group time is being credited to.
    pub fn current_group(&self) -> Option<&str> {
        self.current_group.as_deref()
    }

    /// Whether the strip's button should offer Resume (and so a click resumes) rather than Pause.
    pub fn resume_offered(&self) -> bool {
        self.manual || (self.focused && self.idle && self.started())
    }

    /// True when a tick would change the state (the idle limit has been reached). Lets the caller
    /// write its signal only when something changes.
    pub fn needs_tick(&self, now: Moment) -> bool {
        self.running() && now.mono_ms.saturating_sub(self.last_activity_ms) >= IDLE_LIMIT_MS
    }

    /// Apply the idle rule. Returns whether the state changed. Credits nothing: the stretch since
    /// the last activity is the idle stretch and is not counted.
    pub fn tick(&mut self, now: Moment) -> bool {
        let changed = self.needs_tick(now);
        if changed {
            self.idle = true;
        }
        changed
    }

    /// Credit the gap since the last activity to the current group, when the timer is running and
    /// the gap is under the limit. The caller has applied the idle rule first.
    fn credit(&mut self, now: u64) {
        if !self.running() {
            return;
        }
        let gap = now.saturating_sub(self.last_activity_ms);
        if gap < IDLE_LIMIT_MS {
            if let Some(group) = self.current_group.clone() {
                self.log.review_groups.entry(group).or_default().active_ms += gap;
            }
            self.since_decision_ms += gap;
        }
        self.last_activity_ms = now;
    }

    /// Begin counting from `now`, unless a manual pause or an unfocused window forbids it.
    fn start_running(&mut self, now: u64) {
        if !self.manual && self.focused {
            self.idle = false;
            self.last_activity_ms = now;
        }
    }

    /// A key or click. Counts the time since the previous activity, or resumes an idle timer.
    /// Returns whether the state changed.
    pub fn activity(&mut self, now: Moment) -> bool {
        let before = self.state();
        self.tick(now);
        if self.running() {
            self.credit(now.mono_ms);
        } else if self.idle && self.started() {
            // The idle stretch is not counted: counting restarts from this key or click.
            self.start_running(now.mono_ms);
        }
        self.state() != before
    }

    /// Open (or move to) a group: starts the timer, and time from here on is the group's. Time up
    /// to this moment is credited to the group left. Returns whether the state changed.
    pub fn open_group(&mut self, key: &str, now: Moment) -> bool {
        let before = self.state();
        self.tick(now);
        self.credit(now.mono_ms);
        self.log
            .review_groups
            .entry(key.to_string())
            .or_insert_with(|| GroupTiming {
                opened_at_ms: now.wall_ms,
                ..GroupTiming::default()
            });
        self.current_group = Some(key.to_string());
        self.start_running(now.mono_ms);
        self.state() != before
    }

    /// React to a window event. Returns whether the timer's state changed.
    pub fn apply(&mut self, event: ClockEvent, now: Moment) -> bool {
        match event {
            ClockEvent::Activity => self.activity(now),
            ClockEvent::Focus(focused) => self.focus_changed(focused, now),
        }
    }

    /// The manual Pause. Counts what ran up to now, then stops until [`ReviewTimer::resume`].
    pub fn pause(&mut self, now: Moment) {
        self.tick(now);
        self.credit(now.mono_ms);
        self.manual = true;
    }

    /// The manual Resume (also what the button does on an idle pause). Counts from now.
    pub fn resume(&mut self, now: Moment) {
        self.manual = false;
        if self.focused && self.started() {
            self.start_running(now.mono_ms);
        }
    }

    /// The strip button: Resume when it offers Resume, else Pause.
    pub fn toggle(&mut self, now: Moment) {
        if self.resume_offered() {
            self.resume(now);
        } else {
            self.pause(now);
        }
    }

    /// The window gained or lost focus. Losing it counts what ran up to now and stops; gaining it
    /// starts the clock again from now (the unfocused stretch is not counted), and leaves the
    /// timer idle if it was idle before. Returns whether the state changed.
    pub fn focus_changed(&mut self, focused: bool, now: Moment) -> bool {
        if focused == self.focused {
            return false;
        }
        let before = self.state();
        self.tick(now);
        if focused {
            self.focused = true;
            self.last_activity_ms = now.mono_ms;
        } else {
            self.credit(now.mono_ms);
            self.focused = false;
        }
        self.state() != before
    }

    /// Rows were just decided (the dispositions are already written; the timer only observes).
    /// Starts the timer, records each row as reviewed by the operator, and attributes the active
    /// time since the previous decision to them, split evenly across the batch.
    pub fn decided(&mut self, rows: &[RowRef], now: Moment) {
        if rows.is_empty() {
            return;
        }
        self.tick(now);
        self.credit(now.mono_ms);
        for row in rows {
            self.log
                .review_groups
                .entry(row.group_key.clone())
                .or_insert_with(|| GroupTiming {
                    opened_at_ms: now.wall_ms,
                    ..GroupTiming::default()
                });
        }
        if self.current_group.is_none() {
            self.current_group = Some(rows[0].group_key.clone());
        }
        self.start_running(now.mono_ms);
        let each = self.since_decision_ms / rows.len() as u64;
        self.since_decision_ms = 0;
        for row in rows {
            self.log.review_rows.insert(
                row.row_key.clone(),
                RowReview {
                    reviewed_by: LOCAL_OPERATOR.to_string(),
                    reviewed_at_ms: now.wall_ms,
                    review_decision_ms: each,
                },
            );
        }
    }

    /// Rows are no longer decided (an undo, or a move back to Unresolved): they are not reviewed.
    /// The time already spent stays counted: it was spent.
    pub fn undecided(&mut self, row_keys: &[String]) {
        for key in row_keys {
            self.log.review_rows.remove(key);
        }
    }

    /// The strip's view of the timer.
    pub fn strip_view(&self) -> TimerStripView {
        let label = match self.state() {
            TimerState::Running => "Review timer running",
            TimerState::Paused(PauseReason::Manual) => "Review timer paused",
            TimerState::Paused(PauseReason::Unfocused) => {
                "Review timer paused (window not focused)"
            }
            TimerState::Paused(PauseReason::Idle) if self.started() => {
                "Review timer paused (no key or click for 3 minutes)"
            }
            TimerState::Paused(PauseReason::Idle) => "Review timer starts when you open a group",
        };
        TimerStripView {
            label: label.to_string(),
            button: if self.resume_offered() {
                "Resume"
            } else {
                "Pause"
            },
            running: self.running(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_api_types::review_timing::{GENERIC_KEYS, INTERNAL_ONLY_KEYS};

    /// A fake clock: milliseconds from a fixed start, wall time a fixed offset later.
    fn at(mono_ms: u64) -> Moment {
        Moment::new(mono_ms, 1_700_000_000_000 + mono_ms)
    }

    const A: &str = "me/api\u{1f}RULE-A";
    const B: &str = "me/api\u{1f}RULE-B";

    fn row(n: usize, group: &str) -> RowRef {
        RowRef {
            row_key: format!("row-{n}"),
            group_key: group.to_string(),
        }
    }

    fn active(timer: &ReviewTimer, group: &str) -> u64 {
        timer
            .log()
            .review_groups
            .get(group)
            .map_or(0, |g| g.active_ms)
    }

    #[test]
    fn a_new_timer_is_not_running_and_nothing_resumes_it_until_a_group_is_opened() {
        let mut t = ReviewTimer::default();
        assert_eq!(t.state(), TimerState::Paused(PauseReason::Idle));
        assert!(!t.started());
        // A stray key or click is not a review: there is no group to credit.
        t.activity(at(1_000));
        assert_eq!(t.state(), TimerState::Paused(PauseReason::Idle));
        assert_eq!(t.log().total_active_ms(), 0);
        assert_eq!(
            t.strip_view().label,
            "Review timer starts when you open a group"
        );
        assert_eq!(t.strip_view().button, "Pause");
    }

    #[test]
    fn opening_a_group_starts_it_and_records_when_it_was_opened() {
        let mut t = ReviewTimer::default();
        assert!(t.open_group(A, at(5_000)));
        assert_eq!(t.state(), TimerState::Running);
        assert_eq!(t.current_group(), Some(A));
        assert_eq!(t.log().review_groups[A].opened_at_ms, at(5_000).wall_ms);
        assert!(!t.log().review_groups[A].group_reviewed);
        // Opening it again does not move the opened-at stamp.
        t.open_group(A, at(9_000));
        assert_eq!(t.log().review_groups[A].opened_at_ms, at(5_000).wall_ms);
    }

    #[test]
    fn time_between_activities_is_credited_to_the_group_that_was_open() {
        let mut t = ReviewTimer::default();
        t.open_group(A, at(0));
        t.activity(at(10_000));
        t.activity(at(25_000));
        assert_eq!(active(&t, A), 25_000);
        // Moving to B credits the time up to the move to A, then B starts at zero.
        t.open_group(B, at(40_000));
        assert_eq!(active(&t, A), 40_000);
        assert_eq!(active(&t, B), 0);
        t.activity(at(50_000));
        assert_eq!(active(&t, B), 10_000);
        assert_eq!(t.log().total_active_ms(), 50_000);
    }

    #[test]
    fn three_minutes_without_a_key_or_click_pauses_and_the_idle_stretch_is_not_counted() {
        let mut t = ReviewTimer::default();
        t.open_group(A, at(0));
        t.activity(at(60_000));
        assert_eq!(active(&t, A), 60_000);
        // Just under the limit: still running, and the next activity counts the whole gap.
        assert!(!t.needs_tick(at(60_000 + IDLE_LIMIT_MS - 1)));
        // At the limit the timer pauses, crediting nothing for the stretch.
        assert!(t.needs_tick(at(60_000 + IDLE_LIMIT_MS)));
        assert!(t.tick(at(60_000 + IDLE_LIMIT_MS)));
        assert_eq!(t.state(), TimerState::Paused(PauseReason::Idle));
        assert_eq!(active(&t, A), 60_000, "the idle stretch is not counted");
        // A long time later, a key resumes it and counts from that key.
        assert!(t.activity(at(1_000_000)));
        assert_eq!(t.state(), TimerState::Running);
        assert_eq!(active(&t, A), 60_000);
        t.activity(at(1_005_000));
        assert_eq!(active(&t, A), 65_000);
    }

    #[test]
    fn an_activity_that_arrives_after_the_limit_with_no_tick_between_counts_nothing_for_the_gap() {
        // The tick is a convenience for showing the pause; the rule must hold without it.
        let mut t = ReviewTimer::default();
        t.open_group(A, at(0));
        t.activity(at(30_000));
        t.activity(at(30_000 + IDLE_LIMIT_MS + 5_000));
        assert_eq!(
            active(&t, A),
            30_000,
            "the gap reached the limit: not counted"
        );
        assert_eq!(t.state(), TimerState::Running, "and the key resumed it");
        t.activity(at(30_000 + IDLE_LIMIT_MS + 8_000));
        assert_eq!(active(&t, A), 33_000);
    }

    #[test]
    fn a_gap_just_under_the_limit_is_counted_in_full() {
        let mut t = ReviewTimer::default();
        t.open_group(A, at(0));
        t.activity(at(IDLE_LIMIT_MS - 1));
        assert_eq!(active(&t, A), IDLE_LIMIT_MS - 1);
        assert_eq!(t.state(), TimerState::Running);
    }

    #[test]
    fn a_manual_pause_counts_to_the_pause_and_is_not_resumed_by_activity_or_focus() {
        let mut t = ReviewTimer::default();
        t.open_group(A, at(0));
        t.pause(at(20_000));
        assert_eq!(t.state(), TimerState::Paused(PauseReason::Manual));
        assert_eq!(active(&t, A), 20_000);
        assert_eq!(t.strip_view().button, "Resume");
        // Keys, clicks, a decision, opening another group and a focus round trip all leave it.
        t.activity(at(30_000));
        t.open_group(B, at(40_000));
        t.decided(&[row(1, B)], at(50_000));
        t.focus_changed(false, at(60_000));
        t.focus_changed(true, at(70_000));
        assert_eq!(t.state(), TimerState::Paused(PauseReason::Manual));
        assert_eq!(
            t.log().total_active_ms(),
            20_000,
            "nothing counted while paused"
        );
        // Only Resume does, and it counts from then.
        t.resume(at(80_000));
        assert_eq!(t.state(), TimerState::Running);
        t.activity(at(85_000));
        assert_eq!(active(&t, B), 5_000);
        assert_eq!(t.strip_view().button, "Pause");
    }

    #[test]
    fn the_button_resumes_an_idle_pause_and_pauses_a_running_one() {
        let mut t = ReviewTimer::default();
        t.open_group(A, at(0));
        t.tick(at(IDLE_LIMIT_MS));
        assert_eq!(t.state(), TimerState::Paused(PauseReason::Idle));
        assert_eq!(t.strip_view().button, "Resume");
        t.toggle(at(IDLE_LIMIT_MS + 1_000));
        assert_eq!(t.state(), TimerState::Running);
        assert_eq!(t.strip_view().button, "Pause");
        t.toggle(at(IDLE_LIMIT_MS + 4_000));
        assert_eq!(t.state(), TimerState::Paused(PauseReason::Manual));
        assert_eq!(active(&t, A), 3_000);
        t.toggle(at(IDLE_LIMIT_MS + 9_000));
        assert_eq!(t.state(), TimerState::Running);
    }

    #[test]
    fn pausing_before_anything_is_open_keeps_the_next_group_from_starting_it() {
        let mut t = ReviewTimer::default();
        assert_eq!(t.strip_view().button, "Pause");
        t.toggle(at(0));
        assert_eq!(t.state(), TimerState::Paused(PauseReason::Manual));
        t.open_group(A, at(1_000));
        assert_eq!(t.state(), TimerState::Paused(PauseReason::Manual));
        t.toggle(at(2_000));
        assert_eq!(t.state(), TimerState::Running);
    }

    #[test]
    fn losing_focus_pauses_and_counts_up_to_the_loss_and_nothing_after() {
        let mut t = ReviewTimer::default();
        t.open_group(A, at(0));
        assert!(t.focus_changed(false, at(30_000)));
        assert_eq!(t.state(), TimerState::Paused(PauseReason::Unfocused));
        assert_eq!(active(&t, A), 30_000);
        assert_eq!(
            t.strip_view().label,
            "Review timer paused (window not focused)"
        );
        // Keys that somehow arrive while unfocused count for nothing.
        t.activity(at(60_000));
        assert_eq!(active(&t, A), 30_000);
        // Focus back: running again, counting from the return, not from the loss.
        assert!(t.focus_changed(true, at(600_000)));
        assert_eq!(t.state(), TimerState::Running);
        t.activity(at(604_000));
        assert_eq!(active(&t, A), 34_000);
        // A repeated event is not a change.
        assert!(!t.focus_changed(true, at(605_000)));
    }

    #[test]
    fn a_timer_that_was_idle_when_the_window_lost_focus_is_still_idle_when_it_returns() {
        let mut t = ReviewTimer::default();
        t.open_group(A, at(0));
        t.tick(at(IDLE_LIMIT_MS));
        t.focus_changed(false, at(IDLE_LIMIT_MS + 1_000));
        assert_eq!(t.state(), TimerState::Paused(PauseReason::Unfocused));
        t.focus_changed(true, at(IDLE_LIMIT_MS + 5_000));
        assert_eq!(t.state(), TimerState::Paused(PauseReason::Idle));
        t.activity(at(IDLE_LIMIT_MS + 6_000));
        assert_eq!(t.state(), TimerState::Running);
    }

    /// The pure reaction to the webview's events: blur pauses, focus resumes, a key or click
    /// counts. (That the real window sends them is a manual check.)
    #[test]
    fn window_events_map_to_the_timer_reactions() {
        let mut t = ReviewTimer::default();
        t.open_group(A, at(0));
        assert!(t.apply(ClockEvent::Focus(false), at(10_000)));
        assert_eq!(t.state(), TimerState::Paused(PauseReason::Unfocused));
        assert!(
            !t.apply(ClockEvent::Focus(false), at(11_000)),
            "a repeat is not a change"
        );
        assert!(t.apply(ClockEvent::Focus(true), at(50_000)));
        assert_eq!(t.state(), TimerState::Running);
        assert!(
            !t.apply(ClockEvent::Activity, at(52_000)),
            "counting, no state change"
        );
        assert_eq!(active(&t, A), 10_000 + 2_000);
    }

    #[test]
    fn a_window_that_lost_focus_before_any_group_was_open_does_not_start_on_return() {
        let mut t = ReviewTimer::default();
        t.focus_changed(false, at(0));
        t.focus_changed(true, at(10_000));
        assert_eq!(t.state(), TimerState::Paused(PauseReason::Idle));
        assert!(!t.started());
    }

    #[test]
    fn a_decision_starts_the_timer_records_the_row_and_the_group_it_was_in() {
        let mut t = ReviewTimer::default();
        t.decided(&[row(1, A)], at(8_000));
        assert_eq!(t.state(), TimerState::Running);
        assert_eq!(t.current_group(), Some(A));
        let r = &t.log().review_rows["row-1"];
        assert_eq!(r.reviewed_by, LOCAL_OPERATOR);
        assert_eq!(r.reviewed_at_ms, at(8_000).wall_ms);
        assert_eq!(r.review_decision_ms, 0, "no measured time before the start");
        assert_eq!(t.log().review_groups[A].opened_at_ms, at(8_000).wall_ms);
    }

    #[test]
    fn each_decision_is_given_the_active_time_since_the_one_before() {
        let mut t = ReviewTimer::default();
        t.open_group(A, at(0));
        t.decided(&[row(1, A)], at(12_000));
        t.decided(&[row(2, A)], at(20_000));
        assert_eq!(t.log().review_rows["row-1"].review_decision_ms, 12_000);
        assert_eq!(t.log().review_rows["row-2"].review_decision_ms, 8_000);
    }

    #[test]
    fn a_batch_decision_splits_the_time_across_its_rows() {
        let mut t = ReviewTimer::default();
        t.open_group(A, at(0));
        t.decided(&[row(1, A), row(2, A), row(3, B), row(4, B)], at(40_000));
        for n in 1..=4 {
            assert_eq!(
                t.log().review_rows[&format!("row-{n}")].review_decision_ms,
                10_000
            );
        }
        // The batch touched group B without opening it: B exists, opened now.
        assert_eq!(t.log().review_groups[B].opened_at_ms, at(40_000).wall_ms);
        assert_eq!(t.log().total_active_ms(), 40_000);
    }

    #[test]
    fn idle_time_is_not_attributed_to_the_next_decision() {
        let mut t = ReviewTimer::default();
        t.open_group(A, at(0));
        t.decided(&[row(1, A)], at(5_000));
        // A long break, then the next decision 7 seconds after coming back.
        t.decided(&[row(2, A)], at(5_000 + 30 * 60_000));
        assert_eq!(
            t.log().review_rows["row-2"].review_decision_ms,
            0,
            "the break is not time on this decision"
        );
        t.decided(&[row(3, A)], at(5_000 + 30 * 60_000 + 7_000));
        assert_eq!(t.log().review_rows["row-3"].review_decision_ms, 7_000);
    }

    #[test]
    fn an_undone_decision_is_no_longer_reviewed_but_its_time_stays_spent() {
        let mut t = ReviewTimer::default();
        t.open_group(A, at(0));
        t.decided(&[row(1, A), row(2, A)], at(10_000));
        t.undecided(&["row-1".to_string()]);
        assert!(!t.log().review_rows.contains_key("row-1"));
        assert!(t.log().review_rows.contains_key("row-2"));
        assert_eq!(active(&t, A), 10_000);
    }

    #[test]
    fn a_decision_with_no_rows_changes_nothing() {
        let mut t = ReviewTimer::default();
        t.decided(&[], at(1_000));
        assert_eq!(t, ReviewTimer::default());
    }

    // ── the group flag ──

    fn finding(n: usize, rule: &str) -> FindingView {
        serde_json::from_value(serde_json::json!({
            "repo": "me/api", "path": "src/a.rs", "line": n, "rule_id": rule,
            "severity": "high", "snippet": format!("code {n}"), "detail": "d",
            "status": "active", "kind": "defect"
        }))
        .expect("fixture")
    }

    #[test]
    fn a_group_is_reviewed_when_none_of_its_rows_is_undecided() {
        let rows = vec![
            finding(1, "RULE-A"),
            finding(2, "RULE-A"),
            finding(3, "RULE-B"),
        ];
        let mut disp: HashMap<String, Disposition> = HashMap::new();
        let mut t = ReviewTimer::default();
        t.open_group(&group_key("me/api", "RULE-A"), at(0));
        t.open_group(&group_key("me/api", "RULE-B"), at(1_000));
        let mut log = t.log().clone();
        log.sync_groups(&undecided_by_group(&rows, &disp));
        assert!(log.review_groups.values().all(|g| !g.group_reviewed));

        for f in &rows[..2] {
            disp.insert(
                finding_key(f),
                Disposition {
                    state: TriageState::Confirmed,
                    ..Disposition::default()
                },
            );
        }
        log.sync_groups(&undecided_by_group(&rows, &disp));
        assert!(log.review_groups[&group_key("me/api", "RULE-A")].group_reviewed);
        assert!(!log.review_groups[&group_key("me/api", "RULE-B")].group_reviewed);
        // A row moved back to Unresolved un-reviews the group.
        disp.remove(&finding_key(&rows[0]));
        log.sync_groups(&undecided_by_group(&rows, &disp));
        assert!(!log.review_groups[&group_key("me/api", "RULE-A")].group_reviewed);
    }

    #[test]
    fn a_group_key_round_trips_and_a_bare_rule_id_has_no_repo() {
        let key = group_key("me/api", "RULE-A");
        assert_eq!(split_group_key(&key), ("me/api", "RULE-A"));
        assert_eq!(split_group_key("RULE-A"), ("", "RULE-A"));
    }

    // ── persistence ──

    fn populated() -> ReviewLog {
        let mut t = ReviewTimer::default();
        t.open_group(A, at(0));
        t.decided(&[row(1, A)], at(9_000));
        let mut log = t.log().clone();
        log.sync_groups(&HashMap::from([(A.to_string(), 0)]));
        log
    }

    #[test]
    fn the_log_round_trips_through_json() {
        let log = populated();
        let json = serde_json::to_string(&log).expect("serializes");
        let back: ReviewLog = serde_json::from_str(&json).expect("reads");
        assert_eq!(back, log);
    }

    #[test]
    fn an_old_or_partial_log_reads_with_defaults() {
        let back: ReviewLog = serde_json::from_str("{}").expect("empty");
        assert_eq!(back, ReviewLog::default());
        let back: ReviewLog =
            serde_json::from_str(r#"{"review_groups":{"g":{"active_ms":5}}}"#).expect("partial");
        assert_eq!(back.review_groups["g"].active_ms, 5);
        assert!(!back.review_groups["g"].group_reviewed);
        // A field a future build added is ignored.
        let back: ReviewLog =
            serde_json::from_str(r#"{"review_groups":{},"from_the_future":1}"#).expect("future");
        assert_eq!(back, ReviewLog::default());
    }

    /// The draft field reads through the lenient helper: a log of an unexpected shape is an empty
    /// log and the rest of the draft survives.
    #[test]
    fn a_log_of_the_wrong_shape_in_a_draft_is_an_empty_log() {
        #[derive(Deserialize)]
        struct Draft {
            #[serde(default, deserialize_with = "crate::lenient::deserialize_or_default")]
            review_log: ReviewLog,
            kept: u32,
        }
        for json in [
            r#"{"review_log":"later","kept":3}"#,
            r#"{"review_log":[1],"kept":3}"#,
            r#"{"review_log":{"review_groups":{"g":{"active_ms":"x"}}},"kept":3}"#,
            r#"{"kept":3}"#,
        ] {
            let d: Draft = serde_json::from_str(json).expect(json);
            assert_eq!(d.review_log, ReviewLog::default(), "{json}");
            assert_eq!(d.kept, 3);
        }
        let d: Draft = serde_json::from_str(
            r#"{"review_log":{"review_groups":{"g":{"active_ms":9}}},"kept":3}"#,
        )
        .expect("good");
        assert_eq!(d.review_log.review_groups["g"].active_ms, 9);
    }

    /// Every key the log serializes is on the deny-list the server's export tests scan for (or is
    /// one of the generic words), and every listed log key is really emitted: a field added here
    /// without being listed fails, and a stale listing fails.
    #[test]
    fn every_key_the_log_writes_is_on_the_internal_only_deny_list() {
        fn keys(v: &serde_json::Value, out: &mut std::collections::BTreeSet<String>) {
            match v {
                serde_json::Value::Object(map) => {
                    for (k, v) in map {
                        out.insert(k.clone());
                        keys(v, out);
                    }
                }
                serde_json::Value::Array(items) => items.iter().for_each(|i| keys(i, out)),
                _ => {}
            }
        }
        let mut written = std::collections::BTreeSet::new();
        // The map KEYS are group and row keys, not field names: walk the values only.
        let value = serde_json::to_value(populated()).expect("value");
        for (field, map) in value.as_object().expect("object") {
            written.insert(field.clone());
            for entry in map.as_object().expect("map").values() {
                keys(entry, &mut written);
            }
        }
        for key in &written {
            assert!(
                INTERNAL_ONLY_KEYS.contains(&key.as_str()) || GENERIC_KEYS.contains(&key.as_str()),
                "the log writes `{key}`, which is not in camerata_api_types::review_timing"
            );
        }
        for key in [
            "review_groups",
            "review_rows",
            "opened_at_ms",
            "active_ms",
            "group_reviewed",
            "reviewed_by",
            "reviewed_at_ms",
            "review_decision_ms",
        ] {
            assert!(written.contains(key), "{key} is listed but not written");
        }
        // And the key the draft saves it under is listed too.
        assert!(INTERNAL_ONLY_KEYS.contains(&camerata_api_types::review_timing::REVIEW_LOG_KEY));
    }

    // ── the estimate ──

    #[test]
    fn the_estimate_shows_nothing_before_twenty_decisions() {
        assert_eq!(eta(100, &[]), None);
        assert_eq!(eta(100, &[30_000; ETA_MIN_DECISIONS - 1]), None);
        assert!(eta(100, &[30_000; ETA_MIN_DECISIONS]).is_some());
    }

    #[test]
    fn the_estimate_is_the_median_of_the_last_fifty_times_the_rows_left() {
        // 60 samples: 10 old slow ones that must fall out of the window, then 50 recent.
        let mut samples = vec![600_000u64; 10];
        samples.extend((1..=50u64).map(|n| n * 1_000)); // median of 1000..=50000 is 25500
        let left = eta(40, &samples).expect("enough decisions");
        assert_eq!(left, Duration::from_millis(25_500 * 40));
        // Odd count: the middle one.
        assert_eq!(
            eta(10, &[1_000, 2_000, 9_000].repeat(7)[..21]),
            Some(Duration::from_millis(2_000 * 10))
        );
        // Nothing left is a zero estimate, not a missing one.
        assert_eq!(eta(0, &[5_000; 20]), Some(Duration::ZERO));
    }

    #[test]
    fn the_estimate_reads_like_the_work_order() {
        let ten_minutes_past_three_hours = Duration::from_secs(3 * 3600 + 10 * 60);
        assert_eq!(
            eta_text(ten_minutes_past_three_hours),
            "about 3h 10m left at your pace"
        );
        assert_eq!(
            eta_text(Duration::from_secs(42 * 60)),
            "about 42m left at your pace"
        );
        assert_eq!(
            eta_text(Duration::from_secs(10)),
            "less than a minute left at your pace"
        );
        assert!(!eta_text(ten_minutes_past_three_hours).contains('\u{2014}'));
        assert_eq!(short_duration(0), "0m");
        assert_eq!(short_duration(59 * 60_000 + 40_000), "1h 0m");
    }

    /// B.7 check 1: the estimate is absent before 20 decisions and within 10 percent of the
    /// median formula on a scripted sequence, driven through the real timer on a fake clock.
    #[test]
    fn b7_scripted_review_has_no_estimate_until_twenty_decisions_then_tracks_the_median() {
        let mut t = ReviewTimer::default();
        let total_rows = 300usize;
        t.open_group(A, at(0));
        let mut now = 0u64;
        let mut seen_first = None;
        for n in 1..=60usize {
            // 20s per decision for the first 30, then 10s: the window must follow the recent pace.
            now += if n <= 30 { 20_000 } else { 10_000 };
            t.decided(&[row(n, A)], at(now));
            let undecided = total_rows - n;
            let estimate = t.log().eta(undecided);
            if n < ETA_MIN_DECISIONS {
                assert_eq!(estimate, None, "decision {n}");
            } else if seen_first.is_none() {
                seen_first = Some(n);
            }
            if n == 25 {
                // All 25 samples are 20s: 275 rows at 20s.
                let want = 20_000.0 * 275.0;
                let got = estimate.expect("estimate").as_millis() as f64;
                assert!((got - want).abs() / want < 0.10, "{got} vs {want}");
            }
            if n == 60 {
                // The last 50 are 20 slow ones (decisions 11 to 30) and 30 fast: the median is
                // 10s, so 240 rows at 10s.
                let want = 10_000.0 * 240.0;
                let got = estimate.expect("estimate").as_millis() as f64;
                assert!((got - want).abs() / want < 0.10, "{got} vs {want}");
            }
        }
        assert_eq!(seen_first, Some(ETA_MIN_DECISIONS));
    }

    #[test]
    fn a_decision_made_while_paused_carries_no_pace_and_is_left_out_of_the_estimate() {
        let mut log = ReviewLog::default();
        for n in 0..30u64 {
            log.review_rows.insert(
                format!("r{n}"),
                RowReview {
                    reviewed_by: LOCAL_OPERATOR.to_string(),
                    reviewed_at_ms: n,
                    review_decision_ms: if n < 15 { 0 } else { 8_000 },
                },
            );
        }
        // Only 15 samples carry time: not enough for an estimate, rather than a median of zeros.
        assert_eq!(log.decision_samples().len(), 15);
        assert_eq!(log.eta(100), None);
    }

    #[test]
    fn samples_are_ordered_by_when_they_were_decided_not_by_key() {
        let mut log = ReviewLog::default();
        for (key, at_ms, ms) in [("z", 1u64, 100u64), ("a", 3, 300), ("m", 2, 200)] {
            log.review_rows.insert(
                key.to_string(),
                RowReview {
                    reviewed_by: LOCAL_OPERATOR.to_string(),
                    reviewed_at_ms: at_ms,
                    review_decision_ms: ms,
                },
            );
        }
        assert_eq!(log.decision_samples(), vec![100, 200, 300]);
    }

    #[test]
    fn the_strip_view_has_no_em_dash_and_names_each_state() {
        let mut t = ReviewTimer::default();
        t.open_group(A, at(0));
        let mut labels = vec![t.strip_view().label];
        t.pause(at(1_000));
        labels.push(t.strip_view().label);
        t.resume(at(2_000));
        t.focus_changed(false, at(3_000));
        labels.push(t.strip_view().label);
        t.focus_changed(true, at(4_000));
        t.tick(at(4_000 + IDLE_LIMIT_MS));
        labels.push(t.strip_view().label);
        let unique: std::collections::BTreeSet<_> = labels.iter().collect();
        assert_eq!(unique.len(), 4, "{labels:?}");
        assert!(labels.iter().all(|l| !l.contains('\u{2014}')));
    }
}
