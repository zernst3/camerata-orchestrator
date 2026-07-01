//! Parsing of a routine's human-readable `schedule` string and the "is this routine due right now?"
//! decision the auto-fire scheduler ticks on. Framework-agnostic (RUST-HEADLESS-CORE-1): the server
//! adapter's `auto_fire` loop drives these pure functions; nothing here touches the transport layer.
//!
//! The schedule strings are authored UI-side (`crates/ui/src/routines.rs`):
//!   - `daily HH:MM`
//!   - `weekly Mon,Wed,Fri HH:MM`
//!   - `monthly day 15 HH:MM`
//!   - `once YYYY-MM-DD HH:MM`
//!   - `manual` (or anything unrecognized) — never auto-fires
//!
//! [`is_due`] is intentionally a PURE function of `(schedule, now, last_fired)` so it
//! is deterministic and unit-testable without touching the clock. The scheduler passes
//! the real wall-clock local time; tests pass fixed `NaiveDateTime`s.

use chrono::{Datelike, NaiveDate, NaiveDateTime, Weekday};

/// A parsed routine schedule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Schedule {
    Daily {
        h: u32,
        m: u32,
    },
    Weekly {
        days: Vec<Weekday>,
        h: u32,
        m: u32,
    },
    Monthly {
        day: u32,
        h: u32,
        m: u32,
    },
    Once {
        date: NaiveDate,
        h: u32,
        m: u32,
    },
    /// `manual` or anything we don't recognize: the scheduler never auto-fires it.
    Manual,
}

fn parse_hm(s: &str) -> Option<(u32, u32)> {
    let (h, m) = s.split_once(':')?;
    let h: u32 = h.trim().parse().ok()?;
    let m: u32 = m.trim().parse().ok()?;
    if h < 24 && m < 60 {
        Some((h, m))
    } else {
        None
    }
}

fn parse_weekday(s: &str) -> Option<Weekday> {
    match s.trim() {
        "Mon" => Some(Weekday::Mon),
        "Tue" => Some(Weekday::Tue),
        "Wed" => Some(Weekday::Wed),
        "Thu" => Some(Weekday::Thu),
        "Fri" => Some(Weekday::Fri),
        "Sat" => Some(Weekday::Sat),
        "Sun" => Some(Weekday::Sun),
        _ => None,
    }
}

/// Parse a schedule string. Unrecognized / malformed input maps to [`Schedule::Manual`]
/// (never auto-fires) rather than erroring — a routine with a weird schedule simply
/// won't be auto-run, which is the safe default.
pub fn parse(schedule: &str) -> Schedule {
    let s = schedule.trim();
    let mut parts = s.split_whitespace();
    match parts.next() {
        Some("daily") => match parts.next().and_then(parse_hm) {
            Some((h, m)) => Schedule::Daily { h, m },
            None => Schedule::Manual,
        },
        Some("weekly") => {
            // `weekly Mon,Wed HH:MM`
            let days_tok = parts.next().unwrap_or_default();
            let days: Vec<Weekday> = days_tok.split(',').filter_map(parse_weekday).collect();
            match (days.is_empty(), parts.next().and_then(parse_hm)) {
                (false, Some((h, m))) => Schedule::Weekly { days, h, m },
                _ => Schedule::Manual,
            }
        }
        Some("monthly") => {
            // `monthly day 15 HH:MM`
            if parts.next() != Some("day") {
                return Schedule::Manual;
            }
            let day: Option<u32> = parts.next().and_then(|d| d.parse().ok());
            match (day, parts.next().and_then(parse_hm)) {
                (Some(day), Some((h, m))) if (1..=31).contains(&day) => {
                    Schedule::Monthly { day, h, m }
                }
                _ => Schedule::Manual,
            }
        }
        Some("once") => {
            // `once YYYY-MM-DD HH:MM`
            let date = parts
                .next()
                .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok());
            match (date, parts.next().and_then(parse_hm)) {
                (Some(date), Some((h, m))) => Schedule::Once { date, h, m },
                _ => Schedule::Manual,
            }
        }
        _ => Schedule::Manual,
    }
}

