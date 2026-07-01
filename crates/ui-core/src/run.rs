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

/// Whether a run in `status` (with `done` set) can still be cancelled. Terminal statuses
/// (`failed`, `cancelled`) and any `done` run are never cancellable.
pub fn run_is_cancellable(status: &str, done: bool) -> bool {
    !done && !matches!(status, "failed" | "cancelled")
}

/// Whether the stall banner should show: the run is stalled and not yet done.
pub fn run_stall_banner_visible(stalled: bool, done: bool) -> bool {
    stalled && !done
}

/// The `(label, css-class)` styling for a live activity-log event, keyed by its `layer` and
/// `verdict`. Pure lookup table; unrecognised layers fall back to verdict-based styling.
pub fn live_event_style(layer: &str, verdict: &str) -> (&'static str, &'static str) {
    match layer {
        // Layer-1 deny-before-execute gate: allow / deny (the bounce-back).
        "layer-1" => match verdict {
            "deny" => ("GATE DENY", "live-event deny"),
            "allow" => ("GATE ALLOW", "live-event allow"),
            _ => ("GATE", "live-event info"),
        },
        // Layer-2 post-task lint/test check + the bounce-and-revise pass.
        "layer-2" => match verdict {
            "pass" => ("LAYER-2 PASS", "live-event allow"),
            "fail" => ("LAYER-2 FAIL", "live-event deny"),
            "revise" => ("REVISE", "live-event revise"),
            // legacy scripted "bounce" verdict.
            "bounce" => ("REVISE", "live-event revise"),
            _ => ("LAYER-2", "live-event info"),
        },
        // Delegation dispatch / return (+ INCOMPLETE escalation).
        "delegate" => match verdict {
            "dispatch" => ("DELEGATE", "live-event delegate"),
            "incomplete" => ("DELEGATE INCOMPLETE", "live-event deny"),
            _ => ("DELEGATE RETURN", "live-event delegate"),
        },
        // Phase 3b: the agent raised a structured clarifying question; the run paused
        // ("pause") or resumed on the answer ("info").
        "clarification" => match verdict {
            "pause" => ("WAITING ON YOU", "live-event revise"),
            _ => ("CLARIFICATION", "live-event info"),
        },
        // Model/tier routing per spawned agent.
        "tier" => ("TIER", "live-event tier"),
        // cargo build/test verification.
        "checks" => match verdict {
            "allow" => ("CHECKS PASS", "live-event allow"),
            "deny" => ("CHECKS FAIL", "live-event deny"),
            _ => ("CHECKS", "live-event info"),
        },
        // Stage / fleet lifecycle + setup.
        "stage" => match verdict {
            "fail" => ("STAGE", "live-event deny"),
            _ => ("STAGE", "live-event info"),
        },
        // Stall-detection synthetic event: the run has been idle longer than the threshold.
        "stall" => ("STALL", "live-event stall"),
        "setup" => ("SETUP", "live-event info"),
        // Default (incl. "fleet" lifecycle, empty/legacy): fall back to the verdict.
        _ => match verdict {
            "deny" | "error" => (
                if verdict == "error" { "ERROR" } else { "DENY" },
                "live-event deny",
            ),
            "allow" => ("ALLOW", "live-event allow"),
            _ => ("INFO", "live-event info"),
        },
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

    /// The per-layer/verdict styling gives each observability kind a distinct label +
    /// class so the activity log reads clearly. Asserts the load-bearing mappings.
    #[test]
    fn live_event_style_labels_each_layer_distinctly() {
        assert_eq!(live_event_style("layer-1", "deny"), ("GATE DENY", "live-event deny"));
        assert_eq!(live_event_style("layer-1", "allow"), ("GATE ALLOW", "live-event allow"));
        assert_eq!(live_event_style("layer-2", "pass"), ("LAYER-2 PASS", "live-event allow"));
        assert_eq!(live_event_style("layer-2", "fail"), ("LAYER-2 FAIL", "live-event deny"));
        assert_eq!(live_event_style("layer-2", "revise"), ("REVISE", "live-event revise"));
        assert_eq!(live_event_style("tier", "info"), ("TIER", "live-event tier"));
        assert_eq!(
            live_event_style("delegate", "dispatch"),
            ("DELEGATE", "live-event delegate")
        );
        assert_eq!(
            live_event_style("delegate", "incomplete"),
            ("DELEGATE INCOMPLETE", "live-event deny")
        );
        assert_eq!(live_event_style("checks", "allow"), ("CHECKS PASS", "live-event allow"));
        // Legacy/empty layer falls back to verdict-based styling.
        assert_eq!(live_event_style("", "deny"), ("DENY", "live-event deny"));
        assert_eq!(live_event_style("", "allow"), ("ALLOW", "live-event allow"));
    }

    /// `live_event_style` maps the "stall" family to the amber/warning treatment.
    #[test]
    fn live_event_style_stall_family() {
        let (label, cls) = live_event_style("stall", "");
        assert_eq!(label, "STALL");
        assert_eq!(cls, "live-event stall");
    }

    /// Pure: cancellable-state predicate.
    #[test]
    fn run_is_cancellable_predicate() {
        // Running states are cancellable.
        assert!(run_is_cancellable("executing", false));
        assert!(run_is_cancellable("gating", false));
        assert!(run_is_cancellable("awaiting_clarification", false));
        // Terminal states are not cancellable.
        assert!(!run_is_cancellable("failed", true));
        assert!(!run_is_cancellable("cancelled", true));
        // done=true always non-cancellable.
        assert!(!run_is_cancellable("executing", true));
        // failed/cancelled with done=false are also non-cancellable (status check).
        assert!(!run_is_cancellable("failed", false));
        assert!(!run_is_cancellable("cancelled", false));
    }

    /// Pure: stall banner visibility predicate.
    #[test]
    fn run_stall_banner_visible_predicate() {
        assert!(run_stall_banner_visible(true, false));
        assert!(!run_stall_banner_visible(false, false));
        assert!(!run_stall_banner_visible(true, true));
        assert!(!run_stall_banner_visible(false, true));
    }
}
