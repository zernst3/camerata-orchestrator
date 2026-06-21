//! The Camerata AI chat bubble — ONE context-rich assistant.
//!
//! A single floating chat panel that bundles ALL available context into one
//! system prompt, sent to `POST /api/chat`. No mode selector: the user's
//! prompt determines which part of the grounding the assistant draws from.
//!
//! ## What the assistant can see
//!
//! The unified system prompt is assembled in four layers, in this order:
//!
//! 1. **Technical reference** (`docs/TECHNICAL.md`, static, baked in at compile
//!    time). How Camerata works: crates, modules, structs, algorithms.
//!
//! 2. **Project rules + config** (live corpus catalog from
//!    `GET /api/corpus-rules`, fetched once per session). Every governance rule
//!    the corpus knows about, with domain, scope, and alternatives.
//!
//! 3. **Development state** (live UoW snapshot from `GET /api/uow`, refreshed
//!    per turn). Per-story id, lifecycle stage, gate status, sign-off state,
//!    and last activity — so "what is story CAM-42 blocked on?" gets a real
//!    answer.
//!
//! 4. **Active finding** (optional, injected when the architect clicks
//!    "Ask about this finding"). Adds a focused `=== FOCUSED FINDING ===`
//!    section so the assistant answers "why was this flagged / how do I fix it?"
//!    from the actual gate detail. Does NOT change the assistant's persona or
//!    replace the other context; it is additive.
//!
//! ## Prompt-cache strategy
//!
//! Layers 1 and 2 are STATIC within a session: they are assembled once and do
//! not change across turns (the rules catalog is fetched once; TECHNICAL_DOC is
//! compile-time). The Anthropic API automatically caches stable system-prompt
//! prefixes, so these layers benefit from caching without any explicit
//! `cache_control` annotation needed. Layer 3 (UoW snapshot) is refreshed per
//! turn — it is the tail of the system prompt and does not disturb the cached
//! prefix.
//!
//! ## Honesty guardrail
//!
//! [`UNIFIED_NOT_COVERED_PHRASE`] is the exact string the assistant must say
//! when none of the four layers cover the question. It is tested: changing the
//! wording requires updating both the prompt builder and the tests.

use dioxus::prelude::*;

use crate::md::md_to_html;

// Layer 1: technical reference, baked in at compile time so it can't drift.
// Grounded in docs/TECHNICAL.md — the canonical source for how Camerata works.
const TECHNICAL_DOC: &str = include_str!("../../../docs/TECHNICAL.md");

// Layer 1b: the user guide, baked in alongside the technical doc.
// Grounded in docs/USER_GUIDE.md — flows, how-to steps, feature descriptions.
const USER_GUIDE: &str = include_str!("../../../docs/USER_GUIDE.md");

// ── wire types ────────────────────────────────────────────────────────────────

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

// ── Layer 2: corpus rules ─────────────────────────────────────────────────────

/// A corpus rule, trimmed to what the assistant needs to name and describe it.
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

/// Fetch the whole rule corpus and render it as a compact catalog (one line per
/// rule). This is Layer 2 of the system prompt. Fetched once per session.
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

// ── Layer 3: development state (UoW snapshot) ─────────────────────────────────

/// One story's UoW snapshot as returned by `GET /api/development/context`.
///
/// Field names match the `UnitOfWork` wire format from `crates/server/src/uow.rs`.
/// `#[serde(default)]` on every optional field lets old and future server
/// responses deserialise without error.
///
/// `gate_status` is derived client-side from `gate_provenance` presence +
/// `stage`: once a story reaches `awaiting_qa` or `signed_off` we know a run
/// completed; `gate_provenance.deny_count > 0` means the gate blocked something.
#[derive(Clone, PartialEq, serde::Deserialize, Debug)]
pub(crate) struct UowSnapshot {
    #[serde(default)]
    pub story_id: String,
    /// The governed-development lifecycle stage wire string:
    /// `intake` | `investigating` | `decisions_approved` | `development`
    /// | `awaiting_qa` | `signed_off`.
    #[serde(default)]
    pub stage: String,
    /// Whether the architect has signed off this story.
    #[serde(default)]
    pub sign_off: Option<serde_json::Value>,
    /// RFC 3339 of the last mutation to this UoW.
    #[serde(default)]
    pub updated: String,
    /// The frozen gate provenance from the most recent completed governed run.
    #[serde(default)]
    pub gate_provenance: Option<GateProvenanceLite>,
}

/// The subset of `GateProvenance` the assistant needs to describe gate status.
#[derive(Clone, PartialEq, serde::Deserialize, Debug)]
pub(crate) struct GateProvenanceLite {
    #[serde(default)]
    pub allow_count: u64,
    #[serde(default)]
    pub deny_count: u64,
    #[serde(default)]
    pub total_bounces: u64,
    #[serde(default)]
    pub rules_fired: Vec<String>,
    #[serde(default)]
    pub mode: String,
}

/// Derive a short `gate_status` label from a UoW snapshot for display + prompt injection.
fn gate_status_label(snap: &UowSnapshot) -> &'static str {
    match snap.gate_provenance.as_ref() {
        None => "no run yet",
        Some(gp) if gp.deny_count > 0 => "gate blocked",
        Some(_) => "gate passed",
    }
}

