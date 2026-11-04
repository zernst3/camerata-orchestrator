use super::*;
use crate::cockpit::tree::{FOCUSED_ROW_SELECTOR, TREE_REGION_ID};
use crate::style::GLOBAL_CSS;
use camerata_ui_core::test_support::review_fixtures::{dataset_30k, dataset_base};
use camerata_ui_core::tree::shell::{NOT_GROUP_REVIEWED, NO_INSPECTION};
use camerata_ui_core::tree::{counts_line, from_dom_token};

fn page(id: &str) -> String {
    render(id).replace(GLOBAL_CSS, "")
}

/// Every value of `attr="..."` in `html`.
fn attr_values<'a>(html: &'a str, attr: &str) -> Vec<&'a str> {
    let needle = format!("{attr}=\"");
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(at) = rest.find(&needle) {
        let after = &rest[at + needle.len()..];
        let end = after.find('"').unwrap_or(after.len());
        out.push(&after[..end]);
        rest = &after[end..];
    }
    out
}

#[test]
fn every_tree_state_renders_with_no_em_dash_and_no_raw_nul() {
    assert_eq!(states().len(), 9, "every tree state is built");
    for (id, render) in states() {
        let html = render().replace(GLOBAL_CSS, "");
        assert!(!html.contains('\u{2014}'), "{id} has an em dash");
        assert!(!html.contains('\0'), "{id} has a raw NUL");
        assert!(html.contains("tree-lane"), "{id}");
    }
}

#[test]
fn a_rendered_tree_page_has_no_raw_nul_and_each_key_round_trips_from_the_attribute() {
    let ds = dataset_base();
    let html = page("tree/markers");
    assert!(!html.contains('\0'), "no raw NUL anywhere in the page");
    let tokens = attr_values(&html, "data-site");
    assert!(tokens.len() > 20, "{} site rows", tokens.len());
    for token in &tokens {
        assert!(
            token.chars().all(|c| c.is_ascii_hexdigit()),
            "the attribute is a plain token: {token}"
        );
        let key = from_dom_token(token).unwrap_or_else(|| panic!("{token} did not decode"));
        assert!(key.contains('\0'), "decoded back to a real NUL key");
        assert!(ds.row(&key).is_some(), "the key names a site of the run");
    }
    // Element ids built from a key use the same token.
    for id in attr_values(&html, "id")
        .iter()
        .filter(|i| i.starts_with("site-"))
    {
        assert!(from_dom_token(&id["site-".len()..]).is_some(), "{id}");
    }
}

#[test]
fn tree_markers_draws_every_chip_the_indentation_and_the_counts_line() {
    let html = page("tree/markers");
    for class in [
        "mark-held",
        "mark-esc",
        "mark-outlier",
        "mark-new",
        "mark-stale",
        "mark-moved",
        "mark-dup",
        "mark-edited",
    ] {
        assert!(
            html.contains(&format!("class=\"{class}")) || html.contains(&format!("{class} ")),
            "no {class} chip on the markers page"
        );
    }
    // Category at 0, rule at 16px, sub-group at 32px.
    assert!(html.contains("margin-left: 0px"));
    assert!(html.contains("margin-left: 16px"));
    assert!(html.contains("margin-left: 32px"));
    // The counts line of a node: its sites, then its escalations, outliers and decisions.
    let ds = dataset_base();
    let some_node = ds
        .tree
        .nodes
        .iter()
        .find(|n| n.counts.escalated > 0)
        .unwrap();
    let line = counts_line(&some_node.counts);
    // The terms appear as separate spans, so look for each term.
    for term in line.split(" \u{b7} ") {
        assert!(html.contains(term), "no `{term}` in the page");
    }
    assert!(html.contains("count-amber"), "an escalation shows amber");
    assert!(html.contains("count-orange"), "an outlier shows orange");
    // The ticked outlier's checkbox is checked.
    assert!(html.contains("checked"), "{}", "an outlier is ticked");
    assert!(html.contains("pane-keys armed"), "the keys are armed");
}

#[test]
fn tree_30k_pages_a_big_sub_group_with_show_next_500_and_holds_one_page() {
    let ds = dataset_30k();
    let html = page("tree/30k");
    assert!(
        html.contains("Show next 500 of "),
        "{}",
        &html[..html.len().min(400)]
    );
    let rows = html.matches("class=\"site-row").count();
    assert!((400..=500).contains(&rows), "one page of sites, got {rows}");
    // 58 categories and the thrown-out one, all closed except the path to the big sub-group.
    assert_eq!(
        html.matches("margin-left: 0px").count(),
        59,
        "the top level"
    );
    assert!(ds.tree.nodes.len() > 2_800);
    // The tree never renders a closed node's children.
    let node_rows = html.matches("class=\"tree-node").count();
    assert!(node_rows < 100, "{node_rows} node rows on the page");
}

