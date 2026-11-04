use super::*;
use crate::style::GLOBAL_CSS;
use camerata_ui_core::tree::shell::NO_INSPECTION;

fn page(id: &str) -> String {
    render(id).replace(GLOBAL_CSS, "")
}

#[test]
fn every_shell_state_renders_with_no_em_dash_and_no_raw_nul() {
    for (id, render) in states() {
        let html = render().replace(GLOBAL_CSS, "");
        assert!(!html.contains('\u{2014}'), "{id}");
        assert!(!html.contains('\0'), "{id}");
        assert!(html.contains("review-shell"), "{id}");
    }
    assert_eq!(states().len(), 4);
}

#[test]
fn shell_empty_says_there_is_no_inspection_and_has_no_strip() {
    let html = page("shell/empty");
    assert!(html.contains(NO_INSPECTION), "{html}");
    assert!(!html.contains("progress-strip"));
    assert!(html.contains("Tree"), "the lanes are there");
}

#[test]
fn shell_loading_disables_the_tabs_and_says_so() {
    let html = page("shell/loading");
    assert!(html.contains("Loading the review"));
    assert!(html.contains("disabled"));
}

#[test]
fn shell_error_shows_the_servers_reason_and_a_retry() {
    let html = page("shell/error");
    assert!(html.contains("review-banner"));
    assert!(html.contains("did not answer at 127.0.0.1:8787"), "{html}");
    assert!(html.contains("Retry"));
    assert!(html.contains("role=\"alert\""));
}

#[test]
fn shell_held_count_names_the_lanes_and_draws_held_in_red() {
    let ds = camerata_ui_core::test_support::review_fixtures::dataset_base();
    let html = page("shell/held-count");
    for tab in [
        "Tree",
        "Escalations",
        "Engine Check",
        "Flat view",
        "Settings",
    ] {
        assert!(html.contains(tab), "tab {tab}");
    }
    assert!(
        html.contains("undecided"),
        "the tree tab counts what is undecided"
    );
    assert!(ds.shell.counts.held > 0, "the fixture holds rows");
    assert!(
        html.contains(&format!("strip-held\">Held {}<", ds.shell.counts.held)),
        "{html}"
    );
    assert!(html.contains("Undecided "), "the Queue's own strip words");
    assert!(html.contains("aria-selected=\"true\""), "one tab is active");
}
