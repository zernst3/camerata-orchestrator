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
//! The transcript is driven by the real RefinementSession: questions and
//! suggestions come from StubRefinementReviewer, confidence is read from the
//! live AppState.confidence(), and every answer is folded back into the session.

use std::collections::HashSet;
use std::sync::Arc;

use dioxus::prelude::*;

use camerata_intake::{
    DesignReference, InMemoryDesignCorpus, StoryId, StubRefinementReviewer, UserStory,
};

use crate::app_state::AppState;
use crate::data;
use crate::Screen;

/// One entry in the running transcript.
#[derive(Clone, PartialEq)]
enum Entry {
    /// The engineer's warm opening line, before the first question.
    Opener,
    /// A clarifying question from the reviewer.
    Question(String),
    /// A proactive product suggestion from the reviewer.
    Suggestion { text: String, rationale: String },
    /// The user's free-text answer.
    User(String),
    /// A spinner shown while the AI review is running.
    Thinking,
}

// ─── helpers ─────────────────────────────────────────────────────────────────

/// Trigger one async review turn. Extracts any new suggestions and the next
/// open question from the updated session and pushes them into the transcript.
/// Sets `planned` to true when the reviewer has no more questions.
///
/// Threads the clarify screen's reactive signals (all `Copy` Dioxus signals);
/// grouping them into a struct would not improve clarity here, so the arg count is
/// allowed for this one render helper.
#[allow(clippy::too_many_arguments)]
fn trigger_review(
    mut app: Signal<Option<AppState>>,
    mut transcript: Signal<Vec<Entry>>,
    mut current_q: Signal<Vec<String>>,
    mut answered_count: Signal<usize>,
    mut pending_answers: Signal<Vec<String>>,
    mut reviewing: Signal<bool>,
    mut planned: Signal<bool>,
    mut seen_suggestions: Signal<HashSet<String>>,
) {
    // Show a thinking indicator while the review runs.
    transcript.write().push(Entry::Thinking);
    reviewing.set(true);

    spawn(async move {
        // Clone, mutate, set back (Dioxus pattern for async AppState mutation).
        let mut snap = app.peek().clone();
        if let Some(state) = snap.as_mut() {
            let reviewer = StubRefinementReviewer::new();
            let _ = state.run_review_turn(&reviewer).await;
        }

        // Extract new suggestions and open questions from the updated state.
        let new_suggestions: Vec<(String, String, String)> = snap
            .as_ref()
            .map(|s| s.active_suggestions())
            .unwrap_or_default()
            .into_iter()
            .map(|s| (s.id, s.suggestion, s.rationale))
            .collect();
        let open: Vec<String> = snap
            .as_ref()
            .and_then(|s| s.active_session())
            .map(|sess| sess.open_questions().to_vec())
            .unwrap_or_default();

        app.set(snap);

        // Remove the Thinking entry; add new suggestions (first-time only).
        let mut t = transcript.peek().clone();
        if t.last() == Some(&Entry::Thinking) {
            t.pop();
        }
        let mut seen = seen_suggestions.peek().clone();
        for (id, text, rationale) in new_suggestions {
            if !seen.contains(&id) {
                seen.insert(id);
                t.push(Entry::Suggestion { text, rationale });
            }
        }

        // Show the first open question (the rest come after each answer).
        if !open.is_empty() {
            t.push(Entry::Question(open[0].clone()));
        }

        transcript.set(t);
        seen_suggestions.set(seen);
        current_q.set(open.clone());
        answered_count.set(0);
        pending_answers.set(vec![]);
        reviewing.set(false);

        if open.is_empty() {
            // The reviewer has no more questions: the session is ready.
            planned.set(true);
        }
    });
}

// ─── the screen ──────────────────────────────────────────────────────────────

