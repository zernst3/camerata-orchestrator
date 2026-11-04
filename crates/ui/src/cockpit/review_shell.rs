//! The review shell (review UI spec section 4): the lane switcher and the progress strip above
//! whichever lane is showing.
//!
//! ```text
//! [ Tree 1,204 undecided ] [ Escalations 347 ] [ Engine Check 50 ] [ Flat view ] [ Settings ]
//! Undecided 1,204 of 30,112 . Confirmed 27,900 . Ignored 600 . FP 310 . Tech debt 98 . Held 3
//! ```
//!
//! [`ReviewShell`] is props-only (the gallery renders it from fixtures); [`ReviewShellHost`] owns
//! the lane and the shell read, and mounts the lane that is showing. The lanes other work orders
//! build (Escalations, Engine Check, Settings) show their tab and say so until they land; the
//! Flat view is the existing Queue, handed in as `flat`.

use camerata_api_types::review_view::ReviewShellView;
use camerata_ui_core::tree::shell::{
    held_text, is_empty_run, lane_for_key, lane_tabs, strip_text, Lane, NO_INSPECTION,
};
use camerata_ui_core::tree::ViewStatus;
use dioxus::prelude::*;

use super::tree::{DoorRequest, ShellRefresh, TreeHost};
use camerata_ui::review_io::ReviewApi;

/// The lane signal, provided by the host so a lane can switch to another with `1` to `5`.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct LaneControl(pub Signal<Lane>);

/// The shell: lane tabs, the strip, and the lane's body as `children`.
#[component]
pub(crate) fn ReviewShell(
    status: ViewStatus<ReviewShellView>,
    active: Lane,
    #[props(default)] on_lane: Callback<Lane>,
    /// A key pressed on the tab row, as `(key, a modifier that claims it is held)`. The host runs
    /// it through `lane_for_key`.
    #[props(default)]
    on_tab_key: Callback<(String, bool)>,
    #[props(default)] on_retry: Callback<()>,
    /// Say "No inspection to review" under the tabs when the run has no sites. The lane that is
    /// showing says it itself, so only a shell shown alone asks for it.
    #[props(default)]
    note_when_empty: bool,
    children: Element,
) -> Element {
    rsx! {
        div { class: "review-shell",
            {
                match &status {
                    ViewStatus::Loading => rsx! {
                        div { class: "triage-switch",
                            for lane in Lane::ALL {
                                button { key: "{lane.slug()}", class: "triage-tab", disabled: true, "{lane.label()}" }
                            }
                        }
                        p { class: "review-loading", "Loading the review" }
                    },
                    ViewStatus::Failed(message) => rsx! {
                        div { class: "review-banner", role: "alert",
                            span { class: "review-banner-msg", "{message}" }
                            button { class: "btn-edit-sm", onclick: move |_| on_retry.call(()), "Retry" }
                        }
                    },
                    ViewStatus::Ready(shell) => rsx! {
                        div {
                            class: "triage-switch",
                            role: "tablist",
                            "aria-label": "Review lanes",
                            onkeydown: move |e: KeyboardEvent| {
                                let m = e.modifiers();
                                let claimed = m.contains(Modifiers::CONTROL)
                                    || m.contains(Modifiers::ALT)
                                    || m.contains(Modifiers::META);
                                on_tab_key.call((e.key().to_string(), claimed));
                            },
                            for tab in lane_tabs(&shell.lanes, active) {
                                button {
                                    key: "{tab.lane.slug()}",
                                    class: if tab.active { "triage-tab active" } else { "triage-tab" },
                                    role: "tab",
                                    "aria-selected": "{tab.active}",
                                    title: "Press {tab.lane.digit()} to switch to {tab.lane.label()}",
                                    onclick: move |_| on_lane.call(tab.lane),
                                    "{tab.label} "
                                    if let Some(count) = tab.count.clone() {
                                        span { class: "triage-tab-count", "{count}" }
                                    }
                                }
                            }
                        }
                        if is_empty_run(&shell.counts) {
                            if note_when_empty {
                                p { class: "pane-empty", "{NO_INSPECTION}" }
                            }
                        } else {
                            div { class: "progress-strip review-strip", role: "group", "aria-label": "Review progress",
                                span { class: "progress-counts", "{strip_text(&shell.counts)}" }
                                if let Some(held) = held_text(&shell.counts) {
                                    span { class: "progress-eta", " \u{b7} " }
                                    span { class: "strip-held", "{held}" }
                                }
                            }
                        }
                    },
                }
            }
            {children}
        }
    }
}

/// The shell with its I/O: it reads the shell once and again after every write, owns the lane,
/// listens for `1` to `5`, and mounts the lane that is showing.
#[component]
pub(crate) fn ReviewShellHost(
    project_id: String,
    /// The existing Queue, shown in the Flat view lane.
    children: Element,
) -> Element {
    let api = use_context_provider(|| ReviewApi::http(crate::bff_base(), project_id.clone()));
    // One counter for everything that reads the shell: the results screen provides it so the
    // export panel's gate re-reads after a decision too; a shell mounted alone makes its own.
    let own = use_signal(|| 0u32);
    let mut refresh = try_use_context::<ShellRefresh>().map_or(own, |r| r.0);
    use_context_provider(|| ShellRefresh(refresh));
    // A request from outside (the export panel's `open`) to show a site in the tree.
    let door = try_use_context::<DoorRequest>();
    let mut lane = use_signal(|| Lane::Tree);
    use_context_provider(|| LaneControl(lane));
    let mut shell = use_signal(|| ViewStatus::<ReviewShellView>::Loading);

    let load = use_callback({
        let api = api.clone();
        move |()| {
            let api = api.clone();
            spawn(async move {
                match api.reads.shell().await {
                    Ok(v) => shell.set(ViewStatus::Ready(v)),
                    // A refresh that fails keeps the counts already on screen.
                    Err(e) if shell.peek().ready().is_none() => {
                        shell.set(ViewStatus::Failed(e.message));
                    }
                    Err(_) => {}
                }
            });
        }
    });
    // Read the shell now and whenever a lane writes.
    use_effect(move || {
        let _ = refresh.read();
        load.call(());
    });

    // A door from the export panel switches to the tree.
    use_effect(move || {
        if door.is_some_and(|d| d.0.read().is_some()) {
            lane.set(Lane::Tree);
        }
    });

    let active = lane();
    rsx! {
        div {
            ReviewShell {
                status: shell.read().clone(),
                active,
                on_lane: move |l: Lane| lane.set(l),
                // `1` to `5` switch lanes from the tab row; the tree switches them the same way
                // while its own region has focus. No text field is inside either.
                on_tab_key: move |(key, claimed): (String, bool)| {
                    if let Some(next) = lane_for_key(&key, claimed, false) {
                        lane.set(next);
                    }
                },
                on_retry: move |()| {
                    shell.set(ViewStatus::Loading);
                    refresh += 1;
                },
                {
                    match active {
                        Lane::Tree => rsx! { TreeHost {} },
                        Lane::Flat => children.clone(),
                        other => rsx! {
                            p { class: "pane-empty",
                                "The {other.label()} lane is not built yet. The Tree and the Flat view are."
                            }
                        },
                    }
                }
            }
        }
    }
}
