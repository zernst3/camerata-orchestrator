use super::*;
use crate::style::GLOBAL_CSS;
use camerata_api_types::review_view::QueueRow;
use camerata_ui_core::queue::{layout, Entry, ROWS_PER_PAGE};
use camerata_ui_core::test_support::review_fixtures::{
    dataset_30k, dataset_base, fixture, Body, Loaded,
};

fn page(id: &str) -> String {
    render(id).replace(GLOBAL_CSS, "")
}

fn count(html: &str, needle: &str) -> usize {
    html.matches(needle).count()
}

#[test]
fn every_queue_state_renders_with_no_em_dash_and_no_raw_nul() {
    assert_eq!(states().len(), 10, "every queue state is built");
    for (id, render) in states() {
        let html = render().replace(GLOBAL_CSS, "");
        assert!(!html.contains('\u{2014}'), "{id} has an em dash");
        assert!(!html.contains('\0'), "{id} has a raw NUL");
        assert!(html.contains("review-queue-lane"), "{id}");
    }
}

#[test]
fn the_over_budget_page_draws_the_head_the_line_and_the_band_with_their_sentences() {
    let ds = dataset_base();
    let q = ds.queue(false, 0, 100);
    let html = page("queue/over-budget");
    assert!(
        html.contains(&format!("Budget {}", q.budget.budget)),
        "{html}"
    );
    assert!(html.contains(&format!("Escalated {}", q.escalated.len())));
    assert!(html.contains(" over budget (all "));
    assert!(html.contains("candidates below the line"));
    assert!(html.contains(&format!(
        "Needed me: {} of {} decided ({}%)",
        q.needed_me.needed_me, q.needed_me.decided, q.needed_me.percent
    )));
    assert_eq!(
        count(&html, "class=\"queue-line\""),
        1,
        "the line is drawn once"
    );
    assert!(html.contains("the line: "));
    assert!(html.contains("class=\"queue-over-band\""));
    // The sentence is read from the rows over budget (D3): the severities it names are theirs.
    let over: Vec<&QueueRow> = q
        .escalated
        .iter()
        .filter(|r| r.candidate.over_budget_reason.is_some())
        .collect();
    assert!(!over.is_empty());
    let sentence = camerata_ui_core::queue::over_budget_reasons_text(&over);
    assert!(
        html.contains(&format!("over budget: {sentence}")),
        "the over-budget band says {sentence:?}: {html}"
    );
    for r in &over {
        assert!(
            r.candidate.severity == "critical"
                || r.candidate.reason
                    == camerata_api_types::review::EscalationReason::EvidenceUnverified,
            "{} at {} is under a band that would claim a severity it does not have",
            r.candidate.reason.as_str(),
            r.candidate.severity
        );
    }
    assert!(sentence.contains(" at critical"), "{sentence}");
    // The candidates stay hidden until asked, with the view-only button.
    assert!(!html.contains("queue-below-heading"));
    assert!(html.contains("(hidden)"));
    assert!(html.contains("Show below the line"));
    assert!(
        !html.contains("mark-cand"),
        "no candidate chip until they are shown"
    );
}

#[test]
fn the_at_budget_page_has_a_line_and_no_over_band() {
    let html = page("queue/at-budget");
    assert_eq!(count(&html, "class=\"queue-line\""), 1);
    assert!(!html.contains("queue-over-band"), "nothing is over budget");
    assert!(
        !html.contains(" over budget "),
        "the counts line has no over term"
    );
    let b = dataset_base().queue(false, 0, 100).budget;
    assert!(html.contains("the line: "), "{b:?}");
}

#[test]
fn the_line_lowered_page_shows_candidates_with_their_rank_and_no_needed_me_box() {
    let ds = dataset_base();
    let q = ds.queue(true, 0, 100);
    assert!(!q.below.is_empty());
    let html = page("queue/line-lowered");
    assert!(html.contains("class=\"queue-below-heading\""));
    assert_eq!(count(&html, "row-candidate"), q.below.len());
    assert_eq!(count(&html, "<span class=\"mark-cand\""), q.below.len());
    // A Needed me box on every escalated row and on no candidate.
    let boxes = count(&html, "aria-label=\"Needed me\"");
    assert_eq!(boxes, q.escalated.len());
    assert!(html.contains("Hide the candidates"));
    // The global rank of the first candidate is in its row.
    let first = &q.below[0];
    assert!(
        html.contains(&format!("rank {}", first.candidate.global_rank)),
        "the candidate chip's tooltip carries its rank"
    );
}

