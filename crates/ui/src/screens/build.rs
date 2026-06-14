//! Screen 3 — BUILD. Calm, governed construction, narrated as a single slow
//! progress story (CONSUMER_UX.md §3). No logs, no terminal, no jitter, no
//! percentages — just human-readable stages completing one by one with a quiet
//! check, the way a well-made product reassures you that something is handled.
//!
//! The lead engineer stays reachable: one genuine mid-build question surfaces
//! once, calmly, and the progress quietly waits on the user's answer (never an
//! error, just the engineer still listening). When the gate bounces an agent
//! underneath, a stage would simply take a little longer — here that's mocked as
//! the hand-tuned dwell on each stage.
//!
//! The motion is driven by a single `use_future` that walks the stage list with
//! `tokio::time::sleep`. The desktop renderer runs on Tokio, so the sleep just
//! works; the future pauses itself on the question and resumes when answered.

use std::time::Duration;

use dioxus::prelude::*;

use crate::data;
use crate::Screen;

#[component]
pub fn BuildScreen(screen: Signal<Screen>) -> Element {
    let stages = data::BUILD_STAGES;
    let mid_q = use_signal(data::mid_build_question);

    // How many stages have completed. `active` is `done`, and `done == len` means
    // the build is finished and we can move the user to QA.
    let mut done = use_signal(|| 0usize);
    // When `Some`, the build is paused on the mid-build question and waits for an
    // answer before continuing past `after_stage`.
    let mut pending_q = use_signal(|| false);
    let answered_q = use_signal(|| false);

    // The single driver: advance one stage at a time on a calm cadence, pausing
    // when the mid-build question is due until the user has answered it.
    let _driver = use_future(move || {
        let stages = stages;
        let after = mid_q().after_stage;
        async move {
            loop {
                let i = done();
                if i >= stages.len() {
                    // Finished. A short, settled beat, then hand off to QA.
                    tokio::time::sleep(Duration::from_millis(900)).await;
                    screen.set(Screen::Qa);
                    break;
                }
                // If the genuine question is due after this stage and unanswered,
                // surface it and wait. The progress holds calmly, not as an error.
                if i == after + 1 && !answered_q() {
                    pending_q.set(true);
                    // Poll gently until the user answers; cheap, and keeps the
                    // driver as the single source of motion.
                    loop {
                        tokio::time::sleep(Duration::from_millis(120)).await;
                        if answered_q() {
                            break;
                        }
                    }
                    pending_q.set(false);
                }
                tokio::time::sleep(Duration::from_millis(stages[i].dwell_ms)).await;
                done.set(i + 1);
            }
        }
    });

    let total = stages.len();
    let done_n = done();

    rsx! {
        div { class: "page build",
            p { class: "eyebrow", "Building" }
            h1 { class: "h1", "Putting it together" }
            p { class: "lede", "No need to watch — but if you like, here's what I'm doing. I'm checking every piece against your rules as I go." }

            div { class: "build-list",
                for (i , stage) in stages.iter().enumerate() {
                    {
                        let cls = if i < done_n {
                            "build-stage done"
                        } else if i == done_n && !pending_q() {
                            "build-stage active"
                        } else if i == done_n && pending_q() {
                            // Held at the current stage while the question is open.
                            "build-stage active"
                        } else {
                            "build-stage pending"
                        };
                        rsx! {
                            div { class: "{cls}",
                                span { class: "stage-mark",
                                    if i < done_n {
                                        "✓"
                                    } else if i == done_n {
                                        span { class: "spinner" }
                                    }
                                }
                                span { class: "stage-text", "{stage.label}" }
                            }
                        }
                    }
                }
            }

            // The one genuine mid-build question, surfaced calmly inline.
            if pending_q() {
                MidBuildQuestion { q: mid_q(), answered: answered_q }
            }

            if done_n < total && !pending_q() {
                p { class: "build-caption", "This usually takes a moment. Everything you see ticked has already passed the rules." }
            }
        }
    }
}

/// The mid-build question: same calm voice as clarify, never an error. Answering
/// it (chip or free text) lets the build continue.
#[component]
fn MidBuildQuestion(q: data::MidBuildQuestion, answered: Signal<bool>) -> Element {
    let mut answered = answered;
    let mut draft = use_signal(String::new);
    let mut chosen = use_signal(|| Option::<String>::None);

    if let Some(text) = chosen() {
        // Once answered, show the exchange settled, then it folds away as the
        // build resumes.
        return rsx! {
            div { class: "midq settled",
                div { class: "midq-answer", "✓ " "{text}" }
                span { class: "midq-resume", "Got it — carrying on." }
            }
        };
    }

    rsx! {
        div { class: "midq",
            div { class: "who",
                span { class: "who-avatar", "LE" }
                span { "Lead engineer · one quick check" }
            }
            p { class: "q-text", "{q.question}" }
            p { class: "q-reason", "{q.reason}" }
            div { class: "chips dock-chips",
                for chip in q.chips.clone() {
                    button {
                        class: "chip",
                        onclick: {
                            let chip = chip.clone();
                            move |_| {
                                chosen.set(Some(chip.clone()));
                                answered.set(true);
                            }
                        },
                        "{chip}"
                    }
                }
            }
            div { class: "dock-row",
                input {
                    class: "dock-input",
                    value: "{draft}",
                    placeholder: "…or say it your way",
                    oninput: move |e| draft.set(e.value()),
                    onkeydown: move |e| {
                        if e.key() == Key::Enter && !draft().trim().is_empty() {
                            chosen.set(Some(draft().trim().to_string()));
                            answered.set(true);
                        }
                    },
                }
                button {
                    class: "dock-send",
                    onclick: move |_| {
                        let text = draft().trim().to_string();
                        if !text.is_empty() {
                            chosen.set(Some(text));
                            answered.set(true);
                        }
                    },
                    "Send"
                }
            }
        }
    }
}
