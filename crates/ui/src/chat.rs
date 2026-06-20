//! The research chat bubble: a floating, always-available AI chat panel (model
//! selectable) that sits side-by-side with the rest of the app. Strictly a research
//! aid — it talks to the same provider seam as everything else (`POST /api/chat`,
//! CLI locally / API in production), so it doubles as a live smoke test that the model
//! wiring works. It is NOT governed (no gate); it is a scratchpad, not a build path.

use dioxus::prelude::*;

use crate::md::md_to_html;

// Grounds the chat bubble's Guide mode in the CANONICAL repo user guide (docs/USER_GUIDE.md),
// not a UI-local copy — so the assistant tracks the same doc the rest of the project maintains
// and can't drift into describing features that aren't shipped.
const USER_GUIDE: &str = include_str!("../../../docs/USER_GUIDE.md");

// Grounds the Technical mode in the CANONICAL technical reference (docs/TECHNICAL.md).
const TECHNICAL_DOC: &str = include_str!("../../../docs/TECHNICAL.md");

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
        let domain = if r.domain.is_empty() {
            "general"
        } else {
            r.domain.as_str()
        };
        let scope = if r.scope.is_empty() {
            "repo-local"
        } else {
            r.scope.as_str()
        };
        s.push_str(&format!("- {} [{} · {}]: {}", r.id, domain, scope, r.title));
        if !r.options.is_empty() {
            let labels: Vec<&str> = r
                .options
                .iter()
                .map(|o| o.label.as_str())
                .filter(|l| !l.is_empty())
                .collect();
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
    /// Grounded in docs/TECHNICAL.md: answers HOW Camerata works under the hood.
    Technical,
}