#[test]
fn tree_node_prompt_says_how_many_records_and_what_is_left_out() {
    let html = page("tree/node-prompt");
    assert!(html.contains("This writes "), "{html}");
    assert!(html.contains(" site records."));
    assert!(
        html.contains("excluded unless you tick"),
        "outliers are named"
    );
    assert!(html.contains("held row"), "held rows are named");
    assert!(html.contains("queue-reason-prompt"));
    assert!(html.contains("Commit"));
    assert!(html.contains("Keys are off while you type"));
    assert!(html.contains("(required)"));
}

#[test]
fn tree_node_decided_shows_the_decision_the_undo_bar_and_new_since() {
    let html = page("tree/node-decided");
    assert!(
        html.contains(" rows confirmed.") || html.contains("rows confirmed."),
        "{html}"
    );
    assert!(html.contains("pane-undo"));
    assert!(html.contains("Undo"));
    assert!(
        html.contains("new since"),
        "a member the decision did not cover"
    );
    assert!(
        html.contains("inherited"),
        "a site that took its group's decision"
    );
    assert!(
        html.contains("sites covered"),
        "the node's own decision line"
    );
}

#[test]
fn the_undo_bar_sits_above_both_columns_and_sticks_so_a_narrow_window_cannot_bury_it() {
    for id in ["tree/node-decided", "tree/narrow"] {
        let html = page(id);
        let Some(bar) = html.find("pane-undo\"") else {
            continue;
        };
        let lane = html.find("class=\"tree-lane").expect("the lane");
        assert!(
            bar < lane,
            "{id}: the undo bar is drawn before the lane, not inside the right column that drops under the table at 1000"
        );
        let dock = html.find("tree-undo-dock").expect("the dock");
        assert!(dock < bar, "{id}: the bar is inside the dock");
    }
    let html = page("tree/node-decided");
    assert!(
        html.contains("pane-undo"),
        "node-decided shows the five second undo"
    );
    let rule = GLOBAL_CSS
        .split('}')
        .find(|r| r.contains(".tree-undo-dock"))
        .expect("a rule for the dock");
    assert!(rule.contains("position: sticky"), "{rule}");
}

#[test]
fn the_decision_cell_is_not_cut_by_the_stylesheet_and_has_the_room_for_its_word() {
    // "Confirmed" over "inherited" must read whole: a pattern decision that clips to "Confirmed
    // i" looks like the reviewer's own.
    let rule = |sel: &str| -> String {
        GLOBAL_CSS
            .split('}')
            .find(|r| r.contains(sel))
            .unwrap_or_else(|| panic!("no rule for {sel}"))
            .to_string()
    };
    let cell = rule(".site-table td.col-dec");
    assert!(cell.contains("overflow: visible"), "{cell}");
    assert!(cell.contains("white-space: normal"), "{cell}");
    assert!(
        !cell.contains("ellipsis") || cell.contains("text-overflow: clip"),
        "{cell}"
    );
    assert!(rule(".dec-inherited").contains("display: block"));

    let html = page("tree/markers");
    let widths: Vec<u32> = html
        .split("<col style=\"width: ")
        .skip(1)
        .take(9)
        .filter_map(|c| c.split('%').next()?.parse().ok())
        .collect();
    // Eight percentage columns after the 22px tick, then Decision is the last of them.
    assert_eq!(widths.len(), 8, "{widths:?}");
    assert!(widths[7] >= 16, "Decision gets at least 16%: {widths:?}");
    // Markers need room for two chips to read whole (wave 13 D8: at 12% "new since" and
    // "different object" clipped); Severity and Stage 1 hold their headers (D7).
    assert!(widths[0] >= 15, "two chips read whole: {widths:?}");
    assert!(
        widths[1] >= 9 && widths[5] >= 9,
        "Severity and Stage 1 headers fit: {widths:?}"
    );
    assert!(widths[4] >= 12, "Headline takes the remainder: {widths:?}");
    assert_eq!(widths.iter().sum::<u32>(), 100, "{widths:?}");
}

