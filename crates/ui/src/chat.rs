//! The research chat bubble: a floating, always-available AI chat panel (model
//! selectable) that sits side-by-side with the rest of the app. Strictly a research
//! aid — it talks to the same provider seam as everything else (`POST /api/chat`,
//! CLI locally / API in production), so it doubles as a live smoke test that the model
//! wiring works. It is NOT governed (no gate); it is a scratchpad, not a build path.

use dioxus::prelude::*;

// Grounds the chat bubble's Guide mode in the CANONICAL repo user guide (docs/USER_GUIDE.md),
// not a UI-local copy — so the assistant tracks the same doc the rest of the project maintains
// and can't drift into describing features that aren't shipped.
const USER_GUIDE: &str = include_str!("../../../docs/USER_GUIDE.md");

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

/// A corpus rule, trimmed to what the Guide assistant needs to NAME and describe it.
#[derive(Clone, serde::Deserialize)]
struct CorpusRuleLite {
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    domain: String,
    #[serde(default)]
    scope: String,
    #[serde(default)]
    options: Vec<CorpusOptLite>,
}

#[derive(Clone, serde::Deserialize)]
struct CorpusOptLite {
    #[serde(default)]
    label: String,
}

/// Fetch the whole rule corpus (`GET /api/corpus-rules`) and render it as a compact catalog
/// the Guide assistant can cite from — so "give me an example of a repo-level rule" gets a real
/// rule id + title, not "the guide doesn't say". One line per rule: id [domain · scope]: title,
/// plus the alternatives it offers. Grounded in the live corpus, so it can't go stale.
async fn fetch_rules_catalog() -> Option<String> {
    let mut rules: Vec<CorpusRuleLite> =
        reqwest::get(format!("{}/api/corpus-rules", crate::BFF_URL))
            .await
            .ok()?
            .json()
            .await
            .ok()?;
    if rules.is_empty() {
        return None;
    }
    rules.sort_by(|a, b| (&a.domain, &a.id).cmp(&(&b.domain, &b.id)));
    let mut s = String::new();
    for r in &rules {
        let domain = if r.domain.is_empty() { "general" } else { r.domain.as_str() };
        let scope = if r.scope.is_empty() { "repo-local" } else { r.scope.as_str() };
        s.push_str(&format!("- {} [{} · {}]: {}", r.id, domain, scope, r.title));
        if !r.options.is_empty() {
            let labels: Vec<&str> =
                r.options.iter().map(|o| o.label.as_str()).filter(|l| !l.is_empty()).collect();
            if !labels.is_empty() {
                s.push_str(&format!("  (alternatives: {})", labels.join(" / ")));
            }
        }
        s.push('\n');
    }
    Some(s)
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

/// Build the Guide-mode system prompt at call time (avoids a large const). Grounded in the
/// canonical user guide PLUS the live rules catalog, so the assistant can both explain flows and
/// name/describe actual governance rules. Both are real, maintained sources — it must not
/// improvise features or rules that aren't in them.
fn guide_system_prompt(rules_catalog: &str) -> String {
    let mut p = format!(
        "You are Camerata's in-app assistant. Answer the user's question about Camerata using \
         ONLY the materials below: the USER GUIDE for how-to and flows, and the RULES CATALOG for \
         specific governance rules. When asked for examples of rules (e.g. a repo-local rule), \
         cite REAL rule ids + titles from the catalog (scope=repo-local are repo-level; \
         cross-repo/process are project-level; the security floor is always-on). If something \
         isn't in these materials, say so briefly rather than guessing. Be concise and concrete.\
         \n\n=== CAMERATA USER GUIDE ===\n{USER_GUIDE}"
    );
    if !rules_catalog.trim().is_empty() {
        p.push_str("\n\n=== CAMERATA RULES CATALOG (every governance rule, with domain · scope) ===\n");
        p.push_str(rules_catalog);
    }
    p
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

    // The live rules catalog, fetched once, fed into the Guide system prompt so the assistant
    // can cite real rule ids/titles. Empty string until it loads (the guide alone still answers).
    let rules_res = use_resource(fetch_rules_catalog);
    let rules_catalog = rules_res.read().clone().flatten().unwrap_or_default();
    // One clone per send closure (onkeydown + onclick each move-capture their own).
    let catalog_kd = rules_catalog.clone();
    let catalog_btn = rules_catalog;

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
                                let sys = if mode() == ChatMode::Guide { Some(guide_system_prompt(&catalog_kd)) } else { None };
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
                            let sys = if mode() == ChatMode::Guide { Some(guide_system_prompt(&catalog_btn)) } else { None };
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
