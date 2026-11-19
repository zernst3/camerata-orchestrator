use super::*;
use crate::style::GLOBAL_CSS;
use camerata_ui_core::test_support::review_fixtures::dataset_base;

fn page(id: &str) -> String {
    render(id).replace(GLOBAL_CSS, "")
}

#[test]
fn every_export_state_renders_with_no_em_dash_and_no_raw_nul() {
    for (id, render) in states() {
        let html = render().replace(GLOBAL_CSS, "");
        assert!(!html.contains('\u{2014}'), "{id}");
        assert!(!html.contains('\0'), "{id}");
    }
    assert_eq!(states().len(), 6);
}

#[test]
fn export_blocked_is_one_line_with_a_button_and_no_per_row_list() {
    let ds = dataset_base();
    let html = page("export/blocked");
    assert!(ds.gate.is_blocked());
    // The held-rows-only state counts the held rows alone (no group-decision rows).
    assert!(
        html.contains(&format!(
            "Export blocked: {} held rows need a decision",
            ds.gate.held.len()
        )),
        "{html}"
    );
    assert_eq!(html.matches("role=\"alert\"").count(), 1, "one line");
    assert!(
        !html.contains("gate-row"),
        "the per-row list is gone: {html}"
    );
    assert!(
        !html.contains(">open</button>"),
        "the per-row open buttons are gone"
    );
    assert!(html.contains(">Show them</button>"), "{html}");
    assert!(
        !html.contains("Export product</button>"),
        "the button is replaced"
    );
}

/// A run with forty held rows: the panel is still one line. Three of the rows are only in Review,
/// so the line says so and offers a second button.
#[test]
fn export_blocked_many_is_still_one_line_whatever_the_count() {
    let html = page("export/blocked-many");
    assert!(
        html.contains("Export blocked: 40 held rows need a decision (3 are in Review)."),
        "{html}"
    );
    assert_eq!(html.matches("role=\"alert\"").count(), 1);
    assert!(!html.contains("gate-row"), "no per-row list");
    assert!(!html.contains(">open</button>"));
    assert!(html.contains(">Show them</button>"));
    assert!(html.contains(">Open Review</button>"));
}

/// The shell strip above the many-rows state counts the rows the line counts (it once said 11).
#[test]
fn export_blocked_many_strip_agrees_with_the_line() {
    let html = page("export/blocked-many");
    assert!(html.contains("Held 40"), "the strip says Held 40: {html}");
    assert!(!html.contains("Held 11"), "{html}");
}

/// The two buttons of the export line sit a normal gap apart, and a held row's edge is its own
/// rule, wider and darker than the critical highlight's, so the two read apart.
#[test]
fn the_held_styles_are_their_own_rules() {
    let css = GLOBAL_CSS;
    assert!(
        css.contains(
            ".export-blocked { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; }"
        ),
        "the line's buttons are 8px apart"
    );
    assert!(
        css.contains(".finding-row-held > td:first-child { box-shadow: inset 7px 0 0 0 #8f1d1d,"),
        "the held edge is its own wider, darker rule"
    );
    assert!(
        css.contains(
            ".finding-row-critical > td:first-child { box-shadow: inset 4px 0 0 0 #d4332b; }"
        ),
        "the critical edge is unchanged and not shared"
    );
    assert!(css.contains(".finding-row-held > td { background: repeating-linear-gradient("));
}

/// The held-rows pages render the app's own table container, with no gallery wrapper: it fills
/// the width it is given and scrolls inside its own column from the left edge, so the Held column
/// the table adds is not cut off. The real container's rules are pinned here and the real screen
/// is checked in `held_rows_tests`.
#[test]
fn the_findings_pages_use_the_real_container_that_scrolls_from_the_left_edge() {
    for id in ["findings/held-rows", "findings/held-filter-on"] {
        let html = page(id);
        assert!(
            !html.contains("held-gallery"),
            "{id}: no gallery-only wrapper"
        );
        assert!(html.contains("class=\"queue-split\""), "{id}");
        assert!(html.contains("queue-split-table"), "{id}");
        assert!(html.contains(">Held</"), "{id}: the Held header is drawn");
        assert!(html.contains("Held: decide before export"), "{id}");
        assert!(html.contains("finding-row-held"), "{id}");
    }
    let css = GLOBAL_CSS;
    assert!(!css.contains(".held-gallery"));
    assert!(css.contains(".queue-split { display: flex; align-items: flex-start; gap: 12px; width: 100%; min-width: 0; align-self: stretch; box-sizing: border-box; }"));
    assert!(css.contains(".queue-split-table { flex: 1 1 auto; min-width: 0; overflow-x: auto; }"));
    assert!(
        css.contains(".finding-pane-empty { display: none; }"),
        "an empty pane draws nothing when stacked"
    );
}

