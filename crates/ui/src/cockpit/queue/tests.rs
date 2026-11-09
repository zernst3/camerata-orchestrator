//! The queue host's reads and writes.

use camerata_api_types::review_view::QUEUE_BELOW_PAGE_SIZE;
use camerata_ui::review_io::{RecordingWrites, ReviewWrites, Written};
use camerata_ui::review_source::FixtureReviewSource;
use camerata_ui_core::queue::{layout, needed_me_write, Entry, QueueState};
use camerata_ui_core::test_support::review_fixtures::dataset_base;

use camerata_ui_core::triage::PaneFocus;

use super::*;
use crate::style::GLOBAL_CSS;
use crate::test_harness;

#[tokio::test]
async fn lower_the_line_reveals_candidates_without_a_network_call_or_a_write() {
    let reads = FixtureReviewSource::base();
    // The host reads the queue once, with the first page of candidates below the line.
    let view = read_queue(&reads).await.unwrap();
    assert!(view.show_below && !view.below.is_empty());
    assert_eq!(reads.calls(), ["queue"]);

    // Showing and hiding them is a flag on the view state: nothing is read and nothing written.
    let mut state = QueueState::default();
    let hidden = layout(&view, &state).row_count();
    state.toggle_below();
    let shown = layout(&view, &state);
    assert_eq!(shown.row_count(), hidden + view.below.len());
    assert!(shown
        .entries
        .iter()
        .any(|e| matches!(e, Entry::BelowHeading(_))));
    state.toggle_below();
    assert_eq!(layout(&view, &state).row_count(), hidden);
    assert_eq!(reads.calls(), ["queue"], "no request was sent to draw them");
    assert!(
        reads.recorded.log.borrow().is_empty(),
        "and nothing was written"
    );
}

#[tokio::test]
async fn more_candidates_are_a_read_of_the_next_page_never_a_write() {
    let reads = FixtureReviewSource::scale_30k();
    let first = read_queue(&reads).await.unwrap();
    assert_eq!(
        first.below.len().min(QUEUE_BELOW_PAGE_SIZE),
        first.below.len()
    );
    if first.below_total > first.below.len() {
        let more = read_more_below(&reads, &first).await.unwrap();
        assert!(more.below.len() > first.below.len());
        assert_eq!(
            more.escalated, first.escalated,
            "the escalated rows are untouched"
        );
        assert_eq!(&more.below[..first.below.len()], &first.below[..]);
    }
    assert!(reads.calls().iter().all(|c| c == "queue"));
    assert!(reads.recorded.log.borrow().is_empty());
}

#[tokio::test]
async fn needed_me_sets_the_flag_on_the_decision_and_only_for_a_decided_row() {
    let view = dataset_base().queue(false, 0, 100);
    let writes = RecordingWrites::default();
    let undecided = view
        .escalated
        .iter()
        .find(|r| r.site.decision.is_none())
        .expect("an undecided escalated row");
    assert_eq!(needed_me_write(undecided), None, "nothing to flag yet");

    let decided = view
        .escalated
        .iter()
        .find(|r| {
            r.site
                .decision
                .as_ref()
                .is_some_and(|d| d.needed_me != Some(true))
        })
        .expect("a decided row that is not marked");
    let (id, value) = needed_me_write(decided).expect("enabled once decided");
    assert!(value);
    let done = writes.set_needed_me(&id, value).await.unwrap();
    assert_eq!(done.needed_me, Some(true));
    assert_eq!(
        writes.log.borrow().as_slice(),
        [Written::NeededMe(id, true)]
    );
}

#[test]
fn the_door_a_row_opens_is_its_site_in_the_tree() {
    let view = dataset_base().queue(false, 0, 100);
    let row = &view.escalated[0];
    match camerata_ui_core::queue::site_door(row) {
        Door::Site { key, node_id, .. } => {
            assert_eq!(key, row.candidate.key);
            assert_eq!(Some(&node_id), row.site.group_path.get(2));
        }
        other => panic!("{other:?}"),
    }
}

// ── The site detail in the queue (spec section 8: the right pane for a focused site, in every
// lane) ────────────────────────────────────────────────────────────────────────────────────

#[derive(Clone, PartialEq, Props)]
struct PaneCase {
    view: QueueView,
    cursor: String,
}

#[component]
fn PaneHarness(props: PaneCase) -> Element {
    rsx! {
        QueueScreen {
            view: Some(props.view.clone()),
            status: QueueStatus::Ready,
            state: QueueState {
                cursor: Some(props.cursor.clone()),
                ..QueueState::default()
            },
            focus: PaneFocus::Cursor,
            prompt: None,
            undo: None,
        }
    }
}

/// The right pane of a queue whose cursor is on `row`, without the stylesheet.
fn pane_of(view: &QueueView, row: &QueueRow) -> String {
    let html = test_harness::render_props(
        PaneHarness,
        PaneCase {
            view: view.clone(),
            cursor: row.candidate.key.clone(),
        },
    )
    .replace(GLOBAL_CSS, "");
    let at = html
        .find("review-queue-right")
        .expect("the queue draws its right pane");
    html[at..].to_string()
}

