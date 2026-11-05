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
//! build (Escalations, Engine Check) show their tab and say so until they land; the Flat view is
//! the existing Queue, handed in as `flat`.

use camerata_api_types::review_view::ReviewShellView;
use camerata_ui_core::tree::shell::{
    held_text, is_empty_run, lane_for_key, lane_tabs, strip_text, Lane, NO_INSPECTION,
};
use camerata_ui_core::tree::ViewStatus;
use dioxus::prelude::*;

use super::engine_check::EngineHost;
use super::queue::{AppliedBudget, QueueHost};
use super::review_settings::ReviewSettingsHost;
use super::tree::{DoorRequest, ShellRefresh, TreeHost};
use camerata_api_types::review_view::EscalationBudgetOutcome;
use camerata_ui::review_io::ReviewApi;
use camerata_ui_core::review_settings::effect_sentence;

/// A request, from the shell's palette to the Escalations lane, to put the cursor on the first row
/// that still needs a decision. The lane clears it once it has done so.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct NeedsDecisionRequest(pub Signal<bool>);

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
    use_context_provider(|| camerata_ui::review_io::ReviewProject(project_id.clone()));
    // One counter for everything that reads the shell: the results screen provides it so the
    // export panel's gate re-reads after a decision too; a shell mounted alone makes its own.
    let own = use_signal(|| 0u32);
    let mut refresh = try_use_context::<ShellRefresh>().map_or(own, |r| r.0);
    use_context_provider(|| ShellRefresh(refresh));
    // A request from outside (the export panel's `open`) to show a site in the tree.
    let door = try_use_context::<DoorRequest>();
    let mut lane = use_signal(|| Lane::Tree);
    use_context_provider(|| LaneControl(lane));
    // What the Settings lane last applied, for the queue's "Budget is now ..." banner.
    let applied = use_signal(|| Option::<EscalationBudgetOutcome>::None);
    use_context_provider(|| AppliedBudget(applied));
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

    // A door from the export panel switches to the lane that owns what it opens: the tree for a
    // site or a node, the Engine Check lane for a held card. That lane's host takes the request.
    use_effect(move || {
        if let Some(wanted) = door.and_then(|d| d.0.read().clone()) {
            lane.set(wanted.lane());
        }
    });

    // The command palette (Ctrl or Cmd K): lines to go to each lane and to the first row that
    // needs a decision. The shell registers its link in the slot the results view provides, like
    // the flat Queue does, and takes it back when it unmounts.
    let palette_slot = try_use_context::<super::palette::PaletteSlot>();
    let mut palette_open = use_signal(|| Option::<super::palette::PaletteOpen>::None);
    let mut palette_serial = use_signal(|| 0u64);
    let mut needs_decision = use_signal(|| false);
    use_context_provider(|| NeedsDecisionRequest(needs_decision));
    let run_palette = use_callback(
        move |entry: camerata_ui_core::palette::PaletteEntry| -> bool {
            use camerata_ui_core::palette::PaletteAction;
            match entry.action {
                PaletteAction::GoLane(next) => {
                    lane.set(next);
                    true
                }
                PaletteAction::FirstNeedingDecision => {
                    lane.set(Lane::Escalations);
                    needs_decision.set(true);
                    true
                }
                _ => false,
            }
        },
    );
    let toggle_palette = use_callback(move |dom_modal: bool| {
        if palette_open.peek().is_some() {
            palette_open.set(None);
            super::palette::restore_focus();
            return;
        }
        // A dialog the page has up draws above the palette: refuse, as the flat Queue does.
        if dom_modal {
            return;
        }
        super::palette::remember_focus();
        let id = *palette_serial.peek() + 1;
        palette_serial.set(id);
        palette_open.set(Some(super::palette::PaletteOpen {
            id,
            entries: camerata_ui_core::palette::shell_entries(),
        }));
    });
    {
        let link = use_hook(|| super::palette::PaletteLink {
            open: palette_open,
            on_run: run_palette,
            on_chord: toggle_palette,
        });
        use_effect(move || {
            if let Some(mut slot) = palette_slot {
                slot.0.set(Some(link.clone()));
            }
        });
        use_drop(move || {
            if let Some(mut slot) = palette_slot {
                slot.0.set(None);
            }
        });
    }

    let active = lane();
    // The commit each repo was inspected at, for the code viewer and the stack the tree opens.
    let audited_shas = shell
        .read()
        .ready()
        .map(|v| v.audited_shas.clone())
        .unwrap_or_default();
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
                {lane_body(active, &children, audited_shas)}
            }
        }
    }
}