/// Build the Technical-mode system prompt. Grounded exclusively in the canonical
/// TECHNICAL.md so answers reflect actual code, not improvised architecture.
pub(crate) fn technical_system_prompt() -> String {
    format!(
        "You are Camerata's in-app technical assistant. Answer questions about HOW \
         Camerata works under the hood using ONLY the technical reference below. \
         Cite real crate names, module paths, struct/function names, and file paths \
         exactly as they appear in the doc. If a detail is not covered in the \
         technical reference, say so clearly rather than guessing.\n\n\
         === CAMERATA TECHNICAL REFERENCE ===\n{TECHNICAL_DOC}"
    )
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

/// The explicit "not covered" phrase the Guide assistant must use when a question falls
/// outside the user guide. Hard-coded here so tests can assert it appears in the prompt —
/// a change to the wording requires updating both the prompt and the tests together.
pub(crate) const GUIDE_NOT_COVERED_PHRASE: &str = "That isn't covered in the Camerata user guide.";

/// Build the Guide-mode system prompt at call time (avoids a large const). Grounded in the
/// canonical user guide PLUS the live rules catalog, so the assistant can both explain flows and
/// name/describe actual governance rules. Both are real, maintained sources — it must not
/// improvise features or rules that aren't in them.
///
/// The "not covered" guardrail is explicit: if a question falls outside the grounding
/// materials the assistant must say [`GUIDE_NOT_COVERED_PHRASE`] rather than guessing.
/// This is intentionally stronger than a soft "say so" — the exact phrase anchors the
/// response so users get a clear signal rather than a confident hallucination.
pub(crate) fn guide_system_prompt(rules_catalog: &str) -> String {
    let not_covered = GUIDE_NOT_COVERED_PHRASE;
    let mut p = format!(
        "You are Camerata's in-app assistant. Answer the user's question about Camerata using \
         ONLY the materials below: the USER GUIDE for how-to and flows, and the RULES CATALOG for \
         specific governance rules. When asked for examples of rules (e.g. a repo-local rule), \
         cite REAL rule ids + titles from the catalog (scope=repo-local are repo-level; \
         cross-repo/process are project-level; the security floor is always-on). \
         CRITICAL: if the user asks about anything not described in these materials, respond with \
         exactly \"{not_covered}\" followed by a brief explanation of what IS covered. \
         Never invent features, steps, or rules that do not appear in the materials. \
         Be concise and concrete.\
         \n\n=== CAMERATA USER GUIDE ===\n{USER_GUIDE}"
    );
    if !rules_catalog.trim().is_empty() {
        p.push_str(
            "\n\n=== CAMERATA RULES CATALOG (every governance rule, with domain · scope) ===\n",
        );
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
    let backend = models
        .as_ref()
        .map(|m| m.backend.clone())
        .unwrap_or_default();

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
                        match mode() {
                            ChatMode::Guide => "Guide",
                            ChatMode::Technical => "Technical",
                            ChatMode::Research => "Research chat",
                        }
                    }
                    // Research / Guide / Technical mode toggle
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
                        button {
                            class: if mode() == ChatMode::Technical { "chat-mode-btn active" } else { "chat-mode-btn" },
                            onclick: move |_| { mode.set(ChatMode::Technical); turns.write().clear(); },
                            "Technical"
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
                    match mode() {
                        ChatMode::Guide => "Answers from the Camerata user guide only.",
                        ChatMode::Technical => "Answers grounded in the Camerata technical reference (crates, modules, code).",
                        ChatMode::Research => "Ungoverned scratchpad for research — not a governed build path.",
                    }
                }

                div { class: "chat-log",
                    if turns().is_empty() {
                        p { class: "chat-empty",
                            match mode() {
                                ChatMode::Guide => "Ask how to do something in Camerata…",
                                ChatMode::Technical => "Ask how Camerata works under the hood…",
                                ChatMode::Research => "Ask anything. Pick a model above.",
                            }
                        }
                    }
                    for (i , t) in turns().iter().enumerate() {
                        div { key: "{i}", class: if t.role == "you" { "chat-turn you" } else { "chat-turn ai" },
                            span { class: "chat-turn-role", "{t.role}" }
                            // The assistant replies in markdown; render it (tables/lists/bold/code)
                            // instead of showing raw source. User turns stay plain text.
                            if t.role == "ai" {
                                div { class: "chat-turn-text md", dangerous_inner_html: md_to_html(&t.text) }
                            } else {
                                span { class: "chat-turn-text", "{t.text}" }
                            }
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
                        placeholder: match mode() {
                            ChatMode::Guide => "Ask how to do something in Camerata… (Enter to send)",
                            ChatMode::Technical => "Ask how Camerata works under the hood… (Enter to send)",
                            ChatMode::Research => "Message… (Enter to send, Shift+Enter for newline)",
                        },
                        value: "{draft}",
                        onkeydown: move |e| {
                            if e.key() == Key::Enter && !e.modifiers().shift() {
                                e.prevent_default();
                                let prompt = draft().trim().to_string();
                                if prompt.is_empty() || sending() { return; }
                                let mdl = model();
                                let sys = match mode() {
                                    ChatMode::Guide => Some(guide_system_prompt(&catalog_kd)),
                                    ChatMode::Technical => Some(technical_system_prompt()),
                                    ChatMode::Research => None,
                                };
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
                            let sys = match mode() {
                                ChatMode::Guide => Some(guide_system_prompt(&catalog_btn)),
                                ChatMode::Technical => Some(technical_system_prompt()),
                                ChatMode::Research => None,
                            };
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

// ── unit tests — prompt assembly + grounding ─────────────────────────────────
//
// These tests cover the STATIC side of Guide mode (prompt text construction) and
// do NOT make live model calls. A compile-time `include_str!` bakes the guide into
// the binary, so these tests also serve as a guard that the guide file is present
// and non-empty at build time.

#[cfg(test)]
mod tests {
    use super::{
        guide_system_prompt, technical_system_prompt, GUIDE_NOT_COVERED_PHRASE, TECHNICAL_DOC,
        USER_GUIDE,
    };

    // ── USER_GUIDE grounding ─────────────────────────────────────────────────

    /// The user guide constant is baked in at compile time (include_str!). This test
    /// confirms it is non-empty and contains identifiable content from the real file,
    /// so a stale path or an accidentally-empty file is caught at test time.
    #[test]
    fn user_guide_constant_is_non_empty_and_contains_known_content() {
        assert!(
            !USER_GUIDE.is_empty(),
            "USER_GUIDE is empty — include_str! path likely broken"
        );
        // The header of docs/USER_GUIDE.md has been stable; assert a landmark.
        assert!(
            USER_GUIDE.contains("Camerata"),
            "USER_GUIDE does not mention 'Camerata' — file may be wrong"
        );
    }

    // ── TECHNICAL_DOC grounding ──────────────────────────────────────────────

    /// Symmetric check for TECHNICAL.md — confirms the const is wired to a real file.
    #[test]
    fn technical_doc_constant_is_non_empty_and_contains_known_content() {
        assert!(
            !TECHNICAL_DOC.is_empty(),
            "TECHNICAL_DOC is empty — include_str! path likely broken"
        );
        assert!(
            TECHNICAL_DOC.contains("camerata"),
            "TECHNICAL_DOC does not mention 'camerata' — file may be wrong"
        );
    }

    // ── guide_system_prompt — grounding injection ────────────────────────────

    /// The guide prompt must embed the full USER_GUIDE text so the model can answer
    /// from it. Check that a distinctive landmark from the guide appears verbatim.
    #[test]
    fn guide_prompt_contains_user_guide_content() {
        let prompt = guide_system_prompt("");
        // The guide's section heading is a stable landmark.
        assert!(
            prompt.contains("=== CAMERATA USER GUIDE ==="),
            "Guide prompt is missing the USER GUIDE section header"
        );
        // The guide body itself must be present.
        assert!(
            prompt.contains(USER_GUIDE),
            "Guide prompt does not contain the full USER_GUIDE content"
        );
    }

    /// Rules catalog is appended when non-empty, with the catalog section header.
    #[test]
    fn guide_prompt_appends_rules_catalog_when_present() {
        let catalog = "- RULE-1 [security · repo-local]: no hardcoded secrets\n";
        let prompt = guide_system_prompt(catalog);
        assert!(
            prompt.contains("=== CAMERATA RULES CATALOG"),
            "Guide prompt is missing the RULES CATALOG section header"
        );
        assert!(
            prompt.contains(catalog),
            "Guide prompt does not contain the supplied rules catalog"
        );
    }

    /// When the rules catalog is empty (network unavailable or no corpus yet), the
    /// guide prompt must still assemble without the catalog section — the guide alone
    /// is sufficient grounding for how-to questions.
    #[test]
    fn guide_prompt_omits_catalog_section_when_empty() {
        let prompt = guide_system_prompt("");
        assert!(
            !prompt.contains("=== CAMERATA RULES CATALOG"),
            "Guide prompt should not include the catalog section when the catalog is empty"
        );
    }

    /// A whitespace-only catalog is treated the same as empty (trim check).
    #[test]
    fn guide_prompt_omits_catalog_for_whitespace_only_input() {
        let prompt = guide_system_prompt("   \n\t  ");
        assert!(
            !prompt.contains("=== CAMERATA RULES CATALOG"),
            "Guide prompt should not include the catalog section for whitespace-only input"
        );
    }

    // ── "not covered" guardrail ──────────────────────────────────────────────

    /// The canonical not-covered phrase must appear verbatim in the guide prompt so
    /// the model has a concrete, testable response to copy when a question falls outside
    /// the grounding materials. A vague "say so" is not enough.
    #[test]
    fn guide_prompt_contains_not_covered_phrase() {
        let prompt = guide_system_prompt("");
        assert!(
            prompt.contains(GUIDE_NOT_COVERED_PHRASE),
            "Guide prompt is missing the not-covered phrase: {:?}",
            GUIDE_NOT_COVERED_PHRASE
        );
    }

    /// The guardrail must also appear when the catalog is present — the combined prompt
    /// must not accidentally drop the phrase through string concatenation.
    #[test]
    fn guide_prompt_not_covered_phrase_survives_catalog_append() {
        let catalog = "- RULE-1 [security · repo-local]: no hardcoded secrets\n";
        let prompt = guide_system_prompt(catalog);
        assert!(
            prompt.contains(GUIDE_NOT_COVERED_PHRASE),
            "Guide prompt is missing the not-covered phrase after appending catalog"
        );
    }

    /// The guardrail is marked CRITICAL in the prompt so the model treats it as a
    /// hard constraint, not a soft preference.
    #[test]
    fn guide_prompt_not_covered_guardrail_is_marked_critical() {
        let prompt = guide_system_prompt("");
        assert!(
            prompt.contains("CRITICAL"),
            "Guide prompt should mark the not-covered guardrail as CRITICAL"
        );
    }

    /// The guardrail must appear BEFORE the guide content, not after — the model reads
    /// the system prompt top-to-bottom and should encounter the constraint early.
    #[test]
    fn guide_prompt_not_covered_phrase_appears_before_guide_content() {
        let prompt = guide_system_prompt("");
        let phrase_pos = prompt
            .find(GUIDE_NOT_COVERED_PHRASE)
            .expect("GUIDE_NOT_COVERED_PHRASE not found in prompt");
        let guide_pos = prompt
            .find("=== CAMERATA USER GUIDE ===")
            .expect("USER GUIDE section header not found in prompt");
        assert!(
            phrase_pos < guide_pos,
            "not-covered phrase should appear before the guide content (phrase at {phrase_pos}, \
             guide section at {guide_pos})"
        );
    }

    // ── technical_system_prompt ──────────────────────────────────────────────

    /// The technical prompt must embed TECHNICAL.md and cite the right constraint.
    #[test]
    fn technical_prompt_contains_technical_doc_and_constraint() {
        let prompt = technical_system_prompt();
        assert!(
            prompt.contains("=== CAMERATA TECHNICAL REFERENCE ==="),
            "Technical prompt is missing the TECHNICAL REFERENCE section header"
        );
        assert!(
            prompt.contains(TECHNICAL_DOC),
            "Technical prompt does not contain the full TECHNICAL_DOC content"
        );
        // The technical prompt must also have a "not covered" guardrail.
        assert!(
            prompt.contains("not covered"),
            "Technical prompt should say when something is not covered in the reference"
        );
    }

    // ── GUIDE_NOT_COVERED_PHRASE constant ───────────────────────────────────

    /// The constant itself must be non-empty and end without a leading space — a
    /// trivially wrong value would break the "appears before guide" ordering test.
    #[test]
    fn guide_not_covered_phrase_is_well_formed() {
        assert!(
            !GUIDE_NOT_COVERED_PHRASE.is_empty(),
            "GUIDE_NOT_COVERED_PHRASE should not be empty"
        );
        assert!(
            !GUIDE_NOT_COVERED_PHRASE.starts_with(' '),
            "GUIDE_NOT_COVERED_PHRASE should not start with a space"
        );
        // Must be human-readable (contains at least one letter).
        assert!(
            GUIDE_NOT_COVERED_PHRASE.chars().any(|c| c.is_alphabetic()),
            "GUIDE_NOT_COVERED_PHRASE should contain at least one letter"
        );
    }
}
