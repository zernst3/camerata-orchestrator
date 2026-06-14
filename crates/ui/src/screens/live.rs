//! Screen 6 — PUBLISH / LIVE. The payoff (CONSUMER_UX.md §6).
//!
//! Only when the user is satisfied do they publish: the app leaves DRAFT and goes
//! live on their OWN cloud (bring-your-own-infra for the prototype). "Your app is
//! live" with a real URL on their own account. The draft-to-publish gate keeps the
//! user in control of when their app becomes real.
//!
//! This screen mocks the brief publish beat (draft -> published) and then settles
//! into the live state: the badge, the URL on the user's own cloud, and the gentle
//! next steps (open it, or keep changing it — the tracked-changes loop).

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

#[component]
pub fn LiveScreen(screen: Signal<Screen>) -> Element {
    let app = use_context::<Signal<Option<AppState>>>();
    let mut phase = use_signal(|| Phase::Publishing);
    // The live URL returned by the real deploy seam (LocalDeployTarget by default).
    let mut live_url = use_signal(|| None::<String>);

    // A short, honest publishing beat that REALLY deploys through the seam, then the
    // live state with the returned URL. Honesty over instant magic.
    let _driver = use_future(move || async move {
        let app_name = app
            .peek()
            .as_ref()
            .map(|s| s.project.onboarding.app_name.clone())
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| "your-app".to_string());
        let outcome = crate::deploy_run::publish_app(&app_name).await;
        if let Some(url) = outcome.url {
            live_url.set(Some(url));
        }
        tokio::time::sleep(Duration::from_millis(2000)).await;
        phase.set(Phase::Live);
    });

    match phase() {
        Phase::Publishing => rsx! {
            div { class: "page live",
                div { class: "publishing",
                    div { class: "spinner big" }
                    h1 { class: "h1", "Publishing to your cloud" }
                    p { class: "lede", "Moving it out of draft and onto your own account. This is the moment it becomes real — and it's entirely yours." }
                }
            }
        },
        Phase::Live => rsx! {
            div { class: "page live",
                div { class: "live-badge", "✓" }
                p { class: "eyebrow", "Published" }
                h1 { class: "h1", "Your app is live" }
                p { class: "lede", "It's running on your own cloud, under your account. Here's the address — share it with whoever you like." }

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
            }
        },
    }
}
