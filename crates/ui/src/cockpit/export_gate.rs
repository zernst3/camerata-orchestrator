//! The export-blocked state (review UI spec section 10, work order E).
//!
//! The export panel reads `ExportGateView` and, when the release gate refuses, replaces its export
//! button with ONE line and a button. The rows themselves are marked in the triage table (a red
//! "Held" status cell), so a run with many held rows no longer stretches the screen.
//! The gate itself stays mechanical in `release_gate.rs`; this only mirrors it. The count and the
//! set of held rows come only from `ExportGateView`: nothing is recomputed here.

use std::collections::HashSet;

use camerata_api_types::review_view::ExportGateView;
use camerata_ui_core::tree::export::{blocked_line, Door};
use dioxus::prelude::*;

use super::tree::{DoorRequest, ShellRefresh};
use camerata_ui::review_io::ReviewApi;

/// The element id of the triage table's wrapper, so "Show them" can scroll it into view.
pub(crate) const TABLE_ANCHOR_ID: &str = "camerata-findings-table";

/// What the export panel and the triage table share, one provider line each at the top of the
/// screens that hold both (`scan_results.rs` and `liveview.rs`): the gate's last read, the
/// "show only held rows" filter and which rows the table holds. Read with `try_use_context`, so a
/// screen with no provider (a test of one component) behaves exactly as before.
#[derive(Clone, Copy)]
pub(crate) struct HeldRows {
    /// The release gate's last read. Written by [`ExportGateHost`], read by the table.
    pub gate: Signal<ExportGateView>,
    /// Whether the operator wants the table to show only the held rows. Set by the panel's "Show
    /// them" button; the table clears it when no held row remains or the operator takes the filter
    /// off in the column header.
    pub filter: Signal<bool>,
    /// How many times "Show them" has been pressed. Each press also clears the table's other column
    /// filters, once, which `filter` alone cannot say.
    pub show_requests: Signal<u32>,
    /// The site keys the table holds, or `None` while no table is on screen.
    pub table_keys: Signal<Option<HashSet<String>>>,
}

impl HeldRows {
    /// The provider's value: nothing held, no filter, no table.
    pub(crate) fn new() -> Self {
        Self {
            gate: Signal::new(ExportGateView::default()),
            filter: Signal::new(false),
            show_requests: Signal::new(0),
            table_keys: Signal::new(None),
        }
    }
}

/// Scroll the triage table into view (a no-op where there is no document).
pub(crate) fn scroll_table_into_view() {
    let _ = dioxus::document::eval(&format!(
        "document.getElementById('{TABLE_ANCHOR_ID}')?.scrollIntoView({{block: 'start'}});"
    ));
}

/// The line when the gate refuses, or `children` (the export button) when it does not.
///
/// `table_keys` is the site keys the triage table holds (`None`: no table on screen). `on_show`
/// runs on "Show them" (the host sets the table's filter and scrolls it into view) and `on_open`
/// on "Open Review" with a door to the first row only Review holds.
#[component]
pub(crate) fn ExportGate(
    gate: ExportGateView,
    #[props(default)] table_keys: Option<HashSet<String>>,
    #[props(default)] on_show: Callback<()>,
    #[props(default)] on_open: Callback<Door>,
    children: Element,
) -> Element {
    let Some(line) = blocked_line(&gate, table_keys.as_ref()) else {
        return rsx! { div { class: "export-clear", {children} } };
    };
    let door = line.review_door.clone();
    rsx! {
        div { class: "export-blocked", role: "alert",
            p { class: "export-blocked-head", "{line.text}" }
            if line.in_table > 0 {
                button {
                    class: "btn-edit-sm export-show-held",
                    onclick: move |_| on_show.call(()),
                    "Show them"
                }
            }
            if let Some(door) = door {
                button {
                    class: "btn-edit-sm export-open-review",
                    onclick: move |_| on_open.call(door.clone()),
                    "Open Review"
                }
            }
        }
    }
}

/// The gate with its read: it asks the server what the release gate refuses, and shows the line
/// or `children`. A read that fails shows the children: the gate itself still refuses at export,
/// and this panel only mirrors it. `open` asks the shell to show a row in the tree.
#[component]
pub(crate) fn ExportGateHost(
    project_id: String,
    #[props(default)] on_open: Callback<Door>,
    children: Element,
) -> Element {
    // Read the context outside the hook: a hook may not be called inside another hook's closure.
    let shared = try_use_context::<ReviewApi>();
    let api = use_hook(|| {
        shared.unwrap_or_else(|| ReviewApi::http(crate::bff_base(), project_id.clone()))
    });
    let door = try_use_context::<DoorRequest>();
    let refresh = try_use_context::<ShellRefresh>();
    let held = try_use_context::<HeldRows>();
    // Where the read lands: the shared signal when the screen provides one (so the table marks the
    // same rows), else this host's own.
    let own = use_signal(ExportGateView::default);
    let mut gate = held.map_or(own, |h| h.gate);
    // Read the gate now and again whenever a decision is written (the tree bumps the counter).
    use_effect(move || {
        if let Some(r) = refresh {
            let _ = r.0.read();
        }
        let api = api.clone();
        spawn(async move {
            if let Ok(g) = api.reads.export_gate().await {
                gate.set(g);
            }
        });
    });
    let table_keys = held.and_then(|h| h.table_keys.read().clone());
    rsx! {
        ExportGate {
            gate: gate.read().clone(),
            table_keys,
            on_show: move |()| {
                if let Some(mut h) = held {
                    h.filter.set(true);
                    h.show_requests += 1;
                }
                scroll_table_into_view();
            },
            on_open: move |d: Door| {
                if let Some(mut door) = door {
                    door.0.set(Some(d.clone()));
                }
                on_open.call(d);
            },
            {children}
        }
    }
}
