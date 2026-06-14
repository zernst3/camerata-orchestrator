//! Screen 3 — BUILD. Calm, governed construction, narrated as a single slow
//! progress story (CONSUMER_UX.md §3). No logs, no terminal, no jitter, no
//! percentages, just human-readable stages completing one by one with a quiet
//! check, the way a well-made product reassures you that something is handled.
//!
//! Two modes share this screen:
//!
//! - DEFAULT (the recordable demo): a calm MOCKED staged narrative with one genuine
//!   mid-build question. Always fast, always smooth, no environment required. This
//!   is what you screen-record.
//! - LIVE (set `CAMERATA_LIVE_BUILD=1`): the screen derives the real `Plan` from the
//!   project and runs the REAL governed fleet via `build_run::run_build` (gateway +
//!   `claude -p` agents), streaming its `BuildEvent`s into the same calm stage list.
//!   Gated behind an env var because a live agent build is slow and spends tokens, so
//!   it is opt-in, not the default demo path. If the live build errors (no gateway
//!   built, `claude` unavailable), the screen still ends calmly into QA, never an
//!   error message.
//!
//! The motion is driven by a single `use_future`. The desktop renderer runs on
//! Tokio, so the `tokio::time::sleep` cadence (demo mode) and the awaited live build
//! both just work.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dioxus::prelude::*;

use crate::app_state::AppState;
use crate::build_run::{self, BuildEvent};
use crate::data;
use crate::Screen;

#[component]
pub fn BuildScreen(screen: Signal<Screen>) -> Element {
    let app = use_context::<Signal<Option<AppState>>>();
    // Opt-in live build: only run the real governed fleet when explicitly asked.
    let live_mode = std::env::var("CAMERATA_LIVE_BUILD").is_ok();

    // ── demo (mocked) state ──
    let stages = data::BUILD_STAGES;
    let mid_q = use_signal(data::mid_build_question);
    let mut done = use_signal(|| 0usize);
    let mut pending_q = use_signal(|| false);
    let answered_q = use_signal(|| false);

    // ── live state ──
    let mut live_stages = use_signal(Vec::<(String, bool)>::new);

    let _driver = use_future(move || {
        let stages = stages;
        let after = mid_q().after_stage;
        async move {
            // LIVE: derive the plan and run the real governed fleet. The build's
            // progress callback must be Send+Sync (it may run off-thread), but a
            // Dioxus signal is not Sync, so the callback pushes events into a shared
            // buffer and THIS future (on the UI side) drains the buffer into the
            // signal.
            if live_mode {
                let plan = app.peek().as_ref().map(|s| s.build_plan());
                if let Some(plan) = plan {
                    let buffer: Arc<Mutex<Vec<BuildEvent>>> = Arc::new(Mutex::new(Vec::new()));
                    let done_flag = Arc::new(AtomicBool::new(false));

                    // Run the real governed build off in its own task.
                    {
                        let buffer = buffer.clone();
                        let done_flag = done_flag.clone();
                        spawn(async move {
                            let on_event = move |ev: BuildEvent| {
                                if let Ok(mut b) = buffer.lock() {
                                    b.push(ev);
                                }
                            };
                            // Best-effort: an error (no gateway/claude) ends calmly.
                            let _ = build_run::run_build(&plan, &on_event).await;
                            done_flag.store(true, Ordering::SeqCst);
                        });
                    }

                    // Drain the buffer into the visible stage list until the build
                    // finishes, mapping each event to a calm label.
                    let drain = move |live_stages: &mut Signal<Vec<(String, bool)>>| {
                        let evs: Vec<BuildEvent> =
                            buffer.lock().map(|mut b| b.drain(..).collect()).unwrap_or_default();
                        for ev in evs {
                            if let Some(last) = live_stages.write().last_mut() {
                                last.1 = true;
                            }
                            if let Some(label) = build_run::event_label(&ev) {
                                live_stages.write().push((label, false));
                            }
                        }
                    };
                    loop {
                        drain(&mut live_stages);
                        if done_flag.load(Ordering::SeqCst) {
                            drain(&mut live_stages); // final events
                            if let Some(last) = live_stages.write().last_mut() {
                                last.1 = true;
                            }
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(150)).await;
                    }
                }
                tokio::time::sleep(Duration::from_millis(900)).await;
                screen.set(Screen::Qa);
                return;
            }

            // DEMO: the calm mocked narrative, pausing on the one genuine question.
            loop {
                let i = done();
                if i >= stages.len() {
                    tokio::time::sleep(Duration::from_millis(900)).await;
                    screen.set(Screen::Qa);
                    break;
                }
                if i == after + 1 && !answered_q() {
                    pending_q.set(true);
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

    if live_mode {
        let live = live_stages();
        return rsx! {
            div { class: "page build",
                p { class: "eyebrow", "Building" }
                h1 { class: "h1", "Putting it together" }
                p { class: "lede", "I'm building your app for real and checking every piece against your rules as I go." }
                div { class: "build-list",
                    for (label , is_done) in live.iter().cloned() {
                        div { class: if is_done { "build-stage done" } else { "build-stage active" },
                            span { class: "stage-mark",
                                if is_done { "✓" } else { span { class: "spinner" } }
                            }
                            span { class: "stage-text", "{label}" }
                        }
                    }
                }
                if live.is_empty() {
                    p { class: "build-caption", "Setting things up." }
                }
            }
        };
    }

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
                        } else if i == done_n {
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
