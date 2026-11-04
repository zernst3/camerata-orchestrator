//! The review shell: the lane switcher and the progress strip (spec section 4).
//!
//! ```text
//! [ Tree 1,204 undecided ] [ Escalations 347 ] [ Engine Check 50 ] [ Flat view ] [ Settings ]
//! Undecided 1,204 of 30,112 . Confirmed 27,900 . Ignored 600 . FP 310 . Tech debt 98 . Held 3
//! ```

use camerata_api_types::review_view::{LaneCounts, ReviewCounts};

use crate::progress::progress_text;
use crate::triage::TriageCounts;

use super::thousands;

/// One lane of the review screen. `1` to `5` switch to them in this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lane {
    Tree,
    Escalations,
    EngineCheck,
    Flat,
    Settings,
}

impl Lane {
    /// Every lane, in switcher order.
    pub const ALL: [Lane; 5] = [
        Self::Tree,
        Self::Escalations,
        Self::EngineCheck,
        Self::Flat,
        Self::Settings,
    ];

    /// The tab's name.
    pub fn label(self) -> &'static str {
        match self {
            Self::Tree => "Tree",
            Self::Escalations => "Escalations",
            Self::EngineCheck => "Engine Check",
            Self::Flat => "Flat view",
            Self::Settings => "Settings",
        }
    }

    /// The digit that switches to the lane.
    pub fn digit(self) -> char {
        match self {
            Self::Tree => '1',
            Self::Escalations => '2',
            Self::EngineCheck => '3',
            Self::Flat => '4',
            Self::Settings => '5',
        }
    }

    /// A stable id for element ids and test selectors.
    pub fn slug(self) -> &'static str {
        match self {
            Self::Tree => "tree",
            Self::Escalations => "escalations",
            Self::EngineCheck => "engine-check",
            Self::Flat => "flat",
            Self::Settings => "settings",
        }
    }

    /// Whether the lane's screen is built. The tabs of a lane that is not show and say so.
    pub fn is_built(self) -> bool {
        matches!(self, Self::Tree | Self::Flat)
    }
}

/// One tab of the switcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneTab {
    pub lane: Lane,
    pub label: String,
    /// The count after the label (`1,204 undecided`, `347`), when the tab has one.
    pub count: Option<String>,
    pub active: bool,
}

/// The tabs for the lane counts the shell returned, with `active` marked.
pub fn lane_tabs(lanes: &LaneCounts, active: Lane) -> Vec<LaneTab> {
    Lane::ALL
        .iter()
        .map(|&lane| {
            let (label, count) = match lane {
                Lane::Tree => (
                    lane.label().to_string(),
                    Some(format!("{} undecided", thousands(lanes.tree_undecided))),
                ),
                Lane::Escalations => (lane.label().to_string(), Some(thousands(lanes.escalations))),
                Lane::EngineCheck if lanes.engine_check_off => {
                    ("Engine Check off".to_string(), None)
                }
                Lane::EngineCheck => (
                    lane.label().to_string(),
                    Some(thousands(lanes.engine_check)),
                ),
                Lane::Flat | Lane::Settings => (lane.label().to_string(), None),
            };
            LaneTab {
                lane,
                label,
                count,
                active: lane == active,
            }
        })
        .collect()
}

/// The lane a key switches to: `1` to `5`, only when no text field holds focus and no modifier
/// that changes the press is held.
pub fn lane_for_key(key: &str, ctrl_alt_meta: bool, text_focused: bool) -> Option<Lane> {
    if text_focused || ctrl_alt_meta {
        return None;
    }
    Lane::ALL
        .into_iter()
        .find(|l| key.len() == 1 && key.starts_with(l.digit()))
}

/// The triage tally behind the strip.
pub fn triage_counts(c: &ReviewCounts) -> TriageCounts {
    TriageCounts {
        unresolved: c.unresolved,
        confirmed: c.confirmed,
        ignored: c.ignored,
        tech_debt: c.tech_debt,
        false_positive: c.false_positive,
        merged: c.merged,
    }
}

/// The strip's counts text: the Queue's own words ([`progress_text`]), nothing counted here.
pub fn strip_text(c: &ReviewCounts) -> String {
    progress_text(triage_counts(c))
}

/// `Held 3`, drawn after the counts and red; `None` when nothing is held.
pub fn held_text(c: &ReviewCounts) -> Option<String> {
    (c.held > 0).then(|| format!("Held {}", thousands(c.held)))
}

/// What the tree column says when there is no inspection (spec section 5, "Empty").
pub const NO_INSPECTION: &str = "No inspection to review. Run a scan or open a saved inspection.";