#[test]
fn the_rate_changed_page_says_the_budget_the_move_and_that_no_model_was_called() {
    let html = page("queue/rate-changed-toast");
    assert!(html.contains("class=\"review-queue-toast\""));
    assert!(html.contains("Budget is now "), "{html}");
    assert!(html.contains("moved above the line."));
    assert!(html.contains("No model call was made."));
}

#[test]
fn the_empty_loading_and_error_pages_say_so() {
    let empty = page("queue/empty");
    assert!(empty.contains("No escalations. The group reviewer found nothing that needs you"));
    assert!(
        !empty.contains("queue-region"),
        "no table with nothing in it"
    );
    let loading = page("queue/loading");
    assert!(loading.contains("Loading the escalations"));
    assert!(loading.contains("tree-skel-row"));
    let error = page("queue/error");
    assert!(error.contains("The queue request failed with status 500."));
    assert!(error.contains(">Retry<"));
}

#[test]
fn the_thirty_thousand_site_queue_never_draws_every_row() {
    let big = dataset_30k().queue(false, 0, 100);
    let html = page("queue/30k");
    let drawn = count(&html, "class=\"site-row queue-row");
    assert!(drawn <= ROWS_PER_PAGE, "{drawn} rows drawn");
    if big.escalated.len() > ROWS_PER_PAGE {
        assert_eq!(drawn, ROWS_PER_PAGE);
        assert!(html.contains("Show 100 more"));
        assert!(html.contains("escalated rows not shown"));
    }
    let l = layout(&big, &QueueState::default());
    assert_eq!(
        l.entries
            .iter()
            .filter(|e| matches!(e, Entry::Row(_)))
            .count(),
        drawn
    );
}

#[test]
fn a_rendered_queue_page_has_each_key_as_a_plain_token() {
    let html = page("queue/over-budget");
    let mut seen = 0;
    let mut rest = html.as_str();
    while let Some(at) = rest.find("data-site=\"") {
        let after = &rest[at + "data-site=\"".len()..];
        let end = after.find('"').unwrap();
        let token = &after[..end];
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()), "{token}");
        assert!(camerata_ui_core::tree::from_dom_token(token).is_some_and(|k| k.contains('\0')));
        seen += 1;
        rest = &after[end..];
    }
    assert!(seen > 10, "{seen}");
}

/// D3: every queue page that draws an over-budget band says what the rows in it are, read from
/// the rows: no Low row sits under a band that claims "at critical".
#[test]
fn no_queue_page_claims_a_severity_the_rows_over_budget_do_not_have() {
    use camerata_ui_core::test_support::review_fixtures::{fixture, Body, StateFixture};
    let mut with_band = 0;
    for id in [
        "queue/over-budget",
        "queue/line-lowered",
        "queue/rate-changed-toast",
        "queue/30k",
    ] {
        let Some(StateFixture {
            body: Body::Queue(body),
            ..
        }) = fixture(id)
        else {
            panic!("{id}");
        };
        let q = body.queue.ready().cloned().expect("a ready queue");
        let over: Vec<&QueueRow> = q
            .escalated
            .iter()
            .filter(|r| r.candidate.over_budget_reason.is_some())
            .collect();
        if over.is_empty() {
            continue;
        }
        with_band += 1;
        let sentence = camerata_ui_core::queue::over_budget_reasons_text(&over);
        let html = page(id);
        assert!(html.contains(&sentence), "{id}: {sentence}");
        if sentence.contains("at critical") {
            for r in over.iter().filter(|r| {
                r.candidate.reason
                    != camerata_api_types::review::EscalationReason::EvidenceUnverified
            }) {
                assert_eq!(r.candidate.severity, "critical", "{id}");
            }
        }
    }
    assert!(with_band >= 3, "a vacuous pass proves nothing");
}

