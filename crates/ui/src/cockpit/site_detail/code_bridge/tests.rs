//! `O` and `B` on the tree, mounted with the real overlays: the viewer and the stacked view open
//! on a site of the tree, move through the sites it loaded, and decide through the lane's own
//! site decision.

use std::cell::RefCell;
use std::time::Duration;

use camerata_api_types::review_view::{SiteRow, TreeLevel};
use camerata_ui_core::test_support::review_fixtures::dataset_base;
use camerata_ui_core::triage::TriageState;
use dioxus::prelude::*;

use super::*;
use crate::cockpit::stack::{StackRoot, STACK_ID};
use crate::cockpit::viewer::{ViewerRoot, FOCUS_LOG, VIEWER_ID};

thread_local! {
    static BRIDGE: RefCell<Option<CodeBridge>> = const { RefCell::new(None) };
    static SLOTS: RefCell<Option<(ViewerSlot, StackSlot)>> = const { RefCell::new(None) };
    static DECIDED: RefCell<Vec<(String, String, TriageState, String)>> = const { RefCell::new(Vec::new()) };
    static LANDED: RefCell<Vec<(String, String)>> = const { RefCell::new(Vec::new()) };
}

/// The sites of one sub-group, loaded, and the node's id.
fn sub_group() -> (String, Vec<SiteRow>) {
    let ds = dataset_base();
    let node = ds
        .tree
        .nodes
        .iter()
        .find(|n| n.level == TreeLevel::Subgroup && !n.thrown_out && n.counts.sites > 3)
        .expect("a sub-group")
        .id
        .clone();
    let rows = ds.node_rows[&node]
        .iter()
        .take(6)
        .map(|&i| ds.rows[i].clone())
        .collect();
    (node, rows)
}

fn harness() -> Element {
    use_context_provider(|| Signal::new(Vec::<Toast>::new()));
    let viewer = use_context_provider(|| ViewerSlot(Signal::new(None)));
    let stack = use_context_provider(|| StackSlot(Signal::new(None)));
    let (node, rows) = sub_group();
    let sites = use_memo(move || {
        rows.iter()
            .map(|r| (node.clone(), r.clone()))
            .collect::<Vec<(String, SiteRow)>>()
    });
    let bridge = use_code_bridge(
        sites,
        "p1".to_string(),
        Callback::new(|d: (String, String, TriageState, String)| {
            DECIDED.with(|l| l.borrow_mut().push(d))
        }),
        Callback::new(|s: (String, String)| LANDED.with(|l| l.borrow_mut().push(s))),
        "camerata-tree-region",
    );
    BRIDGE.with(|b| *b.borrow_mut() = Some(bridge));
    SLOTS.with(|s| *s.borrow_mut() = Some((viewer, stack)));
    rsx! {
        StackRoot {}
        ViewerRoot {}
    }
}

async fn settle(vdom: &mut VirtualDom) {
    for _ in 0..6 {
        let _ = tokio::time::timeout(Duration::from_millis(15), vdom.wait_for_work()).await;
        let _ = vdom.render_immediate_to_vec();
    }
}

async fn mounted() -> VirtualDom {
    BRIDGE.with(|b| *b.borrow_mut() = None);
    DECIDED.with(|l| l.borrow_mut().clear());
    LANDED.with(|l| l.borrow_mut().clear());
    FOCUS_LOG.with(|l| l.borrow_mut().clear());
    let mut vdom = VirtualDom::new(harness);
    vdom.rebuild_in_place();
    settle(&mut vdom).await;
    vdom
}

fn bridge() -> CodeBridge {
    BRIDGE.with(|b| (*b.borrow()).expect("the harness stored the bridge"))
}

fn links() -> (ViewerLink, StackLink) {
    let (v, st) = SLOTS.with(|s| (*s.borrow()).expect("slots"));
    let viewer = v.0.peek().clone().expect("the viewer link is registered");
    let stack = st.0.peek().clone().expect("the stack link is registered");
    (viewer, stack)
}

#[tokio::test]
async fn o_registers_the_trees_sites_and_opens_the_viewer_on_the_focused_one() {
    let mut vdom = mounted().await;
    let (node, rows) = sub_group();
    // Registered: one finding per loaded site, under the site's own key.
    let (viewer, _) = links();
    assert_eq!(viewer.all_findings.len(), rows.len());
    for (f, r) in viewer.all_findings.iter().zip(&rows) {
        assert_eq!(finding_key(f), r.site_key);
    }
    assert_eq!(viewer.project_id, "p1");
    assert!(!*viewer.open.peek(), "closed until asked");
    assert!(!dioxus_ssr::render(&vdom).contains(VIEWER_ID));

    let target = rows[2].clone();
    vdom.in_runtime(|| {
        bridge()
            .open_code
            .call((node.clone(), target.site_key.clone()))
    });
    settle(&mut vdom).await;

    let (viewer, _) = links();
    assert!(*viewer.open.peek(), "O opens the viewer");
    assert_eq!(viewer.cursor.peek().key(), Some(target.site_key.as_str()));
    let html = dioxus_ssr::render(&vdom);
    assert!(html.contains(VIEWER_ID), "the viewer overlay is mounted");
    assert!(
        html.contains(&target.path),
        "and it is on the focused site's file"
    );

    // Closing it leaves the tree's cursor on the site the viewer ended on, and gives the
    // keyboard back to the tree's own region.
    vdom.in_runtime(|| viewer.on_close.call(()));
    settle(&mut vdom).await;
    assert!(!*viewer.open.peek());
    assert_eq!(
        LANDED.with(|l| l.borrow().clone()),
        vec![(node, target.site_key)]
    );
    assert_eq!(
        FOCUS_LOG.with(|l| l.borrow().last().cloned()).as_deref(),
        Some("camerata-tree-region")
    );
    assert!(!dioxus_ssr::render(&vdom).contains(VIEWER_ID));
}

