//! The research chat bubble: a floating, always-available AI chat panel (model
//! selectable) that sits side-by-side with the rest of the app. Provides four modes:
//!
//! - **Research**: ungoverned scratchpad, no system prompt.
//! - **Guide**: grounded in `docs/USER_GUIDE.md` + the live rules catalog; answers
//!   how-to questions and cites real rule ids.
//! - **Technical**: grounded in `docs/TECHNICAL.md`; explains Camerata's internals.
//! - **Project**: grounded in the active project's LIVE state (draft, scan report,
//!   findings, selected ruleset). Lets the architect ask about their specific project.
//!   Includes an "ask about this finding" path (injected via `FindingContext`) so a
//!   single finding can be the focal point.
//!
//! All modes send completions through `POST /api/chat` (the same provider seam every
//! other AI step uses). The Project mode fetches project state from
//! `GET /api/projects/active/context` and assembles the system prompt client-side.

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

/// The grounding payload returned by `GET /api/projects/active/context`.
/// Mirrors the server-side `ProjectContextResponse` — only the fields the UI needs.
#[derive(Clone, PartialEq, serde::Deserialize)]
pub(crate) struct ProjectContextResp {
    ok: bool,
    #[serde(default)]
    phase: String, // "blank" | "pre_onboard" | "post_onboard"
    #[serde(default)]
    project_name: Option<String>,
    #[serde(default)]
    repos: Vec<String>,
    #[serde(default)]
    onboarded: Vec<String>,
    #[serde(default)]
    ruleset_summary: Option<String>,
    #[serde(default)]
    finding_count: Option<usize>,
    #[serde(default)]
    findings_summary: Option<String>,
    #[serde(default)]
    draft_json: Option<serde_json::Value>,
    #[serde(default)]
    message: Option<String>,
}

/// Fetch the active project's grounding context from the BFF.
async fn fetch_project_context() -> Option<ProjectContextResp> {
    reqwest::get(format!("{}/api/projects/active/context", crate::BFF_URL))
        .await
        .ok()?
        .json::<ProjectContextResp>()
        .await
        .ok()
}

/// A specific finding the architect wants to discuss, supplied as context so the Project
/// chat assistant can answer "why was this flagged / how do I fix it?" with concrete detail.
///
/// Populated when the user clicks "Ask about this finding" in the findings table, injected
/// into the first system prompt turn so the assistant is focused on that one finding.
/// When `None`, the assistant talks about the project as a whole.
#[derive(Clone, PartialEq, Default)]
pub struct FindingContext {
    /// The rule id that fired (e.g. `SEC-NO-HARDCODED-SECRETS-1`).
    pub rule_id: String,
    /// Severity: `high` | `medium`.
    pub severity: String,
    /// File path + 1-based line.
    pub path: String,
    pub line: usize,
    /// The offending snippet (trimmed, capped).
    pub snippet: String,
    /// The gate's own explanation of the violation.
    pub detail: String,
    /// Repo (`owner/repo`) this finding came from.
    pub repo: String,
}

