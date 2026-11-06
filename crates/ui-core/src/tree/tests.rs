//! Tests of the row and node-header pieces (this module's root).

use super::*;
use camerata_api_types::review::OutlierReason;
use camerata_api_types::review_view::{HoldReason, Marker, SiteRow};
use camerata_api_types::review_view::{NodeCountsView, TreeLevel};

use crate::test_support::review_fixtures::{dataset_base, ReviewDataSet};

/// The 600-site fixture, built once for the tests of this crate that need rows.
pub(crate) fn data() -> &'static ReviewDataSet {
    static DATA: std::sync::OnceLock<ReviewDataSet> = std::sync::OnceLock::new();
    DATA.get_or_init(dataset_base)
}

fn plain_row() -> SiteRow {
    data()
        .rows
        .iter()
        .find(|r| r.markers.is_empty() && r.decision.is_none() && r.stage2.is_some())
        .cloned()
        .expect("the fixture has a plain row")
}

fn row_with(markers: &[Marker]) -> SiteRow {
    let mut r = plain_row();
    r.markers = markers.to_vec();
    if markers.contains(&Marker::Held) {
        r.held = Some(HoldReason::EvidenceUnverified);
    }
    if markers.contains(&Marker::Outlier) {
        if let Some(s) = r.stage2.as_mut() {
            s.outlier_reason = Some(OutlierReason::DifferentObject);
        }
    }
    r
}

#[test]
fn a_site_key_with_nul_bytes_becomes_a_token_safe_in_an_attribute_and_round_trips() {
    let rows = &data().rows;
    assert!(rows.len() > 100);
    for row in rows.iter().take(300) {
        assert!(
            row.site_key.contains('\0'),
            "the fixture keys have the real shape"
        );
        let token = dom_token(&row.site_key);
        assert!(!token.contains('\0'), "no NUL survives into the token");
        assert!(
            token.chars().all(|c| c.is_ascii_hexdigit()),
            "only hex digits: {token}"
        );
        assert_eq!(
            from_dom_token(&token).as_deref(),
            Some(row.site_key.as_str())
        );
    }
    // Node ids go through the same door.
    assert_eq!(
        from_dom_token(&dom_token("sub:SEC-RLS-1/3")).as_deref(),
        Some("sub:SEC-RLS-1/3")
    );
}

#[test]
fn text_that_is_not_a_token_does_not_decode() {
    assert_eq!(from_dom_token(""), None);
    assert_eq!(from_dom_token("abc"), None);
    assert_eq!(from_dom_token("zz"), None);
    assert_eq!(from_dom_token("repo\0path"), None);
}

#[test]
fn the_counts_line_reads_like_the_spec_and_omits_zero_terms() {
    let busy = NodeCountsView {
        sites: 312,
        escalated: 4,
        outliers: 2,
        new_since_decision: 12,
        decided: 298,
        ..NodeCountsView::default()
    };
    assert_eq!(
        counts_line(&busy),
        "312 sites \u{b7} 4 esc \u{b7} 2 outl \u{b7} 12 new \u{b7} 298 decided"
    );
    let clean = NodeCountsView {
        sites: 312,
        decided: 312,
        ..NodeCountsView::default()
    };
    assert_eq!(counts_line(&clean), "312 sites \u{b7} 312 decided");
    let held = NodeCountsView {
        sites: 88,
        escalated: 12,
        held: 3,
        stale: 1,
        ..NodeCountsView::default()
    };
    assert_eq!(
        counts_line(&held),
        "88 sites \u{b7} 12 esc \u{b7} 1 stale \u{b7} 3 held"
    );
    let big = NodeCountsView {
        sites: 2_210,
        ..NodeCountsView::default()
    };
    assert_eq!(counts_line(&big), "2,210 sites");
    let one = NodeCountsView {
        sites: 1,
        ..NodeCountsView::default()
    };
    assert_eq!(counts_line(&one), "1 site");
}

#[test]
fn a_nodes_escalations_show_amber_outliers_and_new_orange_held_red_and_the_rest_plain() {
    let c = NodeCountsView {
        sites: 10,
        escalated: 1,
        outliers: 1,
        new_since_decision: 1,
        held: 1,
        stale: 1,
        decided: 1,
        ..NodeCountsView::default()
    };
    let tones: Vec<(CountKind, CountTone)> =
        count_terms(&c).iter().map(|t| (t.kind, t.tone)).collect();
    assert_eq!(
        tones,
        vec![
            (CountKind::Sites, CountTone::Plain),
            (CountKind::Escalated, CountTone::Amber),
            (CountKind::Outliers, CountTone::Orange),
            (CountKind::NewSinceDecision, CountTone::Orange),
            (CountKind::Stale, CountTone::Plain),
            (CountKind::Held, CountTone::Red),
            (CountKind::Decided, CountTone::Plain),
        ]
    );
}