/// Fetch all UoW snapshots from `GET /api/development/context`.
///
/// Falls back to `GET /api/uow` when the dedicated context endpoint is not yet
/// available (server branches merge separately). Both return the same wire
/// shape for the fields we need.
async fn fetch_uow_snapshot() -> Option<Vec<UowSnapshot>> {
    // Try the dedicated context endpoint first (pw/server branch).
    let dev_url = format!("{}/api/development/context", crate::BFF_URL);
    if let Ok(resp) = reqwest::get(&dev_url).await {
        if resp.status().is_success() {
            if let Ok(snaps) = resp.json::<Vec<UowSnapshot>>().await {
                return Some(snaps);
            }
        }
    }
    // Fallback: the existing /api/uow endpoint returns the full UnitOfWork which
    // is a superset of UowSnapshot (same field names, serde(default) covers extras).
    reqwest::get(format!("{}/api/uow", crate::BFF_URL))
        .await
        .ok()?
        .json::<Vec<UowSnapshot>>()
        .await
        .ok()
}

/// Render the UoW snapshot as a compact section for the system prompt (Layer 3).
/// One line per story: id, stage, gate status, sign-off, last activity.
/// Capped at 100 stories to keep the prompt bounded.
pub(crate) fn render_uow_section(snaps: &[UowSnapshot]) -> String {
    if snaps.is_empty() {
        return "No development stories tracked yet.\n".to_string();
    }
    let mut s = String::new();
    for snap in snaps.iter().take(100) {
        let signed = if snap.sign_off.is_some() {
            "signed-off"
        } else {
            "not signed off"
        };
        let stage = if snap.stage.is_empty() {
            "intake"
        } else {
            snap.stage.as_str()
        };
        let gate = gate_status_label(snap);
        let updated = if snap.updated.is_empty() {
            "(unknown)"
        } else {
            snap.updated.as_str()
        };
        s.push_str(&format!(
            "- {}: stage={stage}, gate={gate}, sign-off={signed}, last-activity={updated}\n",
            snap.story_id
        ));
        // If the gate blocked, surface which rules fired so the assistant can be specific.
        if let Some(gp) = &snap.gate_provenance {
            if !gp.rules_fired.is_empty() {
                s.push_str(&format!(
                    "  rules that blocked: {}\n",
                    gp.rules_fired.join(", ")
                ));
            }
        }
    }
    s
}

// ── Layer 4: focused finding ──────────────────────────────────────────────────

/// A specific finding the architect wants to discuss, supplied as additive
/// context so the assistant can answer "why was this flagged / how do I fix it?"
/// with concrete detail from the actual gate output.
///
/// Populated when the user clicks "Ask about this finding" in the findings table,
/// injected into the system prompt as an optional `=== FOCUSED FINDING ===`
/// section. The rest of the grounding (technical, rules, dev state) remains
/// in scope; this just adds a focused lens on top.
///
/// When `None`, the assistant answers about Camerata in general.
#[derive(Clone, PartialEq, Default)]
pub struct FindingContext {
    /// The rule id that fired (e.g. `SEC-NO-HARDCODED-SECRETS-1`).
    pub rule_id: String,
    /// Severity: `critical` | `high` | `medium` | `low`.
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

// ── system prompt assembly ────────────────────────────────────────────────────

/// The exact phrase the assistant must say when none of the four context layers
/// cover the question. Hard-coded so tests can assert it survives prompt
/// construction unchanged — updating the wording requires updating both the
/// prompt builder and the tests.
pub(crate) const UNIFIED_NOT_COVERED_PHRASE: &str =
    "I don't have that in any of my current context layers.";

/// Build the unified system prompt from all four layers.
///
/// Layer ordering:
///
/// 1. Preamble + honesty guardrail (always first — the model encounters the
///    constraint before reading the grounding data).
/// 2. TECHNICAL_DOC (static, compile-time).
/// 3. USER_GUIDE (static, compile-time).
/// 4. Rules catalog (static per session, fetched once from `/api/corpus-rules`).
/// 5. Development state (per-turn, from `/api/uow` or `/api/development/context`).
/// 6. Focused finding (optional additive lens, present when the architect asked
///    about a specific finding).
///
/// The first three layers are stable across turns within a session, giving
/// Anthropic's automatic system-prompt caching a large stable prefix.
/// Layer 5 is appended last (the tail) so adding a fresh UoW snapshot does
/// not disturb the cached prefix.
pub(crate) fn unified_system_prompt(
    rules_catalog: &str,
    uow_section: &str,
    finding: Option<&FindingContext>,
) -> String {
    let not_covered = UNIFIED_NOT_COVERED_PHRASE;
    let mut p = format!(
        "You are Camerata's in-app AI assistant. You answer questions about Camerata's \
         internals (how it works, crates, modules), how to USE Camerata (onboarding, \
         scanning, rules), which governance rules exist and what they mean, and the \
         current development state of tracked stories. You answer from the four context \
         layers provided below — ONLY from those layers. CRITICAL: if a question cannot \
         be answered from any of the four layers, respond with exactly \
         \"{not_covered}\" followed by a brief statement of what IS available. \
         Never fabricate facts about Camerata's code, architecture, rules, or story \
         states that are not present in the layers. Be concise and concrete.\n\n"
    );

    // ── Layer 1a: Camerata technical reference ────────────────────────────────
    // Stable prefix — benefits from automatic system-prompt caching.
    p.push_str("=== LAYER 1: CAMERATA TECHNICAL REFERENCE ===\n");
    p.push_str(TECHNICAL_DOC);
    p.push_str("\n\n");

    // ── Layer 1b: Camerata user guide ─────────────────────────────────────────
    p.push_str("=== LAYER 1b: CAMERATA USER GUIDE ===\n");
    p.push_str(USER_GUIDE);
    p.push_str("\n\n");

    // ── Layer 2: governance rules catalog ─────────────────────────────────────
    // Also stable per session (fetched once from /api/corpus-rules).
    if !rules_catalog.trim().is_empty() {
        p.push_str(
            "=== LAYER 2: GOVERNANCE RULES CATALOG (every rule: domain · scope, alternatives) ===\n",
        );
        p.push_str(rules_catalog);
        p.push_str("\n\n");
    }

    // ── Layer 3: live development state (UoW snapshot) ────────────────────────
    // Refreshed per turn — appended as the tail so it does not disturb the
    // stable cached prefix formed by Layers 1 and 2.
    p.push_str(
        "=== LAYER 3: LIVE DEVELOPMENT STATE (all tracked stories, refreshed this turn) ===\n",
    );
    p.push_str(uow_section);
    p.push_str("\n");

    // ── Layer 4: focused finding (optional) ───────────────────────────────────
    if let Some(f) = finding {
        if !f.rule_id.is_empty() {
            p.push_str(
                "=== LAYER 4: FOCUSED FINDING (the architect is asking about this specific finding) ===\n",
            );
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
                "\nThe architect wants to understand WHY this was flagged and HOW to fix it. \
                 Answer from the gate detail and rule context in the layers above. If you need \
                 to reference the rule's rationale and it is not in the layers, say so clearly \
                 using the not-covered phrase.\n",
            );
        }
    }

