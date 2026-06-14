//! Camerata — consumer-mode prototype (Dioxus DESKTOP).
//!
//! A runnable, mocked walkthrough of the full consumer journey described in
//! `docs/CONSUMER_UX.md`: Intake -> Clarify -> Build -> QA -> (Report a problem)
//! -> Publish/Live. No engine wiring yet; the goal of this pass is the look, the
//! motion, and the flow. The design bar is best-in-class consumer: generous
//! whitespace, a restrained palette (near-black text on near-white, one warm
//! accent), a clean system-font stack, large calm type, slow and subtle motion,
//! rounded surfaces, one clear primary action per screen.
//!
//! Run it with:
//!     cargo run -p camerata-ui
//! (or `dx serve` from crates/ui if you have the Dioxus CLI and prefer hot-reload).

mod data;
mod screens;
mod style;

use dioxus::prelude::*;

/// The screens of the consumer journey, plus the simple navigation state. One
/// enum + one signal is the whole router — deliberately minimal, because the flow
/// is mostly linear and the magic is in the transitions, not the addressing.
///
/// The journey is Intake -> Clarify -> Build -> Qa -> Live, with Bug as a side
/// loop off Qa (file a problem, watch it get fixed, land back in Qa). The
/// progress rail collapses Qa + Bug into a single "Try it" stop, since to the
/// user they are one activity: kicking the tires on their draft.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Intake,
    Clarify,
    Build,
    Qa,
    Bug,
    Live,
}

fn main() {
    dioxus::launch(App);
}

/// Root. Owns the current-screen signal and injects the global stylesheet once.
/// Each screen receives the signal so its primary action can advance the journey.
#[component]
fn App() -> Element {
    let screen = use_signal(|| Screen::Intake);

    rsx! {
        // Global stylesheet, injected as a raw <style> so it works identically on
        // desktop without the asset pipeline. Keeps the whole look in one place.
        style { dangerous_inner_html: style::GLOBAL_CSS }

        div { class: "app-root",
            // A quiet, fixed progress rail at the top — four dots that fill as the
            // user moves through the journey. It is orientation, not a dashboard.
            ProgressRail { screen }

            div { class: "stage",
                match screen() {
                    Screen::Intake => rsx! { screens::intake::IntakeScreen { screen } },
                    Screen::Clarify => rsx! { screens::clarify::ClarifyScreen { screen } },
                    Screen::Build => rsx! { screens::build::BuildScreen { screen } },
                    Screen::Qa => rsx! { screens::qa::QaScreen { screen } },
                    Screen::Bug => rsx! { screens::bug::BugScreen { screen } },
                    Screen::Live => rsx! { screens::live::LiveScreen { screen } },
                }
            }
        }
    }
}

/// The journey rail. Calm, slow, never numeric. Five stops; Qa and Bug share the
/// "Try it" stop because to the user they are the same activity (kicking the tires
/// on the draft and reporting anything off).
#[component]
fn ProgressRail(screen: Signal<Screen>) -> Element {
    let steps = [
        (Screen::Intake, "Describe"),
        (Screen::Clarify, "Clarify"),
        (Screen::Build, "Build"),
        (Screen::Qa, "Try it"),
        (Screen::Live, "Live"),
    ];
    let current = screen();
    let order = |s: Screen| match s {
        Screen::Intake => 0,
        Screen::Clarify => 1,
        Screen::Build => 2,
        // Qa and Bug are one stop on the rail.
        Screen::Qa | Screen::Bug => 3,
        Screen::Live => 4,
    };
    let current_order = order(current);

    rsx! {
        nav { class: "rail",
            div { class: "rail-inner",
                for (s , label) in steps {
                    {
                        let o = order(s);
                        let cls = if o < current_order {
                            "rail-step done"
                        } else if o == current_order {
                            "rail-step active"
                        } else {
                            "rail-step"
                        };
                        rsx! {
                            div { class: "{cls}",
                                span { class: "rail-dot" }
                                span { class: "rail-label", "{label}" }
                            }
                        }
                    }
                }
            }
        }
    }
}
