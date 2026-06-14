//! Screen 5 — REPORT A PROBLEM. The strict, structured bug form (CONSUMER_UX.md
//! §5). Like the intake form, it is strict in shape: it forces the user to
//! describe the problem in a way the agents can act on — where it happened, what
//! they did, what they expected, what actually happened — never a vague "it's
//! broken."
//!
//! On submit, the report goes back through the governed build loop in miniature
//! (a calm fix narrative reusing the build look), and the user lands back in QA to
//! re-test. No error messages, ever — even a bug report stays calm and handled.

use std::time::Duration;

use dioxus::prelude::*;

use crate::data;
use crate::Screen;

/// Two phases on this screen: filling the strict form, then watching the calm fix.
#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Form,
    Fixing,
}

#[component]
pub fn BugScreen(screen: Signal<Screen>) -> Element {
    let fields = data::BUG_FIELDS;

    // One signal per strict field, keyed by the field's `key`.
    let mut answers = use_signal(|| {
        fields
            .iter()
            .map(|f| (f.key.to_string(), String::new()))
            .collect::<Vec<_>>()
    });

    let mut phase = use_signal(|| Phase::Form);

    // The report is only sendable once every strict field has something in it —
    // the forcing function that keeps "it's broken" out.
    let complete = use_memo(move || answers().iter().all(|(_, v)| !v.trim().is_empty()));

    match phase() {
        Phase::Form => rsx! {
            div { class: "page bug",
                p { class: "eyebrow", "Report a problem" }
                h1 { class: "h1", "Tell me what went wrong" }
                p { class: "lede", "A few specifics help me fix it fast and check the fix actually holds. The more exact, the better — there are no wrong answers here." }

                for (i , f) in fields.iter().enumerate() {
                    div { class: "field bug-field",
                        p { class: "section-label", "{f.label}" }
                        p { class: "section-hint", "{f.hint}" }
                        textarea {
                            class: "textarea",
                            rows: "2",
                            placeholder: "{f.placeholder}",
                            value: "{answers()[i].1}",
                            oninput: move |e| {
                                answers.write()[i].1 = e.value();
                            },
                        }
                    }
                }

                div { class: "actions",
                    button {
                        class: "btn-primary",
                        disabled: !complete(),
                        onclick: move |_| phase.set(Phase::Fixing),
                        "Send it to the engineer"
                    }
                    button {
                        class: "btn-quiet",
                        onclick: move |_| screen.set(Screen::Qa),
                        "Back to trying it"
                    }
                }
                if !complete() {
                    p { class: "bug-gate", "Fill in all four so I have the full picture before I dig in." }
                }
            }
        },
        Phase::Fixing => rsx! { FixingView { screen } },
    }
}

/// The fix loop in miniature: the same calm staged narrative as the build, then a
/// quiet hand-back to QA to re-test. Reuses the build look so a fix feels like the
/// same trustworthy machinery, just smaller.
#[component]
fn FixingView(screen: Signal<Screen>) -> Element {
    let stages = data::FIX_STAGES;
    let mut done = use_signal(|| 0usize);

    let _driver = use_future(move || {
        let stages = stages;
        async move {
            loop {
                let i = done();
                if i >= stages.len() {
                    tokio::time::sleep(Duration::from_millis(900)).await;
                    screen.set(Screen::Qa);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(stages[i].dwell_ms)).await;
                done.set(i + 1);
            }
        }
    });

    let done_n = done();

    rsx! {
        div { class: "page build",
            p { class: "eyebrow", "On it" }
            h1 { class: "h1", "Fixing that for you" }
            p { class: "lede", "I've got your report. I'm making the change and checking it against your rules before you see it again." }

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
            p { class: "build-caption", "When this is done, you'll be back trying your app to make sure it's right now." }
        }
    }
}