#[test]
fn a_queue_row_opens_the_site_detail_with_that_sites_fields() {
    let view = dataset_base().queue(false, 0, 100);
    // A headline with nothing the page would escape, so it can be found as written.
    let row = view
        .escalated
        .iter()
        .find(|r| !r.site.headline.contains(['&', '<', '>', '"', '\'']))
        .expect("an escalated row with a plain headline");
    let pane = pane_of(&view, row);

    // The same pane the tree opens: the head with the rule, the crumb, the fields, the doors.
    assert!(pane.contains("class=\"site-detail\""), "{pane}");
    assert!(pane.contains("site-detail-head") && pane.contains("site-detail-crumb"));
    assert!(pane.contains("site-detail-code"));
    assert!(
        pane.contains(&format!(">{}</span>", row.site.rule_id)),
        "the rule id is in the head"
    );
    assert!(pane.contains(&row.site.headline), "the headline is a field");
    assert!(
        pane.contains(&file_line(&row.site)),
        "the file and line are there"
    );
    assert!(
        pane.contains("site-detail-edit"),
        "every field carries its edit"
    );
    assert!(pane.contains("site-detail-merge") && pane.contains("site-detail-add"));
    // Its position is its rank in the queue, not a place in a sub-group.
    assert!(
        pane.contains(&format!(
            "rank {}",
            thousands(row.candidate.global_rank as usize)
        )),
        "{pane}"
    );

    // The detail draws the four decisions once; the queue's own pane no longer draws them again.
    assert_eq!(pane.matches("pane-dispose").count(), 4, "{pane}");

    // What is the queue's own stays: why the row is here, and the doors to the tree.
    assert!(pane.contains("Escalated for"), "{pane}");
    assert!(pane.contains("Open in the tree"));
    assert!(pane.contains("Go to the node"));
}

#[test]
fn the_queues_detail_does_not_offer_a_batch_tick_the_queue_has_no_batch_for() {
    let mut view = dataset_base().queue(false, 0, 100);
    // The fixture has no escalated outlier, so the first row is made one.
    view.escalated[0]
        .site
        .markers
        .push(camerata_api_types::review_view::Marker::Outlier);
    let outlier = &view.escalated[0];
    assert!(camerata_ui_core::tree::is_outlier(&outlier.site));
    let pane = pane_of(&view, outlier);
    assert!(pane.contains("site-detail"), "the detail is there");
    assert!(
        !pane.contains("Include this outlier in the next batch decision"),
        "{pane}"
    );
}

#[test]
fn the_queues_detail_has_no_stack_button_the_stacked_view_is_not_a_queue_key() {
    let view = dataset_base().queue(false, 0, 100);
    let pane = pane_of(&view, &view.escalated[0]);
    assert!(pane.contains("site-detail-code"), "the code door stays");
    assert!(!pane.contains("site-detail-stack"), "{pane}");
}

#[derive(Clone, PartialEq, Props)]
struct PromptCase {
    view: QueueView,
    cursor: String,
    prompt: QueuePrompt,
}

#[component]
fn PromptHarness(props: PromptCase) -> Element {
    rsx! {
        QueueScreen {
            view: Some(props.view.clone()),
            status: QueueStatus::Ready,
            state: QueueState {
                cursor: Some(props.cursor.clone()),
                ..QueueState::default()
            },
            focus: PaneFocus::ReasonPrompt,
            prompt: Some(props.prompt.clone()),
            undo: None,
        }
    }
}

#[test]
fn a_place_prompt_in_the_queue_names_every_rule_and_asks_for_a_reason_on_this_place() {
    use camerata_api_types::decisions::DecisionState;
    use camerata_api_types::review_view::CarriedRuleView;
    let mut view = dataset_base().queue(false, 0, 100);
    {
        let site = &mut view.escalated[0].site;
        site.decision = None;
        site.held = None;
        site.place_id = "place:0123456789ab".to_string();
        site.carries = vec![CarriedRuleView {
            rule_id: "SEC-OTHER-9".to_string(),
            site_key: "carried-key".to_string(),
            node_id: "sub:SEC-OTHER-9/0".to_string(),
            severity: "low".to_string(),
            kind: Some(camerata_api_types::audit::FindingKind::Defect),
            held: None,
            stage1_real: "yes".to_string(),
            escalation_reason: None,
            escalation_rank: None,
            decision: None,
        }];
    }
    let row = &view.escalated[0];
    let plan = camerata_ui_core::tree::place::place_plan(
        &row.site,
        &DecisionState::Confirmed,
        &std::collections::BTreeSet::new(),
    );
    let html = test_harness::render_props(
        PromptHarness,
        PromptCase {
            cursor: row.candidate.key.clone(),
            prompt: QueuePrompt {
                key: row.candidate.key.clone(),
                state: TriageState::Confirmed,
                reason: String::new(),
                error: None,
                plan: Some(plan),
            },
            view: view.clone(),
        },
    )
    .replace(GLOBAL_CSS, "");
    // The chip is in the table row and again in the pane's head.
    assert!(html.matches("class=\"mark-also\"").count() >= 2, "{html}");
    assert!(html.contains("batch-sentence"), "{html}");
    assert!(html.contains("rule records at this place"), "{html}");
    assert!(html.contains(&row.site.rule_id) && html.contains("SEC-OTHER-9"));
    assert!(html.contains("on this place (required)"), "{html}");
}