#[test]
fn indentation_is_zero_sixteen_thirty_two() {
    assert_eq!(indent_px(TreeLevel::Category), 0);
    assert_eq!(indent_px(TreeLevel::Rule), 16);
    assert_eq!(indent_px(TreeLevel::Subgroup), 32);
}

#[test]
fn a_node_is_fully_decided_only_when_every_site_is() {
    let mut c = NodeCountsView {
        sites: 4,
        decided: 4,
        ..NodeCountsView::default()
    };
    assert!(is_fully_decided(&c));
    c.decided = 3;
    assert!(!is_fully_decided(&c));
    assert!(
        !is_fully_decided(&NodeCountsView::default()),
        "empty is not decided"
    );
}

#[test]
fn thousands_groups_digits() {
    assert_eq!(thousands(0), "0");
    assert_eq!(thousands(999), "999");
    assert_eq!(thousands(1_000), "1,000");
    assert_eq!(thousands(30_112), "30,112");
    assert_eq!(thousands(1_234_567), "1,234,567");
}

#[test]
fn every_marker_has_its_chip_class_from_the_spec_table() {
    let expect = [
        (Marker::Held, "mark-held"),
        (Marker::Escalated, "mark-esc"),
        (Marker::Candidate, "mark-cand"),
        (Marker::OverBudget, "mark-over"),
        (Marker::Outlier, "mark-outlier"),
        (Marker::NewSinceDecision, "mark-new"),
        (Marker::Stale, "mark-stale"),
        (Marker::Moved, "mark-moved"),
        (Marker::PossibleDuplicate, "mark-dup"),
        (Marker::Edited, "mark-edited"),
    ];
    for (marker, class) in expect {
        let chips = marker_chips(&row_with(&[marker]));
        assert_eq!(chips.len(), 1, "{marker:?}");
        assert_eq!(chips[0].class, class, "{marker:?}");
        assert!(!chips[0].label.is_empty(), "{marker:?}");
    }
    assert_eq!(
        expect.len(),
        Marker::PRECEDENCE.len(),
        "every marker is covered"
    );
}

#[test]
fn the_markers_column_shows_two_chips_in_precedence_and_folds_the_rest() {
    let row = row_with(&[
        Marker::Held,
        Marker::Escalated,
        Marker::Outlier,
        Marker::Edited,
    ]);
    let cell = marker_cell(&row);
    let shown: Vec<&str> = cell.shown.iter().map(|c| c.class).collect();
    assert_eq!(
        shown,
        vec!["mark-held", "mark-esc"],
        "held first, then escalated"
    );
    assert_eq!(cell.fold_label().as_deref(), Some("+2"));
    let tip = cell.fold_tip();
    assert!(tip.contains("different object"), "{tip}");
    assert!(tip.contains("edited"), "{tip}");

    let two = marker_cell(&row_with(&[Marker::Escalated, Marker::Edited]));
    assert_eq!(two.shown.len(), 2);
    assert_eq!(two.fold_label(), None, "two chips fit");
    assert!(marker_cell(&plain_row()).shown.is_empty());
}

#[test]
fn an_over_budget_escalation_is_one_chip_that_says_over() {
    let chips = marker_chips(&row_with(&[Marker::Escalated, Marker::OverBudget]));
    assert_eq!(chips.len(), 1);
    assert_eq!(chips[0].class, "mark-esc mark-over");
    assert_eq!(chips[0].label, "ESC over");
}

#[test]
fn the_outlier_chip_says_the_reason_in_words() {
    let chips = marker_chips(&row_with(&[Marker::Outlier]));
    assert_eq!(chips[0].label, "different object");
}

