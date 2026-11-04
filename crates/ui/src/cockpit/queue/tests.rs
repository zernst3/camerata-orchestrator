//! The queue host's reads and writes.

use camerata_api_types::review_view::QUEUE_BELOW_PAGE_SIZE;
use camerata_ui::review_io::{RecordingWrites, ReviewWrites, Written};
use camerata_ui::review_source::FixtureReviewSource;
use camerata_ui_core::queue::{layout, needed_me_write, Entry, QueueState};
use camerata_ui_core::test_support::review_fixtures::dataset_base;

use super::*;

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
