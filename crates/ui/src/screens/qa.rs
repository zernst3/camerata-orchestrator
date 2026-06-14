//! Screen 4 — QA. The user tests their own app, in DRAFT (CONSUMER_UX.md §4).
//!
//! The built app opens in a draft state and the user is the QA: they click around
//! a mocked preview of the generated app, tick off the things they asked for, and
//! decide whether it does what they meant. The product is honest that this is a
//! draft the user verifies, not a finished thing dropped on them.
//!
//! Two clear exits: "It's right — publish it" (-> Live) or "Something's off" (->
//! the structured bug form). The preview is a small, believable rendering of the
//! pottery app (a phone frame showing the class list with the waitlist state),
//! dogfooding the calm look rather than a placeholder rectangle.

use dioxus::prelude::*;

use crate::data;
use crate::Screen;

#[component]
pub fn QaScreen(screen: Signal<Screen>) -> Element {
    let classes = use_signal(data::preview_classes);
    let checks = use_signal(data::qa_checklist);

    // Which "does it do this?" items the user has confirmed. Pure UI delight: the
    // user can tick them off as they verify, and the publish action warms up.
    let mut confirmed = use_signal(Vec::<usize>::new);
    let total = checks().len();
    let n_confirmed = confirmed().len();

    rsx! {
        div { class: "page page-wide qa",
            p { class: "eyebrow", "Draft · your turn to try it" }
            h1 { class: "h1", "Have a look — is this what you meant?" }
            p { class: "lede", "{data::QA_INTRO}" }

            div { class: "qa-grid",
                // Left: the mocked preview of the generated app, in a phone frame.
                div { class: "qa-preview",
                    div { class: "phone",
                        div { class: "phone-notch" }
                        div { class: "phone-screen",
                            div { class: "app-bar",
                                span { class: "app-bar-title", "Riverside Pottery" }
                                span { class: "app-bar-dot" }
                            }
                            div { class: "app-body",
                                p { class: "app-h", "This week's classes" }
                                for c in classes() {
                                    div { class: "app-card",
                                        div { class: "app-card-top",
                                            span { class: "app-card-title", "{c.title}" }
                                            span { class: "app-card-price", "{c.price}" }
                                        }
                                        div { class: "app-card-meta",
                                            span { "{c.when}" }
                                            span { class: "dotsep", "·" }
                                            span { "{c.seats}" }
                                        }
                                        if c.full {
                                            button { class: "app-cta waitlist", "Join the waitlist" }
                                        } else {
                                            button { class: "app-cta", "Book a seat" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    p { class: "qa-draft-tag", "A live, clickable draft — only you can see it." }
                }

                // Right: the honest checklist of what they asked for.
                div { class: "qa-side",
                    p { class: "section-label", "Does it do what you asked for?" }
                    p { class: "section-hint", "Tap each one as you try it. No pressure to check them all — this is just for you." }
                    div { class: "qa-checks",
                        for (i , item) in checks().into_iter().enumerate() {
                            {
                                let on = confirmed().contains(&i);
                                let cls = if on { "qa-check on" } else { "qa-check" };
                                rsx! {
                                    button {
                                        class: "{cls}",
                                        onclick: move |_| {
                                            if confirmed().contains(&i) {
                                                confirmed.write().retain(|x| *x != i);
                                            } else {
                                                confirmed.write().push(i);
                                            }
                                        },
                                        span { class: "qa-tick", if on { "✓" } else { "" } }
                                        span { class: "qa-check-text", "{item}" }
                                    }
                                }
                            }
                        }
                    }
                    p { class: "qa-progress", "{n_confirmed} of {total} confirmed" }
                }
            }

            div { class: "actions",
                button {
                    class: "btn-primary",
                    onclick: move |_| screen.set(Screen::Live),
                    "It's right — publish it"
                }
                button {
                    class: "btn-quiet",
                    onclick: move |_| screen.set(Screen::Bug),
                    "Something's off — tell the engineer"
                }
            }
        }
    }
}
