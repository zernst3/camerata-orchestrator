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
    assert!(
        html.contains(&format!(
            "Export blocked: {} rows need a decision",
            ds.gate.rows_needing_a_decision()
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
        html.contains("Export blocked: 40 rows need a decision (3 are in Review)."),
        "{html}"
    );
    assert_eq!(html.matches("role=\"alert\"").count(), 1);
    assert!(!html.contains("gate-row"), "no per-row list");
    assert!(!html.contains(">open</button>"));
    assert!(html.contains(">Show them</button>"));
    assert!(html.contains(">Open Review</button>"));
}

/// The triage table with held rows: the red cell and its words, and the panel's one line above.
#[test]
fn findings_held_rows_marks_the_held_rows_red_with_words() {
    let html = page("findings/held-rows");
    assert!(
        html.contains("Export blocked: 5 rows need a decision."),
        "{html}"
    );
    assert!(html.matches("class=\"held-status\"").count() >= 1, "{html}");
    assert!(html.contains("Needs a decision"), "{html}");
    assert!(html.contains("finding-row-held"), "{html}");
}

/// With "Show them" applied the table holds the held rows and no other.
#[test]
fn findings_held_filter_on_shows_only_the_held_rows() {
    let off = page("findings/held-rows");
    let on = page("findings/held-filter-on");
    let rows_of = |html: &str| html.matches("call_").count();
    assert!(
        rows_of(&on) < rows_of(&off),
        "the filter removes rows: {} against {}",
        rows_of(&on),
        rows_of(&off)
    );
    assert_eq!(
        on.matches("class=\"held-status\"").count(),
        on.matches("finding-row-held").count()
    );
    assert!(
        !on.contains("Not held"),
        "every row left is a held row: {on}"
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
        "Export blocked: {} rows need a decision",
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