#[test]
fn tree_markers_opens_the_node_carrying_the_markers_and_puts_it_near_the_top() {
    // The narrow page focuses a site, not a node: its row is the one the cursor is on.
    let narrow = page("tree/narrow");
    assert!(
        attr_values(&narrow, "class")
            .iter()
            .any(|c| c.starts_with("site-row") && c.split(' ').any(|w| w == "focused")),
        "the narrow page has a focused site row"
    );
    for id in ["tree/markers"] {
        let html = page(id);
        let head = html
            .find("class=\"tree-node focused\"")
            .unwrap_or_else(|| panic!("{id}: no focused node"));
        // Its sites are open: the focused node is followed by its table before the next node.
        let rest = &html[head + 10..];
        let next_node = rest.find("class=\"tree-node").unwrap_or(rest.len());
        let mine = &rest[..next_node];
        assert!(
            mine.contains("site-table"),
            "{id}: the focused node is open"
        );
        for chip in [
            "mark-held",
            "mark-stale",
            "mark-moved",
            "mark-outlier",
            "mark-esc",
        ] {
            assert!(mine.contains(chip), "{id}: the focused node shows {chip}");
        }
        // Near the top: a row is 30 to 45px, so twelve rows above it leave its own rows, and the
        // chips on them, inside the 1400px the screenshots are taken at.
        let rows_above = html[..head].matches("class=\"site-row").count();
        assert!(
            rows_above <= 12,
            "{id}: {rows_above} site rows sit above the focused node"
        );
    }
    // The live app scrolls the focused row into view; the selector names rows the tree draws.
    let html = page("tree/markers");
    for class in ["tree-node focused", "site-row"] {
        assert!(html.contains(class));
    }
    assert!(FOCUSED_ROW_SELECTOR.contains(".tree-node.focused"));
    assert!(FOCUSED_ROW_SELECTOR.contains(".site-row.focused"));
    assert!(FOCUSED_ROW_SELECTOR.contains(TREE_REGION_ID));
}

#[test]
fn tree_not_group_reviewed_says_so_and_shows_no_summary() {
    let html = page("tree/not-group-reviewed");
    assert!(html.contains(NOT_GROUP_REVIEWED));
    assert!(html.contains("review-note"));
    // The banner says there are no summaries; the node pane must not show one.
    assert!(html.contains("No summary for this node."), "{html}");
    assert!(
        !html.contains("Summary (model)"),
        "a model summary cannot exist when the group review did not run"
    );
}

#[test]
fn tree_empty_loading_and_error_are_the_three_states_of_the_left_column() {
    let empty = page("tree/empty");
    assert!(empty.contains(NO_INSPECTION), "{empty}");
    assert_eq!(
        empty.matches(NO_INSPECTION).count(),
        1,
        "the tree says it once; the shell leaves it to the lane"
    );
    assert!(!empty.contains("tree-region"), "nothing to move through");
    assert!(
        !empty.contains("Select a node or a site"),
        "the right pane is blank"
    );

    let loading = page("tree/loading");
    assert!(loading.contains("Loading the tree"));
    assert_eq!(
        loading.matches("tree-skel-row").count(),
        5,
        "five gray rows"
    );

    let error = page("tree/error");
    assert!(error.contains("review-banner"));
    assert!(error.contains("The tree request failed with status 500."));
    assert!(error.contains("Retry"));
}

#[test]
fn tree_narrow_stacks_the_pane_under_the_tree_with_a_site_focused() {
    let html = page("tree/narrow");
    assert!(html.contains("tree-lane narrow"), "{html}");
    assert!(html.contains("site-pane-head"), "a site is focused");
    assert!(html.contains("Include this outlier") || html.contains("Headline"));
}

#[test]
fn a_fully_decided_node_has_its_green_check_and_an_undecided_one_does_not() {
    let html = page("tree/node-decided");
    assert!(
        html.contains("tree-check"),
        "a fully decided node has its check"
    );
    // Counts only: a node with sites left undecided draws no check.
    let ds = dataset_base();
    let whole = ds
        .tree
        .nodes
        .iter()
        .filter(|n| camerata_ui_core::tree::is_fully_decided(&n.counts))
        .count();
    assert!(html.matches("tree-check").count() <= whole);
}

#[test]
fn a_decided_row_and_an_unresolved_row_read_differently() {
    let html = page("tree/node-decided");
    assert!(html.contains("dec-decided"));
    assert!(html.contains("dec-unresolved"));
}
