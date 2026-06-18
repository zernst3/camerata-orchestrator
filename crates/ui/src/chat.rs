//! The research chat bubble: a floating, always-available AI chat panel (model
//! selectable) that sits side-by-side with the rest of the app. Strictly a research
//! aid — it talks to the same provider seam as everything else (`POST /api/chat`,
//! CLI locally / API in production), so it doubles as a live smoke test that the model
//! wiring works. It is NOT governed (no gate); it is a scratchpad, not a build path.

use dioxus::prelude::*;

const USER_GUIDE: &str = include_str!("../docs/USER_GUIDE.md");

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

/// Which mode the chat panel is in.
#[derive(Clone, Copy, PartialEq)]
enum ChatMode {
    Research,
    Guide,
}

async fn send_chat(prompt: &str, model: &str, system: Option<&str>) -> Option<ChatResp> {
    let mut body = serde_json::json!({ "prompt": prompt, "model": model });
    if let Some(sys) = system {
        body["system"] = serde_json::Value::String(sys.to_string());
    }
    reqwest::Client::new()
        .post(format!("{}/api/chat", crate::BFF_URL))
        .json(&body)
        .send()
        .await
        .ok()?
        .json::<ChatResp>()
        .await
        .ok()
}

/// Build the Guide-mode system prompt at call time (avoids a large const).
fn guide_system_prompt() -> String {
    format!(
        "You are Camerata's in-app assistant. Answer the user's question about HOW TO USE \
         Camerata, using ONLY the user guide below. If the answer isn't in the guide, say so \
         briefly. Be concise and concrete.\n\n=== CAMERATA USER GUIDE ===\n{USER_GUIDE}"
    )
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

    let mut mode = use_signal(|| ChatMode::Research);
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
                    span { class: "chat-title",
                        if mode() == ChatMode::Guide { "Guide" } else { "Research chat" }
                    }
                    // Research / Guide mode toggle
                    div { class: "chat-mode-toggle",
                        button {
                            class: if mode() == ChatMode::Research { "chat-mode-btn active" } else { "chat-mode-btn" },
                            onclick: move |_| { mode.set(ChatMode::Research); turns.write().clear(); },
                            "Research"
                        }
                        button {
                            class: if mode() == ChatMode::Guide { "chat-mode-btn active" } else { "chat-mode-btn" },
                            onclick: move |_| { mode.set(ChatMode::Guide); turns.write().clear(); },
                            "Guide"
                        }
                    }
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
                p { class: "chat-disclaimer",
                    if mode() == ChatMode::Guide {
                        "Answers from the Camerata user guide only."
                    } else {
                        "Ungoverned scratchpad for research — not a governed build path."
                    }
                }

                div { class: "chat-log",
                    if turns().is_empty() {
                        p { class: "chat-empty",
                            if mode() == ChatMode::Guide {
                                "Ask how to do something in Camerata…"
                            } else {
                                "Ask anything. Pick a model above."
                            }
                        }
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
                        placeholder: if mode() == ChatMode::Guide {
                            "Ask how to do something in Camerata… (Enter to send)"
                        } else {
                            "Message… (Enter to send, Shift+Enter for newline)"
                        },
                        value: "{draft}",
                        onkeydown: move |e| {
                            if e.key() == Key::Enter && !e.modifiers().shift() {
                                e.prevent_default();
                                let prompt = draft().trim().to_string();
                                if prompt.is_empty() || sending() { return; }
                                let mdl = model();
                                let sys = if mode() == ChatMode::Guide { Some(guide_system_prompt()) } else { None };
                                turns.write().push(Turn { role: "you", text: prompt.clone() });
                                draft.set(String::new());
                                sending.set(true);
                                spawn(async move {
                                    let reply = send_chat(&prompt, &mdl, sys.as_deref()).await;
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
                            let sys = if mode() == ChatMode::Guide { Some(guide_system_prompt()) } else { None };
                            turns.write().push(Turn { role: "you", text: prompt.clone() });
                            draft.set(String::new());
                            sending.set(true);
                            spawn(async move {
                                let reply = send_chat(&prompt, &mdl, sys.as_deref()).await;
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