#[test]
fn the_chips_that_carry_a_date_or_a_line_say_it() {
    let moved = data()
        .rows
        .iter()
        .find(|r| r.moved_from.is_some())
        .expect("a moved row");
    let chip = marker_chips(moved)
        .into_iter()
        .find(|c| c.class == "mark-moved")
        .unwrap();
    let was = moved.moved_from.as_ref().and_then(|m| m.line).unwrap();
    assert_eq!(chip.label, format!("moved, was line {was}"));

    let new = data()
        .rows
        .iter()
        .find(|r| r.new_since_decision.is_some())
        .expect("a new-since row");
    let chip = marker_chips(new)
        .into_iter()
        .find(|c| c.class == "mark-new")
        .unwrap();
    assert!(
        chip.label.starts_with("\u{25cf} new since 11-0"),
        "{}",
        chip.label
    );

    let stale = data()
        .rows
        .iter()
        .find(|r| r.stale_decision.is_some())
        .expect("a stale-decision row");
    let chip = marker_chips(stale)
        .into_iter()
        .find(|c| c.class == "mark-stale")
        .unwrap();
    assert_eq!(chip.label, "stale: was Confirmed");

    let dup = data()
        .rows
        .iter()
        .find(|r| r.possible_duplicate_of.is_some())
        .expect("a duplicate row");
    let chip = marker_chips(dup)
        .into_iter()
        .find(|c| c.class == "mark-dup")
        .unwrap();
    let d = dup.possible_duplicate_of.as_ref().unwrap();
    assert!(chip.label.starts_with("dup? "), "{}", chip.label);
    assert!(chip.label.contains(&d.rule_id), "{}", chip.label);
}

#[test]
fn the_stage_cells_read_the_wire_values() {
    let mut r = plain_row();
    r.stage1.real = "yes".to_string();
    r.stage1.effort = Some("low".to_string());
    assert_eq!(stage1_cell(&r), "Y low");
    r.stage1.real = "no".to_string();
    r.stage1.effort = None;
    assert_eq!(stage1_cell(&r), "N");
    r.stage1.real = "unclear".to_string();
    r.stage1.effort = Some("high".to_string());
    assert_eq!(stage1_cell(&r), "? high");
    r.stage1.real = String::new();
    assert_eq!(stage1_cell(&r), "unread");

    let esc = data()
        .rows
        .iter()
        .find(|r| r.markers.contains(&Marker::Escalated))
        .expect("an escalated row");
    assert!(!stage2_cell(esc).is_empty());
    let mut quiet = plain_row();
    if let Some(s) = quiet.stage2.as_mut() {
        s.outlier_reason = None;
    }
    assert_eq!(
        stage2_cell(&quiet),
        "",
        "blank when the group reviewer said nothing"
    );
}

#[test]
fn the_decision_column_says_unresolved_decided_and_inherited() {
    let un = decision_cell(&plain_row());
    assert_eq!(un.label, "Unresolved");
    assert_eq!(un.tone, DecisionTone::Unresolved);
    assert!(!un.inherited);

    let inherited = data()
        .rows
        .iter()
        .find(|r| {
            r.decision
                .as_ref()
                .is_some_and(|d| d.inherited_from.is_some())
        })
        .expect("an inherited decision");
    let cell = decision_cell(inherited);
    assert_eq!(cell.tone, DecisionTone::Decided);
    assert!(cell.inherited);

    let merged = data()
        .rows
        .iter()
        .find(|r| {
            r.decision.as_ref().is_some_and(|d| {
                matches!(
                    d.state,
                    camerata_api_types::decisions::DecisionState::MergedInto(_)
                )
            })
        })
        .expect("a merged row");
    assert!(
        decision_cell(merged).label.starts_with("Merged into "),
        "{}",
        decision_cell(merged).label
    );
}

#[test]
fn severity_badges_follow_the_spec_colours() {
    assert_eq!(severity_badge("critical"), "red");
    assert_eq!(severity_badge("high"), "orange");
    assert_eq!(severity_badge("medium"), "yellow");
    assert_eq!(severity_badge("low"), "gray");
    assert_eq!(severity_badge("info"), "default");
    assert_eq!(severity_label("high"), "High");
}

#[test]
fn short_date_takes_the_month_and_day() {
    assert_eq!(short_date("2026-11-04T09:00:00Z"), "11-04");
    assert_eq!(short_date("yesterday"), "");
}

#[test]
fn nothing_these_pieces_say_contains_an_em_dash() {
    for r in &data().rows {
        let mut text = String::new();
        for c in marker_chips(r) {
            text.push_str(&c.label);
            text.push_str(&c.tip);
        }
        text.push_str(&stage1_cell(r));
        text.push_str(&stage2_cell(r));
        text.push_str(&decision_cell(r).label);
        assert!(!text.contains('\u{2014}'), "{}", r.site_key);
    }
    for n in &data().tree.nodes {
        assert!(!counts_line(&n.counts).contains('\u{2014}'));
    }
}