#[component]
pub fn ClarifyScreen(screen: Signal<Screen>) -> Element {
    let mut app = use_context::<Signal<Option<AppState>>>();

    // Transcript state.
    let mut transcript = use_signal(Vec::<Entry>::new);
    let mut draft = use_signal(String::new);
    let mut planned = use_signal(|| false);
    let reviewing = use_signal(|| false);

    // Per-round question tracking: the questions from the latest review turn
    // and how many of them the user has answered so far.
    let current_q = use_signal(Vec::<String>::new);
    let mut answered_count = use_signal(|| 0usize);
    let mut pending_answers = use_signal(Vec::<String>::new);

    // Track which suggestion ids have already been added to the transcript so
    // they are never duplicated across review turns.
    let seen_suggestions = use_signal(HashSet::<String>::new);

    // On mount: push the opener and kick off the first review turn.
    use_hook(|| {
        transcript.write().push(Entry::Opener);
        if app.peek().is_some() {
            trigger_review(
                app,
                transcript,
                current_q,
                answered_count,
                pending_answers,
                reviewing,
                planned,
                seen_suggestions,
            );
        }
    });

    // Accept the user's answer to the current question.
    let mut accept = move |answer: String| {
        if planned() || reviewing() || answer.trim().is_empty() {
            return;
        }
        // Record the user's reply in the transcript.
        transcript.write().push(Entry::User(answer.clone()));
        draft.set(String::new());

        // Add to the answer buffer.
        pending_answers.write().push(answer);
        let next = answered_count() + 1;
        answered_count.set(next);

        let qs = current_q();
        if next < qs.len() {
            // More questions in this round: show the next one.
            transcript.write().push(Entry::Question(qs[next].clone()));
        } else {
            // All questions in this round answered: fold them in, then review.
            let answers = pending_answers();
            if let Some(state) = app.write().as_mut() {
                state.answer_open_questions(answers);
            }
            trigger_review(
                app,
                transcript,
                current_q,
                answered_count,
                pending_answers,
                reviewing,
                planned,
                seen_suggestions,
            );
        }
    };

    // Bypass: converge the session at whatever confidence we have and go to plan.
    let bypass = move |_| {
        if !planned() {
            if let Some(state) = app.write().as_mut() {
                if let Some(sess) = state.project.active_session_mut() {
                    sess.converge();
                }
            }
            planned.set(true);
        }
    };

    // Confidence from the real session (climbs as questions are answered + reviewed).
    let conf = app.read().as_ref().map(|s| s.confidence()).unwrap_or(0);

    rsx! {
        div { class: "clarify",
            // The live confidence header: reads from the real session.
            ConfidenceHeader { confidence: conf }

            // The real, editable source of truth: the user stories.
            StoriesPanel { app }

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
                        Entry::Question(text) => rsx! {
                            div { class: "bubble bubble-eng",
                                div { class: "who",
                                    span { class: "who-avatar", "LE" }
                                    span { "Lead engineer" }
                                }
                                p { class: "q-text", "{text}" }
                            }
                        },
                        Entry::Suggestion { text, rationale } => rsx! {
                            div { class: "bubble bubble-eng suggestion",
                                div { class: "who",
                                    span { class: "who-avatar", "LE" }
                                    span { "Lead engineer · a suggestion" }
                                }
                                div { class: "suggestion-flag", "An idea you didn't ask for" }
                                p { class: "q-text", "{text}" }
                                p { class: "q-reason", "{rationale}" }
                            }
                        },
                        Entry::User(text) => rsx! {
                            div { class: "bubble bubble-user",
                                div { class: "answer", "{text}" }
                            }
                        },
                        Entry::Thinking => rsx! {
                            div { class: "bubble bubble-eng bubble-thinking",
                                div { class: "who",
                                    span { class: "who-avatar", "LE" }
                                    span { "Lead engineer" }
                                }
                                p { class: "q-text thinking-dots", "..." }
                            }
                        },
                    }
                }
            }

            if planned() {
                PlanReveal { screen }
            } else if !reviewing() && !current_q().is_empty() {
                // Show the input dock when there is an open question to answer.
                div { class: "dock",
                    div { class: "chips dock-chips",
                        // Generic quick-reply chips that work for any yes/no question.
                        for chip in ["Yes", "No", "Not sure yet", "Tell me more"] {
                            button {
                                class: "chip",
                                onclick: {
                                    let chip = chip.to_string();
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
                                    accept(draft().trim().to_string());
                                }
                            },
                        }
                        button {
                            class: "dock-send",
                            onclick: move |_| {
                                let text = draft().trim().to_string();
                                if !text.is_empty() {
                                    accept(text);
                                }
                            },
                            "Send"
                        }
                    }
                    div { class: "bypass-row",
                        button {
                            class: "btn-quiet",
                            onclick: bypass,
                            "I have what I need — just build it"
                        }
                    }
                }
            } else if !reviewing() {
                // No open questions and no plan yet: offer bypass only.
                div { class: "dock",
                    div { class: "bypass-row",
                        button {
                            class: "btn-quiet",
                            onclick: bypass,
                            "I have what I need — just build it"
                        }
                    }
                }
            }
        }
    }
}

