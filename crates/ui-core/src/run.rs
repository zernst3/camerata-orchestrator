//! Run/event display helpers, extracted from the cockpit. Pure functions with no rendering-framework
//! dependency, unit-tested here.

/// Format an idle duration from milliseconds into a human-readable string.
/// e.g. 90_000 -> "1m 30s", 5_000 -> "5s", 65_000 -> "1m 5s".
pub fn format_idle(idle_ms: u128) -> String {
    let total_secs = idle_ms / 1000;
    if total_secs < 60 {
        format!("{total_secs}s")
    } else {
        let mins = total_secs / 60;
        let secs = total_secs % 60;
        if secs == 0 {
            format!("{mins}m")
        } else {
            format!("{mins}m {secs}s")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_idle_formats_durations() {
        assert_eq!(format_idle(0), "0s");
        assert_eq!(format_idle(5_000), "5s");
        assert_eq!(format_idle(59_000), "59s");
        assert_eq!(format_idle(60_000), "1m");
        assert_eq!(format_idle(65_000), "1m 5s");
        assert_eq!(format_idle(90_000), "1m 30s");
        assert_eq!(format_idle(3_600_000), "60m");
    }
}
