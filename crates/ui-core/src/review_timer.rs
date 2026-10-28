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
    /// The active milliseconds spent deciding this row (the sum, if it was decided again).
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
    /// The pace samples behind the time-left estimate: ONE per decision event, the per-row figure
    /// (the active time since the previous decision, divided across the rows of a batch), oldest
    /// first, the last [`ETA_WINDOW`] kept. One per EVENT and not per row, so a 30-row stack
    /// confirm is one decision of the last 50 and not 30 of them.
    #[serde(default)]
    pub review_samples: Vec<u64>,
}

/// The review log as the draft carries it: the log, and, when part of what was saved could not be
/// read, the ORIGINAL saved value, kept and written back verbatim as `review_unreadable`.
///
/// A build that cannot read an entry (or the whole log: a newer build's shape) must not turn
/// hours of pricing data into an empty log and then save that over it. So the log is read entry
/// by entry ([`crate::lenient::read_map`]): every readable entry is kept and only unreadable ones
/// are skipped, and if anything was skipped the whole original value is preserved beside the new
/// log. The preserved copy is written on every later save and never replaced by a later failure
/// (the first, fullest copy wins). `just_failed` is set on the read that found the problem, for
/// the toast; it is not saved.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SavedReviewLog {
    pub log: ReviewLog,
    /// The original value, verbatim, when part of it could not be read (now or earlier).
    pub unreadable: Option<serde_json::Value>,
    /// This read found something it could not read.
    pub just_failed: bool,
}

impl SavedReviewLog {
    /// Read a saved value leniently. Never fails.
    pub fn from_value(value: &serde_json::Value) -> Self {
        if value.is_null() {
            return Self::default();
        }
        let Some(object) = value.as_object() else {
            return Self {
                log: ReviewLog::default(),
                unreadable: Some(value.clone()),
                just_failed: true,
            };
        };
        let (review_groups, bad_groups) = crate::lenient::read_map(object.get("review_groups"));
        let (review_rows, bad_rows) = crate::lenient::read_map(object.get("review_rows"));
        let (review_samples, bad_samples) = match object.get("review_samples") {
            None => (Vec::new(), false),
            Some(v) => match serde_json::from_value::<Vec<u64>>(v.clone()) {
                Ok(samples) => (samples, false),
                Err(_) => (Vec::new(), true),
            },
        };
        let failed = bad_groups || bad_rows || bad_samples;
        let kept = object.get(UNREADABLE_KEY).cloned();
        let unreadable = match kept {
            Some(earlier) => Some(earlier),
            None if failed => {
                let mut original = object.clone();
                original.remove(UNREADABLE_KEY);
                Some(serde_json::Value::Object(original))
            }
            None => None,
        };
        Self {
            log: ReviewLog {
                review_groups,
                review_rows,
                review_samples,
            },
            unreadable,
            just_failed: failed,
        }
    }
}

/// The key the preserved original is saved under, inside the saved log.
const UNREADABLE_KEY: &str = "review_unreadable";

impl<'de> Deserialize<'de> for SavedReviewLog {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(&serde_json::Value::deserialize(
            deserializer,
        )?))
    }
}