/// The lane that is showing.
fn lane_body(
    active: Lane,
    flat: &Element,
    audited_shas: std::collections::BTreeMap<String, String>,
) -> Element {
    match active {
        Lane::Tree => rsx! { TreeHost { audited_shas } },
        Lane::Escalations => rsx! { QueueHost { audited_shas } },
        Lane::EngineCheck => rsx! { EngineHost {} },
        Lane::Flat => flat.clone(),
        Lane::Settings => rsx! { SettingsLane {} },
    }
}

/// What to say once the settings lane has re-marked the line: the outcome in words, which always
/// includes that no model was called.
pub(crate) fn applied_message(outcome: &EscalationBudgetOutcome) -> String {
    effect_sentence(outcome)
}

/// The Settings lane: the host over the shell's api, the model registry for its selects, and a
/// run-in-flight flag from the scan panel's job. After an apply it says what happened and has the
/// shell read the counts again (the line moved, so the Escalations count may have).
#[component]
fn SettingsLane() -> Element {
    let api = use_context::<ReviewApi>();
    let mut refresh = use_context::<ShellRefresh>().0;
    let applied = try_use_context::<AppliedBudget>();
    let toasts = try_use_context::<Signal<Vec<crate::toast::Toast>>>();
    // The app-scope in-flight job id (the scan panel's); a job running locks the lane.
    let run_in_flight =
        try_use_context::<Signal<Option<String>>>().is_some_and(|job| job.read().is_some());
    let models = use_resource(crate::chat::fetch_models);
    let models = models.read().clone().flatten();
    rsx! {
        ReviewSettingsHost {
            api,
            run_in_flight,
            models,
            on_applied: move |outcome: EscalationBudgetOutcome| {
                if let Some(mut applied) = applied {
                    applied.0.set(Some(outcome.clone()));
                }
                if let Some(toasts) = toasts {
                    crate::toast::push_toast(
                        toasts,
                        crate::toast::ToastKind::Info,
                        applied_message(&outcome),
                    );
                }
                refresh += 1;
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use camerata_api_types::review::EscalationBudget;
    use camerata_ui::review_source::FixtureReviewSource;

    use super::*;

    #[derive(Clone, PartialEq, Props)]
    struct Case {
        lane: Lane,
        api: ReviewApi,
    }

    fn harness(case: Case) -> Element {
        use_context_provider(|| case.api.clone());
        let refresh = use_signal(|| 0u32);
        use_context_provider(|| ShellRefresh(refresh));
        let flat = rsx! { p { "the flat view" } };
        lane_body(case.lane, &flat, Default::default())
    }

    fn lane_html(lane: Lane) -> String {
        let fixture = Rc::new(FixtureReviewSource::base());
        crate::test_harness::render_fragment(
            harness,
            Case {
                lane,
                api: ReviewApi {
                    reads: fixture.clone(),
                    writes: fixture,
                },
            },
        )
    }

    #[test]
    fn the_settings_lane_mounts_the_settings_host() {
        let html = lane_html(Lane::Settings);
        assert!(html.contains("Loading settings"), "{html}");
        assert!(!html.contains("not built yet"), "{html}");
    }

    #[test]
    fn the_escalations_and_engine_check_lanes_mount_their_hosts_and_the_flat_view_is_handed_in() {
        let queue = lane_html(Lane::Escalations);
        assert!(queue.contains("Loading the escalations"), "{queue}");
        let engine = lane_html(Lane::EngineCheck);
        assert!(engine.contains("Loading the Engine Check"), "{engine}");
        for html in [&queue, &engine] {
            assert!(!html.contains("not built yet"), "{html}");
        }
        assert!(lane_html(Lane::Flat).contains("the flat view"));
    }

    #[test]
    fn an_applied_line_is_announced_with_the_no_model_sentence() {
        let line = EscalationBudget::default();
        let outcome = EscalationBudgetOutcome {
            applied: true,
            before: line,
            after: line,
            moved_above: 103,
            moved_below: 0,
        };
        let said = applied_message(&outcome);
        assert!(
            said.starts_with("103 candidates moved above the line."),
            "{said}"
        );
        assert!(said.ends_with("No model call was made."), "{said}");
    }
}
