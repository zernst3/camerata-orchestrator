//! `O` and `B` on the tree: the code viewer and the stacked view, reused (spec rule 4).
//!
//! Both overlays were built for the flat Queue. Each is hosted at the results root and reads a
//! link its table registers in a slot ([`ViewerSlot`], [`StackSlot`]): the finding the cursor is
//! on, the table of findings it moves through, the dispositions, and the one write path with an
//! undo. This hook registers the same links for the tree. The findings are the sites the tree has
//! loaded, read as [`FindingView`]s under the same key (`camerata_ui_core::edits::bridge`); the
//! table behind them is a hidden one that is never drawn; a decision the viewer or the stack
//! makes is the tree's own site decision (the lane passes it in as `decide`).
//!
//! Nothing here is new behaviour: the viewer and the stack keep every key, every prompt and every
//! rule they have. What the tree adds is where they open from and what a decision does.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use camerata_api_types::review_view::SiteRow;
use camerata_ui_core::edits::bridge::{dispositions_of, finding_view_of};
use camerata_ui_core::stack::StackOrigin;
use camerata_ui_core::triage::{
    finding_key, focused_key, rule_group_label, FindingView, RowCursor, TriageState,
};
use chorale_core::RowId;
use chorale_dioxus::use_table;
use dioxus::prelude::*;

use crate::cockpit::scan::finding_table_state;
use crate::cockpit::stack::{StackLink, StackOpen, StackSlot};
use crate::cockpit::viewer::{focus_element, reach_finding, ViewerLink, ViewerSlot};
use crate::toast::{push_toast, Toast, ToastKind};

/// What a lane calls to open the overlays. Both are `Copy`.
#[derive(Clone, Copy)]
pub(crate) struct CodeBridge {
    /// `O`: the viewer on this site `(node, key)`.
    pub open_code: Callback<(String, String)>,
    /// `B`: the stacked view of this sub-group, starting on the site when there is one.
    pub open_stack: Callback<(String, Option<String>)>,
}

/// The node a key was loaded under.
fn node_of(sites: &[(String, SiteRow)], key: &str) -> Option<String> {
    sites
        .iter()
        .find(|(_, r)| r.site_key == key)
        .map(|(n, _)| n.clone())
}

