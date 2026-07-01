//! Routine schedule string <-> picker-state conversion, extracted from the Routines UI.
//!
//! Pure string/primitive functions with no rendering-framework dependency, unit-tested here (the same
//! assertions that previously lived in `camerata-ui`, translated 1:1 to this core crate).

/// Weekday labels, index 0 = Sunday, matching the schedule picker's toggle order.
pub const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// Serialize the structured schedule picker into the human-readable schedule string the BFF stores
/// (e.g. `daily 09:00`, `weekly Mon,Wed 09:00`, `monthly day 15 09:00`, `once 2026-06-20 14:00`). The
/// empty-field fallbacks keep the string well-formed even before every control is touched.
pub fn build_schedule(freq: &str, time: &str, date: &str, weekdays: &[bool], monthday: u32) -> String {
    let t = if time.is_empty() { "09:00" } else { time };
    match freq {
        "once" => {
            if date.is_empty() {
                format!("once {t}")
            } else {
                format!("once {date} {t}")
            }
        }
        "weekly" => {
            let days: Vec<&str> = weekdays
                .iter()
                .enumerate()
                .filter(|(_, on)| **on)
                .map(|(i, _)| WEEKDAYS[i])
                .collect();
            let days_str = if days.is_empty() {
                "Mon".to_string()
            } else {
                days.join(",")
            };
            format!("weekly {days_str} {t}")
        }
        "monthly" => format!("monthly day {monthday} {t}"),
        _ => format!("daily {t}"),
    }
}

/// Parse a stored schedule string back into the picker state, for Edit prefill. Returns
/// `(freq, time, date, weekdays, monthday)`. Anything that doesn't match a known shape falls back to a
/// daily-09:00 default (the schedule string is still shown verbatim in the row, so nothing is lost —
/// the picker just starts neutral).
pub fn parse_schedule(s: &str) -> (String, String, String, Vec<bool>, u32) {
    let default_days = vec![false, true, false, false, false, false, false];
    let parts: Vec<&str> = s.split_whitespace().collect();
    match parts.as_slice() {
        ["daily", time] => (
            "daily".into(),
            (*time).into(),
            String::new(),
            default_days,
            1,
        ),
        ["weekly", days, time] => {
            let mut wd = vec![false; 7];
            for d in days.split(',') {
                if let Some(i) = WEEKDAYS.iter().position(|w| w.eq_ignore_ascii_case(d)) {
                    wd[i] = true;
                }
            }
            ("weekly".into(), (*time).into(), String::new(), wd, 1)
        }
        ["monthly", "day", n, time] => (
            "monthly".into(),
            (*time).into(),
            String::new(),
            default_days,
            n.parse::<u32>().unwrap_or(1).clamp(1, 31),
        ),
        ["once", date, time] => (
            "once".into(),
            (*time).into(),
            (*date).into(),
            default_days,
            1,
        ),
        ["once", time] => (
            "once".into(),
            (*time).into(),
            String::new(),
            default_days,
            1,
        ),
        _ => (
            "daily".into(),
            "09:00".into(),
            String::new(),
            default_days,
            1,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Pure-logic tests: schedule serialization round-trips (moved verbatim from camerata-ui) ──

    #[test]
    fn build_schedule_daily_uses_time() {
        let s = build_schedule("daily", "07:30", "", &[], 1);
        assert_eq!(s, "daily 07:30");
    }

    #[test]
    fn build_schedule_daily_empty_time_falls_back_to_0900() {
        let s = build_schedule("daily", "", "", &[], 1);
        assert_eq!(s, "daily 09:00");
    }

    #[test]
    fn build_schedule_weekly_joins_selected_days() {
        // Sun..Sat: Mon + Wed on.
        let days = [false, true, false, true, false, false, false];
        let s = build_schedule("weekly", "08:00", "", &days, 1);
        assert_eq!(s, "weekly Mon,Wed 08:00");
    }

    #[test]
    fn build_schedule_weekly_no_days_defaults_to_mon() {
        let days = [false; 7];
        let s = build_schedule("weekly", "08:00", "", &days, 1);
        assert_eq!(s, "weekly Mon 08:00");
    }

    #[test]
    fn build_schedule_monthly_includes_day_of_month() {
        let s = build_schedule("monthly", "06:00", "", &[], 15);
        assert_eq!(s, "monthly day 15 06:00");
    }

    #[test]
    fn build_schedule_once_with_date() {
        let s = build_schedule("once", "14:00", "2026-06-20", &[], 1);
        assert_eq!(s, "once 2026-06-20 14:00");
    }

    #[test]
    fn build_schedule_once_without_date_omits_it() {
        let s = build_schedule("once", "14:00", "", &[], 1);
        assert_eq!(s, "once 14:00");
    }

    #[test]
    fn parse_schedule_round_trips_weekly() {
        let (freq, time, date, wd, _md) = parse_schedule("weekly Mon,Wed 08:00");
        assert_eq!(freq, "weekly");
        assert_eq!(time, "08:00");
        assert!(date.is_empty());
        // Sun=0, Mon=1, Wed=3.
        assert!(wd[1]);
        assert!(wd[3]);
        assert!(!wd[0]);
        assert!(!wd[2]);
    }

    #[test]
    fn parse_schedule_round_trips_monthly() {
        let (freq, time, _date, _wd, md) = parse_schedule("monthly day 22 06:00");
        assert_eq!(freq, "monthly");
        assert_eq!(time, "06:00");
        assert_eq!(md, 22);
    }

    #[test]
    fn parse_schedule_clamps_out_of_range_monthday() {
        let (_freq, _time, _date, _wd, md) = parse_schedule("monthly day 99 06:00");
        assert_eq!(md, 31, "day-of-month is clamped to 31");
    }

    #[test]
    fn parse_schedule_unknown_shape_falls_back_to_daily() {
        let (freq, time, _date, _wd, _md) = parse_schedule("garbage input here");
        assert_eq!(freq, "daily");
        assert_eq!(time, "09:00");
    }
}