/// The last day of the given (year, month), 1-based — so a `monthly day 31` routine
/// clamps to Feb 28/29 etc. rather than silently never firing.
fn last_day_of_month(year: i32, month: u32) -> u32 {
    // First day of next month, minus one day.
    let (ny, nm) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    NaiveDate::from_ymd_opt(ny, nm, 1)
        .and_then(|d| d.pred_opt())
        .map(|d| d.day())
        .unwrap_or(28)
}

fn at(date: NaiveDate, h: u32, m: u32) -> Option<NaiveDateTime> {
    date.and_hms_opt(h, m, 0)
}

/// The most recent scheduled occurrence at or before `now`, if any. `None` means the
/// routine has no slot in the past yet (e.g. a `once` whose time hasn't arrived).
fn most_recent_slot(sched: &Schedule, now: NaiveDateTime) -> Option<NaiveDateTime> {
    match sched {
        Schedule::Manual => None,
        Schedule::Daily { h, m } => {
            let today = at(now.date(), *h, *m)?;
            if today <= now {
                Some(today)
            } else {
                at(now.date().pred_opt()?, *h, *m)
            }
        }
        Schedule::Weekly { days, h, m } => {
            // Walk backward up to 7 days; the first scheduled weekday whose time is
            // at/before `now` is the most recent slot.
            let mut date = now.date();
            for _ in 0..7 {
                if days.contains(&date.weekday()) {
                    if let Some(slot) = at(date, *h, *m) {
                        if slot <= now {
                            return Some(slot);
                        }
                    }
                }
                date = date.pred_opt()?;
            }
            None
        }
        Schedule::Monthly { day, h, m } => {
            let (y, mo) = (now.year(), now.month());
            let this_day = (*day).min(last_day_of_month(y, mo));
            let this = NaiveDate::from_ymd_opt(y, mo, this_day).and_then(|d| at(d, *h, *m))?;
            if this <= now {
                return Some(this);
            }
            // Previous month.
            let (py, pm) = if mo == 1 { (y - 1, 12) } else { (y, mo - 1) };
            let prev_day = (*day).min(last_day_of_month(py, pm));
            NaiveDate::from_ymd_opt(py, pm, prev_day).and_then(|d| at(d, *h, *m))
        }
        Schedule::Once { date, h, m } => {
            let slot = at(*date, *h, *m)?;
            (slot <= now).then_some(slot)
        }
    }
}

/// The next scheduled occurrence STRICTLY AFTER `now`, if any. `None` for `Manual`/unrecognized,
/// or a `once` whose time has already passed. Symmetric to [`most_recent_slot`]; drives the
/// dashboard's next-fire column and the "due soon" status metric.
fn next_slot(sched: &Schedule, now: NaiveDateTime) -> Option<NaiveDateTime> {
    match sched {
        Schedule::Manual => None,
        Schedule::Daily { h, m } => {
            let today = at(now.date(), *h, *m)?;
            if today > now {
                Some(today)
            } else {
                at(now.date().succ_opt()?, *h, *m)
            }
        }
        Schedule::Weekly { days, h, m } => {
            // Walk forward up to 8 days (so a scheduled weekday whose time today has already
            // passed rolls to the same weekday next week).
            let mut date = now.date();
            for _ in 0..8 {
                if days.contains(&date.weekday()) {
                    if let Some(slot) = at(date, *h, *m) {
                        if slot > now {
                            return Some(slot);
                        }
                    }
                }
                date = date.succ_opt()?;
            }
            None
        }
        Schedule::Monthly { day, h, m } => {
            let (y, mo) = (now.year(), now.month());
            let this_day = (*day).min(last_day_of_month(y, mo));
            let this = NaiveDate::from_ymd_opt(y, mo, this_day).and_then(|d| at(d, *h, *m))?;
            if this > now {
                return Some(this);
            }
            // Next month.
            let (ny, nm) = if mo == 12 { (y + 1, 1) } else { (y, mo + 1) };
            let next_day = (*day).min(last_day_of_month(ny, nm));
            NaiveDate::from_ymd_opt(ny, nm, next_day).and_then(|d| at(d, *h, *m))
        }
        Schedule::Once { date, h, m } => {
            let slot = at(*date, *h, *m)?;
            (slot > now).then_some(slot)
        }
    }
}