/// The triage table with held rows: the red cell and its words, and the panel's one line above.
#[test]
fn findings_held_rows_marks_the_held_rows_red_with_words() {
    let html = page("findings/held-rows");
    assert!(
        html.contains("Export blocked: 5 held rows need a decision."),
        "{html}"
    );
    assert!(html.matches("class=\"held-status\"").count() >= 1, "{html}");
    assert!(html.contains("Held: decide before export"), "{html}");
    assert!(html.contains("finding-row-held"), "{html}");
}

/// With "Show them" applied the table holds the held rows and no other. The gallery's held rows
/// are findings 1, 3, 4, 7 and 10 of twelve.
#[test]
fn findings_held_filter_on_shows_only_the_held_rows() {
    let off = page("findings/held-rows");
    let on = page("findings/held-filter-on");
    let held = [1usize, 3, 4, 7, 10];
    let shown = |html: &str, i: usize| html.contains(&format!("call_{i}()"));
    for i in 0..12usize {
        if !held.contains(&i) {
            assert!(
                shown(&off, i),
                "anti-vacuity: row {i} is in the unfiltered table"
            );
            assert!(
                !shown(&on, i),
                "row {i} is not held and must not be shown: {on}"
            );
        }
    }
    for i in held {
        assert!(shown(&on, i), "held row {i} is shown: {on}");
    }
    assert_eq!(
        on.matches("class=\"held-status\"").count(),
        held.len(),
        "one red cell per held row"
    );
}

#[test]
fn export_clear_keeps_the_export_button_and_shows_no_panel() {
    let html = page("export/clear");
    assert!(html.contains("Export product</button>"), "{html}");
    assert!(!html.contains("Export blocked"));
    assert!(!html.contains("gate-row"));
    assert!(!html.contains("Show them"));
}

/// Wave 17 soft (a): a put-back row the Engine Check also said No to is ONE row. The line counts
/// it once, and the table's status cell carries both reasons (checked on the gate's own words in
/// `camerata_ui_core::tree::export`).
#[test]
fn a_row_two_checks_refuse_is_counted_once_and_names_both_reasons() {
    let html = page("export/one-line-two-reasons");
    let fx =
        camerata_ui_core::test_support::review_fixtures::fixture("export/one-line-two-reasons")
            .unwrap();
    let camerata_ui_core::test_support::review_fixtures::Body::Export(body) = fx.body else {
        panic!("not an export state");
    };
    let doubled: Vec<_> = body
        .gate
        .held
        .iter()
        .map(|h| h.key.as_str())
        .filter(|k| body.gate.held.iter().filter(|h| h.key == *k).count() == 2)
        .collect();
    assert_eq!(
        doubled.len(),
        2,
        "the fixture holds one row for two reasons"
    );
    assert!(html.contains(&format!(
        "Export blocked: {} held rows need a decision",
        body.gate.rows_needing_a_decision()
    )));
    let reasons = camerata_ui_core::tree::export::held_reasons(&body.gate);
    let both = &reasons[doubled[0]];
    assert!(
        both.contains("put back after the engine threw it out, no decision")
            && both.contains("Engine Check said No, no correction yet"),
        "{both}"
    );
}

/// The export panel's "?" help button is drawn in every export state (the gallery once drew the
/// heading as bare text, so no blocked or clear state showed it).
#[test]
fn every_export_state_shows_the_help_button() {
    for id in [
        "export/clear",
        "export/blocked",
        "export/one-line-two-reasons",
        "export/blocked-many",
    ] {
        let html = page(id);
        assert!(html.contains("aria-label=\"About Export product\""), "{id}");
        assert!(html.contains("class=\"help-button\""), "{id}");
    }
}

/// `export/blocked` is the held-rows-only state: one reason, no group-decision clause.
#[test]
fn export_blocked_is_held_rows_only() {
    let html = page("export/blocked");
    assert!(html.contains("Export blocked: "), "{html}");
    assert!(!html.contains("group decision"), "{html}");
    assert!(!html.contains("missing records"), "{html}");
}