/// D6: Stage 1 wraps instead of clipping at 1440 ("Y medi..."). The cell carries the class and the
/// stylesheet lets it wrap, while the Stage 2 why gives up the room (ellipsized at a width).
#[test]
fn the_stage_1_cell_wraps_and_the_why_gives_up_the_room() {
    let html = render("queue/at-budget");
    assert!(html.contains("class=\"col-s1\""), "the cell is there");
    let css: String = GLOBAL_CSS.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        css.contains(".queue-table td.col-s1 { white-space: normal; }"),
        "Stage 1 wraps"
    );
    assert!(
        css.contains(".queue-table td.col-why { color: var(--ink-soft); white-space: nowrap; max-width: 200px; overflow: hidden; text-overflow: ellipsis; }"),
        "the why is cut at a width, not Stage 1"
    );
    // The table is fixed-layout, so the room is the column's: Stage 1 (11%) gets more than the why
    // used to leave it (8%), which clipped "Y medium" at 1440.
    assert!(
        html.contains(
            "<col style=\"width: 11%\"/><col style=\"width: 12%\"/><col style=\"width: 11%\"/>"
        ),
        "Stage 1, the why and the decision keep their widths"
    );
}

/// Wave 18 soft: below 1100px the detail sat under the table, so on queue/30k at 1000 the detail
/// of row 1 was under 50 rows. The narrow layout puts it first, pinned to the top of the window so
/// it stays in view while the table scrolls under it.
#[test]
fn at_a_narrow_width_the_detail_comes_before_the_table_and_stays_in_view() {
    let css: String = GLOBAL_CSS.split_whitespace().collect::<Vec<_>>().join(" ");
    let at = css
        .find("@media (max-width: 1100px) { .review-queue-lane")
        .expect("the queue's narrow block");
    let block = &css[at..css[at..].find("} }").map_or(css.len(), |e| at + e + 3)];
    assert!(
        block.contains(".review-queue-right { order: -1;"),
        "the detail is ordered first: {block}"
    );
    assert!(
        block.contains("position: sticky"),
        "and stays in view: {block}"
    );
    assert!(
        block.contains("max-height:"),
        "and cannot take the window from the table: {block}"
    );
    // A form open in the detail needs the room to be written in: the pane stops being pinned and
    // is not capped (a pinned pane taller than the window has a bottom nobody can reach).
    assert!(
        block.contains(
            ".review-queue-right:has(.site-detail-form) { position: static; max-height: none; }"
        ),
        "an open form unpins the pane: {block}"
    );
    // The markup order stays table then detail (the keyboard and the tab order read the table
    // first); only the stacked layout reorders them.
    for id in ["queue/30k", "queue/over-budget", "queue/at-budget"] {
        let html = page(id);
        let left = html.find("review-queue-left").expect("the table side");
        let right = html.find("review-queue-right").expect("the detail side");
        assert!(left < right, "{id}");
        assert!(html.contains("site-detail"), "{id}: the detail is drawn");
    }
}

#[test]
fn the_detail_form_state_shows_the_fix_form_beside_the_table() {
    let html = page("queue/detail-form-open");
    assert!(html.contains("queue-table"), "the table is there");
    assert!(html.contains("site-detail-form"), "{html}");
    assert!(html.contains("Fix"), "the field being edited is named");
    assert!(
        html.contains("Why (required)") || html.contains("required"),
        "the form asks for the reason"
    );
}

#[test]
fn the_merge_picker_on_an_empty_pool_says_what_the_pool_is_and_the_remedy_that_works() {
    let html = page("queue/merge-picker-empty-pool");
    let q = match fixture("queue/merge-picker-empty-pool").unwrap().body {
        Body::Queue(b) => match b.queue {
            Loaded::Ready(v) => v,
            other => panic!("{other:?}"),
        },
        _ => panic!("not a queue state"),
    };
    assert_eq!(q.escalated.len(), 1, "one escalated row");
    assert!(q.below.is_empty(), "nothing below the line is loaded");
    assert!(
        html.contains(
            "No escalated row matches. Open in the tree to merge into a site that is not in the queue."
        ),
        "{html}"
    );
    assert!(html.contains("site-detail-merge-form"));
}