    p
}

// ── network helpers ───────────────────────────────────────────────────────────

async fn fetch_models() -> Option<ModelsResp> {
    reqwest::get(format!("{}/api/models", crate::BFF_URL))
        .await
        .ok()?
        .json::<ModelsResp>()
        .await
        .ok()
}

async fn send_chat(prompt: &str, model: &str, system: &str) -> Option<ChatResp> {
    let body = serde_json::json!({
        "prompt": prompt,
        "model": model,
        "system": system,
    });
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

// ── component ─────────────────────────────────────────────────────────────────

/// Props for `ChatBubble`. The optional `finding` prop wires the
/// "Ask about this finding" path: when present the assistant receives an
/// additive focused-finding section in the system prompt and the panel
/// opens automatically.
#[derive(Props, Clone, PartialEq)]
pub struct ChatBubbleProps {
    /// When set, the chat opens focused on this specific finding (Layer 4).
    /// The panel opens automatically when this prop changes to a non-empty
    /// finding.
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

    let mut turns = use_signal(Vec::<Turn>::new);
    let mut draft = use_signal(String::new);
    let mut sending = use_signal(|| false);

    // Layer 2: rules catalog — fetched once per session, fed into the static
    // prefix of the unified system prompt.
    let rules_res = use_resource(fetch_rules_catalog);
    let rules_catalog = rules_res.read().clone().flatten().unwrap_or_default();

    // Layer 3: UoW snapshot — fetched per turn (when the panel is open and a
    // message is sent). Also pre-fetched when the panel opens so the "what this
    // assistant can see" strip can show a story count without the user needing
    // to send a message first.
    let uow_res = use_resource(fetch_uow_snapshot);
    let uow_snaps: Vec<UowSnapshot> = uow_res.read().clone().flatten().unwrap_or_default();

    // Layer 4: track the last-injected finding by a stable key to avoid
    // re-opening/re-clearing on unrelated re-renders.
    let mut last_injected_finding = use_signal(|| Option::<String>::None);
    if let Some(ref f) = props.finding {
        if !f.rule_id.is_empty() {
            let key = format!("{}\u{0}{}\u{0}{}", f.rule_id, f.path, f.line);
            if last_injected_finding() != Some(key.clone()) {
                last_injected_finding.set(Some(key));
                open.set(true);
                turns.write().clear();
            }
        }
    }

    // The finding in scope for the current chat session. Set when a new finding
    // is injected via props; cleared when the user explicitly starts a new chat.
    let mut active_finding: Signal<Option<FindingContext>> = use_signal(|| None);
    if let Some(ref f) = props.finding {
        if !f.rule_id.is_empty() && *active_finding.read() != Some(f.clone()) {
            active_finding.set(Some(f.clone()));
        }
    }

    // Pre-build the static prefix (Layers 1+2) once — this is what Anthropic
    // caches automatically across turns. The UoW tail (Layer 3) is assembled
    // fresh at send time so it always reflects the latest snapshot.
    let static_prefix_catalog = rules_catalog.clone();
    // Clone for the two send closures (onkeydown + onclick each move-capture).
    let catalog_kd = rules_catalog.clone();
    let catalog_btn = rules_catalog;

    rsx! {
        // Floating launcher.
        button {
            style: "position:fixed;bottom:1.5rem;right:1.5rem;z-index:1000;\
                    width:3rem;height:3rem;border-radius:50%;border:none;\
                    background:#2563eb;color:#fff;font-size:1.25rem;\
                    cursor:pointer;box-shadow:0 2px 8px rgba(0,0,0,.25);\
                    display:flex;align-items:center;justify-content:center;",
            title: "Camerata assistant (AI)",
            onclick: move |_| open.toggle(),
            if open() { "✕" } else { "💬" }
        }

        if open() {
            div {
                style: "position:fixed;bottom:5rem;right:1.5rem;z-index:999;\
                        width:28rem;max-height:80vh;display:flex;flex-direction:column;\
                        background:#fff;border:1px solid #e2e8f0;border-radius:.75rem;\
                        box-shadow:0 8px 32px rgba(0,0,0,.18);overflow:hidden;",

                // ── header ──────────────────────────────────────────────────
                div {
                    style: "display:flex;align-items:center;justify-content:space-between;\
                            padding:.75rem 1rem;border-bottom:1px solid #e2e8f0;\
                            background:#f8fafc;",
                    span {
                        style: "font-weight:600;font-size:.95rem;color:#1e293b;",
                        "Camerata assistant"
                    }
                    div {
                        style: "display:flex;align-items:center;gap:.5rem;",
                        select {
                            style: "font-size:.8rem;padding:.2rem .4rem;border:1px solid #cbd5e1;\
                                    border-radius:.25rem;background:#fff;color:#334155;",
                            value: "{model}",
                            onchange: move |e| model.set(e.value()),
                            if let Some(m) = &models {
                                for opt in m.models.iter() {
                                    option { key: "{opt.id}", value: "{opt.id}", "{opt.label}" }
                                }
                            }
                        }
                        if !backend.is_empty() {
                            span {
                                style: "font-size:.7rem;color:#64748b;background:#f1f5f9;\
                                        padding:.1rem .4rem;border-radius:.2rem;",
                                "{backend}"
                            }
                        }
                    }
                }

                // ── "what this assistant can see" affordance ─────────────
                div {
                    style: "padding:.5rem 1rem;border-bottom:1px solid #e2e8f0;\
                            background:#f0f9ff;font-size:.75rem;color:#0369a1;",
                    div {
                        style: "font-weight:600;margin-bottom:.2rem;",
                        "What this assistant can see:"
                    }
                    div { style: "display:flex;flex-direction:column;gap:.1rem;",
                        div {
                            style: "display:flex;align-items:center;gap:.4rem;",
                            span { style: "color:#0284c7;", "●" }
                            span { "Technical reference (docs/TECHNICAL.md)" }
                        }
                        div {
                            style: "display:flex;align-items:center;gap:.4rem;",
                            span { style: "color:#0284c7;", "●" }
                            span { "User guide (docs/USER_GUIDE.md)" }
                        }
                        div {
                            style: "display:flex;align-items:center;gap:.4rem;",
                            span {
                                style: if rules_catalog_loaded(&static_prefix_catalog) {
                                    "color:#16a34a;"
                                } else {
                                    "color:#94a3b8;"
                                },
                                "●"
                            }
                            span {
                                if rules_catalog_loaded(&static_prefix_catalog) {
                                    "Governance rules catalog (live)"
                                } else {
                                    "Governance rules catalog (loading…)"
                                }
                            }
                        }
                        {
                            let uow_label = if !uow_snaps.is_empty() {
                                format!("Development state ({} stories, refreshed this turn)", uow_snaps.len())
                            } else {
                                "Development state (loading\u{2026})".to_string()
                            };
                            let uow_dot_style = if !uow_snaps.is_empty() { "color:#16a34a;" } else { "color:#94a3b8;" };
                            rsx! {
                                div {
                                    style: "display:flex;align-items:center;gap:.4rem;",
                                    span { style: "{uow_dot_style}", "\u{25cf}" }
                                    span { "{uow_label}" }
                                }
                            }
                        }
                        // Layer 4: only shown when a finding is focused.
                        if let Some(ref f) = *active_finding.read() {
                            if !f.rule_id.is_empty() {
                                div {
                                    style: "display:flex;align-items:center;gap:.4rem;\
                                            margin-top:.2rem;padding:.2rem .4rem;\
                                            background:#fefce8;border-radius:.25rem;\
                                            border:1px solid #fbbf24;",
                                    span { style: "color:#d97706;", "◆" }
                                    span {
                                        style: "font-weight:500;",
                                        "Focused finding: "
                                    }
                                    span { style: "font-family:monospace;", "{f.rule_id}" }
                                    span { style: "color:#64748b;", " {f.path}:{f.line}" }
                                }
                            }
                        }
                    }
                }

                // ── transcript ──────────────────────────────────────────────
                div {
                    style: "flex:1;overflow-y:auto;padding:.75rem 1rem;display:flex;\
                            flex-direction:column;gap:.5rem;min-height:8rem;",
                    if turns().is_empty() {
                        p {
                            style: "color:#94a3b8;font-size:.85rem;text-align:center;\
                                    margin:auto;",
                            if active_finding.read().as_ref().map(|f| !f.rule_id.is_empty()).unwrap_or(false) {
                                "Ask why this finding was flagged, how to fix it, or what the rule means…"
                            } else {
                                "Ask about Camerata's internals, rules, how-to steps, or the state of any tracked story."
                            }
                        }
                    }
                    for (i, t) in turns().iter().enumerate() {
                        div {
                            key: "{i}",
                            style: if t.role == "you" {
                                "align-self:flex-end;max-width:80%;background:#2563eb;\
                                 color:#fff;border-radius:.5rem .5rem 0 .5rem;\
                                 padding:.5rem .75rem;font-size:.875rem;"
                            } else {
                                "align-self:flex-start;max-width:90%;background:#f1f5f9;\
                                 color:#1e293b;border-radius:.5rem .5rem .5rem 0;\
                                 padding:.5rem .75rem;font-size:.875rem;"
                            },
                            if t.role == "ai" {
                                div {
                                    style: "line-height:1.55;",
                                    dangerous_inner_html: md_to_html(&t.text)
                                }
                            } else {
                                "{t.text}"
                            }
                        }
                    }
                    if sending() {
                        div {
                            style: "align-self:flex-start;background:#f1f5f9;color:#94a3b8;\
                                    border-radius:.5rem;padding:.5rem .75rem;font-size:.875rem;",
                            "thinking…"
                        }
                    }
                }

                // ── compose bar ─────────────────────────────────────────────
                div {
                    style: "display:flex;gap:.5rem;padding:.75rem 1rem;\
                            border-top:1px solid #e2e8f0;background:#f8fafc;",
                    textarea {
                        style: "flex:1;resize:none;border:1px solid #cbd5e1;border-radius:.375rem;\
                                padding:.5rem .75rem;font-size:.875rem;font-family:inherit;\
                                line-height:1.4;outline:none;background:#fff;color:#1e293b;",
                        rows: "2",
                        placeholder: "Ask anything about Camerata… (Enter to send, Shift+Enter for newline)",
                        value: "{draft}",
                        onkeydown: {
                            let catalog_kd2 = catalog_kd.clone();
                            let uow_snaps_kd = uow_snaps.clone();
                            let finding_kd = active_finding.read().clone();
                            move |e: Event<KeyboardData>| {
                                if e.key() == Key::Enter && !e.modifiers().shift() {
                                    e.prevent_default();
                                    let prompt = draft().trim().to_string();
                                    if prompt.is_empty() || sending() {
                                        return;
                                    }
                                    let mdl = model();
                                    let uow_sec = render_uow_section(&uow_snaps_kd);
                                    let sys = unified_system_prompt(
                                        &catalog_kd2,
                                        &uow_sec,
                                        finding_kd.as_ref(),
                                    );
                                    turns.write().push(Turn { role: "you", text: prompt.clone() });
                                    draft.set(String::new());
                                    sending.set(true);
                                    spawn(async move {
                                        let reply = send_chat(&prompt, &mdl, &sys).await;
                                        sending.set(false);
                                        match reply {
                                            Some(r) if !r.text.trim().is_empty() => {
                                                turns.write().push(Turn { role: "ai", text: r.text });
                                            }
                                            _ => turns.write().push(Turn {
                                                role: "ai",
                                                text: "(no response — is the model backend \
                                                       reachable? CLI needs `claude` on PATH; \
                                                       API needs ANTHROPIC_API_KEY.)"
                                                    .to_string(),
                                            }),
                                        }
                                    });
                                }
                            }
                        },
                        oninput: move |e| draft.set(e.value()),
                    }
                    div {
                        style: "display:flex;flex-direction:column;gap:.25rem;",
                        button {
                            style: "padding:.5rem .875rem;background:#2563eb;color:#fff;\
                                    border:none;border-radius:.375rem;font-size:.875rem;\
                                    cursor:pointer;white-space:nowrap;\
                                    opacity: if sending() || draft().trim().is_empty() { \"0.5\" } else { \"1\" };",
                            disabled: sending() || draft().trim().is_empty(),
                            onclick: {
                                let catalog_btn2 = catalog_btn.clone();
                                let uow_snaps_btn = uow_snaps.clone();
                                let finding_btn = active_finding.read().clone();
                                move |_| {
                                    let prompt = draft().trim().to_string();
                                    if prompt.is_empty() || sending() {
                                        return;
                                    }
                                    let mdl = model();
                                    let uow_sec = render_uow_section(&uow_snaps_btn);
                                    let sys = unified_system_prompt(
                                        &catalog_btn2,
                                        &uow_sec,
                                        finding_btn.as_ref(),
                                    );
                                    turns.write().push(Turn { role: "you", text: prompt.clone() });
                                    draft.set(String::new());
                                    sending.set(true);
                                    spawn(async move {
                                        let reply = send_chat(&prompt, &mdl, &sys).await;
                                        sending.set(false);
                                        match reply {
                                            Some(r) if !r.text.trim().is_empty() => {
                                                turns.write().push(Turn { role: "ai", text: r.text });
                                            }
                                            _ => turns.write().push(Turn {
                                                role: "ai",
                                                text: "(no response — is the model backend reachable?)"
                                                    .to_string(),
                                            }),
                                        }
                                    });
                                }
                            },
                            "Send"
                        }
                        button {
                            style: "padding:.25rem .5rem;font-size:.75rem;color:#64748b;\
                                    background:none;border:1px solid #e2e8f0;border-radius:.25rem;\
                                    cursor:pointer;",
                            title: "Clear conversation",
                            onclick: move |_| {
                                turns.write().clear();
                                active_finding.set(None);
                                last_injected_finding.set(None);
                            },
                            "New chat"
                        }
                    }
                }
            }
        }
    }
}

/// Whether the rules catalog has been loaded (non-empty).
fn rules_catalog_loaded(catalog: &str) -> bool {
    !catalog.trim().is_empty()
}

// ── unit tests — prompt assembly + grounding ─────────────────────────────────
//
// These tests cover the STATIC side of the unified prompt (text construction)
// and do NOT make live model calls. `include_str!` bakes the docs in at
// compile time, so these tests also guard that the doc files are present and
// non-empty.

#[cfg(test)]
mod tests {
    use super::{
        render_uow_section, unified_system_prompt, FindingContext, GateProvenanceLite,
        UowSnapshot, TECHNICAL_DOC, UNIFIED_NOT_COVERED_PHRASE, USER_GUIDE,
    };

    // ── fixtures ─────────────────────────────────────────────────────────────

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

    fn make_uow(story_id: &str, stage: &str, signed_off: bool) -> UowSnapshot {
        UowSnapshot {
            story_id: story_id.to_string(),
            stage: stage.to_string(),
            sign_off: if signed_off {
                Some(serde_json::json!({"by": "zach", "ts": "2026-06-21T00:00:00Z", "run_id": "r1"}))
            } else {
                None
            },
            updated: "2026-06-21T10:00:00Z".to_string(),
            gate_provenance: None,
        }
    }

    fn make_uow_with_gate(story_id: &str, deny_count: u64, rules: Vec<&str>) -> UowSnapshot {
        UowSnapshot {
            story_id: story_id.to_string(),
            stage: "awaiting_qa".to_string(),
            sign_off: None,
            updated: "2026-06-21T10:00:00Z".to_string(),
            gate_provenance: Some(GateProvenanceLite {
                allow_count: 5,
                deny_count,
                total_bounces: deny_count,
                rules_fired: rules.into_iter().map(|s| s.to_string()).collect(),
                mode: "scripted".to_string(),
            }),
        }
    }

    // ── compile-time doc constants ────────────────────────────────────────────

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

    #[test]
    fn user_guide_constant_is_non_empty_and_contains_known_content() {
        assert!(
            !USER_GUIDE.is_empty(),
            "USER_GUIDE is empty — include_str! path likely broken"
        );
        assert!(
            USER_GUIDE.contains("Camerata"),
            "USER_GUIDE does not mention 'Camerata' — file may be wrong"
        );
    }

    // ── UNIFIED_NOT_COVERED_PHRASE constant ──────────────────────────────────

    #[test]
    fn unified_not_covered_phrase_is_well_formed() {
        assert!(
            !UNIFIED_NOT_COVERED_PHRASE.is_empty(),
            "UNIFIED_NOT_COVERED_PHRASE should not be empty"
        );
        assert!(
            !UNIFIED_NOT_COVERED_PHRASE.starts_with(' '),
            "UNIFIED_NOT_COVERED_PHRASE should not start with a space"
        );
        assert!(
            UNIFIED_NOT_COVERED_PHRASE.chars().any(|c| c.is_alphabetic()),
            "UNIFIED_NOT_COVERED_PHRASE should contain at least one letter"
        );
    }

    // ── unified_system_prompt — structural shape ──────────────────────────────

    #[test]
    fn unified_prompt_contains_technical_reference_layer() {
        let prompt = unified_system_prompt("", "No stories.\n", None);
        assert!(
            prompt.contains("=== LAYER 1: CAMERATA TECHNICAL REFERENCE ==="),
            "Unified prompt missing LAYER 1 header"
        );
        assert!(
            prompt.contains(TECHNICAL_DOC),
            "Unified prompt does not contain TECHNICAL_DOC content"
        );
    }

    #[test]
    fn unified_prompt_contains_user_guide_layer() {
        let prompt = unified_system_prompt("", "No stories.\n", None);
        assert!(
            prompt.contains("=== LAYER 1b: CAMERATA USER GUIDE ==="),
            "Unified prompt missing LAYER 1b header"
        );
        assert!(
            prompt.contains(USER_GUIDE),
            "Unified prompt does not contain USER_GUIDE content"
        );
    }

    #[test]
    fn unified_prompt_includes_rules_catalog_when_present() {
        let catalog = "- RULE-1 [security · repo-local]: no hardcoded secrets\n";
        let prompt = unified_system_prompt(catalog, "No stories.\n", None);
        assert!(
            prompt.contains("=== LAYER 2: GOVERNANCE RULES CATALOG"),
            "Unified prompt missing LAYER 2 header"
        );
        assert!(
            prompt.contains(catalog),
            "Unified prompt does not contain the rules catalog"
        );
    }

    #[test]
    fn unified_prompt_omits_rules_catalog_when_empty() {
        let prompt = unified_system_prompt("", "No stories.\n", None);
        assert!(
            !prompt.contains("=== LAYER 2: GOVERNANCE RULES CATALOG"),
            "Unified prompt should omit LAYER 2 header when catalog is empty"
        );
    }

    #[test]
    fn unified_prompt_omits_rules_catalog_for_whitespace_only_input() {
        let prompt = unified_system_prompt("   \n\t  ", "No stories.\n", None);
        assert!(
            !prompt.contains("=== LAYER 2: GOVERNANCE RULES CATALOG"),
            "Unified prompt should omit LAYER 2 header for whitespace-only catalog"
        );
    }

    #[test]
    fn unified_prompt_contains_layer3_dev_state_header() {
        let prompt = unified_system_prompt("", "No stories.\n", None);
        assert!(
            prompt.contains("=== LAYER 3: LIVE DEVELOPMENT STATE"),
            "Unified prompt missing LAYER 3 header"
        );
    }

    // ── honesty guardrail ─────────────────────────────────────────────────────

    #[test]
    fn unified_prompt_contains_not_covered_phrase() {
        let prompt = unified_system_prompt("", "No stories.\n", None);
        assert!(
            prompt.contains(UNIFIED_NOT_COVERED_PHRASE),
            "Unified prompt missing the not-covered phrase: {:?}",
            UNIFIED_NOT_COVERED_PHRASE
        );
    }

    #[test]
    fn unified_prompt_not_covered_phrase_survives_catalog_and_uow() {
        let catalog = "- RULE-1 [security · repo-local]: no hardcoded secrets\n";
        let uow = "- CAM-1: stage=development, gate=no run yet, sign-off=not signed off\n";
        let prompt = unified_system_prompt(catalog, uow, None);
        assert!(
            prompt.contains(UNIFIED_NOT_COVERED_PHRASE),
            "Unified prompt missing the not-covered phrase after adding catalog + uow"
        );
    }

    #[test]
    fn unified_prompt_not_covered_phrase_marked_critical() {
        let prompt = unified_system_prompt("", "No stories.\n", None);
        assert!(
            prompt.contains("CRITICAL"),
            "Unified prompt should mark the not-covered guardrail as CRITICAL"
        );
    }

    /// The not-covered constraint must appear BEFORE the first layer header,
    /// so the model encounters the constraint before reading grounding data.
    #[test]
    fn unified_prompt_not_covered_phrase_appears_before_first_layer() {
        let prompt = unified_system_prompt("", "No stories.\n", None);
        let phrase_pos = prompt
            .find(UNIFIED_NOT_COVERED_PHRASE)
            .expect("UNIFIED_NOT_COVERED_PHRASE not found");
        let layer1_pos = prompt
            .find("=== LAYER 1: CAMERATA TECHNICAL REFERENCE ===")
            .expect("LAYER 1 header not found");
        assert!(
            phrase_pos < layer1_pos,
            "not-covered phrase must appear before LAYER 1 header \
             (phrase at {phrase_pos}, layer at {layer1_pos})"
        );
    }

    // ── Layer 3 ordering: UoW tail after static prefix ────────────────────────

    /// Layer 3 (UoW, refreshed per turn) must appear AFTER Layers 1+2 (static,
    /// cached prefix). This is the structural guarantee that adding a fresh UoW
    /// snapshot does not disturb the cached prefix.
    #[test]
    fn layer3_appears_after_layers_1_and_2() {
        let catalog = "- RULE-1 [security · repo-local]: foo\n";
        let uow = "- CAM-1: stage=development, gate=no run yet\n";
        let prompt = unified_system_prompt(catalog, uow, None);
        let layer1_pos = prompt
            .find("=== LAYER 1: CAMERATA TECHNICAL REFERENCE ===")
            .expect("LAYER 1 header not found");
        let layer2_pos = prompt
            .find("=== LAYER 2: GOVERNANCE RULES CATALOG")
            .expect("LAYER 2 header not found");
        let layer3_pos = prompt
            .find("=== LAYER 3: LIVE DEVELOPMENT STATE")
            .expect("LAYER 3 header not found");
        assert!(
            layer1_pos < layer2_pos,
            "LAYER 1 must precede LAYER 2 (found at {layer1_pos} and {layer2_pos})"
        );
        assert!(
            layer2_pos < layer3_pos,
            "LAYER 2 must precede LAYER 3 (found at {layer2_pos} and {layer3_pos})"
        );
    }

    // ── Layer 4: focused finding ──────────────────────────────────────────────

    #[test]
    fn unified_prompt_with_finding_includes_focused_finding_section() {
        let f = make_finding();
        let prompt = unified_system_prompt("", "No stories.\n", Some(&f));
        assert!(
            prompt.contains("=== LAYER 4: FOCUSED FINDING"),
            "Prompt with finding missing LAYER 4 header"
        );
        assert!(
            prompt.contains("SEC-NO-HARDCODED-SECRETS-1"),
            "Prompt with finding must include the rule id"
        );
        assert!(
            prompt.contains("high"),
            "Prompt with finding must include severity"
        );
        assert!(
            prompt.contains("src/main.rs"),
            "Prompt with finding must include file path"
        );
        assert!(
            prompt.contains("Hardcoded password literal found"),
            "Prompt with finding must include the gate detail"
        );
    }

    #[test]
    fn unified_prompt_without_finding_has_no_layer4() {
        let prompt = unified_system_prompt("", "No stories.\n", None);
        assert!(
            !prompt.contains("=== LAYER 4: FOCUSED FINDING"),
            "Prompt without finding must not include LAYER 4"
        );
    }

    #[test]
    fn unified_prompt_with_empty_finding_has_no_layer4() {
        let empty = FindingContext::default();
        let prompt = unified_system_prompt("", "No stories.\n", Some(&empty));
        assert!(
            !prompt.contains("=== LAYER 4: FOCUSED FINDING"),
            "Prompt with empty finding must not include LAYER 4"
        );
    }

    #[test]
    fn unified_prompt_with_finding_retains_not_covered_guardrail() {
        let f = make_finding();
        let prompt = unified_system_prompt("", "No stories.\n", Some(&f));
        assert!(
            prompt.contains(UNIFIED_NOT_COVERED_PHRASE),
            "Finding-scoped prompt must retain the not-covered guardrail"
        );
    }

    /// Layer 4 (finding) must appear AFTER Layer 3 (UoW), so it is the
    /// innermost-focused additive context (closest to the conversation).
    #[test]
    fn layer4_appears_after_layer3() {
        let f = make_finding();
        let uow = "- CAM-1: stage=development\n";
        let prompt = unified_system_prompt("", uow, Some(&f));
        let layer3_pos = prompt
            .find("=== LAYER 3: LIVE DEVELOPMENT STATE")
            .expect("LAYER 3 header not found");
        let layer4_pos = prompt
            .find("=== LAYER 4: FOCUSED FINDING")
            .expect("LAYER 4 header not found");
        assert!(
            layer3_pos < layer4_pos,
            "LAYER 3 must precede LAYER 4 (found at {layer3_pos} and {layer4_pos})"
        );
    }

    // ── render_uow_section ────────────────────────────────────────────────────

    #[test]
    fn render_uow_section_empty_input() {
        let s = render_uow_section(&[]);
        assert!(
            s.contains("No development stories"),
            "Empty UoW list should explain no stories"
        );
    }

    #[test]
    fn render_uow_section_single_story_fields() {
        let snap = make_uow("CAM-1", "development", false);
        let s = render_uow_section(&[snap]);
        assert!(s.contains("CAM-1"), "Should include story id");
        assert!(s.contains("stage=development"), "Should include stage");
        assert!(s.contains("gate=no run yet"), "Should include gate status");
        assert!(s.contains("not signed off"), "Should include sign-off state");
        assert!(s.contains("2026-06-21"), "Should include last-activity timestamp");
    }

    #[test]
    fn render_uow_section_signed_off_story() {
        let snap = make_uow("CAM-2", "signed_off", true);
        let s = render_uow_section(&[snap]);
        assert!(s.contains("signed-off"), "Should indicate signed-off state");
        assert!(s.contains("stage=signed_off"), "Should include stage");
    }

    #[test]
    fn render_uow_section_gate_blocked_surfaces_rules() {
        let snap = make_uow_with_gate("CAM-3", 2, vec!["SEC-1", "ARCH-1"]);
        let s = render_uow_section(&[snap]);
        assert!(
            s.contains("gate=gate blocked"),
            "Should label gate as blocked"
        );
        assert!(
            s.contains("SEC-1"),
            "Should surface rules that fired"
        );
        assert!(
            s.contains("ARCH-1"),
            "Should surface all rules that fired"
        );
    }

    #[test]
    fn render_uow_section_gate_passed_no_rules_surfaced() {
        let snap = make_uow_with_gate("CAM-4", 0, vec![]);
        let s = render_uow_section(&[snap]);
        assert!(
            s.contains("gate=gate passed"),
            "Should label gate as passed when deny_count == 0"
        );
        // No "rules that blocked" line when deny_count is zero.
        assert!(
            !s.contains("rules that blocked"),
            "Should not surface rules when gate passed"
        );
    }

    #[test]
    fn render_uow_section_multiple_stories() {
        let snaps = vec![
            make_uow("CAM-1", "intake", false),
            make_uow("CAM-2", "awaiting_qa", false),
            make_uow("CAM-3", "signed_off", true),
        ];
        let s = render_uow_section(&snaps);
        assert!(s.contains("CAM-1"), "Should include story 1");
        assert!(s.contains("CAM-2"), "Should include story 2");
        assert!(s.contains("CAM-3"), "Should include story 3");
    }

    #[test]
    fn render_uow_section_caps_at_100_stories() {
        let snaps: Vec<UowSnapshot> = (0..150)
            .map(|i| make_uow(&format!("CAM-{i}"), "intake", false))
            .collect();
        let s = render_uow_section(&snaps);
        // CAM-99 is the 100th (index 99, 0-based), CAM-100 is the 101st.
        assert!(s.contains("CAM-99"), "Should include 100th story");
        assert!(!s.contains("CAM-100"), "Should cap at 100 stories");
    }

    // ── rules_catalog_loaded helper ───────────────────────────────────────────

    #[test]
    fn rules_catalog_loaded_returns_false_for_empty() {
        assert!(!super::rules_catalog_loaded(""));
        assert!(!super::rules_catalog_loaded("   \n\t  "));
    }

    #[test]
    fn rules_catalog_loaded_returns_true_for_content() {
        assert!(super::rules_catalog_loaded("- RULE-1: foo\n"));
    }
}
