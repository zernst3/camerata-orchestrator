//! The tree host's reads and writes, and the screen's pieces that the gallery pages do not reach.

use std::collections::BTreeSet;
use std::rc::Rc;

use camerata_api_types::decisions::{DecisionState, Scope};
use camerata_api_types::edits::EditField;
use camerata_api_types::review_view::{HoldReason, Marker};
use camerata_ui_core::test_support::review_fixtures::dataset_base;
use camerata_ui_core::tree::state::{TreeIndex, TreeState};
use camerata_ui_core::triage::PaneFocus;
use dioxus::prelude::*;

use super::*;
use crate::style::GLOBAL_CSS;
use crate::test_harness;
use camerata_ui::review_io::{RecordingWrites, Written};
use camerata_ui::review_source::FixtureReviewSource;

/// A sub-group of the base set with an outlier and a held row on it, and what it holds.
fn node_with_outlier_and_held() -> String {
    let ds = dataset_base();
    ds.tree
        .nodes
        .iter()
        .find(|n| {
            n.level == camerata_api_types::review_view::TreeLevel::Subgroup
                && n.counts.outliers > 0
                && n.counts.held > 0
                && n.decision.is_none()
        })
        .map(|n| n.id.clone())
        .expect("a sub-group with an outlier and a held row")
}

#[tokio::test]
async fn node_batch_excludes_outliers_unless_ticked() {
    let node = node_with_outlier_and_held();
    let reads = FixtureReviewSource::base();
    let writes = RecordingWrites::default();
    let ds = dataset_base();
    let members: Vec<_> = ds.node_rows[&node].iter().map(|&i| &ds.rows[i]).collect();
    let outliers: Vec<String> = members
        .iter()
        .filter(|r| r.markers.contains(&Marker::Outlier))
        .map(|r| r.site_key.clone())
        .collect();
    let held: Vec<String> = members
        .iter()
        .filter(|r| r.held.is_some())
        .map(|r| r.site_key.clone())
        .collect();
    assert!(!outliers.is_empty() && !held.is_empty());

    // Nothing ticked: the outliers and the held rows are left out of the one request.
    let plan = plan_batch(
        &reads,
        &node,
        &DecisionState::Confirmed,
        &BTreeSet::new(),
        |_, _| {},
    )
    .await
    .unwrap();
    let id = commit_batch(&writes, &plan, "same pattern across the group", "insp-1")
        .await
        .unwrap();
    assert!(!id.is_empty());
    let log = writes.log.borrow();
    assert_eq!(log.len(), 1, "one decision for the whole node");
    let Written::Decision(req) = &log[0] else {
        panic!("not a decision: {:?}", log[0]);
    };
    assert_eq!(req.scope, Scope::Node(node.clone()));
    assert_eq!(req.state, DecisionState::Confirmed);
    assert_eq!(req.reason, "same pattern across the group");
    for k in outliers.iter().chain(&held) {
        assert!(req.except.contains(k), "{k} is left out");
    }
    drop(log);

    // One outlier ticked: it is in, the rest are still out.
    let ticked: BTreeSet<String> = outliers.iter().take(1).cloned().collect();
    let plan = plan_batch(&reads, &node, &DecisionState::Confirmed, &ticked, |_, _| {})
        .await
        .unwrap();
    let writes = RecordingWrites::default();
    commit_batch(&writes, &plan, "r", "insp-1").await.unwrap();
    let log = writes.log.borrow();
    let Written::Decision(req) = &log[0] else {
        panic!("not a decision");
    };
    assert!(
        !req.except.contains(&outliers[0]),
        "the ticked outlier is covered"
    );
    for k in &held {
        assert!(req.except.contains(k), "a held row is always out");
    }
}

#[tokio::test]
async fn a_batch_without_a_reason_writes_nothing_and_says_so() {
    let node = node_with_outlier_and_held();
    let reads = FixtureReviewSource::base();
    let writes = RecordingWrites::default();
    let plan = plan_batch(
        &reads,
        &node,
        &DecisionState::Confirmed,
        &BTreeSet::new(),
        |_, _| {},
    )
    .await
    .unwrap();
    let err = commit_batch(&writes, &plan, "   ", "insp-1")
        .await
        .unwrap_err();
    assert_eq!(err, "A group decision needs a reason.");
    assert!(writes.log.borrow().is_empty());
}

