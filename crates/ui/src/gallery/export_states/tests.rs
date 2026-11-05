use super::*;
use crate::style::GLOBAL_CSS;
use camerata_ui_core::test_support::review_fixtures::dataset_base;
use camerata_ui_core::tree::export::gate_rows;

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
    assert_eq!(states().len(), 3);
}

#[test]
fn export_blocked_lists_each_held_row_with_an_open_door_and_no_export_button() {
    let ds = dataset_base();
    let html = page("export/blocked");
    let rows = gate_rows(&ds.gate);
    assert!(!rows.is_empty());
    assert!(
        html.contains(&format!(
            "Export blocked: {} rows need a decision before the package can be built.",
            ds.gate.rows_needing_a_decision()
        )),
        "{html}"
    );
    assert_eq!(
        html.matches("class=\"gate-row\"").count(),
        rows.len(),
        "one line per refused row"
    );
    assert_eq!(
        html.matches(">open</button>").count(),
        rows.len(),
        "every line has a door"
    );
    for row in &rows {
        assert!(html.contains(&row.place), "{}", row.place);
    }
    assert!(html.contains("held"), "the held word starts each line");
    assert!(
        !html.contains("Export product</button>"),
        "the button is replaced"
    );
}

#[test]
fn export_clear_keeps_the_export_button_and_shows_no_panel() {
    let html = page("export/clear");
    assert!(html.contains("Export product</button>"), "{html}");
    assert!(!html.contains("Export blocked"));
    assert!(!html.contains("gate-row"));
}

/// Wave 17 soft (a): a put-back row the Engine Check also said No to is one line naming both
/// reasons, and the lines are as many as the headline's rows.
#[test]
fn a_row_two_checks_refuse_is_one_line_with_both_reasons() {
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
    let rows = gate_rows(&body.gate);
    assert_eq!(
        html.matches("class=\"gate-row\"").count(),
        rows.len(),
        "one line per row"
    );
    assert_eq!(
        rows.iter()
            .filter(|r| r.kind != camerata_ui_core::tree::export::GateRowKind::Gap)
            .count(),
        body.gate.rows_needing_a_decision(),
        "the lines count the rows the headline counts"
    );
    assert!(html.contains(&format!(
        "Export blocked: {} rows need a decision",
        body.gate.rows_needing_a_decision()
    )));
    assert!(html.contains("put back after the engine threw it out, no decision; Engine Check said No, no correction yet")
        || html.contains("Engine Check said No, no correction yet; put back after the engine threw it out, no decision"),
        "{html}");
}