/// Which mode the chat panel is in.
#[derive(Clone, Copy, PartialEq)]
enum ChatMode {
    Research,
    Guide,
    /// Grounded in docs/TECHNICAL.md: answers HOW Camerata works under the hood.
    Technical,
    /// Grounded in the active project's LIVE state (phase-aware: draft for pre-onboard,
    /// scan report + findings + ruleset for post-onboard). Lets the architect ask "why did
    /// this rule fire?" or "what does my ruleset look like?" against REAL project data.
    Project,
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

/// The exact phrase the Project-mode assistant must say when a question falls outside
/// the grounded project context. Hard-coded so tests can assert the phrase survives
/// prompt construction — changing the wording requires updating both the prompt and the
/// tests, preventing silent drift.
pub(crate) const PROJECT_NOT_COVERED_PHRASE: &str =
    "That isn't covered by the current project context.";

/// Build the Project-mode system prompt from the fetched grounding context.
///
/// # Phase-aware grounding
///
/// The prompt is assembled differently depending on the project's onboarding phase:
///
/// - **Blank**: the project has no scan or draft data. The assistant explains what the
///   project contains and encourages starting a scan.
/// - **PreOnboard**: an in-progress onboarding draft exists but Apply has not completed.
///   The draft JSON is injected so the assistant can help interpret the in-progress state.
/// - **PostOnboard**: at least one repo is onboarded. The live ruleset summary + findings
///   (if available) are injected. The draft is intentionally omitted (too noisy at this phase).
///
/// # Finding-scoped variant
///
/// When `finding` is `Some`, the prompt includes a focused section for that specific
/// finding so the assistant answers "why was this flagged / how do I fix it?" directly.
/// The project context is still included as background.
///
/// # Not-covered guardrail
///
/// The prompt includes [`PROJECT_NOT_COVERED_PHRASE`] as a CRITICAL constraint: if a
/// question cannot be answered from the project context the assistant must say the exact
/// phrase rather than improvising. Mirrors the Guide mode's guardrail pattern.
pub(crate) fn project_system_prompt(
    ctx: &ProjectContextResp,
    finding: Option<&FindingContext>,
) -> String {
    let not_covered = PROJECT_NOT_COVERED_PHRASE;
    let project_name = ctx
        .project_name
        .as_deref()
        .unwrap_or("the active project");

    let mut p = format!(
        "You are Camerata's in-app project assistant. You help the architect understand and \
         act on the CURRENT STATE of their project (findings, ruleset, onboarding progress). \
         Answer ONLY from the project context below — do not improvise facts about the project \
         that are not in the context. CRITICAL: if the user asks about something NOT present in \
         the project context, respond with exactly \"{not_covered}\" followed by a brief \
         explanation of what IS available. Be concise and concrete.\n\n"
    );

    // Phase-specific header.
    match ctx.phase.as_str() {
        "post_onboard" => {
            p.push_str(&format!(
                "=== PROJECT: {project_name} (ONBOARDED) ===\n"
            ));
            let repos_list = if ctx.repos.is_empty() {
                "(none listed)".to_string()
            } else {
                ctx.repos.join(", ")
            };
            let onboarded_list = if ctx.onboarded.is_empty() {
                "(none)".to_string()
            } else {
                ctx.onboarded.join(", ")
            };
            p.push_str(&format!("Repos in scope: {repos_list}\n"));
            p.push_str(&format!("Onboarded repos: {onboarded_list}\n"));
            if let Some(rs) = &ctx.ruleset_summary {
                if !rs.trim().is_empty() {
                    p.push_str("\n--- SELECTED RULESET ---\n");
                    p.push_str(rs);
                    p.push('\n');
                }
            }
            if let Some(fc) = ctx.finding_count {
                p.push_str(&format!("\nTotal findings from last audit: {fc}\n"));
            }
            if let Some(fs) = &ctx.findings_summary {
                if !fs.trim().is_empty() {
                    p.push_str("\n--- FINDINGS FROM LAST AUDIT (up to 50 shown) ---\n");
                    p.push_str(fs);
                    p.push('\n');
                }
            }
        }
        "pre_onboard" => {
            p.push_str(&format!(
                "=== PROJECT: {project_name} (ONBOARDING IN PROGRESS) ===\n"
            ));
            let repos_list = if ctx.repos.is_empty() {
                "(none listed)".to_string()
            } else {
                ctx.repos.join(", ")
            };
            p.push_str(&format!("Repos in scope: {repos_list}\n"));
            p.push_str(
                "Status: Onboarding has started but Apply has not completed for any repo.\n",
            );
            if let Some(fc) = ctx.finding_count {
                p.push_str(&format!("Findings found so far: {fc}\n"));
            }
            if let Some(fs) = &ctx.findings_summary {
                if !fs.trim().is_empty() {
                    p.push_str("\n--- FINDINGS FROM IN-PROGRESS AUDIT (up to 50 shown) ---\n");
                    p.push_str(fs);
                    p.push('\n');
                }
            }
            if let Some(draft) = &ctx.draft_json {
                // Inject a compact representation of the draft (not the full JSON blob —
                // that could be enormous). Surface the repos + proposed rules only.
                if let Some(scan) = draft.get("scan") {
                    if let Some(proposed) = scan.get("proposed_rules").and_then(|v| v.as_array()) {
                        if !proposed.is_empty() {
                            p.push_str("\n--- PROPOSED RULES FROM SCAN ---\n");
                            for rule in proposed.iter().take(30) {
                                let id = rule
                                    .get("id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("?");
                                let title = rule
                                    .get("title")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                let scope = rule
                                    .get("scope")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                p.push_str(&format!("- {id} [{scope}]: {title}\n"));
                            }
                        }
                    }
                }
            }
        }
        _ => {
            // Blank or unknown phase.
            p.push_str(&format!("=== PROJECT: {project_name} (NO SCAN DATA) ===\n"));
            let repos_list = if ctx.repos.is_empty() {
                "(none listed)".to_string()
            } else {
                ctx.repos.join(", ")
            };
            p.push_str(&format!("Repos in scope: {repos_list}\n"));
            if let Some(msg) = &ctx.message {
                p.push_str(&format!("Status: {msg}\n"));
            }
        }
    }

    // Finding-scoped section: inject when the user asked about a specific finding.
    if let Some(f) = finding {
        if !f.rule_id.is_empty() {
            p.push_str("\n=== FOCUSED FINDING (the user is asking about this specific finding) ===\n");
            p.push_str(&format!("Rule: {}\n", f.rule_id));
            p.push_str(&format!("Severity: {}\n", f.severity));
            p.push_str(&format!("Repo: {}\n", f.repo));
            p.push_str(&format!("File: {} (line {})\n", f.path, f.line));
            if !f.snippet.is_empty() {
                p.push_str(&format!("Snippet: {}\n", f.snippet));
            }
            if !f.detail.is_empty() {
                p.push_str(&format!("Gate detail: {}\n", f.detail));
            }
            p.push_str(
                "\nThe user wants to understand WHY this was flagged and HOW to fix it. \
                 Answer from the gate detail and rule context above. If you need to reference \
                 the rule's rationale and it is not in the context, say so clearly.\n",
            );
        }
    }

    p
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

/// Props for the `ChatBubble` component. The optional `finding` prop wires the
/// "Ask about this finding" path: when present the chat panel opens in Project mode
/// pre-seeded with the finding context, and the user's first message is answered in
/// the context of that specific finding.
#[derive(Props, Clone, PartialEq)]
pub struct ChatBubbleProps {
    /// When set, the chat opens in Project mode focused on this specific finding.
    /// The panel opens automatically when this prop changes (non-None -> open).
    #[props(default)]
    pub finding: Option<FindingContext>,
}

#[component]
pub fn ChatBubble(props: ChatBubbleProps) -> Element {
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

    // Project context: fetched once per open + whenever the mode switches to Project.
    // Stored as an Option so the panel renders while it loads (shows a loading state).
    let project_ctx_res = use_resource(fetch_project_context);
    let project_ctx = project_ctx_res.read().clone().flatten();

    // When a finding is injected via props: open the panel in Project mode and pre-seed
    // the finding context. The panel auto-opens so the user sees the response immediately.
    // We track the last injected finding by rule_id+path+line to avoid re-injecting on
    // unrelated re-renders.
    let mut last_injected_finding = use_signal(|| Option::<String>::None);
    if let Some(ref f) = props.finding {
        if !f.rule_id.is_empty() {
            let key = format!("{}\u{0}{}\u{0}{}", f.rule_id, f.path, f.line);
            if last_injected_finding() != Some(key.clone()) {
                last_injected_finding.set(Some(key));
                mode.set(ChatMode::Project);
                open.set(true);
                turns.write().clear();
            }
        }
    }

    // The finding in scope for the current Project session. Set when we switch to Project
    // mode via the finding prop; cleared when the user switches modes or starts a new chat.
    let mut active_finding: Signal<Option<FindingContext>> = use_signal(|| None);
    if let Some(ref f) = props.finding {
        if !f.rule_id.is_empty() && *active_finding.read() != Some(f.clone()) {
            if mode() == ChatMode::Project {
                active_finding.set(Some(f.clone()));
            }
        }
    }

    // Clones for the two send closures (onkeydown + onclick each move-capture their own).
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
                            ChatMode::Project => "Project chat",
                            ChatMode::Research => "Research chat",
                        }
                    }
                    // Research / Guide / Technical / Project mode toggle
                    div { class: "chat-mode-toggle",
                        button {
                            class: if mode() == ChatMode::Research { "chat-mode-btn active" } else { "chat-mode-btn" },
                            onclick: move |_| {
                                mode.set(ChatMode::Research);
                                turns.write().clear();
                                active_finding.set(None);
                            },
                            "Research"
                        }
                        button {
                            class: if mode() == ChatMode::Guide { "chat-mode-btn active" } else { "chat-mode-btn" },
                            onclick: move |_| {
                                mode.set(ChatMode::Guide);
                                turns.write().clear();
                                active_finding.set(None);
                            },
                            "Guide"
                        }
                        button {
                            class: if mode() == ChatMode::Technical { "chat-mode-btn active" } else { "chat-mode-btn" },
                            onclick: move |_| {
                                mode.set(ChatMode::Technical);
                                turns.write().clear();
                                active_finding.set(None);
                            },
                            "Technical"
                        }
                        button {
                            class: if mode() == ChatMode::Project { "chat-mode-btn active" } else { "chat-mode-btn" },
                            onclick: move |_| {
                                mode.set(ChatMode::Project);
                                turns.write().clear();
                                active_finding.set(None);
                            },
                            "Project"
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
                        ChatMode::Project => "Answers grounded in the active project's live state (findings, ruleset, onboarding).",
                        ChatMode::Research => "Ungoverned scratchpad for research — not a governed build path.",
                    }
                }

                // Project mode: show a context banner when a finding is in scope so the
                // user knows the assistant is focused on that specific violation.
                if mode() == ChatMode::Project {
                    if let Some(ref f) = *active_finding.read() {
                        if !f.rule_id.is_empty() {
                            div { class: "chat-finding-banner",
                                span { class: "chat-finding-label", "Focused finding:" }
                                span { class: "chat-finding-rule", "{f.rule_id}" }
                                span { class: "chat-finding-loc", "{f.path}:{f.line}" }
                            }
                        }
                    }
                }

                div { class: "chat-log",
                    if turns().is_empty() {
                        p { class: "chat-empty",
                            match mode() {
                                ChatMode::Guide => "Ask how to do something in Camerata…",
                                ChatMode::Technical => "Ask how Camerata works under the hood…",
                                ChatMode::Project => {
                                    if active_finding.read().as_ref().map(|f| !f.rule_id.is_empty()).unwrap_or(false) {
                                        "Ask why this finding was flagged, how to fix it, or what the rule means…"
                                    } else {
                                        "Ask about this project's findings, ruleset, or onboarding status…"
                                    }
                                }
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
                            ChatMode::Project => "Ask about this project… (Enter to send)",
                            ChatMode::Research => "Message… (Enter to send, Shift+Enter for newline)",
                        },
                        value: "{draft}",
                        onkeydown: {
                            // Clone all captured state up front; the closure is FnMut and
                            // cannot capture by reference across async boundaries.
                            let catalog_kd2 = catalog_kd.clone();
                            let proj_ctx_kd = project_ctx.clone();
                            let finding_kd = active_finding.read().clone();
                            move |e: Event<KeyboardData>| {
                                if e.key() == Key::Enter && !e.modifiers().shift() {
                                    e.prevent_default();
                                    let prompt = draft().trim().to_string();
                                    if prompt.is_empty() || sending() { return; }
                                    let mdl = model();
                                    let sys = match mode() {
                                        ChatMode::Guide => Some(guide_system_prompt(&catalog_kd2)),
                                        ChatMode::Technical => Some(technical_system_prompt()),
                                        ChatMode::Project => proj_ctx_kd.as_ref().map(|ctx| {
                                            project_system_prompt(ctx, finding_kd.as_ref())
                                        }),
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
                            }
                        },
                        oninput: move |e| draft.set(e.value()),
                    }
                    button {
                        class: "chat-send",
                        disabled: sending() || draft().trim().is_empty(),
                        onclick: {
                            let catalog_btn2 = catalog_btn.clone();
                            let proj_ctx_btn = project_ctx.clone();
                            let finding_btn = active_finding.read().clone();
                            move |_| {
                                let prompt = draft().trim().to_string();
                                if prompt.is_empty() || sending() { return; }
                                let mdl = model();
                                let sys = match mode() {
                                    ChatMode::Guide => Some(guide_system_prompt(&catalog_btn2)),
                                    ChatMode::Technical => Some(technical_system_prompt()),
                                    ChatMode::Project => proj_ctx_btn.as_ref().map(|ctx| {
                                        project_system_prompt(ctx, finding_btn.as_ref())
                                    }),
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
                            }
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
        guide_system_prompt, project_system_prompt, technical_system_prompt, FindingContext,
        ProjectContextResp, GUIDE_NOT_COVERED_PHRASE, PROJECT_NOT_COVERED_PHRASE, TECHNICAL_DOC,
        USER_GUIDE,
    };

    // ── test fixture helpers ─────────────────────────────────────────────────

    /// Build a minimal `ProjectContextResp` for the given phase with optional fields.
    fn make_ctx(phase: &str, project_name: &str) -> ProjectContextResp {
        ProjectContextResp {
            ok: true,
            phase: phase.to_string(),
            project_name: Some(project_name.to_string()),
            repos: vec!["me/api".to_string()],
            onboarded: if phase == "post_onboard" {
                vec!["me/api".to_string()]
            } else {
                vec![]
            },
            ruleset_summary: if phase == "post_onboard" {
                Some(
                    "SEC-NO-HARDCODED-SECRETS-1: repo-local (me/api)\nARCH-LAYER-1: cross-repo"
                        .to_string(),
                )
            } else {
                None
            },
            finding_count: if phase != "blank" { Some(3) } else { None },
            findings_summary: if phase != "blank" {
                Some("[high] SEC-NO-HARDCODED-SECRETS-1 in me/api/src/main.rs:42 — hardcoded credential".to_string())
            } else {
                None
            },
            draft_json: if phase == "pre_onboard" {
                Some(serde_json::json!({
                    "scan": {
                        "repos": ["me/api"],
                        "proposed_rules": [
                            { "id": "SEC-NO-HARDCODED-SECRETS-1", "title": "No hardcoded secrets", "scope": "repo-local" }
                        ]
                    }
                }))
            } else {
                None
            },
            message: None,
        }
    }

    /// Build a minimal `FindingContext` for tests.
    fn make_finding() -> FindingContext {
        FindingContext {
            rule_id: "SEC-NO-HARDCODED-SECRETS-1".to_string(),
            severity: "high".to_string(),
            path: "src/main.rs".to_string(),
            line: 42,
            snippet: "let pwd = \"hunter2\";".to_string(),
            detail: "Hardcoded password literal found.".to_string(),
            repo: "me/api".to_string(),
        }
    }

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

    // ── PROJECT_NOT_COVERED_PHRASE constant ─────────────────────────────────

    /// The project not-covered constant must be non-empty and well-formed.
    #[test]
    fn project_not_covered_phrase_is_well_formed() {
        assert!(
            !PROJECT_NOT_COVERED_PHRASE.is_empty(),
            "PROJECT_NOT_COVERED_PHRASE should not be empty"
        );
        assert!(
            !PROJECT_NOT_COVERED_PHRASE.starts_with(' '),
            "PROJECT_NOT_COVERED_PHRASE should not start with a space"
        );
        assert!(
            PROJECT_NOT_COVERED_PHRASE.chars().any(|c| c.is_alphabetic()),
            "PROJECT_NOT_COVERED_PHRASE should contain at least one letter"
        );
    }

    // ── project_system_prompt — grounding injection (post-onboard) ───────────

    /// Post-onboard prompt must include the project name, repos, and ruleset section.
    #[test]
    fn project_prompt_post_onboard_includes_project_and_ruleset() {
        let ctx = make_ctx("post_onboard", "MyProject");
        let prompt = project_system_prompt(&ctx, None);
        assert!(
            prompt.contains("MyProject"),
            "Project prompt must include the project name"
        );
        assert!(
            prompt.contains("ONBOARDED"),
            "Post-onboard prompt must include ONBOARDED label"
        );
        assert!(
            prompt.contains("SELECTED RULESET"),
            "Post-onboard prompt must include the SELECTED RULESET section"
        );
        assert!(
            prompt.contains("SEC-NO-HARDCODED-SECRETS-1"),
            "Post-onboard prompt must include the actual rule ids from the ruleset summary"
        );
    }

    /// Post-onboard prompt must include the findings section when findings are present.
    #[test]
    fn project_prompt_post_onboard_includes_findings_when_present() {
        let ctx = make_ctx("post_onboard", "MyProject");
        let prompt = project_system_prompt(&ctx, None);
        assert!(
            prompt.contains("FINDINGS FROM LAST AUDIT"),
            "Post-onboard prompt must include the findings section header"
        );
        assert!(
            prompt.contains("hardcoded credential"),
            "Post-onboard prompt must include the findings summary text"
        );
    }

    // ── project_system_prompt — pre-onboard phase ────────────────────────────

    /// Pre-onboard prompt must indicate in-progress status and proposed rules.
    #[test]
    fn project_prompt_pre_onboard_includes_status_and_proposed_rules() {
        let ctx = make_ctx("pre_onboard", "WIPProject");
        let prompt = project_system_prompt(&ctx, None);
        assert!(
            prompt.contains("WIPProject"),
            "Pre-onboard prompt must include the project name"
        );
        assert!(
            prompt.contains("ONBOARDING IN PROGRESS"),
            "Pre-onboard prompt must include ONBOARDING IN PROGRESS label"
        );
        assert!(
            prompt.contains("PROPOSED RULES FROM SCAN"),
            "Pre-onboard prompt must include the proposed rules section"
        );
        assert!(
            prompt.contains("SEC-NO-HARDCODED-SECRETS-1"),
            "Pre-onboard prompt must include proposed rule ids from the draft"
        );
    }

    // ── project_system_prompt — blank phase ──────────────────────────────────

    /// Blank-phase prompt must say no scan data yet.
    #[test]
    fn project_prompt_blank_phase_explains_no_data() {
        let ctx = make_ctx("blank", "EmptyProject");
        let prompt = project_system_prompt(&ctx, None);
        assert!(
            prompt.contains("EmptyProject"),
            "Blank-phase prompt must include the project name"
        );
        assert!(
            prompt.contains("NO SCAN DATA"),
            "Blank-phase prompt must include NO SCAN DATA label"
        );
    }

    // ── project_system_prompt — not-covered guardrail ────────────────────────

    /// The not-covered phrase must appear in the project prompt regardless of phase.
    #[test]
    fn project_prompt_contains_not_covered_phrase_post_onboard() {
        let ctx = make_ctx("post_onboard", "P");
        let prompt = project_system_prompt(&ctx, None);
        assert!(
            prompt.contains(PROJECT_NOT_COVERED_PHRASE),
            "Post-onboard project prompt missing the not-covered phrase"
        );
    }

    #[test]
    fn project_prompt_contains_not_covered_phrase_pre_onboard() {
        let ctx = make_ctx("pre_onboard", "P");
        let prompt = project_system_prompt(&ctx, None);
        assert!(
            prompt.contains(PROJECT_NOT_COVERED_PHRASE),
            "Pre-onboard project prompt missing the not-covered phrase"
        );
    }

    #[test]
    fn project_prompt_contains_not_covered_phrase_blank() {
        let ctx = make_ctx("blank", "P");
        let prompt = project_system_prompt(&ctx, None);
        assert!(
            prompt.contains(PROJECT_NOT_COVERED_PHRASE),
            "Blank-phase project prompt missing the not-covered phrase"
        );
    }

    /// The not-covered guardrail is marked CRITICAL in the project prompt, matching
    /// the Guide mode pattern.
    #[test]
    fn project_prompt_not_covered_guardrail_is_marked_critical() {
        let ctx = make_ctx("post_onboard", "P");
        let prompt = project_system_prompt(&ctx, None);
        assert!(
            prompt.contains("CRITICAL"),
            "Project prompt must mark the not-covered guardrail as CRITICAL"
        );
    }

    // ── project_system_prompt — finding-scoped variant ───────────────────────

    /// When a finding is injected, the prompt must include the FOCUSED FINDING section
    /// with the rule id, severity, file path, and gate detail.
    #[test]
    fn project_prompt_with_finding_includes_focused_finding_section() {
        let ctx = make_ctx("post_onboard", "P");
        let f = make_finding();
        let prompt = project_system_prompt(&ctx, Some(&f));
        assert!(
            prompt.contains("FOCUSED FINDING"),
            "Finding-scoped prompt must include the FOCUSED FINDING header"
        );
        assert!(
            prompt.contains("SEC-NO-HARDCODED-SECRETS-1"),
            "Finding-scoped prompt must include the rule id"
        );
        assert!(
            prompt.contains("high"),
            "Finding-scoped prompt must include the severity"
        );
        assert!(
            prompt.contains("src/main.rs"),
            "Finding-scoped prompt must include the file path"
        );
        assert!(
            prompt.contains("Hardcoded password literal found"),
            "Finding-scoped prompt must include the gate detail"
        );
    }

    /// Without a finding, the FOCUSED FINDING section must NOT appear (no spurious injection).
    #[test]
    fn project_prompt_without_finding_has_no_focused_finding_section() {
        let ctx = make_ctx("post_onboard", "P");
        let prompt = project_system_prompt(&ctx, None);
        assert!(
            !prompt.contains("FOCUSED FINDING"),
            "Project prompt without finding must not include the FOCUSED FINDING section"
        );
    }

    /// An empty finding (default) must be treated the same as None — no focused section.
    #[test]
    fn project_prompt_with_empty_finding_has_no_focused_finding_section() {
        let ctx = make_ctx("post_onboard", "P");
        let empty = FindingContext::default();
        let prompt = project_system_prompt(&ctx, Some(&empty));
        assert!(
            !prompt.contains("FOCUSED FINDING"),
            "Project prompt with empty finding must not include the FOCUSED FINDING section"
        );
    }

    /// The project prompt + finding injection must also contain the not-covered phrase,
    /// confirming the guardrail survives the extra section.
    #[test]
    fn project_prompt_with_finding_retains_not_covered_guardrail() {
        let ctx = make_ctx("post_onboard", "P");
        let f = make_finding();
        let prompt = project_system_prompt(&ctx, Some(&f));
        assert!(
            prompt.contains(PROJECT_NOT_COVERED_PHRASE),
            "Finding-scoped project prompt must retain the not-covered guardrail"
        );
    }

    // ── grounding order: not-covered before project section ──────────────────

    /// The not-covered constraint must appear BEFORE the project section header, so the
    /// model encounters the constraint before reading the data it might over-extrapolate.
    #[test]
    fn project_prompt_not_covered_phrase_appears_before_project_section() {
        let ctx = make_ctx("post_onboard", "MyProject");
        let prompt = project_system_prompt(&ctx, None);
        let phrase_pos = prompt
            .find(PROJECT_NOT_COVERED_PHRASE)
            .expect("PROJECT_NOT_COVERED_PHRASE not found in prompt");
        let header_pos = prompt
            .find("=== PROJECT:")
            .expect("PROJECT section header not found in prompt");
        assert!(
            phrase_pos < header_pos,
            "not-covered phrase must appear before the project section header \
             (phrase at {phrase_pos}, section at {header_pos})"
        );
    }
}