/// The next time this routine will fire after `now` (parsing the human schedule string). `None`
/// for a manual / unrecognized schedule, or a one-off already in the past. Pure + deterministic.
pub fn next_fire(schedule: &str, now: NaiveDateTime) -> Option<NaiveDateTime> {
    next_slot(&parse(schedule), now)
}

/// Is this routine due to fire at `now`, given when it last fired?
///
/// Due when there is a scheduled slot at/before `now` that is strictly newer than the
/// last fire (or it has never fired). This makes each slot fire at most once and lets a
/// routine catch up a single missed slot rather than firing every tick.
pub fn is_due(schedule: &str, now: NaiveDateTime, last_fired: Option<NaiveDateTime>) -> bool {
    match most_recent_slot(&parse(schedule), now) {
        Some(slot) => last_fired.is_none_or(|lf| lf < slot),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, mo, d)
            .unwrap()
            .and_hms_opt(h, mi, 0)
            .unwrap()
    }

    // ── next_fire: the next scheduled slot strictly after `now` ────────────────

    #[test]
    fn next_fire_daily_rolls_to_tomorrow_when_today_passed() {
        let now = dt(2026, 6, 29, 10, 0);
        assert_eq!(next_fire("daily 09:00", now).unwrap(), dt(2026, 6, 30, 9, 0));
        assert_eq!(next_fire("daily 14:00", now).unwrap(), dt(2026, 6, 29, 14, 0));
    }

    #[test]
    fn next_fire_monthly_rolls_to_next_month_when_day_passed() {
        let now = dt(2026, 6, 29, 10, 0);
        assert_eq!(next_fire("monthly day 15 09:00", now).unwrap(), dt(2026, 7, 15, 9, 0));
        assert_eq!(next_fire("monthly day 30 09:00", now).unwrap(), dt(2026, 6, 30, 9, 0));
    }

    #[test]
    fn next_fire_once_future_vs_past() {
        let now = dt(2026, 6, 29, 10, 0);
        assert_eq!(next_fire("once 2026-07-04 12:00", now).unwrap(), dt(2026, 7, 4, 12, 0));
        assert!(next_fire("once 2026-06-01 12:00", now).is_none());
    }

    #[test]
    fn next_fire_manual_and_unrecognized_are_none() {
        let now = dt(2026, 6, 29, 10, 0);
        assert!(next_fire("manual", now).is_none());
        assert!(next_fire("garbage input", now).is_none());
    }

    #[test]
    fn next_fire_weekly_same_day_rolls_to_next_week_when_passed() {
        use chrono::{Datelike, Duration, Weekday};
        let now = dt(2026, 6, 29, 10, 0);
        let day = match now.weekday() {
            Weekday::Mon => "Mon",
            Weekday::Tue => "Tue",
            Weekday::Wed => "Wed",
            Weekday::Thu => "Thu",
            Weekday::Fri => "Fri",
            Weekday::Sat => "Sat",
            Weekday::Sun => "Sun",
        };
        // Today at 09:00 already passed -> same weekday next week (+7 days).
        let passed = next_fire(&format!("weekly {day} 09:00"), now).unwrap();
        assert_eq!(passed, (now.date() + Duration::days(7)).and_hms_opt(9, 0, 0).unwrap());
        // Today at 14:00 still ahead -> today.
        let ahead = next_fire(&format!("weekly {day} 14:00"), now).unwrap();
        assert_eq!(ahead, dt(2026, 6, 29, 14, 0));
    }

    #[test]
    fn parses_each_form() {
        assert_eq!(parse("daily 04:00"), Schedule::Daily { h: 4, m: 0 });
        assert_eq!(
            parse("weekly Mon,Wed 09:00"),
            Schedule::Weekly {
                days: vec![Weekday::Mon, Weekday::Wed],
                h: 9,
                m: 0
            }
        );
        assert_eq!(
            parse("monthly day 15 09:30"),
            Schedule::Monthly {
                day: 15,
                h: 9,
                m: 30
            }
        );
        assert_eq!(
            parse("once 2026-06-20 14:00"),
            Schedule::Once {
                date: NaiveDate::from_ymd_opt(2026, 6, 20).unwrap(),
                h: 14,
                m: 0
            }
        );
        // Unrecognized / malformed -> Manual (never auto-fires).
        assert_eq!(parse("manual"), Schedule::Manual);
        assert_eq!(parse("daily 99:99"), Schedule::Manual);
        assert_eq!(parse("weekly 09:00"), Schedule::Manual);
        assert_eq!(parse(""), Schedule::Manual);
    }

    #[test]
    fn daily_fires_once_per_day_with_catch_up() {
        // 2026-06-18 is a Thursday; 10:00 is after the 09:00 slot.
        let now = dt(2026, 6, 18, 10, 0);
        // Never fired -> due (today's 09:00 slot).
        assert!(is_due("daily 09:00", now, None));
        // Already fired today at 09:00 -> not due again until tomorrow.
        assert!(!is_due("daily 09:00", now, Some(dt(2026, 6, 18, 9, 0))));
        // Fired yesterday -> due again (caught up to today's slot).
        assert!(is_due("daily 09:00", now, Some(dt(2026, 6, 17, 9, 0))));
        // Before today's slot, fired yesterday -> due for yesterday's slot still? No:
        // last_fired == yesterday's slot, most-recent slot is yesterday 09:00 -> not due.
        let before = dt(2026, 6, 18, 8, 0);
        assert!(!is_due("daily 09:00", before, Some(dt(2026, 6, 17, 9, 0))));
    }

    #[test]
    fn weekly_only_on_scheduled_days() {
        // Thursday 2026-06-18 10:00, schedule Mon+Wed.
        let thu = dt(2026, 6, 18, 10, 0);
        // Most recent slot is Wed 06-17 09:00; never fired -> due.
        assert!(is_due("weekly Mon,Wed 09:00", thu, None));
        // Fired at Wed's slot -> not due again until next Mon.
        assert!(!is_due(
            "weekly Mon,Wed 09:00",
            thu,
            Some(dt(2026, 6, 17, 9, 0))
        ));
        // On Wed before the time -> most recent slot is Mon 06-15 09:00.
        let wed_early = dt(2026, 6, 17, 8, 0);
        assert!(is_due("weekly Mon,Wed 09:00", wed_early, None));
        assert!(!is_due(
            "weekly Mon,Wed 09:00",
            wed_early,
            Some(dt(2026, 6, 15, 9, 0))
        ));
    }

    #[test]
    fn monthly_clamps_to_last_day() {
        // day 31 in June (30 days) clamps to the 30th.
        let now = dt(2026, 6, 30, 12, 0);
        assert!(is_due("monthly day 31 09:00", now, None));
        assert!(!is_due(
            "monthly day 31 09:00",
            now,
            Some(dt(2026, 6, 30, 9, 0))
        ));
        // Mid-month before the (clamped) day -> previous month's slot.
        let mid = dt(2026, 6, 15, 12, 0);
        assert!(is_due("monthly day 31 09:00", mid, None)); // May 31 slot
    }

    #[test]
    fn once_fires_exactly_once() {
        let target = NaiveDate::from_ymd_opt(2026, 6, 20).unwrap();
        let _ = target;
        // Before the time -> not due.
        assert!(!is_due(
            "once 2026-06-20 14:00",
            dt(2026, 6, 20, 13, 0),
            None
        ));
        // At/after the time, never fired -> due.
        assert!(is_due(
            "once 2026-06-20 14:00",
            dt(2026, 6, 20, 14, 0),
            None
        ));
        assert!(is_due("once 2026-06-20 14:00", dt(2026, 6, 21, 9, 0), None));
        // Once fired, never again.
        assert!(!is_due(
            "once 2026-06-20 14:00",
            dt(2026, 6, 21, 9, 0),
            Some(dt(2026, 6, 20, 14, 0))
        ));
    }

    #[test]
    fn manual_never_fires() {
        assert!(!is_due("manual", dt(2026, 6, 18, 10, 0), None));
        assert!(!is_due("whatever nonsense", dt(2026, 6, 18, 10, 0), None));
    }

    // ── property: next_fire is strictly after `now` (or None) ────────────────
    //
    // Sweeps a range of `now` values for each recurring schedule type and
    // asserts that the returned next-fire time is always > now.

    #[test]
    fn next_fire_daily_always_strictly_after_now() {
        // Sweep every hour of a two-day window.
        for day in [28u32, 29] {
            for hour in 0u32..24 {
                let now = dt(2026, 6, day, hour, 0);
                let nf = next_fire("daily 09:00", now).expect("daily always has a next slot");
                assert!(
                    nf > now,
                    "daily 09:00: next_fire({now}) = {nf} is not strictly after now"
                );
            }
        }
        // Also check at the exact slot time: next_fire at 09:00 must give tomorrow 09:00.
        let now_at_slot = dt(2026, 6, 1, 9, 0);
        let nf = next_fire("daily 09:00", now_at_slot).unwrap();
        assert!(nf > now_at_slot, "next_fire at exact slot time must still be strictly after now");
    }

    #[test]
    fn next_fire_weekly_always_strictly_after_now() {
        // Sweep every hour across a full week.
        for day_offset in 0u32..7 {
            for hour in 0u32..24 {
                // Start on Monday 2026-06-01 and walk through the week.
                use chrono::Duration;
                let base = NaiveDate::from_ymd_opt(2026, 6, 1).unwrap();
                let date = base + Duration::days(day_offset as i64);
                let now = date.and_hms_opt(hour, 0, 0).unwrap();
                let nf = next_fire("weekly Mon,Wed,Fri 09:00", now)
                    .expect("weekly always has a next slot within 8 days");
                assert!(
                    nf > now,
                    "weekly Mon,Wed,Fri 09:00: next_fire({now}) = {nf} is not strictly after now"
                );
            }
        }
    }

    #[test]
    fn next_fire_monthly_always_strictly_after_now() {
        // Sweep every day of June and July.
        for month in [6u32, 7] {
            let days_in_month = if month == 6 { 30u32 } else { 31 };
            for day in 1..=days_in_month {
                for hour in [0u32, 9, 23] {
                    let now = dt(2026, month, day, hour, 0);
                    let nf = next_fire("monthly day 15 09:00", now)
                        .expect("monthly always has a next slot");
                    assert!(
                        nf > now,
                        "monthly day 15 09:00: next_fire({now}) = {nf} is not strictly after now"
                    );
                }
            }
        }
    }

    // ── property: is_due is false immediately after a fire ───────────────────
    //
    // If `last_fired` equals the most-recent scheduled slot, `is_due` must be
    // false — each slot fires at most once.

    #[test]
    fn is_due_false_immediately_after_daily_fire() {
        // now is 10:00, slot is 09:00 today; last_fired == slot => not due.
        let now = dt(2026, 6, 18, 10, 0);
        let slot = dt(2026, 6, 18, 9, 0); // today's slot, in the past
        assert!(!is_due("daily 09:00", now, Some(slot)));
        // Firing *after* the slot (but still same slot) also blocks.
        assert!(!is_due("daily 09:00", now, Some(dt(2026, 6, 18, 9, 30))));
    }

    #[test]
    fn is_due_false_immediately_after_weekly_fire() {
        // Thursday 2026-06-18 10:00; schedule Mon,Wed 09:00; most recent = Wed 09:00.
        let now = dt(2026, 6, 18, 10, 0);
        let last = dt(2026, 6, 17, 9, 0); // Wed slot
        assert!(!is_due("weekly Mon,Wed 09:00", now, Some(last)));
    }

    #[test]
    fn is_due_false_immediately_after_monthly_fire() {
        // Past the 15th-of-month slot; fire at that slot => not due.
        let now = dt(2026, 6, 18, 10, 0);
        let slot = dt(2026, 6, 15, 9, 0);
        assert!(!is_due("monthly day 15 09:00", now, Some(slot)));
        // Also: last_fired slightly after the slot still blocks.
        assert!(!is_due("monthly day 15 09:00", now, Some(dt(2026, 6, 15, 9, 30))));
    }

    // ── property: manual/unrecognized NEVER fires ─────────────────────────────

    #[test]
    fn manual_and_unrecognized_never_produce_next_fire() {
        let cases = &[
            "manual",
            "",
            "garbage",
            "daily",         // missing HH:MM
            "weekly 09:00",  // missing day list
            "monthly 15 09:00", // missing "day" keyword
            "once 2099-01-01", // missing HH:MM
        ];
        for s in cases {
            let now = dt(2026, 6, 25, 12, 0);
            assert!(
                next_fire(s, now).is_none(),
                "next_fire({s:?}) should be None (parses as Manual)"
            );
        }
    }

    #[test]
    fn manual_and_unrecognized_never_is_due() {
        let cases = &["manual", "", "garbage", "daily", "weekly 09:00"];
        for s in cases {
            // Try several (now, last_fired) combos — should always be false.
            let now = dt(2026, 6, 25, 12, 0);
            assert!(!is_due(s, now, None), "is_due({s:?}, now, None) should be false");
            assert!(!is_due(s, now, Some(dt(2026, 1, 1, 0, 0))), "is_due({s:?}, now, old_last_fired) should be false");
        }
    }

    // ── property: once fires at most once ────────────────────────────────────

    #[test]
    fn once_not_due_before_target_time() {
        // Strictly before target datetime: not due, regardless of last_fired.
        assert!(!is_due("once 2026-07-04 12:00", dt(2026, 7, 4, 11, 59), None));
        assert!(!is_due("once 2026-07-04 12:00", dt(2026, 7, 3, 23, 59), None));
        // Even with last_fired in the distant past:
        assert!(!is_due(
            "once 2026-07-04 12:00",
            dt(2026, 7, 4, 11, 59),
            Some(dt(2025, 1, 1, 0, 0))
        ));
    }

    #[test]
    fn once_due_at_and_after_target_when_never_fired() {
        assert!(is_due("once 2026-07-04 12:00", dt(2026, 7, 4, 12, 0), None));
        assert!(is_due("once 2026-07-04 12:00", dt(2026, 7, 4, 13, 0), None));
        assert!(is_due("once 2026-07-04 12:00", dt(2099, 1, 1, 0, 0), None));
    }

    #[test]
    fn once_not_due_after_last_fired_at_slot() {
        // Once `last_fired` is set to the slot (or later), never due again.
        let slot = dt(2026, 7, 4, 12, 0);
        assert!(!is_due("once 2026-07-04 12:00", dt(2026, 7, 4, 12, 0), Some(slot)));
        assert!(!is_due("once 2026-07-04 12:00", dt(2099, 1, 1, 0, 0), Some(slot)));
        // last_fired *after* the slot also blocks (e.g. fired at 12:05 due to clock drift).
        assert!(!is_due(
            "once 2026-07-04 12:00",
            dt(2099, 1, 1, 0, 0),
            Some(dt(2026, 7, 4, 12, 5))
        ));
    }

    #[test]
    fn once_no_next_fire_after_target_passes() {
        // After the target time, next_fire must be None (the one slot is in the past).
        assert!(next_fire("once 2026-07-04 12:00", dt(2026, 7, 4, 12, 0)).is_none());
        assert!(next_fire("once 2026-07-04 12:00", dt(2026, 7, 5, 0, 0)).is_none());
    }

    // ── boundary: monthly clamps to the last valid day of short months ────────

    #[test]
    fn monthly_day_31_clamps_in_february_non_leap() {
        // 2026 is not a leap year; Feb has 28 days. `monthly day 31` clamps to Feb 28.
        let now = dt(2026, 2, 28, 12, 0);
        assert!(is_due("monthly day 31 09:00", now, None),
            "monthly day 31 should fire on Feb 28 (the clamped day) when now is after 09:00");
        // Not due again once fired at the clamped slot.
        assert!(!is_due("monthly day 31 09:00", now, Some(dt(2026, 2, 28, 9, 0))));
    }

    #[test]
    fn monthly_day_31_clamps_in_february_leap() {
        // 2028 IS a leap year; Feb has 29 days. day 31 clamps to the 29th.
        let now = dt(2028, 2, 29, 12, 0);
        assert!(is_due("monthly day 31 09:00", now, None),
            "monthly day 31 should fire on Feb 29 in a leap year");
        assert!(!is_due("monthly day 31 09:00", now, Some(dt(2028, 2, 29, 9, 0))));
    }

    #[test]
    fn monthly_day_31_clamps_in_thirty_day_month() {
        // June has 30 days; day 31 fires on the 30th.
        let now = dt(2026, 6, 30, 12, 0);
        assert!(is_due("monthly day 31 09:00", now, None));
        assert!(!is_due("monthly day 31 09:00", now, Some(dt(2026, 6, 30, 9, 0))));
    }

    #[test]
    fn monthly_day_31_next_fire_clamps_correctly() {
        // Before the (clamped) June 30 slot: next fire should be June 30.
        let now = dt(2026, 6, 29, 8, 0);
        let nf = next_fire("monthly day 31 09:00", now).unwrap();
        assert_eq!(nf, dt(2026, 6, 30, 9, 0),
            "next_fire for monthly day 31 in June should clamp to June 30");
    }

    #[test]
    fn monthly_day_30_clamps_in_february() {
        // Day 30 in Feb 2026 (28 days) clamps to the 28th.
        let now = dt(2026, 2, 28, 12, 0);
        assert!(is_due("monthly day 30 09:00", now, None));
    }

    // ── boundary: weekly only fires on its listed weekday(s) ─────────────────

    #[test]
    fn weekly_fires_only_on_listed_days() {
        use chrono::{Datelike, Duration};
        // Week starting Mon 2026-06-01; schedule = Tue,Thu 09:00.
        let all_days: Vec<NaiveDateTime> = (0u32..7)
            .map(|off| {
                let base = NaiveDate::from_ymd_opt(2026, 6, 1).unwrap();
                (base + Duration::days(off as i64))
                    .and_hms_opt(10, 0, 0)
                    .unwrap()
            })
            .collect();

        for now in &all_days {
            let due = is_due("weekly Tue,Thu 09:00", *now, None);
            let weekday = now.weekday();
            // Due only when the most-recent slot is Tue or Thu at 09:00.
            // On Mon (2026-06-01) before any Tue/Thu in this week: most-recent slot
            // is last Thu (2026-05-28) — so it IS due.
            // On Tue (2026-06-02) 10:00: most-recent slot is today 09:00 — due.
            // On Wed (2026-06-03): most-recent is Tue 09:00 — due (never fired).
            // On Thu (2026-06-04): most-recent is today 09:00 — due.
            // On Fri (2026-06-05): most-recent is Thu 09:00 — due.
            // On Sat / Sun: most-recent is Thu 09:00 — due.
            // With last_fired=None all of these are due (there's always a past slot).
            // The key assertion: with last_fired set to the CORRECT slot for that day
            // range, it is no longer due.
            let _ = (due, weekday); // we use the per-day assertions below
        }

        // More precise: on Wednesday (non-listed day), firing at the previous Tuesday's
        // slot removes the "due" state until Thursday.
        let wed = dt(2026, 6, 3, 10, 0);
        let tue_slot = dt(2026, 6, 2, 9, 0);
        assert!(!is_due("weekly Tue,Thu 09:00", wed, Some(tue_slot)),
            "after Tuesday's slot is consumed, should not be due again until Thursday");
        // On Thursday it becomes due again (unfired Thu slot).
        let thu = dt(2026, 6, 4, 10, 0);
        assert!(is_due("weekly Tue,Thu 09:00", thu, Some(tue_slot)),
            "should be due on Thursday even though Tuesday's slot was consumed");
        // After Thursday slot fires, not due until next Tuesday.
        let thu_slot = dt(2026, 6, 4, 9, 0);
        let fri = dt(2026, 6, 5, 10, 0);
        assert!(!is_due("weekly Tue,Thu 09:00", fri, Some(thu_slot)),
            "after Thursday's slot, should not be due on Friday");
    }

    #[test]
    fn weekly_ignores_unlisted_days_for_next_fire() {
        use chrono::Datelike;
        // Schedule: only Saturday. From a Monday, next should be the coming Saturday.
        let mon = dt(2026, 6, 1, 10, 0); // Monday
        let nf = next_fire("weekly Sat 09:00", mon).unwrap();
        assert_eq!(nf.weekday(), chrono::Weekday::Sat,
            "next_fire for 'weekly Sat 09:00' from Monday must land on a Saturday");
        assert!(nf > mon);
    }

    // ── boundary: is_due uses < (strict) on last_fired vs slot ───────────────
    //
    // is_due: due when last_fired < slot (the slot is strictly newer than the
    // last fire). Verify the boundary precisely.

    #[test]
    fn is_due_boundary_strict_less_than_on_last_fired() {
        // Slot is 09:00 today; now is 10:00.
        let now = dt(2026, 6, 25, 10, 0);
        let slot = dt(2026, 6, 25, 9, 0);

        // last_fired == slot: NOT due (lf < slot is false).
        assert!(!is_due("daily 09:00", now, Some(slot)));
        // last_fired one minute before slot: due (lf < slot is true).
        assert!(is_due("daily 09:00", now, Some(dt(2026, 6, 25, 8, 59))));
        // last_fired one minute after slot: NOT due.
        assert!(!is_due("daily 09:00", now, Some(dt(2026, 6, 25, 9, 1))));
    }

    // ── parse edge cases ──────────────────────────────────────────────────────

    #[test]
    fn parse_invalid_times_become_manual() {
        assert_eq!(parse("daily 24:00"), Schedule::Manual); // hour out of range
        assert_eq!(parse("daily 23:60"), Schedule::Manual); // minute out of range
        assert_eq!(parse("daily 9:5"),   Schedule::Daily { h: 9, m: 5 }); // single-digit ok
        assert_eq!(parse("monthly day 0 09:00"), Schedule::Manual);  // day 0 invalid
        assert_eq!(parse("monthly day 32 09:00"), Schedule::Manual); // day 32 invalid
        assert_eq!(parse("weekly  09:00"), Schedule::Manual); // missing day tokens
    }

    #[test]
    fn parse_weekly_partial_invalid_days_dropped() {
        // "Mon,BADDAY,Fri" — only valid tokens survive; result is Weekly if >=1 valid day.
        match parse("weekly Mon,BADDAY,Fri 09:00") {
            Schedule::Weekly { days, h: 9, m: 0 } => {
                assert!(days.contains(&chrono::Weekday::Mon));
                assert!(days.contains(&chrono::Weekday::Fri));
                assert_eq!(days.len(), 2, "BADDAY should be silently dropped");
            }
            other => panic!("expected Weekly with Mon+Fri, got {other:?}"),
        }
    }
}
