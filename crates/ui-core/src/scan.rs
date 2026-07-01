//! Scan-surface formatting helpers, extracted from the scan UI. Pure string/number formatting with no
//! rendering-framework dependency, unit-tested here.

/// Human-readable token count by magnitude (900 -> "900", 2_000 -> "2k", 2_000_000 -> "2.0M").
pub fn human_tokens(t: u64) -> String {
    if t >= 1_000_000 {
        format!("{:.1}M", t as f64 / 1_000_000.0)
    } else if t >= 1_000 {
        format!("{:.0}k", t as f64 / 1_000.0)
    } else {
        t.to_string()
    }
}

/// Display label for a deterministic scan tool (known tools get a friendly name; others pass through).
pub fn det_tool_label(tool: &str) -> String {
    match tool {
        "floor" => "Security floor".to_string(),
        "unrouted" => "Unrouted rules".to_string(),
        other => other.to_string(),
    }
}

/// The default triage/finding status (`"active"`).
pub fn default_finding_status() -> String {
    "active".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_tokens_formats_by_magnitude() {
        assert_eq!(human_tokens(900), "900");
        assert_eq!(human_tokens(2_000), "2k");
        assert_eq!(human_tokens(350_000), "350k");
        assert_eq!(human_tokens(2_000_000), "2.0M");
    }

    #[test]
    fn det_tool_label_maps_known_and_passes_through_unknown() {
        assert_eq!(det_tool_label("floor"), "Security floor");
        assert_eq!(det_tool_label("unrouted"), "Unrouted rules");
        assert_eq!(det_tool_label("clippy"), "clippy");
        // (the "ruff" passthrough case was a duplicate test in cockpit.rs; merged here.)
        assert_eq!(det_tool_label("ruff"), "ruff");
    }

    #[test]
    fn default_finding_status_is_active() {
        assert_eq!(default_finding_status(), "active");
    }
}
