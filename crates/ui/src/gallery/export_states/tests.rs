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
    assert_eq!(states().len(), 2);
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