impl Serialize for SavedReviewLog {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Wire<'a> {
            #[serde(flatten)]
            log: &'a ReviewLog,
            #[serde(rename = "review_unreadable", skip_serializing_if = "Option::is_none")]
            unreadable: &'a Option<serde_json::Value>,
        }
        Wire {
            log: &self.log,
            unreadable: &self.unreadable,
        }
        .serialize(serializer)
    }
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

    /// The group's entry, created (opened at `wall_ms`) if the log has none. THE ONE place a group
    /// entry is made, so a group is never created with an opened-at of 0.
    pub fn ensure_group(&mut self, key: &str, wall_ms: u64) -> &mut GroupTiming {
        self.review_groups
            .entry(key.to_string())
            .or_insert_with(|| GroupTiming {
                opened_at_ms: wall_ms,
                ..GroupTiming::default()
            })
    }

    /// The decision pace samples, oldest first. A decision with no measured active time (made
    /// while the timer was paused) says nothing about pace and was never recorded.
    pub fn decision_samples(&self) -> Vec<u64> {
        self.review_samples.clone()
    }

    /// Record one decision event's pace, keeping the last [`ETA_WINDOW`].
    fn push_sample(&mut self, per_row_ms: u64) {
        if per_row_ms == 0 {
            return;
        }
        self.review_samples.push(per_row_ms);
        let extra = self.review_samples.len().saturating_sub(ETA_WINDOW);
        self.review_samples.drain(..extra);
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
    fn credit(&mut self, now: Moment) {
        if !self.running() {
            return;
        }
        let gap = now.mono_ms.saturating_sub(self.last_activity_ms);
        if gap < IDLE_LIMIT_MS {
            if let Some(group) = self.current_group.clone() {
                // The entry normally exists; it does not after `set_log` swapped in a log that
                // lacks the open group, and then it is made here, stamped now and not with 0.
                self.log.ensure_group(&group, now.wall_ms).active_ms += gap;
            }
            self.since_decision_ms += gap;
        }
        self.last_activity_ms = now.mono_ms;
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
            self.credit(now);
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
        self.credit(now);
        self.log.ensure_group(key, now.wall_ms);
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
        self.credit(now);
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
            self.credit(now);
            self.focused = false;
        }
        self.state() != before
    }

    /// Rows were just decided (the dispositions are already written; the timer only observes).
    /// Starts the timer, records each row as reviewed by the operator, and attributes the active
    /// time since the previous decision to them, split evenly across the batch.
    ///
    /// The stretch BEFORE the decision is credited first, to the group that was being read (the
    /// one the cursor was in), however far the decided rows are from it. Only after that does the
    /// current group move to the decided rows' group, when they all share one (a bulk-bar
    /// confirm of a group the cursor was not in), so the time that follows is theirs. A decision
    /// that spans several groups leaves the current group alone. A row decided again adds this
    /// decision's time to its `review_decision_ms` (the row cost the sum), while the pace sample
    /// for the estimate is this event's figure alone.
    pub fn decided(&mut self, rows: &[RowRef], now: Moment) {
        if rows.is_empty() {
            return;
        }
        self.tick(now);
        self.credit(now);
        for row in rows {
            self.log.ensure_group(&row.group_key, now.wall_ms);
        }
        let first = &rows[0].group_key;
        if self.current_group.is_none() || rows.iter().all(|r| &r.group_key == first) {
            self.current_group = Some(first.clone());
        }
        self.start_running(now.mono_ms);
        let each = self.since_decision_ms / rows.len() as u64;
        self.since_decision_ms = 0;
        self.log.push_sample(each);
        for row in rows {
            let earlier = self
                .log
                .review_rows
                .get(&row.row_key)
                .map_or(0, |r| r.review_decision_ms);
            self.log.review_rows.insert(
                row.row_key.clone(),
                RowReview {
                    reviewed_by: LOCAL_OPERATOR.to_string(),
                    reviewed_at_ms: now.wall_ms,
                    review_decision_ms: earlier + each,
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
    use camerata_api_types::review_timing::INTERNAL_ONLY_KEYS;

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

    fn saved(json: &str) -> SavedReviewLog {
        serde_json::from_str(json).expect("a saved log never fails to read")
    }

    #[test]
    fn a_clean_saved_log_reads_whole_and_writes_back_identically() {
        let log = populated();
        let wire = serde_json::to_value(SavedReviewLog {
            log: log.clone(),
            ..SavedReviewLog::default()
        })
        .expect("writes");
        assert!(wire.get("review_unreadable").is_none());
        let back: SavedReviewLog = serde_json::from_value(wire.clone()).expect("reads");
        assert_eq!(back.log, log);
        assert!(back.unreadable.is_none() && !back.just_failed);
        assert_eq!(serde_json::to_value(&back).expect("writes"), wire);
        // Absent and null are an empty log, not a failure.
        for json in ["null", "{}"] {
            let read = saved(json);
            assert_eq!(read.log, ReviewLog::default());
            assert!(read.unreadable.is_none() && !read.just_failed, "{json}");
        }
    }

    /// M2: a log whose whole shape is unreadable is an empty log for this session, and the
    /// original is kept and written back verbatim; the next save does not destroy it.
    #[test]
    fn a_log_of_an_unknown_shape_is_kept_verbatim_and_never_overwritten_by_an_empty_one() {
        for json in [r#""later""#, r#"[1,2]"#, r#"7"#, r#"true"#] {
            let read = saved(json);
            assert_eq!(read.log, ReviewLog::default(), "{json}");
            assert!(read.just_failed, "{json}");
            let original: serde_json::Value = serde_json::from_str(json).unwrap();
            assert_eq!(read.unreadable.as_ref(), Some(&original), "{json}");
            // The next save writes the (new, empty) log AND the original, verbatim.
            let written = serde_json::to_value(&read).expect("writes");
            assert_eq!(written["review_unreadable"], original, "{json}");
        }
    }

    /// M2: ONE bad entry costs that entry and nothing else. Every readable entry is kept, the
    /// whole original is preserved verbatim beside them, and the preserved copy survives later
    /// saves (the first copy wins; a later failure does not replace it).
    #[test]
    fn one_bad_entry_loses_only_itself_and_the_original_is_preserved() {
        let json = r#"{
            "review_groups": {
                "good": {"opened_at_ms": 1, "active_ms": 72000000, "group_reviewed": true},
                "bad": {"active_ms": "twenty hours"}
            },
            "review_rows": {"r1": {"reviewed_by": "operator", "reviewed_at_ms": 2, "review_decision_ms": 9}},
            "review_samples": [10, 20]
        }"#;
        let read = saved(json);
        assert!(read.just_failed);
        assert_eq!(read.log.review_groups.len(), 1);
        assert_eq!(read.log.review_groups["good"].active_ms, 72_000_000);
        assert_eq!(read.log.review_rows.len(), 1);
        assert_eq!(read.log.review_samples, vec![10, 20]);
        let original: serde_json::Value = serde_json::from_str(json).unwrap();
        assert_eq!(read.unreadable.as_ref(), Some(&original));

        // Saved, then read again by this same build: the entry that could not be read is still in
        // `review_unreadable`, nothing new fails, and the preserved copy is untouched.
        let written = serde_json::to_value(&read).expect("writes");
        assert_eq!(written["review_unreadable"], original);
        let again: SavedReviewLog = serde_json::from_value(written.clone()).expect("reads");
        assert!(
            !again.just_failed,
            "the bad entry was dropped from the live log"
        );
        assert_eq!(again.unreadable.as_ref(), Some(&original));
        assert_eq!(serde_json::to_value(&again).unwrap(), written);

        // A bad samples list is the same story.
        let read = saved(r#"{"review_groups":{"g":{"active_ms":5}},"review_samples":"x"}"#);
        assert!(read.just_failed && read.unreadable.is_some());
        assert_eq!(read.log.review_groups["g"].active_ms, 5);
    }

    /// Every key the log serializes is on the deny-list the server's export tests scan for (or is
    /// one of the generic words). (That every LISTED key is written by some internal shape is
    /// pinned in `review_stats`, which can see both shapes.)
    #[test]
    fn every_key_the_log_writes_is_on_the_internal_only_deny_list() {
        use crate::test_support::{assert_all_listed, keys_written, LOG_ENTRY_MAPS};
        let wire = serde_json::to_value(SavedReviewLog {
            log: populated(),
            unreadable: Some(serde_json::json!("kept")),
            just_failed: false,
        })
        .expect("value");
        assert_all_listed(&keys_written(&wire, LOG_ENTRY_MAPS), "the saved log");
        assert!(INTERNAL_ONLY_KEYS.contains(&camerata_api_types::review_timing::REVIEW_LOG_KEY));
    }

    /// L4: a log swapped in after a group was opened (the draft restore) lacks the open group;
    /// the next credit makes its entry stamped now, never with an opened-at of 0.
    #[test]
    fn a_group_missing_after_set_log_is_recreated_with_a_real_opened_at() {
        let mut t = ReviewTimer::default();
        t.open_group(A, at(0));
        t.set_log(ReviewLog::default());
        t.activity(at(5_000));
        let timing = &t.log().review_groups[A];
        assert_eq!(timing.active_ms, 5_000);
        assert_eq!(timing.opened_at_ms, at(5_000).wall_ms, "not 0");
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

    /// M3: the estimate's window is the last 50 DECISIONS, not the last 50 rows. 25 single
    /// decisions at 30 s, then a 30-row stack confirm that took 40 s: the batch is one sample
    /// (1.33 s a row) among 26, so the median stays 30 s and the estimate is 30 s a row left.
    #[test]
    fn a_thirty_row_batch_is_one_decision_not_thirty() {
        let mut t = ReviewTimer::default();
        t.open_group(A, at(0));
        let mut now = 0;
        for n in 1..=25 {
            now += 30_000;
            t.decided(&[row(n, A)], at(now));
        }
        now += 40_000;
        let batch: Vec<RowRef> = (100..130).map(|n| row(n, A)).collect();
        t.decided(&batch, at(now));
        let samples = t.log().decision_samples();
        assert_eq!(samples.len(), 26, "25 singles and ONE batch");
        assert_eq!(samples[25], 40_000 / 30, "the per-row figure of the batch");
        assert_eq!(t.log().review_rows.len(), 55, "every row is still recorded");
        let left = t.log().eta(100).expect("26 decisions");
        assert_eq!(left, Duration::from_millis(30_000 * 100));
    }

    #[test]
    fn the_sample_window_keeps_only_the_last_fifty_decisions() {
        let mut t = ReviewTimer::default();
        t.open_group(A, at(0));
        let mut now = 0;
        for n in 1..=70 {
            now += 1_000 * n as u64;
            t.decided(&[row(n, A)], at(now));
        }
        let samples = t.log().decision_samples();
        assert_eq!(samples.len(), ETA_WINDOW);
        assert_eq!(samples[0], 21_000, "decisions 1 to 20 fell out");
        assert_eq!(samples[49], 70_000);
    }

    #[test]
    fn a_decision_made_while_paused_carries_no_pace_and_adds_no_sample() {
        let mut t = ReviewTimer::default();
        t.open_group(A, at(0));
        t.pause(at(0));
        for n in 1..=30 {
            t.decided(&[row(n, A)], at(1_000 + n as u64));
        }
        assert!(t.log().decision_samples().is_empty());
        assert_eq!(t.log().eta(100), None, "not a median of zeros");
        assert_eq!(t.log().review_rows.len(), 30, "but every row is recorded");
    }

    /// L5: deciding a row again adds the new time to the row's total instead of replacing it.
    #[test]
    fn a_row_decided_again_accumulates_its_time() {
        let mut t = ReviewTimer::default();
        t.open_group(A, at(0));
        t.decided(&[row(1, A)], at(10_000));
        t.decided(&[row(1, A)], at(25_000));
        let r = &t.log().review_rows["row-1"];
        assert_eq!(r.review_decision_ms, 25_000, "10 s then 15 s");
        assert_eq!(r.reviewed_at_ms, at(25_000).wall_ms, "the latest decision");
        assert_eq!(t.log().decision_samples(), vec![10_000, 15_000]);
    }

    /// L3: the stretch before a decision stays with the group that was being read; the time after
    /// it is the decided rows' group's (one group), and a multi-group batch moves nothing.
    #[test]
    fn a_decision_credits_the_group_being_read_then_moves_to_the_decided_rows_group() {
        let mut t = ReviewTimer::default();
        t.open_group(A, at(0));
        // The cursor is in A; a bulk confirm of group B arrives after 20 s of reading A.
        t.decided(&[row(1, B), row(2, B)], at(20_000));
        assert_eq!(active(&t, A), 20_000, "the reading was A's");
        assert_eq!(active(&t, B), 0);
        assert_eq!(t.current_group(), Some(B));
        t.activity(at(26_000));
        assert_eq!(active(&t, B), 6_000, "what follows is B's");
        // A batch across groups leaves the current group alone.
        t.decided(&[row(3, A), row(4, B)], at(30_000));
        assert_eq!(t.current_group(), Some(B));
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