// ─── sub-components ──────────────────────────────────────────────────────────

/// The editable user-story list: the living source of truth the user and the AI
/// both shape. Each story is the consumer-abstracted unit (who it is for + a plain
/// list of wants). Adding or removing a story mutates the real RefinementSession
/// and queues a versioned revision (the App effect persists it).
#[component]
fn StoriesPanel(mut app: Signal<Option<AppState>>) -> Element {
    let corpus = use_context::<Arc<InMemoryDesignCorpus>>();
    let mut historical = use_signal(Vec::<DesignReference>::new);

    let stories: Vec<UserStory> = app
        .read()
        .as_ref()
        .map(|s| s.active_stories().to_vec())
        .unwrap_or_default();
    let (share_on, hist_on) = app
        .read()
        .as_ref()
        .map(|s| {
            (
                s.project.sharing.contribute_design,
                s.project.sharing.use_historical,
            )
        })
        .unwrap_or((false, false));
    let (phase_label, ctx_label, session_conf) = match app.read().as_ref() {
        Some(s) => (
            format!("{:?}", s.phase()),
            s.active_session()
                .map(|x| x.context.label())
                .unwrap_or("—")
                .to_string(),
            s.confidence(),
        ),
        None => ("—".to_string(), "—".to_string(), 0),
    };

    rsx! {
        div { class: "stories-panel",
            div { class: "stories-head",
                p { class: "section-label", "Your app, as a set of stories" }
                span { class: "stories-status",
                    "{phase_label} · {ctx_label} · {session_conf}% pinned · {stories.len()} stories"
                }
            }
            p { class: "section-hint",
                "These are the source of truth. Edit or remove anything that's not right; every change is saved with full history."
            }

            div { class: "stories-list",
                for story in stories.iter().cloned() {
                    {
                        let id = story.id.as_str().to_string();
                        rsx! {
                            div { class: "story-card",
                                div { class: "story-card-head",
                                    span { class: "story-title", "{story.title}" }
                                    span { class: "story-for", "for {story.for_whom}" }
                                    button {
                                        class: "story-edit",
                                        title: "Mark this as a must-have",
                                        onclick: {
                                            let story = story.clone();
                                            move |_| {
                                                let mut edited = story.clone();
                                                if edited.so_that.is_none() {
                                                    edited.so_that = Some("this one matters to me".to_string());
                                                    if let Some(state) = app.write().as_mut() {
                                                        state.upsert_story(edited);
                                                    }
                                                }
                                            }
                                        },
                                        "★"
                                    }
                                    button {
                                        class: "story-remove",
                                        title: "Remove this story",
                                        onclick: move |_| {
                                            let sid = StoryId::new(id.clone());
                                            if let Some(state) = app.write().as_mut() {
                                                state.remove_story(&sid);
                                            }
                                        },
                                        "✕"
                                    }
                                }
                                ul { class: "story-wants",
                                    for want in story.wants.iter().cloned() {
                                        li { "{want}" }
                                    }
                                }
                                if let Some(so_that) = story.so_that.clone() {
                                    p { class: "story-sothat", "★ so that {so_that}" }
                                }
                            }
                        }
                    }
                }
            }

            button {
                class: "btn-quiet add-story",
                onclick: move |_| {
                    if let Some(state) = app.write().as_mut() {
                        let n = state.active_stories().len();
                        state.add_story(UserStory::user_added(
                            format!("added_{n}"),
                            "Something I want to add",
                            "Me",
                            vec!["I can ...".to_string()],
                        ));
                    }
                },
                "+ Add a story"
            }

            // ── Shared-design opt-ins ─────────────────────────────────────────
            div { class: "refine-controls",
                label { class: "opt-in",
                    input {
                        r#type: "checkbox",
                        checked: share_on,
                        onclick: {
                            let corpus = corpus.clone();
                            move |_| {
                                let on = !share_on;
                                let mut app = app;
                                if let Some(state) = app.write().as_mut() {
                                    state.project.sharing.contribute_design = on;
                                }
                                if on {
                                    let corpus = corpus.clone();
                                    spawn(async move {
                                        let snap = app.peek().clone();
                                        if let Some(state) = &snap {
                                            let _ = state.contribute_if_consented(&*corpus).await;
                                        }
                                    });
                                } else {
                                    let corpus = corpus.clone();
                                    spawn(async move {
                                        let snap = app.peek().clone();
                                        if let Some(state) = &snap {
                                            state.withdraw_from_corpus(&*corpus).await;
                                        }
                                    });
                                }
                            }
                        },
                    }
                    span { "Share my design to help improve future apps (only the shape, never your data). You can turn this off anytime, and your shared design is deleted." }
                }

                label { class: "opt-in",
                    input {
                        r#type: "checkbox",
                        checked: hist_on,
                        onclick: {
                            let corpus = corpus.clone();
                            move |_| {
                                let on = !hist_on;
                                let mut app = app;
                                if let Some(state) = app.write().as_mut() {
                                    state.project.sharing.use_historical = on;
                                }
                                if on {
                                    let corpus = corpus.clone();
                                    spawn(async move {
                                        let snap = app.peek().clone();
                                        if let Some(state) = &snap {
                                            let refs = state.historical_references(&*corpus).await;
                                            historical.set(refs);
                                        }
                                    });
                                } else {
                                    historical.set(vec![]);
                                }
                            }
                        },
                    }
                    span { "Use proven designs from similar apps to speed up setup" }
                }

                if !historical().is_empty() {
                    p { class: "historical-note",
                        "Found {historical().len()} similar design(s) you can draw on."
                    }
                }
            }
        }
    }
}

