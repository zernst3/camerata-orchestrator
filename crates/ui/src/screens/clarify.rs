//! Screen 2 — CLARIFY. The hero, and the centerpiece of the whole flow.
//!
//! The lead engineer reads the brief and works the user through a short checklist
//! of the things it needs pinned down before it's comfortable building. Per
//! CONSUMER_UX.md this screen carries the differentiator, so it gets the spotlight:
//!
//!  - a running transcript (engineer questions + the user's answers as bubbles),
//!  - free-text-led answering with quick-reply chips and a short reason per turn,
//!  - a visible CONFIDENCE SCORE that climbs as the checklist fills,
//!  - a product-level SUGGESTION the user didn't think of (the admin/permissions
//!    area), given a warmer treatment than a plain question,
//!  - an "I have what I need / just build it" BYPASS available at any time, with
//!    the score showing honestly what's being traded off,
//!  - and an ending PLAN shown as BOTH prose AND a simple visual entity/action map.
//!
//! All mocked: tapping a chip (or Send) accepts the canned answer for that turn
//! and advances. The point is the rhythm and the feel.

use dioxus::prelude::*;

use crate::data::{self, TurnKind};
use crate::Screen;

/// One entry in the running transcript.
#[derive(Clone, PartialEq)]
enum Entry {
    /// The engineer's warm opening line, before the first question.
    Opener,
    Engineer { turn: data::ClarifyTurn },
    User { text: String },
}