#[tokio::test]
async fn a_refused_batch_comes_back_in_the_servers_words_and_the_plan_stays() {
    let node = node_with_outlier_and_held();
    let reads = FixtureReviewSource::base();
    let writes = RecordingWrites::default();
    *writes.refuse.borrow_mut() = Some("The decision file is read only.".to_string());
    let plan = plan_batch(
        &reads,
        &node,
        &DecisionState::Ignored,
        &BTreeSet::new(),
        |_, _| {},
    )
    .await
    .unwrap();
    let err = commit_batch(&writes, &plan, "accepted risk", "insp-1")
        .await
        .unwrap_err();
    assert_eq!(err, "The decision file is read only.");
}

#[tokio::test]
async fn a_plan_that_cannot_read_the_sites_says_why() {
    let reads = FixtureReviewSource::failing("the server is down");
    let err = plan_batch(
        &reads,
        "cat:authorization",
        &DecisionState::Confirmed,
        &BTreeSet::new(),
        |_, _| {},
    )
    .await
    .unwrap_err();
    assert_eq!(err, "the server is down");
}

#[tokio::test]
async fn editing_a_summary_posts_to_the_edits_route_keyed_by_the_node() {
    let ds = dataset_base();
    let node = ds
        .tree
        .nodes
        .iter()
        .find(|n| n.summary.is_some())
        .unwrap()
        .clone();
    let writes = RecordingWrites::default();
    commit_summary(
        &writes,
        &node,
        "The reviewer's reading.",
        "the model missed the helper",
        "insp-1",
    )
    .await
    .unwrap();
    let log = writes.log.borrow();
    let Written::Edit(req) = &log[0] else {
        panic!("not an edit");
    };
    assert_eq!(req.key, node.id);
    assert_eq!(req.field, EditField::Summary);
    assert_eq!(req.new, "The reviewer's reading.");
    drop(log);
    let err = commit_summary(&writes, &node, "Another.", "", "insp-1")
        .await
        .unwrap_err();
    assert!(err.contains("needs a reason"), "{err}");
    assert_eq!(writes.log.borrow().len(), 1, "the refusal wrote nothing");
}

#[tokio::test]
async fn a_held_row_is_found_under_its_rule_and_the_door_lands_on_its_sub_group() {
    let ds = dataset_base();
    let reads = FixtureReviewSource::base();
    let index = TreeIndex::new(&ds.tree);
    let held = ds
        .rows
        .iter()
        .find(|r| r.held == Some(HoldReason::EvidenceUnverified))
        .unwrap();
    let sub = locate_site(&reads, &index, &held.rule_id, &held.site_key)
        .await
        .unwrap();
    assert_eq!(Some(&sub), held.group_path.get(2));
    assert!(index.holds_sites(&sub));
    // A rule the tree does not have, and a key it does not hold, are not found.
    assert_eq!(
        locate_site(&reads, &index, "NOPE-9", &held.site_key).await,
        None
    );
    assert_eq!(
        locate_site(&reads, &index, &held.rule_id, "no\0such\0key\01").await,
        None
    );
}

