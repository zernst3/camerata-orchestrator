use super::*;
use crate::style::GLOBAL_CSS;
use camerata_api_types::decisions::DecisionState;
use camerata_ui_core::edits::{ack_line, decision_line};
use camerata_ui_core::test_support::review_fixtures::dataset_base;

fn page(id: &str) -> String {
    render(id).replace(GLOBAL_CSS, "")
}

fn site_of(id: &str) -> SiteRow {
    match fixture(id).expect("a fixture").body {
        Body::Detail(b) => b.site.expect("a site"),
        _ => panic!("not a detail state"),
    }
}

/// The text a page shows, the way a reader sees it: apostrophes and angle brackets unescaped.
fn plain_text(html: &str) -> String {
    html.replace("&#39;", "'")
        .replace("&#62;", ">")
        .replace("&#60;", "<")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
}

#[test]
fn every_detail_state_renders_with_no_em_dash_and_no_raw_nul() {
    assert_eq!(states().len(), 24, "every detail state is built");
    for (id, render) in states() {
        let html = render().replace(GLOBAL_CSS, "");
        assert!(!html.contains('\u{2014}'), "{id} has an em dash");
        assert!(!html.contains('\0'), "{id} has a raw NUL");
        assert!(html.contains("site-detail"), "{id}");
        // The shell strip is above the pane, as in every lane.
        assert!(html.contains("review-shell"), "{id}");
    }
}

#[test]
fn detail_empty_says_select_a_site() {
    let html = page("detail/empty");
    assert!(
        html.contains("pane-empty") && html.contains("Select a site"),
        "{html}"
    );
    assert!(!html.contains("site-detail-head"));
}

#[test]
fn detail_plain_has_the_whole_pane_and_none_of_the_marks() {
    let row = site_of("detail/plain");
    let html = plain_text(&page("detail/plain"));
    let crumbs = context_of(&row).crumbs;
    assert_eq!(crumbs.len(), 3);
    for want in [
        row.rule_id.as_str(),
        row.headline.as_str(),
        row.path.as_str(),
        crumbs[0].as_str(),
        crumbs[2].as_str(),
        "Stage 1",
        "Unresolved",
        "Merge into",
        "Add finding",
    ] {
        assert!(html.contains(want), "no `{want}`");
    }
    assert_eq!(html.matches("site-detail-edit").count(), 9);
    for absent in [
        "mark-stale",
        "mark-moved",
        "mark-held",
        "site-detail-held",
        "site-detail-form",
    ] {
        assert!(!html.contains(absent), "{absent}");
    }
}

#[test]
fn detail_escalated_shows_the_esc_chip_and_the_stage_2_line() {
    let html = plain_text(&page("detail/escalated"));
    assert!(html.contains("mark-esc"), "{html}");
    assert!(html.contains(">Stage 2<"));
    assert!(html.contains("escalated, "), "{html}");
}

#[test]
fn detail_outlier_shows_the_chip_the_reason_and_the_tick() {
    let html = plain_text(&page("detail/outlier"));
    assert!(html.contains("mark-outlier"));
    assert!(html.contains("outlier: "), "the stage 2 line names it");
    assert!(html.contains("Include this outlier in the next batch decision"));
}

#[test]
fn detail_edited_field_shows_the_mark_the_hover_and_the_undo_bar() {
    let row = site_of("detail/edited-field");
    let html = plain_text(&page("detail/edited-field"));
    assert!(html.contains("mark-edited"));
    let e = row.edits.iter().find(|e| !e.stale).unwrap();
    assert!(html.contains("Was: "), "{html}");
    assert!(html.contains(&e.reason));
    assert!(
        html.contains("pane-undo") && html.contains("Undo edit"),
        "{html}"
    );
}

#[test]
fn detail_stale_edit_shows_the_second_line_with_reapply_and_discard() {
    let row = site_of("detail/stale-edit");
    let stale = row.edits.iter().find(|e| e.stale).unwrap();
    let html = plain_text(&page("detail/stale-edit"));
    assert!(html.contains("mark-stale"));
    assert!(
        html.contains(&format!("Your edit (stale): {}", stale.new)),
        "{html}"
    );
    assert!(html.contains("Reapply") && html.contains("Discard"));
}

#[test]
fn detail_stale_decision_reads_what_it_was_and_offers_reapply_and_discard() {
    let row = site_of("detail/stale-decision");
    let ctx = context_of(&row);
    let line = decision_line(&row, &ctx);
    let html = plain_text(&page("detail/stale-decision"));
    assert!(html.contains(&line.text), "{html}");
    assert!(html.contains("stale: was "));
    assert!(html.contains("Reapply") && html.contains("Discard"));
}