#[component]
pub fn ClarifyScreen(screen: Signal<Screen>) -> Element {
    let turns = use_signal(data::clarify_turns);
    let total = turns().len();

    // The transcript, the index of the turn we're currently asking, the live
    // confidence score, and whether the plan has been revealed.
    let mut transcript = use_signal(Vec::<Entry>::new);
    let mut turn_idx = use_signal(|| 0usize);
    let mut confidence = use_signal(|| data::CONFIDENCE_START);
    let mut draft = use_signal(String::new);
    let mut planned = use_signal(|| false);

    // Seed the transcript with the opener + the first question, once.
    use_hook(|| {
        let first = turns()[0].clone();
        transcript.write().push(Entry::Opener);
        transcript.write().push(Entry::Engineer { turn: first });
    });

    // Accept an answer for the current turn: record it, bump confidence, then
    // either pose the next question or reveal the plan.
    let mut accept = move |answer: String| {
        if planned() {
            return;
        }
        let idx = turn_idx();
        let gain = turns()[idx].confidence_gain;
        transcript.write().push(Entry::User { text: answer });
        confidence.set((confidence() + gain).min(98));
        draft.set(String::new());

        let next = idx + 1;
        if next < total {
            turn_idx.set(next);
            let nxt = turns()[next].clone();
            transcript.write().push(Entry::Engineer { turn: nxt });
        } else {
            planned.set(true);
        }
    };

    // The bypass: stop here and go to the plan with whatever confidence we have.
    let mut bypass = move |_| {
        if !planned() {
            planned.set(true);
        }
    };

    let conf = confidence();
    let answered = turn_idx() + usize::from(planned());
    let answered = answered.min(total);

    rsx! {
        div { class: "clarify",
            // The "still working through it" header: the checklist progress and
            // the live confidence score, the honest signal of readiness.
            ConfidenceHeader { confidence: conf, answered, total }

            div { class: "transcript",
                for entry in transcript() {
                    match entry {
                        Entry::Opener => rsx! {
                            div { class: "bubble bubble-eng",
                                div { class: "who",
                                    span { class: "who-avatar", "LE" }
                                    span { "Lead engineer" }
                                }
                                p { class: "q-text", "{data::CLARIFY_OPENER}" }
                            }
                        },
                        Entry::Engineer { turn } => rsx! { EngineerBubble { turn } },
                        Entry::User { text } => rsx! {
                            div { class: "bubble bubble-user",
                                div { class: "answer", "{text}" }
                            }
                        },
                    }
                }
            }

            if planned() {
                PlanReveal { screen }
            } else {
                // The input dock: quick-reply chips for the current turn, a free-text
                // box (free text is primary), and the always-available bypass.
                {
                    let current = turns()[turn_idx()].clone();
                    rsx! {
                        div { class: "dock",
                            div { class: "chips dock-chips",
                                for chip in current.chips.clone() {
                                    button {
                                        class: "chip",
                                        onclick: {
                                            let chip = chip.clone();
                                            move |_| accept(chip.clone())
                                        },
                                        "{chip}"
                                    }
                                }
                            }
                            div { class: "dock-row",
                                input {
                                    class: "dock-input",
                                    value: "{draft}",
                                    placeholder: "…or tell me in your own words",
                                    oninput: move |e| draft.set(e.value()),
                                    onkeydown: move |e| {
                                        if e.key() == Key::Enter && !draft().trim().is_empty() {
                                            let text = draft().trim().to_string();
                                            accept(text);
                                        }
                                    },
                                }
                                button {
                                    class: "dock-send",
                                    onclick: move |_| {
                                        let text = draft().trim().to_string();
                                        let fallback = current.answer.clone();
                                        accept(if text.is_empty() { fallback } else { text });
                                    },
                                    "Send"
                                }
                            }
                            div { class: "bypass-row",
                                button {
                                    class: "btn-quiet",
                                    onclick: move |e| bypass(e),
                                    "I have what I need — just build it"
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The checklist + confidence header. The score is a calm bar plus a number; it
/// climbs as the checklist fills, and the label softens the trade-off of stopping
/// early ("ready to build well" vs "I could build this now").
#[component]
fn ConfidenceHeader(confidence: u8, answered: usize, total: usize) -> Element {
    let pct = confidence as i32;
    let read = if confidence >= 90 {
        "Ready to build this well"
    } else if confidence >= 75 {
        "Getting clear"
    } else {
        "Still a few unknowns"
    };
    rsx! {
        div { class: "conf",
            div { class: "conf-top",
                span { class: "conf-read", "{read}" }
                span { class: "conf-count", "{answered} of {total} settled" }
            }
            div { class: "conf-bar",
                div { class: "conf-fill", style: "width: {pct}%;" }
            }
            div { class: "conf-meta",
                span { class: "conf-pct", "{confidence}% confident" }
                span { class: "conf-note", "this climbs as we settle things" }
            }
        }
    }
}

/// An engineer turn rendered as a bubble. A suggestion gets a warmer frame (a
/// labelled card) than a plain question, so the moment the engineer offers an idea
/// the user didn't think of reads as exactly that.
#[component]
fn EngineerBubble(turn: data::ClarifyTurn) -> Element {
    let is_suggestion = turn.kind == TurnKind::Suggestion;
    let outer = if is_suggestion { "bubble bubble-eng suggestion" } else { "bubble bubble-eng" };
    rsx! {
        div { class: "{outer}",
            div { class: "who",
                span { class: "who-avatar", "LE" }
                span { if is_suggestion { "Lead engineer · a suggestion" } else { "Lead engineer" } }
            }
            if is_suggestion {
                div { class: "suggestion-flag", "An idea you didn't ask for" }
            }
            p { class: "q-text", "{turn.question}" }
            p { class: "q-reason", "{turn.reason}" }
        }
    }
}

/// The plan reveal: prose AND a visual entity/action map. The single primary
/// action ("Build it") advances to the build narrative.
#[component]
fn PlanReveal(screen: Signal<Screen>) -> Element {
    let nodes = use_signal(data::plan_map);
    rsx! {
        div { class: "plan",
            div { class: "bubble bubble-eng",
                div { class: "who",
                    span { class: "who-avatar", "LE" }
                    span { "Lead engineer" }
                }
                p { class: "q-text", "{data::CLARIFY_READY}" }
            }

            p { class: "plan-prose", "{data::PLAN_PROSE}" }

            p { class: "section-label", "What I'll build" }
            p { class: "section-hint", "Each card is a thing your app keeps track of, and what a person can do with it." }
            div { class: "plan-map",
                for node in nodes() {
                    div { class: "plan-node",
                        div { class: "plan-node-head",
                            span { class: "plan-node-glyph", "{node.entity.chars().next().unwrap_or('•')}" }
                            span { class: "plan-node-name", "{node.entity}" }
                        }
                        div { class: "plan-actions",
                            for action in node.actions {
                                span { class: "action-pill", "{action}" }
                            }
                        }
                        if let Some(note) = node.note {
                            div { class: "plan-note", "{note}" }
                        }
                    }
                }
            }

            div { class: "actions",
                button {
                    class: "btn-primary",
                    onclick: move |_| screen.set(Screen::Build),
                    "Build it"
                }
                button {
                    class: "btn-quiet",
                    onclick: move |_| screen.set(Screen::Build),
                    "Tweak later — build now"
                }
            }
        }
    }
}