#[test]
fn the_blocks_are_the_top_level_until_something_opens() {
    let ds = dataset_base();
    let index = TreeIndex::new(&ds.tree);
    let closed = tree_blocks(&index, &TreeState::default());
    assert_eq!(closed.len(), index.roots().count());
    assert!(closed
        .iter()
        .all(|b| matches!(b, Block::Node { open: false, .. })));

    let mut state = TreeState::default();
    let sub = ds
        .tree
        .nodes
        .iter()
        .find(|n| n.level == camerata_api_types::review_view::TreeLevel::Subgroup && !n.thrown_out)
        .unwrap()
        .id
        .clone();
    let requests = state.open_path(&index, &sub);
    for r in &requests {
        let page = ds
            .sites_page(&r.node, r.cursor.as_deref(), r.limit)
            .unwrap();
        state.apply_page(&page);
    }
    state.focus_node(&sub);
    let blocks = tree_blocks(&index, &state);
    let sites_at = blocks
        .iter()
        .position(|b| matches!(b, Block::Sites { node, .. } if *node == sub))
        .expect("the open sub-group shows its sites");
    // The sub-group's own header sits right above its sites, focused.
    assert!(
        matches!(&blocks[sites_at - 1], Block::Node { node, focused: true, open: true } if node.id == sub)
    );
    // The path above it is open, the rest of the top level is not.
    let open_count = blocks
        .iter()
        .filter(|b| matches!(b, Block::Node { open: true, .. }))
        .count();
    assert_eq!(open_count, 3, "category, rule and sub-group");
}

// ── Screens that the gallery pages do not reach ───────────────────────────────────────────

#[derive(Clone, PartialEq, Props)]
struct ScreenProps {
    index: Rc<TreeIndex>,
    state: TreeState,
    prompt: Option<TreePrompt>,
    focus: PaneFocus,
}

#[component]
fn ScreenHarness(props: ScreenProps) -> Element {
    rsx! {
        TreeScreen {
            index: props.index.clone(),
            state: props.state.clone(),
            status: TreeStatus::Ready,
            group_reviewed: true,
            focus: props.focus,
            prompt: props.prompt.clone(),
            undo: None,
        }
    }
}

fn screen(state: TreeState, prompt: Option<TreePrompt>, focus: PaneFocus) -> String {
    let ds = dataset_base();
    test_harness::render_props(
        ScreenHarness,
        ScreenProps {
            index: Rc::new(TreeIndex::new(&ds.tree)),
            state,
            prompt,
            focus,
        },
    )
    .replace(GLOBAL_CSS, "")
}

fn focused_on_a_node(with_edit: bool) -> (TreeState, String) {
    let ds = dataset_base();
    let node = ds
        .tree
        .nodes
        .iter()
        .find(|n| {
            n.summary.is_some()
                && !n.thrown_out
                && n.level == camerata_api_types::review_view::TreeLevel::Rule
        })
        .unwrap()
        .id
        .clone();
    let mut state = TreeState::default();
    state.focus_node(&node);
    let _ = with_edit;
    (state, node)
}

#[test]
fn the_summary_editor_shows_the_summary_a_reason_field_and_commit() {
    let (state, node) = focused_on_a_node(false);
    let html = screen(
        state,
        Some(TreePrompt::Summary {
            node,
            text: "The reviewer's draft.".to_string(),
            reason: String::new(),
            error: Some(
                "An edit needs a reason: say why the model's summary was wrong.".to_string(),
            ),
        }),
        PaneFocus::ReasonPrompt,
    );
    assert!(html.contains("Edit the summary of this node"));
    assert!(html.contains("The reviewer&#39;s draft.") || html.contains("The reviewer's draft."));
    assert!(
        html.contains("Why the model&#39;s summary was wrong")
            || html.contains("Why the model's summary was wrong")
    );
    assert!(html.contains("An edit needs a reason"), "the refusal shows");
    assert!(html.contains("Commit"));
    assert!(
        !html.contains(">Edit</button>"),
        "the Edit button gives way to the editor"
    );
    assert!(!html.contains('\u{2014}'));
}

#[test]
fn a_node_pane_offers_the_four_batch_decisions_and_a_summary_edit() {
    let (state, _) = focused_on_a_node(false);
    let html = screen(state, None, PaneFocus::Cursor);
    for label in [
        "Confirm all",
        "Ignore all",
        "False positive all",
        "Tech debt all",
        "Summary (model)",
    ] {
        assert!(html.contains(label), "no {label}");
    }
    assert!(html.contains(">Edit</button>"));
}

