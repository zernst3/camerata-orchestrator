//! Screen 6 — PUBLISH / LIVE. The payoff (CONSUMER_UX.md §6).
//!
//! Only when the user is satisfied do they publish: the app leaves DRAFT and goes
//! live on their OWN cloud (bring-your-own-infra for the prototype). "Your app is
//! live" with a real URL on their own account. The draft-to-publish gate keeps the
//! user in control of when their app becomes real.
//!
//! This screen mocks the brief publish beat (draft -> published) and then settles
//! into the live state: the badge, the URL on the user's own cloud, the gentle
//! next steps (open it, or keep changing it), and the standing Camerata maintenance
//! panel that keeps the app healthy after launch.

use std::time::Duration;

use dioxus::prelude::*;

use crate::app_state::AppState;
use crate::data;
use crate::Screen;

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Publishing,
    Live,
}

/// The view-model the maintenance panel renders from. Avoids storing a full
/// `MaintenanceScan` (which is not `Copy`) directly in a Dioxus signal.
#[derive(Clone, PartialEq)]
enum MaintenanceView {
    /// Scan has not completed yet.
    Loading,
    /// Scan completed: optional security warning text and the raw scan for
    /// building the approval plan on button click.
    Ready {
        warning: Option<String>,
        scan: camerata_maintenance::MaintenanceScan,
    },
}

#[component]
pub fn LiveScreen(screen: Signal<Screen>) -> Element {
    let app = use_context::<Signal<Option<AppState>>>();
    let mut phase = use_signal(|| Phase::Publishing);
    // The live URL returned by the real deploy seam (LocalDeployTarget by default).
    let mut live_url = use_signal(|| None::<String>);
    // Maintenance panel state. Starts Loading; populated once publish completes.
    let mut maintenance = use_signal(|| MaintenanceView::Loading);
    // Whether the user clicked "Update now" and we showed the calm confirmation.
    let mut update_confirmed = use_signal(|| false);

    // A short, honest publishing beat that REALLY deploys through the seam, then
    // transitions to Live and kicks off the background maintenance scan.
    // No signal guards are held across awaits: each await stores its result in
    // a local variable and calls `.set()` once it is done.
    let _driver = use_future(move || async move {
        let app_name = app
            .peek()
            .as_ref()
            .map(|s| s.project.onboarding.app_name.clone())
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| "your-app".to_string());

        // Deploy first.
        let outcome = crate::deploy_run::publish_app(&app_name).await;
        if let Some(url) = outcome.url {
            live_url.set(Some(url));
        }
        tokio::time::sleep(Duration::from_millis(2000)).await;
        phase.set(Phase::Live);

        // Scan for maintenance items after the app is live.
        let scan = crate::maintenance_run::scan_app(&app_name).await;
        let warning = crate::maintenance_run::warning_for(&scan);
        maintenance.set(MaintenanceView::Ready { warning, scan });
    });

    match phase() {
        Phase::Publishing => rsx! {
            div { class: "page live",
                div { class: "publishing",
                    div { class: "spinner big" }
                    h1 { class: "h1", "Publishing to your cloud" }
                    p { class: "lede", "Moving it out of draft and onto your own account. This is the moment it becomes real, and it is entirely yours." }
                }
            }
        },
        Phase::Live => rsx! {
            div { class: "page live",
                div { class: "live-badge", "✓" }
                p { class: "eyebrow", "Published" }
                h1 { class: "h1", "Your app is live" }
                p { class: "lede", "It's running on your own cloud, under your account. Here's the address. Share it with whoever you like." }

                div { class: "live-url",
                    span { class: "lock", "🔒" }
                    span { "{live_url().unwrap_or_else(|| data::LIVE_URL.to_string())}" }
                }
                p { class: "live-own", "On your own cloud · you own it, you control it" }

                div { class: "live-actions",
                    button { class: "btn-primary", "Open my app" }
                    button {
                        class: "btn-quiet",
                        onclick: move |_| screen.set(Screen::Qa),
                        "Keep changing it"
                    }
                    button {
                        class: "btn-quiet",
                        onclick: move |_| screen.set(Screen::Intake),
                        "Start a new app"
                    }
                }

                // Maintenance panel. Shown below the live actions; calm and
                // unobtrusive. Only surfaces a call to action when there is
                // a security finding worth the user's attention.
                MaintenancePanel {
                    view: maintenance(),
                    update_confirmed: update_confirmed(),
                    on_update_now: move |scan: camerata_maintenance::MaintenanceScan| {
                        // Build and approve the plan (real deployment would route
                        // this through the governed build-and-QA loop).
                        let _plan = crate::maintenance_run::approve_all_security(&scan);
                        update_confirmed.set(true);
                    }
                }
            }
        },
    }
}

/// The calm maintenance panel. Displayed after the app goes live.
///
/// When the scan is still loading it shows a quiet "looking after your app"
/// note. When a security finding is present it surfaces the plain-language
/// recommendation and an "Update now" button that, on click, approves all
/// security findings and shows a calm confirmation.
#[component]
fn MaintenancePanel(
    view: MaintenanceView,
    update_confirmed: bool,
    on_update_now: EventHandler<camerata_maintenance::MaintenanceScan>,
) -> Element {
    rsx! {
        div { class: "maintenance-panel",
            div { class: "maintenance-header",
                span { class: "maintenance-icon", "○" }
                span { class: "maintenance-title", "Camerata is looking after your app" }
            }

            match view {
                MaintenanceView::Loading => rsx! {
                    p { class: "maintenance-note",
                        "Checking your app for anything worth knowing about."
                    }
                },
                MaintenanceView::Ready { warning, scan } => {
                    if update_confirmed {
                        rsx! {
                            p { class: "maintenance-note maintenance-confirmed",
                                "Scheduling the update through the same checks that built your app."
                            }
                        }
                    } else if let Some(msg) = warning {
                        rsx! {
                            p { class: "maintenance-note", "{msg}" }
                            button {
                                class: "maintenance-update-btn",
                                onclick: move |_| on_update_now.call(scan.clone()),
                                "Update now"
                            }
                        }
                    } else {
                        rsx! {
                            p { class: "maintenance-note",
                                "Everything looks good. Nothing needs attention right now."
                            }
                        }
                    }
                }
            }
        }
    }
}
