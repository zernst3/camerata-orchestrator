//! Server-side parsing of a routine's human-readable `schedule` string and the
//! "is this routine due right now?" decision the auto-fire scheduler ticks on.
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
}