/// The muted banner on an old inspection the group reviewer never read (spec section 5).
pub const NOT_GROUP_REVIEWED: &str =
    "This inspection was not reviewed by the group reviewer. The tree shows the groups the engine made, with no summaries or stage 2 columns.";

/// Whether a run has nothing in it: no sites, so the screens show their empty state.
pub fn is_empty_run(c: &ReviewCounts) -> bool {
    c.total == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::tests::data;

    #[test]
    fn the_tabs_carry_the_counts_the_shell_returned() {
        let lanes = LaneCounts {
            tree_undecided: 1_204,
            escalations: 347,
            engine_check: 50,
            engine_check_off: false,
            flat_rows: 600,
        };
        let tabs = lane_tabs(&lanes, Lane::Tree);
        let text: Vec<(String, Option<String>, bool)> = tabs
            .iter()
            .map(|t| (t.label.clone(), t.count.clone(), t.active))
            .collect();
        assert_eq!(
            text,
            vec![
                (
                    "Tree".to_string(),
                    Some("1,204 undecided".to_string()),
                    true
                ),
                ("Escalations".to_string(), Some("347".to_string()), false),
                ("Engine Check".to_string(), Some("50".to_string()), false),
                ("Flat view".to_string(), None, false),
                ("Settings".to_string(), None, false),
            ]
        );
    }

    #[test]
    fn the_engine_check_tab_reads_off_when_the_sample_size_is_zero() {
        let lanes = LaneCounts {
            engine_check_off: true,
            ..data().shell.lanes
        };
        let tab = lane_tabs(&lanes, Lane::Tree)
            .into_iter()
            .find(|t| t.lane == Lane::EngineCheck)
            .unwrap();
        assert_eq!(tab.label, "Engine Check off");
        assert_eq!(tab.count, None);
    }

    #[test]
    fn one_to_five_switch_lanes_only_without_a_text_field_or_a_modifier() {
        for (key, lane) in [
            ("1", Lane::Tree),
            ("2", Lane::Escalations),
            ("3", Lane::EngineCheck),
            ("4", Lane::Flat),
            ("5", Lane::Settings),
        ] {
            assert_eq!(lane_for_key(key, false, false), Some(lane));
            assert_eq!(
                lane_for_key(key, false, true),
                None,
                "a text field holds focus"
            );
            assert_eq!(
                lane_for_key(key, true, false),
                None,
                "Ctrl, Alt or Cmd is held"
            );
        }
        assert_eq!(lane_for_key("6", false, false), None);
        assert_eq!(lane_for_key("11", false, false), None);
        assert_eq!(lane_for_key("a", false, false), None);
        // The digits are in the order of ALL.
        let digits: String = Lane::ALL.iter().map(|l| l.digit()).collect();
        assert_eq!(digits, "12345");
    }

    #[test]
    fn the_strip_is_the_queues_text_and_held_follows_in_its_own_span() {
        let counts = ReviewCounts {
            total: 612,
            unresolved: 187,
            confirmed: 310,
            ignored: 71,
            false_positive: 29,
            tech_debt: 15,
            merged: 0,
            held: 3,
        };
        assert_eq!(
            strip_text(&counts),
            "Undecided 187 of 612 \u{b7} Confirmed 310 \u{b7} Ignored 71 \u{b7} FP 29 \u{b7} Tech debt 15"
        );
        assert_eq!(held_text(&counts).as_deref(), Some("Held 3"));
        assert_eq!(held_text(&ReviewCounts { held: 0, ..counts }), None);
    }

    #[test]
    fn the_fixture_shell_strip_agrees_with_its_own_total() {
        let c = &data().shell.counts;
        assert_eq!(
            triage_counts(c).total(),
            c.total,
            "the strip's total is the shell's"
        );
        assert!(!is_empty_run(c));
        assert!(is_empty_run(&ReviewCounts::default()));
    }

    #[test]
    fn only_the_tree_and_the_flat_lane_are_built_here() {
        let built: Vec<Lane> = Lane::ALL.into_iter().filter(|l| l.is_built()).collect();
        assert_eq!(built, vec![Lane::Tree, Lane::Flat]);
        let slugs: Vec<&str> = Lane::ALL.iter().map(|l| l.slug()).collect();
        assert_eq!(
            slugs,
            ["tree", "escalations", "engine-check", "flat", "settings"]
        );
        assert!(!NO_INSPECTION.contains('\u{2014}'));
        assert!(!NOT_GROUP_REVIEWED.contains('\u{2014}'));
    }
}