#[test]
fn detail_moved_says_where_the_row_was() {
    let row = site_of("detail/moved");
    let html = plain_text(&page("detail/moved"));
    assert!(html.contains("mark-moved"));
    if let Some(line) = row.moved_from.as_ref().and_then(|m| m.line) {
        assert!(html.contains(&format!("moved, was line {line}")), "{html}");
    }
}

#[test]
fn detail_new_since_decision_says_when_and_how_many_others() {
    let html = plain_text(&page("detail/new-since-decision"));
    assert!(html.contains("mark-new"));
    assert!(html.contains("new since you "), "{html}");
    assert!(html.contains(" this group on "));
}

#[test]
fn detail_held_has_the_red_line() {
    let html = plain_text(&page("detail/held"));
    assert!(
        html.contains("Held from export: evidence unverified. Decide it to release."),
        "{html}"
    );
    assert!(html.contains("site-detail-held") && html.contains("mark-held"));
}

#[test]
fn detail_merged_points_at_the_target_and_the_row_is_still_drawn() {
    let row = site_of("detail/merged");
    let target = row
        .merged_into
        .as_ref()
        .expect("a merge pointer")
        .label
        .clone();
    let html = plain_text(&page("detail/merged"));
    assert!(html.contains(&format!("Merged into {target}")), "{html}");
    assert!(html.contains(&row.headline));
}

#[test]
fn detail_added_carries_the_edit_mark_and_says_it_was_added() {
    let html = plain_text(&page("detail/added"));
    assert!(html.contains("mark-edited"));
    assert!(html.contains("Finding added."), "{html}");
}

#[test]
fn detail_ignored_rows_show_the_client_line_with_and_without_the_acknowledgement() {
    let needs = site_of("detail/ignored-needs-ack");
    let html = plain_text(&page("detail/ignored-needs-ack"));
    assert_eq!(
        ack_line(&needs).as_deref(),
        Some("Needs client confirmation")
    );
    assert!(html.contains("Needs client confirmation") && html.contains("record acknowledgement"));

    let acked = site_of("detail/ignored-acked");
    let html = plain_text(&page("detail/ignored-acked"));
    let line = ack_line(&acked).unwrap();
    assert!(line.starts_with("Accepted risk"));
    assert!(html.contains(&line), "{html}");
    assert!(!html.contains("record acknowledgement"));
    assert!(html.contains("Confirmed by "), "who and when");
    assert_eq!(
        acked.decision.as_ref().map(|d| d.state.clone()),
        Some(DecisionState::Ignored)
    );
}

#[test]
fn detail_edit_open_swaps_the_fix_for_its_control_a_reason_and_commit() {
    let row = site_of("detail/edit-open");
    let html = plain_text(&page("detail/edit-open"));
    assert!(
        html.contains("site-detail-form") && html.contains("Edit the fix"),
        "{html}"
    );
    assert!(html.contains("<textarea"));
    assert!(
        html.contains(&row.fix),
        "the control starts on the current text"
    );
    assert!(html.contains("(required)") && html.contains("Commit") && html.contains("Cancel"));
}

#[test]
fn detail_merge_picker_lists_sites_and_asks_for_a_reason() {
    let html = plain_text(&page("detail/merge-picker"));
    assert!(html.contains("site-detail-merge-form"));
    assert!(html.contains("the row stays in the tree"));
    assert!(html.matches("site-detail-candidate").count() > 3, "{html}");
    assert!(html.contains("Why these are one problem (required)"));
}

#[test]
fn detail_split_picker_offers_the_rules_sub_groups_and_a_new_one() {
    let html = plain_text(&page("detail/split-picker"));
    assert!(html.contains("site-detail-split-form"));
    assert!(html.contains("Move this site to a sub-group"));
    assert!(html.contains("New sub-group"));
    let ds = dataset_base();
    let row = site_of("detail/split-picker");
    let sibling = ds
        .tree
        .nodes
        .iter()
        .find(|n| {
            n.parent.as_deref() == row.group_path.get(1).map(String::as_str)
                && Some(&n.id) != row.group_path.get(2)
        })
        .map(|n| n.label.clone());
    if let Some(label) = sibling {
        assert!(
            html.contains(&label),
            "another sub-group of the rule is listed"
        );
    }
}

#[test]
fn detail_add_form_is_prefilled_with_the_site_and_the_code_read_only() {
    let row = site_of("detail/add-form");
    let html = plain_text(&page("detail/add-form"));
    assert!(html.contains("site-detail-add-form"));
    assert!(html.contains(&row.path) && html.contains(&row.repo));
    assert!(
        html.contains(&format!("value=\"{}\"", row.line)),
        "the line is prefilled"
    );
    assert!(html.contains("The code at that line (read-only)"));
    assert!(html.contains("Why you are adding it (required)"));
}

