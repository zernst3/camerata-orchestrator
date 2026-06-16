//! The research chat bubble: a floating, always-available AI chat panel (model
//! selectable) that sits side-by-side with the rest of the app. Strictly a research
//! aid — it talks to the same provider seam as everything else (`POST /api/chat`,
//! CLI locally / API in production), so it doubles as a live smoke test that the model
//! wiring works. It is NOT governed (no gate); it is a scratchpad, not a build path.

use dioxus::prelude::*;

/// One model the selector offers (`GET /api/models`).
#[derive(Clone, PartialEq, serde::Deserialize)]
struct ModelOption {
    label: String,
    id: String,
}

#[derive(Clone, PartialEq, serde::Deserialize)]
struct ModelsResp {
    models: Vec<ModelOption>,
    #[serde(default)]
    default: String,
    #[serde(default)]
    backend: String,
}

#[derive(Clone, PartialEq, serde::Deserialize)]
struct ChatResp {
    text: String,
    #[serde(default)]
    backend: String,
}

/// One turn in the local transcript.
#[derive(Clone, PartialEq)]
struct Turn {
    role: &'static str, // "you" | "ai"
    text: String,
}

async fn fetch_models() -> Option<ModelsResp> {
    reqwest::get(format!("{}/api/models", crate::BFF_URL))
        .await
        .ok()?
        .json::<ModelsResp>()
        .await
        .ok()
}

async fn send_chat(prompt: &str, model: &str) -> Option<ChatResp> {
    reqwest::Client::new()
        .post(format!("{}/api/chat", crate::BFF_URL))
        .json(&serde_json::json!({ "prompt": prompt, "model": model }))
        .send()
        .await
        .ok()?
        .json::<ChatResp>()
        .await
        .ok()
}

#[component]
pub fn ChatBubble() -> Element {
    let mut open = use_signal(|| false);
    let models_res = use_resource(fetch_models);
    let models = models_res.read().clone().flatten();

    let mut model = use_signal(String::new);
    // Seed the model selection from the server default once models load.
    if model().is_empty() {
        if let Some(m) = &models {
            if !m.default.is_empty() {
                model.set(m.default.clone());
            }
        }
    }
    let backend = models.as_ref().map(|m| m.backend.clone()).unwrap_or_default();

    let mut turns = use_signal(Vec::<Turn>::new);
    let mut draft = use_signal(String::new);
    let mut sending = use_signal(|| false);

    rsx! {
        // Floating launcher.
        button {
            class: "chat-fab",
            title: "Research chat (AI)",
            onclick: move |_| open.toggle(),
            if open() { "✕" } else { "💬" }
        }

        if open() {
            div { class: "chat-panel",
                div { class: "chat-head",
                    span { class: "chat-title", "Research chat" }
                    select {
                        class: "chat-model",
                        value: "{model}",
                        onchange: move |e| model.set(e.value()),
                        if let Some(m) = &models {
                            for opt in m.models.iter() {
                                option { key: "{opt.id}", value: "{opt.id}", "{opt.label}" }
                            }
                        }
                    }
                    if !backend.is_empty() {
                        span { class: "chat-backend", "{backend}" }
                    }
                }
                p { class: "chat-disclaimer", "Ungoverned scratchpad for research — not a governed build path." }

                div { class: "chat-log",
                    if turns().is_empty() {
                        p { class: "chat-empty", "Ask anything. Pick a model above." }
                    }
                    for (i , t) in turns().iter().enumerate() {
                        div { key: "{i}", class: if t.role == "you" { "chat-turn you" } else { "chat-turn ai" },
                            span { class: "chat-turn-role", "{t.role}" }
                            span { class: "chat-turn-text", "{t.text}" }
                        }
                    }
                    if sending() {
                        div { class: "chat-turn ai",
                            span { class: "chat-turn-role", "ai" }
                            span { class: "chat-turn-text dim", "thinking…" }
                        }
                    }
                }

                div { class: "chat-compose",
                    textarea {
                        class: "chat-input",
                        rows: "2",
                        placeholder: "Message… (Enter to send, Shift+Enter for newline)",
                        value: "{draft}",
                        onkeydown: move |e| {
                            if e.key() == Key::Enter && !e.modifiers().shift() {
                                e.prevent_default();
                                let prompt = draft().trim().to_string();
                                if prompt.is_empty() || sending() { return; }
                                let mdl = model();
                                turns.write().push(Turn { role: "you", text: prompt.clone() });
                                draft.set(String::new());
                                sending.set(true);
                                spawn(async move {
                                    let reply = send_chat(&prompt, &mdl).await;
                                    sending.set(false);
                                    match reply {
                                        Some(r) if !r.text.trim().is_empty() => {
                                            turns.write().push(Turn { role: "ai", text: r.text });
                                        }
                                        _ => turns.write().push(Turn {
                                            role: "ai",
                                            text: "(no response — is the model backend reachable? CLI needs `claude` on PATH; API needs ANTHROPIC_API_KEY.)".to_string(),
                                        }),
                                    }
                                });
                            }
                        },
                        oninput: move |e| draft.set(e.value()),
                    }
                    button {
                        class: "chat-send",
                        disabled: sending() || draft().trim().is_empty(),
                        onclick: move |_| {
                            let prompt = draft().trim().to_string();
                            if prompt.is_empty() || sending() { return; }
                            let mdl = model();
                            turns.write().push(Turn { role: "you", text: prompt.clone() });
                            draft.set(String::new());
                            sending.set(true);
                            spawn(async move {
                                let reply = send_chat(&prompt, &mdl).await;
                                sending.set(false);
                                match reply {
                                    Some(r) if !r.text.trim().is_empty() => {
                                        turns.write().push(Turn { role: "ai", text: r.text });
                                    }
                                    _ => turns.write().push(Turn {
                                        role: "ai",
                                        text: "(no response — is the model backend reachable?)".to_string(),
                                    }),
                                }
                            });
                        },
                        "Send"
                    }
                }
            }
        }
    }
}
