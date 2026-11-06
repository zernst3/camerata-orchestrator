//! The `tree/*` gallery states (review UI spec section 12): the tree lane under the shell, from
//! the review fixtures, with the app's real stylesheet. The grouping check (indentation, counts,
//! chips, bars) is done on `tree/markers` and `tree/30k`.

use std::collections::BTreeSet;
use std::rc::Rc;

use camerata_api_types::decisions::DecisionState;
use camerata_api_types::review_view::TreeView;
use camerata_ui_core::test_support::review_fixtures::{
    dataset_base, fixture, Body, Loaded, StateFixture, TreeBody,
};
use camerata_ui_core::tree::batch::batch_plan;
use camerata_ui_core::tree::shell::Lane;
use camerata_ui_core::tree::state::{TreeIndex, TreeRef, TreeState};
use camerata_ui_core::triage::{PaneFocus, TriageState};
use dioxus::prelude::*;

use crate::cockpit::review_shell::ReviewShell;
use crate::cockpit::tree::{TreePrompt, TreeScreen, TreeStatus, UndoBanner};
use crate::test_harness;

#[derive(Clone, PartialEq, Props)]
pub(super) struct PageProps {
    pub id: String,
}

/// The tree state a fixture's body stands for: the open nodes, the first page of each open
/// sub-group, and the cursor.
fn tree_state_of(body: &TreeBody) -> TreeState {
    let mut state = TreeState {
        open: body.open_nodes.iter().cloned().collect(),
        ..TreeState::default()
    };
    for page in body.pages.values() {
        state.apply_page(page);
    }
    state
}

/// The status, the index and whether the group reviewer read the run.
fn tree_parts(tree: &Loaded<TreeView>) -> (TreeStatus, Rc<TreeIndex>, bool) {
    match tree {
        Loaded::Loading => (TreeStatus::Loading, Rc::new(TreeIndex::default()), true),
        Loaded::Failed(m) => (
            TreeStatus::Failed(m.clone()),
            Rc::new(TreeIndex::default()),
            true,
        ),
        Loaded::Ready(t) if t.nodes.is_empty() => {
            (TreeStatus::Empty, Rc::new(TreeIndex::default()), true)
        }
        Loaded::Ready(t) => (
            TreeStatus::Ready,
            Rc::new(TreeIndex::new(t)),
            t.group_reviewed,
        ),
    }
}

/// Where the cursor sits for each state, so each page shows the pane its state is about.
fn place_cursor(id: &str, body: &TreeBody, state: &mut TreeState, index: &TreeIndex) {
    if let Some(node) = &body.prompt_node {
        state.focus_node(node);
        return;
    }
    if let Some(key) = &body.selected_site {
        let found = state
            .loaded
            .iter()
            .find(|(_, l)| l.rows.iter().any(|r| &r.site_key == key))
            .map(|(node, _)| node.clone());
        if let Some(node) = found {
            state.focus_site(&node, key);
            return;
        }
    }
    // The first opened sub-group, so the node pane shows its summary, counts and decisions.
    let first_sub = body
        .open_nodes
        .iter()
        .find(|n| index.holds_sites(n))
        .cloned();
    if let Some(sub) = first_sub {
        state.focus_node(&sub);
    } else if let Some(root) = index.roots().next() {
        state.focus_node(&root.id.clone());
    }
    if id == "tree/markers" {
        // One outlier ticked into the next batch, so the checked box is on the page.
        let ticked = state
            .loaded
            .values()
            .flat_map(|l| l.rows.iter())
            .find(|r| camerata_ui_core::tree::is_outlier(r))
            .map(|r| r.site_key.clone());
        if let Some(key) = ticked {
            state.ticked.insert(key);
        }
    }
}

/// The batch prompt the `tree/node-prompt` state has open: the plan over every site of the node
/// (the fixture's own data), Confirm all, with the reason still blank.
fn prompt_of(body: &TreeBody) -> Option<TreePrompt> {
    let node = body.prompt_node.clone()?;
    let ds = dataset_base();
    let rows: Vec<_> = ds
        .node_rows
        .get(&node)?
        .iter()
        .map(|&i| ds.rows[i].clone())
        .collect();
    let plan = batch_plan(&node, &DecisionState::Confirmed, &rows, &BTreeSet::new());
    Some(TreePrompt::Batch {
        node,
        state: TriageState::Confirmed,
        plan,
        reason: String::new(),
        error: None,
    })
}

/// The undo bar a state shows, if it follows a decision.
fn undo_of(id: &str, state: &TreeState) -> Option<UndoBanner> {
    if id != "tree/node-decided" {
        return None;
    }
    let covered = match &state.cursor {
        Some(TreeRef::Node(node)) => dataset_base()
            .tree
            .nodes
            .iter()
            .find(|n| &n.id == node)
            .and_then(|n| n.decision.as_ref().map(|d| d.covered))?,
        _ => return None,
    };
    Some(UndoBanner {
        message: format!("{covered} rows confirmed."),
        seconds_left: 4,
    })
}

#[component]
pub(super) fn TreePage(props: PageProps) -> Element {
    let Some(StateFixture {
        shell,
        body: Body::Tree(body),
    }) = fixture(&props.id)
    else {
        return rsx! { p { "no tree fixture for {props.id}" } };
    };
    let (status, index, group_reviewed) = tree_parts(&body.tree);
    let mut state = tree_state_of(&body);
    place_cursor(&props.id, &body, &mut state, &index);
    let prompt = prompt_of(&body);
    let undo = undo_of(&props.id, &state);
    let focus = if prompt.is_some() {
        PaneFocus::ReasonPrompt
    } else {
        PaneFocus::Cursor
    };
    rsx! {
        ReviewShell { status: (&shell).into(), active: Lane::Tree,
            TreeScreen {
                index,
                state,
                status,
                group_reviewed,
                focus,
                prompt,
                undo,
                narrow: body.narrow,
            }
        }
    }
}

pub(super) fn render(id: &str) -> String {
    test_harness::render_props(TreePage, PageProps { id: id.to_string() })
}

macro_rules! states {
    ($($name:ident => $id:literal),* $(,)?) => {
        $(fn $name() -> String { render($id) })*
        /// Every state this file renders, with its renderer.
        pub(super) fn states() -> Vec<super::BuiltRenderer> {
            vec![$(($id, $name as fn() -> String)),*]
        }
    };
}

states! {
    empty => "tree/empty",
    loading => "tree/loading",
    error => "tree/error",
    thirty_k => "tree/30k",
    markers => "tree/markers",
    node_prompt => "tree/node-prompt",
    node_decided => "tree/node-decided",
    not_group_reviewed => "tree/not-group-reviewed",
    narrow => "tree/narrow",
}

#[cfg(test)]
mod tests;