/// The confidence header: a calm bar plus the percentage and a plain-language
/// read of where we are. The score climbs as the reviewer gains certainty.
#[component]
fn ConfidenceHeader(confidence: u8) -> Element {
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

/// The plan reveal: prose AND a visual entity/action map. The single primary
/// action ("Build it") advances to the build narrative.
#[component]
fn PlanReveal(screen: Signal<Screen>) -> Element {
    // Derive the plan map from the REAL onboarding entities (falls back to the
    // generic prose if there is somehow no project).
    let app = use_context::<Signal<Option<AppState>>>();
    let plan = app
        .read()
        .as_ref()
        .map(|s| (s.plan_nodes(), s.plan_prose()));
    let (nodes, prose) = plan.unwrap_or_else(|| (Vec::new(), data::PLAN_PROSE.to_string()));
    rsx! {
        div { class: "plan",
            div { class: "bubble bubble-eng",
                div { class: "who",
                    span { class: "who-avatar", "LE" }
                    span { "Lead engineer" }
                }
                p { class: "q-text", "{data::CLARIFY_READY}" }
            }

            p { class: "plan-prose", "{prose}" }

            p { class: "section-label", "What I'll build" }
            p { class: "section-hint", "Each card is a thing your app keeps track of, and what a person can do with it." }
            div { class: "plan-map",
                for (entity , actions , note) in nodes {
                    div { class: "plan-node",
                        div { class: "plan-node-head",
                            span { class: "plan-node-glyph", "{entity.chars().next().unwrap_or('•')}" }
                            span { class: "plan-node-name", "{entity}" }
                        }
                        div { class: "plan-actions",
                            for action in actions {
                                span { class: "action-pill", "{action}" }
                            }
                        }
                        if let Some(note) = note {
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