#[test]
fn detail_add_refused_shows_the_gates_reason_in_the_form() {
    let row = site_of("detail/add-refused");
    let html = plain_text(&page("detail/add-refused"));
    assert!(html.contains("site-detail-add-form"));
    let reason = format!(
        "A finding for {} already exists at {}:{}. Edit that one instead.",
        row.rule_id, row.path, row.line
    );
    assert!(html.contains(&reason), "{html}");
    assert!(html.contains("site-detail-error"));
    // What was typed is still there.
    assert!(html.contains("Second copy of the key"));
}

/// Waves 16 and 17: the possible-duplicate line, the duplicate-first merge pick, the over-budget
/// chip and the commit in the viewer's header had no state.
#[test]
fn detail_possible_duplicate_shows_the_link_and_its_merge_door() {
    let row = site_of("detail/possible-duplicate");
    let dup = row.possible_duplicate_of.clone().expect("a duplicate link");
    let html = plain_text(&page("detail/possible-duplicate"));
    assert!(html.contains("Possible duplicate of"), "{html}");
    assert!(html.contains(&dup.rule_id), "{}", dup.rule_id);
    assert!(html.contains(&format!("{}:{}", dup.path, dup.line)));
    assert!(html.contains("Merge this row into the possible duplicate"));
}

#[test]
fn detail_merge_picker_lists_the_possible_duplicate_first() {
    let row = site_of("detail/merge-picker-duplicate-first");
    let dup = row.possible_duplicate_of.clone().expect("a duplicate link");
    let html = plain_text(&page("detail/merge-picker-duplicate-first"));
    let first_item = html.find("site-detail-candidate").expect("a candidate");
    let list = &html[first_item..];
    let note = list.find("possible duplicate").expect("the duplicate note");
    let label = list
        .find(&format!("{}:{}", dup.path, dup.line))
        .expect("its label");
    let next = list[label + 1..].find("site-detail-candidate");
    assert!(
        next.is_none_or(|n| label < label + 1 + n),
        "the duplicate is the first item"
    );
    assert!(note < label + 400, "its note sits on that item");
}

#[test]
fn detail_over_budget_shows_the_over_chip() {
    let row = site_of("detail/over-budget");
    assert!(row
        .markers
        .contains(&camerata_api_types::review_view::Marker::OverBudget));
    let html = page("detail/over-budget");
    assert!(html.contains("mark-over"), "{html}");
}

/// Wave 17: no state showed a commit in the viewer header. The page opens the viewer on a site
/// with the commit the run inspected its repo at, and the header names its first seven characters.
#[test]
fn detail_viewer_commit_names_the_commit_the_run_inspected() {
    let fx = fixture("detail/viewer-commit").unwrap();
    let shell = fx.shell.ready().expect("a shell").clone();
    let Body::Detail(body) = fx.body else {
        panic!("not a detail state");
    };
    assert!(body.viewer);
    let row = body.site.expect("a site");
    let sha = shell
        .audited_shas
        .get(&row.repo)
        .expect("the run recorded a commit for the repo");
    let html = page("detail/viewer-commit");
    assert!(
        html.contains(&format!("@ {}", camerata_ui_core::source::short_sha(sha))),
        "the header names the commit: {html}"
    );
    // A page with no viewer carries no revision.
    assert!(!page("detail/plain").contains("cv-rev"));
}

/// Wave 16 soft: the meta grid fitted as many 230px columns as the pane had, so four items drew
/// three and sent Sub-group to a row of its own (and cramped Category at both widths). Four items
/// are two columns of two at every width.
#[test]
fn the_meta_grid_is_two_columns_so_four_items_never_leave_one_alone() {
    let html = page("detail/plain");
    let meta = html.find("site-detail-meta").expect("the meta grid");
    let end = html[meta..]
        .find("</div></div>")
        .map_or(html.len(), |e| meta + e);
    let items = html[meta..end]
        .matches("class=\"site-detail-item\"")
        .count();
    assert_eq!(items % 2, 0, "an even number of items fills two columns");
    assert_eq!(items, 4, "rule, kind, category and sub-group");
    let css: String = GLOBAL_CSS.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        css.contains(
            ".site-detail-meta { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr));"
        ),
        "two columns at every width"
    );
}

/// Wave 19 D1: a thrown-out row offers its two real choices and none the server would refuse.
#[test]
fn detail_thrown_out_offers_put_it_back_and_confirm_thrown_out_only() {
    let row = site_of("detail/thrown-out");
    assert!(row.thrown_out_reason.is_some());
    let html = plain_text(&page("detail/thrown-out"));
    assert!(html.contains("Put it back into review"), "{html}");
    assert!(html.contains("Confirm thrown out"));
    for refused in [
        ">Confirmed<",
        ">Ignored<",
        ">Tech debt<",
        "pane-dispose",
        "Merge into",
    ] {
        assert!(!html.contains(refused), "{refused}");
    }
}