#[tokio::test]
async fn b_opens_the_stacked_view_of_the_sub_group_anchored_on_the_focused_site() {
    let mut vdom = mounted().await;
    let (node, rows) = sub_group();
    let anchor = rows[3].site_key.clone();
    vdom.in_runtime(|| {
        bridge()
            .open_stack
            .call((node.clone(), Some(anchor.clone())))
    });
    settle(&mut vdom).await;
    let (_, stack) = links();
    let open = stack.open.peek().clone().expect("the stack is open");
    assert_eq!(
        open.rows.len(),
        rows.len(),
        "every loaded site of the sub-group"
    );
    assert_eq!(open.anchor.as_deref(), Some(anchor.as_str()));
    let html = dioxus_ssr::render(&vdom);
    assert!(
        html.contains(STACK_ID),
        "the stack overlay is mounted: {html}"
    );

    // A card's open door goes to the viewer, on that site.
    let key = rows[1].site_key.clone();
    let opened = vdom.in_runtime(|| stack.on_open_viewer.call(key.clone()));
    assert!(opened);
    let (viewer, _) = links();
    assert!(*viewer.open.peek());
    assert_eq!(viewer.cursor.peek().key(), Some(key.as_str()));

    // Closing the stack lands the tree's cursor on the card the reviewer was on.
    vdom.in_runtime(|| stack.on_close.call(Some(key.clone())));
    settle(&mut vdom).await;
    assert!(stack.open.peek().is_none());
    assert_eq!(
        LANDED.with(|l| l.borrow().last().cloned()),
        Some((node, key))
    );
}

#[tokio::test]
async fn b_on_a_node_with_no_loaded_sites_says_so_and_opens_nothing() {
    let mut vdom = mounted().await;
    vdom.in_runtime(|| {
        bridge()
            .open_stack
            .call(("sub:not-loaded".to_string(), None))
    });
    settle(&mut vdom).await;
    let (_, stack) = links();
    assert!(stack.open.peek().is_none());
}

#[tokio::test]
async fn a_decision_in_the_viewer_or_the_stack_is_the_lanes_own_site_decision() {
    let mut vdom = mounted().await;
    let (node, rows) = sub_group();
    let key = rows[0].site_key.clone();
    vdom.in_runtime(|| bridge().open_code.call((node.clone(), key.clone())));
    settle(&mut vdom).await;
    let (viewer, stack) = links();

    // Confirm needs no reason: it goes to the lane's decision with the site's node.
    let wrote = vdom.in_runtime(|| {
        viewer
            .apply_disposition
            .call((TriageState::Confirmed, String::new()))
    });
    assert!(wrote);
    // Ignore without a reason writes nothing and answers `false`, as the Queue's does.
    let wrote = vdom.in_runtime(|| {
        viewer
            .apply_disposition
            .call((TriageState::Ignored, "  ".to_string()))
    });
    assert!(!wrote);
    let wrote = vdom.in_runtime(|| {
        viewer
            .apply_disposition
            .call((TriageState::Ignored, "accepted".to_string()))
    });
    assert!(wrote);
    assert_eq!(
        DECIDED.with(|l| l.borrow().clone()),
        vec![
            (
                node.clone(),
                key.clone(),
                TriageState::Confirmed,
                String::new()
            ),
            (
                node.clone(),
                key.clone(),
                TriageState::Ignored,
                "accepted".to_string()
            ),
        ]
    );

    // The stack decides several cards: one decision each, all with the one reason.
    DECIDED.with(|l| l.borrow_mut().clear());
    let cards = viewer
        .all_findings
        .iter()
        .take(3)
        .cloned()
        .collect::<Vec<_>>();
    let wrote = vdom.in_runtime(|| {
        stack.on_dispose.call((
            cards.clone(),
            TriageState::FalsePositive,
            "same helper".to_string(),
        ))
    });
    assert!(wrote);
    let decided = DECIDED.with(|l| l.borrow().clone());
    assert_eq!(decided.len(), 3);
    assert!(decided.iter().all(|(n, _, st, why)| *n == node
        && *st == TriageState::FalsePositive
        && why == "same helper"));
    let refused = vdom.in_runtime(|| {
        stack
            .on_dispose
            .call((cards, TriageState::FalsePositive, String::new()))
    });
    assert!(!refused, "a stack decision without a reason writes nothing");
    assert_eq!(DECIDED.with(|l| l.borrow().len()), 3);
}

#[tokio::test]
async fn the_viewers_dispositions_follow_the_sites_decisions() {
    let vdom = mounted().await;
    let (_, rows) = sub_group();
    let (viewer, _) = links();
    let map = viewer.dispositions.peek().clone();
    for r in &rows {
        assert_eq!(
            map.contains_key(&r.site_key),
            r.decision.is_some()
                && !matches!(
                    r.decision.as_ref().map(|d| &d.state),
                    Some(camerata_api_types::decisions::DecisionState::EngineRestored)
                ),
            "{}",
            r.site_key
        );
    }
    drop(vdom);
}
