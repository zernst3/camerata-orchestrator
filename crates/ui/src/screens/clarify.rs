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

use std::sync::Arc;

use dioxus::prelude::*;

use camerata_intake::{
    DesignReference, InMemoryDesignCorpus, StoryId, StubRefinementReviewer, UserStory,
};

use crate::app_state::AppState;
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

    // The real, editable user stories — the source of truth, built by intake and
    // persisted on every edit. Rendered alongside the conversation.
    let app = use_context::<Signal<Option<AppState>>>();

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

/// The editable user-story list: the living source of truth the user and the AI
/// both shape. Each story is the consumer-abstracted unit (who it is for + a plain
/// list of wants). Adding or removing a story mutates the real `RefinementSession`
/// and queues a versioned revision (the App effect persists it). A small status
/// line shows the real lifecycle phase, the session context, and the session's own
/// confidence (distinct from the scripted conversation score above).
#[component]
fn StoriesPanel(mut app: Signal<Option<AppState>>) -> Element {
    // The shared design corpus (opt-in flywheel) and a place to hold any historical
    // matches we fetch for display.
    let corpus = use_context::<Arc<InMemoryDesignCorpus>>();
    let mut historical = use_signal(Vec::<DesignReference>::new);

    // Snapshot the real state for rendering.
    let stories: Vec<UserStory> = app
        .read()
        .as_ref()
        .map(|s| s.active_stories().to_vec())
        .unwrap_or_default();
    let (share_on, hist_on) = app
        .read()
        .as_ref()
        .map(|s| (s.project.sharing.contribute_design, s.project.sharing.use_historical))
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
                                                // A real edit: pin the story as a
                                                // must-have. Upserting records a new
                                                // version (the user changed it).
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

            // ── Refinement controls: a real AI review turn + the shared-design opt-ins ──
            div { class: "refine-controls",
                button {
                    class: "btn-quiet review-btn",
                    onclick: move |_| {
                        // Snapshot, run one real review turn off the UI thread, write back.
                        let mut app = app;
                        spawn(async move {
                            let mut snap = app.peek().clone();
                            if let Some(state) = snap.as_mut() {
                                let _ = state.run_review_turn(&StubRefinementReviewer::new()).await;
                            }
                            app.set(snap);
                        });
                    },
                    "Have the engineer review your stories"
                }

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
                                    // Opt-out: actually delete the shared data.
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