#[test]
fn planning_a_batch_says_how_far_the_read_has_got() {
    let (state, node) = focused_on_a_node(false);
    let html = screen(
        state,
        Some(TreePrompt::Planning {
            node,
            state: TriageState::Confirmed,
            read: 500,
            total: 2_210,
        }),
        PaneFocus::ReasonPrompt,
    );
    assert!(
        html.contains("Reading the sites to plan the batch: 500 of 2,210."),
        "{html}"
    );
}

#[test]
fn a_plan_that_could_not_be_read_shows_the_reason() {
    let (state, node) = focused_on_a_node(false);
    let html = screen(
        state,
        Some(TreePrompt::PlanFailed {
            node,
            message: "the server is down".to_string(),
        }),
        PaneFocus::ReasonPrompt,
    );
    assert!(html.contains("the server is down"));
    assert!(html.contains("role=\"alert\""));
}

#[test]
fn a_thrown_out_node_takes_no_batch() {
    let ds = dataset_base();
    let node = ds
        .tree
        .nodes
        .iter()
        .find(|n| n.thrown_out)
        .unwrap()
        .id
        .clone();
    let mut state = TreeState::default();
    state.focus_node(&node);
    let html = screen(state, None, PaneFocus::Cursor);
    assert!(html.contains("disabled"), "the batch buttons are off");
    assert!(
        html.contains("thrown out by the engine"),
        "and the tooltip says why"
    );
}

#[test]
fn a_stale_summary_edit_is_shown_beside_the_models_summary() {
    let ds = dataset_base();
    let mut tree = ds.tree.clone();
    let node = tree
        .nodes
        .iter_mut()
        .find(|n| n.summary.is_some() && !n.thrown_out)
        .unwrap();
    node.summary_edit = Some(camerata_api_types::review_view::EditView {
        id: "edit-000001".into(),
        field: EditField::Summary,
        old: "The old model summary.".into(),
        new: "My reading from before the rerun.".into(),
        reason: "r".into(),
        by: "zach".into(),
        at: "2026-11-04T09:00:00Z".into(),
        stale: true,
        current_value: node.summary.clone().unwrap_or_default(),
        reapplied_from: None,
    });
    let id = node.id.clone();
    let mut state = TreeState::default();
    state.focus_node(&id);
    let html = test_harness::render_props(
        ScreenHarness,
        ScreenProps {
            index: Rc::new(TreeIndex::new(&tree)),
            state,
            prompt: None,
            focus: PaneFocus::Cursor,
        },
    );
    assert!(
        html.contains(
            "Your edit (stale, the model&#39;s summary changed): My reading from before the rerun."
        ) || html.contains(
            "Your edit (stale, the model's summary changed): My reading from before the rerun."
        ),
        "{}",
        &html[html.len().saturating_sub(3000)..]
    );
    assert!(
        html.contains("Summary (model)"),
        "the summary is still the model's"
    );
}

#[test]
fn the_site_pane_shows_its_decision_buttons_and_a_site_prompt() {
    let ds = dataset_base();
    let index = TreeIndex::new(&ds.tree);
    let sub = ds
        .tree
        .nodes
        .iter()
        .find(|n| {
            n.level == camerata_api_types::review_view::TreeLevel::Subgroup
                && !n.thrown_out
                && n.counts.sites > 3
        })
        .unwrap()
        .id
        .clone();
    let mut state = TreeState::default();
    for r in state.open_path(&index, &sub) {
        let page = ds
            .sites_page(&r.node, r.cursor.as_deref(), r.limit)
            .unwrap();
        state.apply_page(&page);
    }
    let key = state.loaded[&sub].rows[0].site_key.clone();
    state.focus_site(&sub, &key);
    let html = screen(
        state.clone(),
        Some(TreePrompt::Site {
            node: sub.clone(),
            key: key.clone(),
            state: TriageState::Ignored,
            reason: String::new(),
            error: None,
        }),
        PaneFocus::ReasonPrompt,
    );
    for label in ["Confirmed", "Ignored", "Tech debt", "False positive"] {
        assert!(html.contains(label), "no {label}");
    }
    assert!(html.contains("Reason for \u{201c}Ignored\u{201d} on 1 site (required)"));
    assert!(!html.contains('\0'));
}