/// Register the viewer's and the stack's links for the lane and give back the two doors.
///
/// - `sites`: every site loaded (the viewer's gutter and its next and previous file, the
///   stack's rows);
/// - `decide`: the lane's site decision `(node, key, state, reason)`, the one write path;
/// - `focus_site`: put the lane's cursor on a site `(node, key)` (the viewer's close and the
///   stack's close leave it where the reviewer ended);
/// - `region_id`: the DOM id of the lane's focusable region, which takes the keyboard back.
pub(crate) fn use_code_bridge(
    sites: Memo<Vec<(String, SiteRow)>>,
    project_id: String,
    decide: Callback<(String, String, TriageState, String)>,
    focus_site: Callback<(String, String)>,
    region_id: &'static str,
) -> CodeBridge {
    let viewer_slot = try_use_context::<ViewerSlot>();
    let stack_slot = try_use_context::<StackSlot>();
    let toasts = try_use_context::<Signal<Vec<Toast>>>();
    let say = move |kind: ToastKind, message: String| {
        if let Some(toasts) = toasts {
            push_toast(toasts, kind, message);
        }
    };

    // The findings, one per loaded site, under the site's own key.
    let views = use_memo(move || {
        Arc::new(
            sites
                .read()
                .iter()
                .map(|(_, r)| finding_view_of(r))
                .collect::<Vec<FindingView>>(),
        )
    });
    // The hidden table the viewer moves through. Row ids are minted once per key, so a reload
    // that brings the same site back does not rename it to the table.
    let ids =
        use_hook(|| std::rc::Rc::new(std::cell::RefCell::new(HashMap::<String, RowId>::new())));
    let rows_now = {
        let ids = ids.clone();
        move |views: &[FindingView]| -> Vec<(RowId, FindingView)> {
            let mut ids = ids.borrow_mut();
            views
                .iter()
                .map(|f| {
                    let id = *ids.entry(finding_key(f)).or_insert_with(RowId::new);
                    (id, f.clone())
                })
                .collect()
        }
    };
    let handle = use_table({
        let rows_now = rows_now.clone();
        let initial = views.peek().clone();
        move || finding_table_state(rows_now(&initial), Vec::new(), false)
    });
    // Reload the table when the sites change.
    use_effect({
        let rows_now = rows_now.clone();
        move || {
            let current = views.read().clone();
            handle.set_rows(rows_now(&current));
        }
    });

    let sort_signal = handle.signal();
    let visible_keys = use_memo(move || {
        let state = sort_signal.read();
        let keys: HashMap<RowId, String> = state
            .rows
            .iter()
            .map(|(id, f)| (*id, finding_key(f)))
            .collect();
        chorale_core::visible_grouped_view(&state)
            .into_iter()
            .filter_map(|r| match r {
                chorale_core::GroupedRow::Data(rid, _) => keys.get(&rid).cloned(),
                _ => None,
            })
            .collect::<Vec<String>>()
    });
    let mut cursor = use_signal(RowCursor::new);
    let mut viewer_open = use_signal(|| false);
    let mut stack_open = use_signal(|| Option::<StackOpen>::None);
    let mut stack_serial = use_signal(|| 0u64);
    let dispositions = use_signal(HashMap::new);
    let undo_entry = use_signal(|| None);
    let now_ms = use_signal(|| 0u64);

    // The disposition map follows the sites' decisions.
    use_effect(move || {
        let map = dispositions_of(sites.read().iter().map(|(_, r)| r));
        let mut dispositions = dispositions;
        dispositions.set(map);
    });

    // A decision from the viewer: the lane's own site decision on the finding under the viewer's
    // cursor. A reason-carrying state without a reason writes nothing and says so by returning
    // `false`, as the Queue's does.
    let apply_one = use_callback(
        move |(key, state, reason): (String, TriageState, String)| -> bool {
            if state.requires_reason() && reason.trim().is_empty() {
                return false;
            }
            let Some(node) = node_of(&sites.peek(), &key) else {
                return false;
            };
            decide.call((node, key, state, reason));
            true
        },
    );
    let apply_disposition = use_callback(move |(state, reason): (TriageState, String)| -> bool {
        let Some(key) = focused_key(&cursor.peek(), &visible_keys.peek()) else {
            return false;
        };
        apply_one.call((key, state, reason))
    });
    let on_dispose = use_callback(
        move |(rows, state, reason): (Vec<FindingView>, TriageState, String)| -> bool {
            if rows.is_empty() || (state.requires_reason() && reason.trim().is_empty()) {
                return false;
            }
            for f in &rows {
                apply_one.call((finding_key(f), state, reason.clone()));
            }
            true
        },
    );

    // Closing: the lane's cursor goes to the site the reviewer ended on, and the lane's own
    // region takes the keyboard back.
    let land_on = use_callback(move |key: String| {
        if let Some(node) = node_of(&sites.peek(), &key) {
            focus_site.call((node, key));
        }
    });
    let viewer_close = use_callback(move |()| {
        let ended_on = focused_key(&cursor.peek(), &visible_keys.peek());
        viewer_open.set(false);
        if let Some(key) = ended_on {
            land_on.call(key);
        }
        if stack_open.peek().is_some() {
            focus_element(crate::cockpit::stack::STACK_ID);
        } else {
            focus_element(region_id);
        }
    });
    let stack_close = use_callback(move |card: Option<String>| {
        stack_open.set(None);
        if let Some(key) = card {
            land_on.call(key);
        }
        focus_element(region_id);
    });
    let open_viewer_on = use_callback(move |key: String| -> bool {
        match reach_finding(handle, &key) {
            Ok(()) => {
                cursor.set(RowCursor::focused_on(key));
                viewer_open.set(true);
                true
            }
            Err(why) => {
                say(ToastKind::Info, why.to_string());
                false
            }
        }
    });

    // Register both links, and drop them when the lane goes. Rebuilt when the sites change so the
    // gutter and the stack's decision memory see the rows the lane has now.
    use_effect(move || {
        let all = views.read().clone();
        if let Some(mut slot) = viewer_slot {
            slot.0.set(Some(ViewerLink {
                handle,
                cursor,
                visible_keys,
                dispositions,
                all_findings: all.clone(),
                shas: Arc::new(HashMap::new()),
                sha_conflicts: Arc::new(HashSet::new()),
                project_id: project_id.clone(),
                open: viewer_open,
                undo_entry,
                now_ms,
                on_undo: Callback::new(|()| {}),
                apply_disposition,
                on_close: viewer_close,
                on_open_modal: Callback::new(move |()| {
                    say(
                        ToastKind::Info,
                        "The full detail is the pane beside the tree. Close the viewer to see it."
                            .to_string(),
                    );
                }),
                origins: Arc::new(HashMap::new()),
                descriptions: Arc::new(HashMap::new()),
                on_open_hub: Callback::new(move |_| {
                    say(
                        ToastKind::Info,
                        "The Rule Hub opens from the Flat view.".to_string(),
                    );
                }),
            }));
        }
        if let Some(mut slot) = stack_slot {
            slot.0.set(Some(StackLink {
                open: stack_open,
                dispositions,
                all_findings: all,
                shas: Arc::new(HashMap::new()),
                project_id: project_id.clone(),
                undo_entry,
                now_ms,
                on_undo: Callback::new(|()| {}),
                on_dispose,
                on_open_viewer: open_viewer_on,
                on_close: stack_close,
            }));
        }
    });
    use_drop(move || {
        if let Some(mut slot) = viewer_slot {
            slot.0.set(None);
        }
        if let Some(mut slot) = stack_slot {
            slot.0.set(None);
        }
    });

    let open_code = use_callback(move |(_node, key): (String, String)| {
        let _ = open_viewer_on.call(key);
    });
    let open_stack = use_callback(move |(node, anchor): (String, Option<String>)| {
        let rows: Vec<FindingView> = sites
            .peek()
            .iter()
            .filter(|(n, _)| *n == node)
            .map(|(_, r)| finding_view_of(r))
            .collect();
        let Some(first) = rows.first() else {
            say(
                ToastKind::Info,
                "B stacks the sites of a sub-group. Open one and focus it first.".to_string(),
            );
            return;
        };
        let label = rule_group_label(first);
        let id = *stack_serial.peek() + 1;
        stack_serial.set(id);
        stack_open.set(Some(StackOpen {
            id,
            label,
            rows,
            origin: StackOrigin::Pane,
            anchor,
        }));
    });

    CodeBridge {
        open_code,
        open_stack,
    }
}

#[cfg(test)]
mod tests;
