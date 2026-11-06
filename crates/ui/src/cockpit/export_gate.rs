//! The export-blocked state (review UI spec section 10).
//!
//! The export panel reads `ExportGateView` and, when any list is non-empty, replaces its export
//! button with the panel below: one line per row the release gate refuses, each with a door to
//! it. The gate itself stays mechanical in `release_gate.rs`; this only mirrors it.

use camerata_api_types::review_view::ExportGateView;
use camerata_ui_core::tree::export::{blocked_headline, gate_rows, Door};
use dioxus::prelude::*;

use super::tree::{DoorRequest, ShellRefresh};
use camerata_ui::review_io::ReviewApi;

/// The panel when the gate refuses, or `children` (the export button) when it does not.
#[component]
pub(crate) fn ExportGate(
    gate: ExportGateView,
    #[props(default)] on_open: Callback<Door>,
    children: Element,
) -> Element {
    let Some(headline) = blocked_headline(&gate) else {
        return rsx! { div { class: "export-clear", {children} } };
    };
    let rows = gate_rows(&gate);
    rsx! {
        div { class: "export-blocked", role: "alert",
            p { class: "export-blocked-head", "{headline}" }
            div { class: "gate-rows",
                for (i, row) in rows.into_iter().enumerate() {
                    div { key: "{i}", class: "gate-row",
                        span { class: "gate-word", "{row.kind.word()}" }
                        span { class: "gate-place", "{row.place}" }
                        span { class: "gate-rule", "{row.rule_id}" }
                        span { class: "gate-reason", "{row.reason}" }
                        button {
                            class: "btn-edit-sm",
                            onclick: {
                                let door = row.door.clone();
                                move |_| on_open.call(door.clone())
                            },
                            "open"
                        }
                    }
                }
            }
        }
    }
}

/// The gate with its read: it asks the server what the release gate refuses, and shows the panel
/// or `children`. A read that fails shows the children: the gate itself still refuses at export,
/// and this panel only mirrors it. `open` asks the shell to show the row in the tree.
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
    let mut gate = use_signal(ExportGateView::default);
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
    rsx! {
        ExportGate {
            gate: gate.read().clone(),
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
